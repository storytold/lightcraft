use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSApplicationWillFinishLaunchingNotification};
use objc2_foundation::{MainThreadMarker, NSAppleEventDescriptor, NSAppleEventManager, NSNotification, NSNotificationCenter, NSObject};

const CORE: u32 = u32::from_be_bytes(*b"aevt");
const OPEN: u32 = u32::from_be_bytes(*b"odoc");
thread_local! { static INSTALLED: Cell<bool> = const { Cell::new(false) }; }

const DIRECT: u32 = u32::from_be_bytes(*b"----");

#[derive(Default)]
struct Inbox {
    batches: RefCell<Vec<Vec<Result<PathBuf, String>>>>,
    wake: RefCell<Option<Rc<dyn Fn()>>>,
}

define_class!(
    // SAFETY: NSObject has no subclass requirements. The class is main-thread-only and has
    // no custom deallocator. Its ivars own all queued paths and the optional repaint callback.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Inbox]
    #[name = "LightCraftOpenDocuments"]
    struct Handler;

    impl Handler {
        // SAFETY: NSNotificationCenter calls a selector observer with one NSNotification.
        #[unsafe(method(lightcraftWillFinishLaunching:))]
        fn will_finish_launching(&self, _notification: &NSNotification) {
            self.register();
        }

        // SAFETY: NSAppleEventManager calls the registered selector with event and reply
        // NSAppleEventDescriptor objects (the reply may be nil). Neither reference escapes this call.
        #[unsafe(method(lightcraftOpenDocuments:withReplyEvent:))]
        fn open_documents(&self, event: &NSAppleEventDescriptor, _reply: Option<&NSAppleEventDescriptor>) {
            let paths = document_paths(event);
            log::info!("macOS Open Documents: {} file entries", paths.len());
            // Do not hold a mutable borrow while calling the repaint callback.
            self.ivars().batches.borrow_mut().push(paths);
            let wake = self.ivars().wake.borrow().clone();
            if let Some(wake) = wake {
                // A Rust callback must not unwind through Objective-C's event dispatcher.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wake()));
            }
        }
    }
);

impl Handler {
    fn register(&self) {
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: The selector above has exactly the (event, reply) signature expected by
        // NSAppleEventManager. The handler is retained here until after unregistering in Drop.
        // AEEventClass and AEEventID are UInt32. All access is on the main thread.
        unsafe {
            let _: () = msg_send![&*manager,
                setEventHandler: self,
                andSelector: sel!(lightcraftOpenDocuments:withReplyEvent:),
                forEventClass: CORE,
                andEventID: OPEN
            ];
        }
        log::debug!("macOS Open Documents handler registered");
    }
}

fn document_paths(event: &NSAppleEventDescriptor) -> Vec<Result<PathBuf, String>> {
    // SAFETY: This is Foundation's documented selector, with AEKeyword (UInt32) and a
    // nullable retained NSAppleEventDescriptor result. The bindings gate it on Carbon types;
    // spelling the ABI here avoids adding that otherwise unused framework dependency.
    let list: Option<Retained<NSAppleEventDescriptor>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT] };
    let Some(list) = list else { return vec![Err("The open request contains no files.".into())] };
    // SAFETY: Foundation's descriptorType selector returns DescType (UInt32). The generated
    // binding gates this selector on Carbon types, as it does paramDescriptorForKeyword above.
    let descriptor_type: u32 = unsafe { msg_send![&*list, descriptorType] };
    if descriptor_type != u32::from_be_bytes(*b"list") {
        // A direct file descriptor is one document; only lists have an item count.
        return vec![file_path(&list, 1)];
    }
    let count = list.numberOfItems();
    // Bound input-derived work and allocation. Report an oversized request instead of silently
    // opening only part of it. Photo probing and decoding are left to the import worker.
    if !(1..=100_000).contains(&count) {
        return vec![Err("The open request contains an invalid number of files.".into())];
    }
    (1..=count)
        .map(|i| list.descriptorAtIndex(i).ok_or_else(|| format!("File {i} in the open request is not a file URL.")).and_then(|d| file_path(&d, i)))
        .collect()
}

fn file_path(descriptor: &NSAppleEventDescriptor, index: isize) -> Result<PathBuf, String> {
    // Foundation coerces file URLs, aliases and legacy file descriptors to a URL.
    descriptor
        .fileURLValue()
        .filter(|url| url.isFileURL())
        .and_then(|url| url.to_file_path())
        .ok_or_else(|| format!("File {index} in the open request is not a file URL."))
}

/// Main-thread-only registration. Keep it alive until the application closes.
/// Dropping it removes only the handler this instance installed. Install once per process.
pub struct OpenDocuments {
    handler: Retained<Handler>,
}

impl OpenDocuments {
    pub fn install() -> Result<Self, String> {
        let mtm = MainThreadMarker::new().ok_or("Document opening must be installed on the main thread.")?;
        if INSTALLED.get() {
            return Err("Document opening is already installed.".into());
        }
        // Ensure NSApplication exists before registering with its event manager.
        let _app = NSApplication::sharedApplication(mtm);
        let handler = Handler::alloc(mtm).set_ivars(Inbox::default());
        // SAFETY: NSObject's init initializes our allocated subclass and returns the same type.
        let handler: Retained<Handler> = unsafe { msg_send![super(handler), init] };
        // AppKit replaces its handlers during startup. Reinstall at WillFinishLaunching,
        // before it delivers the cold-start document event, without changing winit's delegate.
        // SAFETY: The notification name is an immutable AppKit global. Our selector accepts
        // exactly one NSNotification; the observer lives until removal in Drop, on this thread.
        unsafe {
            NSNotificationCenter::defaultCenter().addObserver_selector_name_object(
                &handler,
                sel!(lightcraftWillFinishLaunching:),
                Some(NSApplicationWillFinishLaunchingNotification),
                None,
            );
        }
        handler.register();
        INSTALLED.set(true);
        Ok(Self { handler })
    }

    /// Attach the window's repaint callback after eframe creates it. Cold-start events already
    /// queued remain available, including events delivered before the library could be opened.
    pub fn set_waker(&self, wake: impl Fn() + 'static) {
        *self.handler.ivars().wake.borrow_mut() = Some(Rc::new(wake));
    }

    pub fn has_pending(&self) -> bool {
        !self.handler.ivars().batches.borrow().is_empty()
    }

    pub fn take(&self) -> Vec<Vec<Result<PathBuf, String>>> {
        std::mem::take(&mut *self.handler.ivars().batches.borrow_mut())
    }
}

impl Drop for OpenDocuments {
    fn drop(&mut self) {
        // SAFETY: This is the same live observer registered by install on the main thread.
        unsafe {
            NSNotificationCenter::defaultCenter().removeObserver(&self.handler);
        }
        INSTALLED.set(false);
        let manager = NSAppleEventManager::sharedAppleEventManager();
        // SAFETY: Documented Foundation selector with two UInt32 arguments. Registration and
        // removal happen on the main thread, while our handler is still retained.
        unsafe {
            let _: () = msg_send![&*manager, removeEventHandlerForEventClass: CORE, andEventID: OPEN];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2::ClassType;
    use objc2_foundation::NSURL;

    fn event() -> Retained<NSAppleEventDescriptor> {
        // SAFETY: Foundation's Apple event constructor takes UInt32 class/id, a nullable target,
        // SInt16 return id and SInt32 transaction id. This creates an unsent event for parsing.
        unsafe {
            msg_send![NSAppleEventDescriptor::class(),
                appleEventWithEventClass: CORE, eventID: OPEN,
                targetDescriptor: None::<&NSAppleEventDescriptor>, returnID: -1i16, transactionID: 0i32
            ]
        }
    }

    #[test]
    fn document_event_decodes_multiple_urls_without_touching_the_files() {
        let paths = ["/Volumes/Offline/撮影 cafe\u{301} #1.ARW", "/tmp/missing %20.dng"];
        let event = event();
        let list = NSAppleEventDescriptor::listDescriptor();
        for (i, path) in paths.iter().enumerate() {
            let url = NSURL::from_file_path(path).unwrap();
            list.insertDescriptor_atIndex(&NSAppleEventDescriptor::descriptorWithFileURL(&url), (i + 1) as isize);
        }
        // SAFETY: Foundation's setParamDescriptor:forKeyword: takes a descriptor and UInt32.
        unsafe {
            let _: () = msg_send![&*event, setParamDescriptor: &*list, forKeyword: DIRECT];
        }
        assert_eq!(document_paths(&event), paths.map(|p| Ok(PathBuf::from(p))));
    }

    #[test]
    fn direct_file_descriptor_is_one_document_and_empty_lists_stay_invalid() {
        let path = "/Volumes/Offline/撮影 cafe\u{301} #1.ARW";
        let descriptor = NSAppleEventDescriptor::descriptorWithFileURL(&NSURL::from_file_path(path).unwrap());
        assert_eq!(descriptor.numberOfItems(), 0);
        for (direct, expected) in [
            (descriptor, vec![Ok(PathBuf::from(path))]),
            (NSAppleEventDescriptor::listDescriptor(), vec![Err("The open request contains an invalid number of files.".to_string())]),
            (NSAppleEventDescriptor::new(), vec![Err("File 1 in the open request is not a file URL.".to_string())]),
        ] {
            let event = event();
            // SAFETY: Foundation's setParamDescriptor:forKeyword: takes a descriptor and UInt32.
            unsafe {
                let _: () = msg_send![&*event, setParamDescriptor: &*direct, forKeyword: DIRECT];
            }
            assert_eq!(document_paths(&event), expected);
        }
    }

    #[test]
    fn missing_direct_object_is_an_error() {
        assert!(document_paths(&NSAppleEventDescriptor::new())[0].is_err());
    }
}

#[cfg(test)]
mod review_file_references {
    use super::*;
    use objc2_foundation::NSURL;

    #[test]
    fn review_file_reference_url_resolves_to_an_owned_path() {
        let path = std::env::temp_dir().join(format!("lc-open-reference-{}.png", std::process::id()));
        std::fs::write(&path, b"file reference test").unwrap();
        let original = std::fs::canonicalize(&path).unwrap();
        let url = NSURL::from_file_path(&original).unwrap();
        let reference = url.fileReferenceURL().unwrap();
        assert!(reference.isFileReferenceURL());
        let descriptor = NSAppleEventDescriptor::descriptorWithFileURL(&reference);
        let actual = descriptor.fileURLValue().and_then(|u| u.to_file_path()).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(actual, original);
    }
}
