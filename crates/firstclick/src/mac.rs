//! The Objective-C runtime call. The replacement keeps the method's own type encoding, so AppKit
//! calls it exactly as it called winit's: `- (BOOL)acceptsFirstMouse:(NSEvent *)event`.

use std::ffi::CStr;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2::MainThreadMarker;
use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::sel;
use objc2_app_kit::NSApplication;

use crate::{Error, Result};

/// Whether a background click is also acted on: the replaced method's answer.
pub(crate) static ACCEPT: AtomicBool = AtomicBool::new(true);
/// Whether the method has been replaced on some class.
pub(crate) static REPLACED: AtomicBool = AtomicBool::new(false);

/// winit's view class. Only it is changed, never the views of AppKit's own windows (open and save
/// panels, menus).
const WINIT_VIEW: &CStr = c"WinitView";

pub(crate) fn set(accept: bool) -> Result<()> {
    // still winit's own answer, which is yes: nothing to replace
    if accept && !REPLACED.load(Ordering::Relaxed) {
        ACCEPT.store(accept, Ordering::Relaxed);
        return Ok(());
    }
    let mtm = MainThreadMarker::new().ok_or(Error::NotMainThread)?;
    ACCEPT.store(accept, Ordering::Relaxed);
    replace_on_windows(mtm)
}

/// Replaces the method on winit's view class, found through the app's windows' content views.
fn replace_on_windows(mtm: MainThreadMarker) -> Result<()> {
    for window in NSApplication::sharedApplication(mtm).windows().iter() {
        let Some(view) = window.contentView() else { continue };
        // the object's class may be a runtime subclass of winit's (key-value observing makes one)
        if let Some(class) = named_in(view.class(), WINIT_VIEW) {
            // one window is enough: every window's view is of that class
            return replace_on(class);
        }
    }
    Err(Error::NoWindow)
}

/// `class` or the nearest of its superclasses called `name`.
pub(crate) fn named_in<'a>(class: &'a AnyClass, name: &CStr) -> Option<&'a AnyClass> {
    std::iter::successors(Some(class), |c| c.superclass()).find(|c| c.name() == name)
}

/// Replaces `acceptsFirstMouse:` on `class`, which must define it itself (not only inherit it),
/// so its superclasses keep theirs. Doing it again changes nothing.
pub(crate) fn replace_on(class: &AnyClass) -> Result<()> {
    let sel = sel!(acceptsFirstMouse:);
    let own = class.instance_methods().iter().find(|m| m.name() == sel).copied();
    let Some(method) = own else {
        return Err(Error::NoMethod(class.name().to_string_lossy().into_owned()));
    };
    let reply: extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool = accepts_first_mouse;
    // SAFETY: `Imp` is the runtime's untyped function pointer, cast back to the method's own
    // signature by `objc_msgSend` at each call. `accepts_first_mouse` has that signature: the
    // receiver, the selector, one object argument (the event) and a `BOOL` result, as the type
    // encoding passed below says, which is the replaced method's own.
    let imp = unsafe { std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool, Imp>(reply) };
    // SAFETY: `class` is a live, registered class (we hold a reference to it) and `method` one of
    // its own methods, so its type encoding is a valid C string owned by the runtime. The class
    // pointer is only written by the runtime, under its own lock; it is never read as `&mut` here.
    unsafe {
        let types = ffi::method_getTypeEncoding(method);
        ffi::class_replaceMethod(std::ptr::from_ref(class).cast_mut(), sel, imp, types);
    }
    REPLACED.store(true, Ordering::Relaxed);
    Ok(())
}

/// The replacement: yes (the click also acts) unless background clicks are turned off. It touches
/// neither its receiver nor the event.
extern "C-unwind" fn accepts_first_mouse(_this: *mut AnyObject, _sel: Sel, _event: *mut AnyObject) -> Bool {
    Bool::new(ACCEPT.load(Ordering::Relaxed))
}
