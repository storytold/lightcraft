//! Files the operating system asks LightCraft to open: on macOS, a photo opened from Finder (Open
//! With, a drop on the Dock icon), from another app (Photo Mechanic's "Open with", Files) or with
//! `open -a LightCraft photo.arw` (issue #506).
//!
//! macOS doesn't pass these on the command line: AppKit hands them to the application delegate's
//! `application:openURLs:`, and when the delegate has no such method it asks `NSDocumentController`,
//! which fails with "LightCraft cannot open files in the … format" because LightCraft isn't an
//! `NSDocument` app. winit 0.30's delegate doesn't implement the method, so [`install`] adds it to
//! the delegate's class when the app is about to finish launching: after winit set the delegate and
//! before AppKit delivers the files the app was launched with. The handler queues the paths and
//! calls the waker ([`set_waker`]); the app collects them with [`take`]. VectorCraft does the same
//! (`apps/vectorcraft/src/open_documents.rs`).
//!
//! Adding a method to another crate's Objective-C class has no safe API: this crate and
//! `lightcraft-sysmem` are the only ones allowed `unsafe` (AGENTS.md), here inside `macos`. Its API
//! is safe, and elsewhere [`install`] does nothing and [`take`] stays empty.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::{Mutex, OnceLock, PoisonError};

/// Paths that arrived and haven't been taken yet.
static PENDING: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Called when paths arrive (unset until the window exists; the app takes them on its first frame).
static WAKER: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Receive the files the operating system asks the app to open from now on. Call once, before the
/// event loop runs. Does nothing except on macOS, where such files come as arguments elsewhere.
pub fn install() {
    #[cfg(target_os = "macos")]
    macos::install();
}

/// Call `wake` whenever files arrive, e.g. to repaint the window that takes them. Only the first
/// waker set is kept.
pub fn set_waker(wake: impl Fn() + Send + Sync + 'static) {
    let _ = WAKER.set(Box::new(wake));
}

/// The paths that arrived since the last call, in order.
pub fn take() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock().unwrap_or_else(PoisonError::into_inner))
}

/// Queue `paths` and wake the app.
fn arrived(paths: impl IntoIterator<Item = String>) {
    let len = {
        let mut pending = PENDING.lock().unwrap_or_else(PoisonError::into_inner);
        pending.extend(paths);
        pending.len()
    };
    if len > 0
        && let Some(wake) = WAKER.get()
    {
        wake();
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use std::ptr::NonNull;

    use block2::RcBlock;
    use objc2::MainThreadMarker;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
    use objc2_foundation::{NSArray, NSNotification, NSNotificationCenter, NSURL};

    /// The handler's signature, as the Objective-C runtime calls it.
    type OpenUrls = extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, &NSArray<NSURL>);

    pub fn install() {
        let block = RcBlock::new(|_: NonNull<NSNotification>| add_to_delegate());
        // SAFETY: the name is AppKit's notification constant; no object or queue filter, so the
        // block runs on the posting thread (AppKit posts it on the main thread). The notification
        // center keeps the registration (and the block) for the life of the app; the returned
        // observer token is only needed to unregister, which we never do.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
                None,
                &block,
            );
        }
    }

    /// `application:openURLs:` on the application delegate's class (winit's).
    fn add_to_delegate() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let Some(delegate) = NSApplication::sharedApplication(mtm).delegate() else {
            log::warn!("no application delegate: photos opened from Finder or other apps won't arrive");
            return;
        };
        if !add_open_urls(AsRef::<AnyObject>::as_ref(&*delegate).class()) {
            log::warn!("the application delegate already handles open-document events");
        }
    }

    /// Add [`open_urls`] to `class` as `application:openURLs:`. False when the class already has
    /// that method (it is then left as it is).
    pub(crate) fn add_open_urls(class: &AnyClass) -> bool {
        let handler: OpenUrls = open_urls;
        // SAFETY: `Imp` is the untyped form of a method implementation; the Objective-C runtime
        // calls it with the arguments the type encoding declares, which match `open_urls`: no
        // return value (`v`), the receiver (`@`), the selector (`:`), the application (`@`) and the
        // URL array (`@`). `class_addMethod` copies the encoding string and never replaces an
        // existing method; the class is a registered class, alive for the life of the process.
        let added = unsafe {
            let imp = std::mem::transmute::<OpenUrls, Imp>(handler);
            objc2::ffi::class_addMethod(std::ptr::from_ref(class).cast_mut(), sel!(application:openURLs:), imp, c"v@:@@".as_ptr())
        };
        added.as_bool()
    }

    /// Queue the files among `urls`. Never panics: it runs inside AppKit.
    extern "C-unwind" fn open_urls(_delegate: &AnyObject, _cmd: Sel, _app: &AnyObject, urls: &NSArray<NSURL>) {
        // Only file URLs: macOS 27 turns an https URL's path into a file path too (VectorCraft #433).
        super::arrived(urls.iter().filter(|u| u.isFileURL()).filter_map(|u| u.to_file_path()).map(|p| p.to_string_lossy().into_owned()));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// The queue is global: tests that fill it take turns.
    static QUEUE: Mutex<()> = Mutex::new(());
    static WOKEN: AtomicUsize = AtomicUsize::new(0);

    fn turn() -> std::sync::MutexGuard<'static, ()> {
        let guard = QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = take();
        guard
    }

    #[test]
    fn files_wait_until_taken_and_wake_the_app() {
        let _turn = turn();
        set_waker(|| {
            WOKEN.fetch_add(1, Ordering::SeqCst);
        });
        let before = WOKEN.load(Ordering::SeqCst);
        arrived(["/tmp/a.arw".to_string(), "/tmp/b c.jpg".to_string()]);
        assert_eq!(WOKEN.load(Ordering::SeqCst), before + 1);
        arrived(["/tmp/d.heic".to_string()]);
        assert_eq!(take(), ["/tmp/a.arw", "/tmp/b c.jpg", "/tmp/d.heic"]);
        assert!(take().is_empty());
        // nothing arrived: nothing to wake the app for
        let before = WOKEN.load(Ordering::SeqCst);
        arrived(Vec::new());
        assert_eq!(WOKEN.load(Ordering::SeqCst), before);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn install_elsewhere_queues_nothing() {
        let _turn = turn();
        install();
        assert!(take().is_empty());
    }

    /// The method `install` adds, called the way AppKit calls it: only file URLs are queued, and a
    /// class that already has the method keeps its own.
    #[cfg(target_os = "macos")]
    #[test]
    #[allow(unsafe_code)]
    fn an_open_urls_message_queues_the_files() {
        use objc2::rc::Retained;
        use objc2::runtime::{AnyObject, ClassBuilder, NSObject};
        use objc2::{ClassType, msg_send, sel};
        use objc2_foundation::{NSArray, NSString, NSURL};

        let _turn = turn();
        let class = ClassBuilder::new(c"LightCraftOpenDocsTestDelegate", NSObject::class()).unwrap().register();
        assert!(!class.responds_to(sel!(application:openURLs:)));
        assert!(macos::add_open_urls(class));
        assert!(!macos::add_open_urls(class), "a second add leaves the method alone");

        let web = NSURL::URLWithString(&NSString::from_str("https://example.com/a.jpg")).unwrap();
        let urls =
            NSArray::from_retained_slice(&[NSURL::from_file_path("/tmp/a b.arw").unwrap(), web, NSURL::from_file_path("/tmp/c.heic").unwrap()]);
        // SAFETY: `class` is an NSObject subclass (`new` returns a retained instance), and it now
        // has `application:openURLs:` with the signature the message is sent with.
        let delegate: Retained<AnyObject> = unsafe { msg_send![class, new] };
        let app = NSObject::new();
        let () = unsafe { msg_send![&*delegate, application: &*app, openURLs: &*urls] };
        assert_eq!(take(), ["/tmp/a b.arw", "/tmp/c.heic"]);
    }
}
