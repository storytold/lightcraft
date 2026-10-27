//! iOS: the Files app's document picker (UIDocumentPickerViewController) for the commands that
//! open files or a folder: Import from Folder…, Find Missing Photos…, Locate…, Import Presets…,
//! Import Keywords…, Auto-Tag from Tracklog…, a watermark graphic, a face model. Files and folders
//! can be on iCloud Drive, On My iPad, another app's storage, a USB drive or a file server.
//!
//! It answers the UI's background [`Picker`](lightcraft_ui_egui::pick::Picker): [`pick`] shows
//! the picker and returns a channel at once, and the UI runs the command again with the answer
//! when it arrives (`pick::poll`). Files are opened as copies (UIKit puts them in the app's
//! `tmp/`), so they stay readable; the shell moves photos among them into the library
//! (`settle_picked`). A folder outside the app's sandbox can only be read while the system grants
//! access to it, so access starts as it is picked and lasts until the app quits (or [`KEEP`]
//! newer folders were picked); the import review starts on Copy (`import_copies`), so the library
//! keeps its own copies.
#![allow(unsafe_code)] // Objective-C classes, messages and the runtime

use std::sync::mpsc::{Sender, channel};

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::{MainThreadOnly, define_class, extern_class, extern_methods, msg_send};
use objc2_foundation::{NSArray, NSObjectProtocol, NSString, NSURL};
use objc2_ui_kit::{UIResponder, UIViewController};

use lightcraft_ui_egui::pick::{PickKind, PickRequest};

/// How many picked folders stay readable at once (each holds a sandbox extension).
const KEEP: usize = 16;

extern_class!(
    /// UIKit's document picker.
    #[unsafe(super(UIViewController, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "UIDocumentPickerViewController"]
    struct DocumentPicker;
);

impl DocumentPicker {
    extern_methods!(
        /// iOS 14+: `types` are `UTType`s; `as_copy` opens copies in the app's `tmp/`.
        #[unsafe(method(initForOpeningContentTypes:asCopy:))]
        fn init_for_opening(this: Allocated<Self>, types: &NSArray<AnyObject>, as_copy: bool) -> Retained<Self>;

        #[unsafe(method(setDelegate:))]
        fn set_delegate(&self, delegate: Option<&AnyObject>);

        #[unsafe(method(setAllowsMultipleSelection:))]
        fn set_allows_multiple_selection(&self, allows: bool);
    );
}

define_class!(
    /// The picker's delegate: hands the chosen files or folder to the UI.
    #[unsafe(super(NSObject))]
    #[name = "LCDocumentPickerDelegate"]
    #[thread_kind = MainThreadOnly]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    impl Delegate {
        #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
        fn did_pick(&self, _picker: &DocumentPicker, urls: &NSArray<NSURL>) {
            let folder = UP.with_borrow(|u| u.as_ref().is_some_and(|u| u.folder));
            let paths: Vec<String> = if folder {
                urls.firstObject()
                    .and_then(|url| {
                        // SAFETY: a URL the picker handed over; balanced by `stopAccessing…` in `keep`
                        let granted: Bool = unsafe { msg_send![&*url, startAccessingSecurityScopedResource] };
                        let path = url.path().map(|p| p.to_string());
                        if granted.as_bool() {
                            keep(url);
                        }
                        path
                    })
                    .into_iter()
                    .collect()
            } else {
                // copies in the app's own storage: readable without a grant
                urls.iter().filter_map(|url| url.path().map(|p| p.to_string())).collect()
            };
            answer(paths);
        }

        #[unsafe(method(documentPickerWasCancelled:))]
        fn cancelled(&self, _picker: &DocumentPicker) {
            answer(Vec::new());
        }
    }
);

/// The picker that is up: its delegate (UIKit holds it weakly), whether it picks a folder, and
/// where the answer goes.
struct Up {
    _delegate: Retained<Delegate>,
    folder: bool,
    tx: Sender<Vec<String>>,
}

thread_local! {
    static UP: std::cell::RefCell<Option<Up>> = const { std::cell::RefCell::new(None) };
    // folders being read (their access ends when they leave)
    static ACCESSED: std::cell::RefCell<Vec<Retained<NSURL>>> = const { std::cell::RefCell::new(Vec::new()) };
    static CTX: std::cell::RefCell<Option<egui::Context>> = const { std::cell::RefCell::new(None) };
}

/// Repaint `ctx` when something has been chosen (main thread).
pub fn init(ctx: egui::Context) {
    CTX.with_borrow_mut(|c| *c = Some(ctx));
}

/// A `UTType` (looked up at run time: UIKit loads UniformTypeIdentifiers) by `selector`'s
/// argument, e.g. `typeWithIdentifier:` "public.folder" or `typeWithFilenameExtension:` "gpx".
fn ut_type(by_extension: bool, s: &str) -> Option<Retained<AnyObject>> {
    let ty = AnyClass::get(c"UTType")?;
    let s = NSString::from_str(s);
    // SAFETY: both class methods take a string and may return nil
    if by_extension { unsafe { msg_send![ty, typeWithFilenameExtension: &*s] } } else { unsafe { msg_send![ty, typeWithIdentifier: &*s] } }
}

/// Shows the document picker for `request` (folders, one file or several; not saving: the shell
/// answers a save itself).
pub fn pick(request: &PickRequest) -> Result<std::sync::mpsc::Receiver<Vec<String>>, String> {
    let folder = match request.kind {
        PickKind::Folder => true,
        PickKind::File | PickKind::Files => false,
        PickKind::Save => return Err("the document picker doesn't save".into()),
    };
    let mtm = objc2::MainThreadMarker::new().ok_or("not on the main thread")?;
    let root = crate::ios_scene::root_view_controller().ok_or("no view controller to present from")?;
    let types: Vec<Retained<AnyObject>> = if folder {
        vec![ut_type(false, "public.folder").ok_or("no folder type")?]
    } else {
        // the filter's extensions (a dynamic type for those the system doesn't know), else anything
        let mut t: Vec<_> = request.filter.iter().flat_map(|(_, exts)| exts.iter()).filter_map(|e| ut_type(true, e)).collect();
        if t.is_empty() {
            t.push(ut_type(false, "public.item").ok_or("no item type")?);
        }
        t
    };
    let types = NSArray::from_retained_slice(&types);
    let picker = DocumentPicker::init_for_opening(DocumentPicker::alloc(mtm), &types, !folder);
    picker.set_allows_multiple_selection(request.kind == PickKind::Files);
    // SAFETY: a fresh NSObject subclass instance
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    picker.set_delegate(Some(&delegate));
    let (tx, rx) = channel();
    // a picker already up answers "cancelled" (its sender is dropped) and this one takes over
    UP.with_borrow_mut(|u| *u = Some(Up { _delegate: delegate, folder, tx }));
    // SAFETY: presenting a view controller from the root one, on the main thread
    let _: () = unsafe { msg_send![&*root, presentViewController: &*picker, animated: true, completion: std::ptr::null::<AnyObject>()] };
    Ok(rx)
}

/// Send the picker's answer (empty: cancelled) and wake the UI.
fn answer(paths: Vec<String>) {
    if let Some(up) = UP.with_borrow_mut(Option::take) {
        let _ = up.tx.send(crate::settle_picked(paths));
    }
    CTX.with_borrow(|c| {
        if let Some(c) = c {
            c.request_repaint();
        }
    });
}

/// Keep reading `url`; the oldest folder beyond [`KEEP`] gives its access back.
fn keep(url: Retained<NSURL>) {
    ACCESSED.with_borrow_mut(|a| {
        a.push(url);
        while a.len() > KEEP {
            let old = a.remove(0);
            // SAFETY: balancing the `startAccessing…` that put it here
            let _: () = unsafe { msg_send![&*old, stopAccessingSecurityScopedResource] };
        }
    });
}

/// Open `url` in the system (Safari for web links): Help ▸ links, model download pages.
pub fn open_url(url: &str) -> Result<(), String> {
    let mtm = objc2::MainThreadMarker::new().ok_or("not on the main thread")?;
    let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("not a link")?;
    let app = objc2_ui_kit::UIApplication::sharedApplication(mtm);
    let options = objc2_foundation::NSDictionary::<NSString, AnyObject>::new();
    // SAFETY: `openURL:options:completionHandler:` with an empty options dictionary and no handler
    let _: () = unsafe { msg_send![&*app, openURL: &*url, options: &*options, completionHandler: std::ptr::null::<AnyObject>()] };
    Ok(())
}
