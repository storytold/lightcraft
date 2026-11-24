use super::*;

#[cfg(not(target_os = "macos"))]
#[test]
fn the_choice_is_unsupported_off_macos() {
    assert_eq!(set_accept_background_clicks(false), Err(Error::Unsupported));
    assert_eq!(set_accept_background_clicks(true), Err(Error::Unsupported));
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::{msg_send, sel};

    use std::sync::atomic::Ordering;

    use super::*;
    use crate::mac::{self, ACCEPT, REPLACED};

    extern "C-unwind" fn always_yes(_this: &AnyObject, _sel: Sel, _event: *mut AnyObject) -> Bool {
        Bool::YES
    }

    /// A view class that answers `acceptsFirstMouse:` itself, as winit's does, and a subclass that
    /// only inherits the answer.
    fn view_classes() -> (&'static AnyClass, &'static AnyClass) {
        let object = AnyClass::get(c"NSObject").unwrap();
        let mut view = ClassBuilder::new(c"FirstClickTestView", object).unwrap();
        // SAFETY: the function's signature matches the selector's (one object argument, a BOOL).
        unsafe { view.add_method(sel!(acceptsFirstMouse:), always_yes as extern "C-unwind" fn(_, _, _) -> _) };
        let view = view.register();
        let child = ClassBuilder::new(c"FirstClickTestChild", view).unwrap().register();
        (view, child)
    }

    fn answer(class: &AnyClass) -> bool {
        // SAFETY: `new` makes a plain instance of a registered NSObject subclass; the method takes
        // a nullable event and returns a BOOL.
        unsafe {
            let obj: Retained<AnyObject> = msg_send![class, new];
            msg_send![&*obj, acceptsFirstMouse: std::ptr::null_mut::<AnyObject>()]
        }
    }

    // one test: the choice is a process-wide flag
    #[test]
    fn the_replaced_method_answers_from_the_choice() {
        assert!(ACCEPT.load(Ordering::Relaxed), "background clicks are accepted by default");
        assert_eq!(set_accept_background_clicks(true), Ok(()), "keeping the default replaces nothing, on any thread");
        assert!(!REPLACED.load(Ordering::Relaxed));

        // AppKit is main-thread only: a test thread is refused rather than touching windows
        let off = std::thread::spawn(|| set_accept_background_clicks(false)).join().unwrap();
        assert_eq!(off, Err(Error::NotMainThread));
        assert!(ACCEPT.load(Ordering::Relaxed), "a refused call leaves the choice as it was");

        let (view, child) = view_classes();
        // the view is found by name through a runtime subclass (as key-value observing makes)
        assert!(std::ptr::eq(mac::named_in(child, c"FirstClickTestView").unwrap(), view));
        assert!(mac::named_in(AnyClass::get(c"NSObject").unwrap(), c"FirstClickTestView").is_none());
        assert!(matches!(mac::replace_on(child), Err(Error::NoMethod(c)) if c == "FirstClickTestChild"));
        ACCEPT.store(false, Ordering::Relaxed);
        assert!(answer(view), "a refused subclass leaves its superclass's method alone");

        mac::replace_on(view).unwrap();
        assert!(!answer(view), "turned off, the click only brings the window forward");
        assert!(!answer(child), "a subclass follows its superclass");
        ACCEPT.store(true, Ordering::Relaxed);
        assert!(answer(view), "turned back on, the click acts again, with no relaunch");
        mac::replace_on(view).unwrap();
        assert!(answer(view), "replacing again changes nothing");
    }
}
