//! iOS: offer exported files in the share sheet (UIActivityViewController): AirDrop, Messages,
//! Save to Photos / Files, other apps.
#![allow(unsafe_code)] // Objective-C messages

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, msg_send};
use objc2_foundation::{NSArray, NSString, NSURL};
use objc2_ui_kit::{UIActivityViewController, UIPopoverPresentationController};

/// Show the share sheet for `paths` over the app (main thread).
pub fn share(paths: &[String]) {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let Some(root) = crate::ios_scene::root_view_controller() else {
        log::warn!("share: no view controller to present from");
        return;
    };
    let urls: Vec<Retained<AnyObject>> =
        paths.iter().map(|p| Retained::into_super(Retained::into_super(NSURL::fileURLWithPath(&NSString::from_str(p))))).collect();
    let items = NSArray::from_retained_slice(&urls);
    // SAFETY: an array of file URLs is what the activity items may be
    let sheet = unsafe { UIActivityViewController::initWithActivityItems_applicationActivities(UIActivityViewController::alloc(mtm), &items, None) };
    // iPad shows it as a popover, which needs an anchor (without one UIKit throws): the bottom
    // centre of the window, where the bottom bar is
    // SAFETY: a property getter of UIViewController that returns a nullable popover controller
    let popover: Option<Retained<UIPopoverPresentationController>> = unsafe { msg_send![&*sheet, popoverPresentationController] };
    if let (Some(popover), Some(view)) = (popover, root.view()) {
        let b = view.bounds();
        popover.setSourceView(Some(&view));
        popover.setSourceRect(objc2_core_foundation::CGRect::new(
            objc2_core_foundation::CGPoint::new(b.size.width / 2.0, b.size.height - 60.0),
            objc2_core_foundation::CGSize::new(1.0, 1.0),
        ));
    }
    // SAFETY: presenting from the root view controller, on the main thread
    let _: () = unsafe { msg_send![&*root, presentViewController: &*sheet, animated: true, completion: std::ptr::null::<AnyObject>()] };
}
