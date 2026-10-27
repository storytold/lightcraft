//! iOS: pick photos from the photo library (PHPicker) for import.
//!
//! The picker is asynchronous. [`present`] shows it and returns at once (as the web build's picker
//! does). Each chosen photo is copied into `<library>/Picked`, and once the whole selection has
//! arrived, [`take_ready`] hands its paths to the shell, which opens the usual Import review.
//!
//! The engine has no HEIC decoder (no permissively licensed pure-Rust HEVC yet), so each photo
//! is fetched as its raw file when it has one, as JPEG/PNG/TIFF/WebP when it's already one of
//! those, and otherwise as JPEG, which iOS converts to.
//!
//! objc2-photos-ui 0.3 declares `PHPickerViewController` for macOS only (an NSViewController),
//! so the UIKit class is declared here.
#![allow(unsafe_code)] // Objective-C classes, messages and blocks

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use block2::RcBlock;
use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadOnly, define_class, extern_class, extern_methods, msg_send};
use objc2_foundation::{NSArray, NSError, NSItemProvider, NSObjectProtocol, NSString, NSURL};
use objc2_photos_ui::{PHPickerConfiguration, PHPickerConfigurationAssetRepresentationMode, PHPickerFilter, PHPickerResult};
use objc2_ui_kit::{UIResponder, UIViewController};

extern_class!(
    /// PhotosUI's photo picker (iOS 14+).
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PHPickerViewController"]
    struct PickerController;
);

impl PickerController {
    extern_methods!(
        #[unsafe(method(initWithConfiguration:))]
        fn init_with_configuration(this: Allocated<Self>, configuration: &PHPickerConfiguration) -> Retained<Self>;

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>);
    );
}

define_class!(
    /// The picker's delegate: gets the selection, copies each photo in the background.
    #[unsafe(super(NSObject))]
    #[name = "LCPickerDelegate"]
    #[thread_kind = MainThreadOnly]
    struct PickerDelegate;

    unsafe impl NSObjectProtocol for PickerDelegate {}

    impl PickerDelegate {
        #[unsafe(method(picker:didFinishPicking:))]
        fn did_finish_picking(&self, picker: &PickerController, results: &NSArray<PHPickerResult>) {
            // SAFETY: dismissing the picker that called us, from the main thread
            let _: () = unsafe { msg_send![picker, dismissViewControllerAnimated: true, completion: std::ptr::null::<AnyObject>()] };
            DELEGATE.with_borrow_mut(|d| *d = None);
            let results: Vec<_> = results.iter().collect();
            if results.is_empty() {
                return;
            }
            set_batch(results.len());
            for result in results {
                // SAFETY: a picker result's item provider
                fetch(&*unsafe { result.itemProvider() });
            }
        }
    }
);

thread_local! {
    // UIKit holds the delegate weakly: keep it while the picker is up
    static DELEGATE: std::cell::RefCell<Option<Retained<PickerDelegate>>> = const { std::cell::RefCell::new(None) };
}

/// The selection being copied: how many photos are still to come, and the paths so far.
static BATCH: Mutex<(usize, Vec<String>)> = Mutex::new((0, Vec::new()));
/// Selections whose photos have all arrived.
static READY: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
/// Where picked photos are copied, and the context to repaint when they're in.
static TARGET: OnceLock<(PathBuf, egui::Context)> = OnceLock::new();

/// Copy picked photos into `dir`; repaint `ctx` when a selection is ready.
pub fn init(dir: PathBuf, ctx: egui::Context) {
    let _ = TARGET.set((dir, ctx));
}

/// Show the photo picker over the app (from the main thread).
pub fn present() {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let Some(root) = crate::ios_scene::root_view_controller() else {
        log::warn!("photo picker: no view controller to present from");
        return;
    };
    // SAFETY: plain configuration of a fresh object
    let config = unsafe {
        let c = PHPickerConfiguration::new();
        c.setSelectionLimit(0); // any number
        c.setFilter(Some(&PHPickerFilter::imagesFilter()));
        // the file as stored (raw stays raw); HEIC is converted when JPEG is asked for
        c.setPreferredAssetRepresentationMode(PHPickerConfigurationAssetRepresentationMode::Automatic);
        c
    };
    let picker = PickerController::init_with_configuration(PickerController::alloc(mtm), &config);
    // SAFETY: `init` on a freshly allocated instance of our own NSObject subclass, on the main thread
    let delegate: Retained<PickerDelegate> = unsafe { msg_send![PickerDelegate::alloc(mtm), init] };
    picker.set_delegate(Some(&delegate));
    DELEGATE.with_borrow_mut(|d| *d = Some(delegate));
    // SAFETY: presenting a view controller from the root one, on the main thread
    let _: () = unsafe { msg_send![&*root, presentViewController: &*picker, animated: true, completion: std::ptr::null::<AnyObject>()] };
}

/// Paths of each selection whose photos have all been copied in, oldest first.
pub fn take_ready() -> Vec<Vec<String>> {
    READY.lock().map(|mut r| std::mem::take(&mut *r)).unwrap_or_default()
}

fn set_batch(n: usize) {
    if let Ok(mut b) = BATCH.lock() {
        *b = (n, Vec::new());
    }
}

/// One photo done (`path`, or `None` when it couldn't be fetched).
fn arrived(path: Option<String>) {
    let Ok(mut b) = BATCH.lock() else { return };
    b.0 = b.0.saturating_sub(1);
    b.1.extend(path);
    if b.0 == 0 {
        let paths = std::mem::take(&mut b.1);
        if !paths.is_empty()
            && let Ok(mut r) = READY.lock()
        {
            r.push(paths);
        }
        if let Some((_, ctx)) = TARGET.get() {
            ctx.request_repaint();
        }
    }
}

/// The representation to ask for: the file as it is when the engine reads it, else JPEG.
fn type_for(provider: &NSItemProvider) -> &'static str {
    const READABLE: [&str; 5] = ["public.camera-raw-image", "public.jpeg", "public.png", "public.tiff", "org.webmproject.webp"];
    READABLE.into_iter().find(|t| provider.hasItemConformingToTypeIdentifier(&NSString::from_str(t))).unwrap_or("public.jpeg")
}

/// Copy the photo `provider` holds into the target folder (UIKit calls back on a background queue).
fn fetch(provider: &NSItemProvider) {
    let ty = NSString::from_str(type_for(provider));
    let done = RcBlock::new(|url: *mut NSURL, _error: *mut NSError| {
        // SAFETY: UIKit passes a file URL (or null with an error); the file goes when we return
        let path = unsafe { url.as_ref() }.and_then(|u| u.path()).map(|p| PathBuf::from(p.to_string()));
        arrived(path.and_then(|p| copy_in(&p)));
    });
    // SAFETY: the type is a UTI string, the block matches the documented handler
    let _ = unsafe { provider.loadFileRepresentationForTypeIdentifier_completionHandler(&ty, &done) };
}

/// Copy a temporary file UIKit handed over into the target folder; its new path.
fn copy_in(from: &std::path::Path) -> Option<String> {
    let (dir, _) = TARGET.get()?;
    std::fs::create_dir_all(dir).ok()?;
    let name = from.file_name()?.to_string_lossy().to_string();
    // a free name: picking the same photo again doesn't overwrite the first copy
    let mut to = dir.join(&name);
    let mut n = 1;
    while to.exists() {
        let stem = std::path::Path::new(&name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        let ext = std::path::Path::new(&name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
        to = dir.join(format!("{stem}-{n}{ext}"));
        n += 1;
    }
    match std::fs::copy(from, &to) {
        Ok(_) => Some(to.to_string_lossy().to_string()),
        Err(e) => {
            log::warn!("photo picker: copying {}: {e}", from.display());
            None
        }
    }
}
