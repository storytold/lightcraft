//! The window's close, minimize and zoom buttons (macOS "traffic lights"), centred on the top bar.
//!
//! The window has a full-size content view and no title, so the top bar is the title bar. AppKit
//! still lays the buttons out for its own 28 pt title bar: their centre sits 16 pt below the top
//! edge while the bar's controls sit at half the bar's height (20.5 pt). [`centre_on_bar`] makes the
//! title-bar container as tall as the bar and puts the buttons in the middle of it. AppKit
//! resets that layout when the window is resized, enters or leaves full screen, or changes
//! appearance, so the host calls it every frame; it only touches AppKit when a frame differs.
//!
//! Only safe objc2 calls: the container is found as the child of the window's frame view that
//! holds the close button, not through `superview` (an `unsafe` binding).

use objc2::rc::Retained;
use objc2::runtime::NSObjectProtocol as _;
use objc2::{ClassType as _, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSPanel, NSView, NSWindowButton, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::traffic_layout::layout;

/// Moves the buttons of every window with a full-size content view and visible buttons so their
/// centre is the middle of a `bar_h` tall bar. Call it once per frame, on the main thread;
/// anywhere else, or in full screen (the buttons are gone), it does nothing. LightCraft's root
/// window is the only one built with a full-size content view (`with_fullsize_content_view` in
/// `main.rs`; a file dialog or other panel is skipped), so `bar_h` is that window's bar.
pub fn centre_on_bar(bar_h: f32) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    for window in NSApplication::sharedApplication(mtm).windows().to_vec() {
        let style = window.styleMask();
        // a file dialog or other panel is not the window the bar belongs to
        if !style.contains(NSWindowStyleMask::FullSizeContentView)
            || style.contains(NSWindowStyleMask::FullScreen)
            || window.isKindOfClass(NSPanel::class())
        {
            continue;
        }
        let buttons: Vec<_> = [NSWindowButton::CloseButton, NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton]
            .into_iter()
            .filter_map(|b| window.standardWindowButton(b))
            .collect();
        let (Some(close), Some(content)) = (buttons.first(), window.contentView()) else {
            continue;
        };
        if close.isHidden() {
            continue;
        }
        // the window's frame view holds the content view and the title-bar container side by side
        let Some(frame_view) = close.ancestorSharedWithView(&content) else {
            continue;
        };
        // `isDescendantOf:` is also true of a view and itself: a button is never the container or the strip
        let is_button =
            |v: &Retained<NSView>| buttons.iter().any(|b| std::ptr::eq(Retained::as_ptr(v).cast::<()>(), Retained::as_ptr(b).cast::<()>()));
        let Some(container) = frame_view.subviews().to_vec().into_iter().find(|v| !is_button(v) && close.isDescendantOf(v)) else {
            continue;
        };
        // the view between the container and the buttons, when there is one
        let strip = container.subviews().to_vec().into_iter().find(|v| !is_button(v) && close.isDescendantOf(v));
        let l = layout(frame_view.bounds().size.height as f32, bar_h, close.frame().size.height as f32, window.backingScaleFactor() as f32);
        set_frame(&container, |f| {
            NSRect::new(NSPoint::new(f.origin.x, f64::from(l.container_y)), NSSize::new(f.size.width, f64::from(l.container_h)))
        });
        if let Some(strip) = strip {
            set_frame(&strip, |f| NSRect::new(NSPoint::new(f.origin.x, 0.0), NSSize::new(f.size.width, f64::from(l.container_h))));
        }
        for button in &buttons {
            set_frame(button, |f| NSRect::new(NSPoint::new(f.origin.x, f64::from(l.button_y)), f.size));
        }
    }
}

/// Gives `view` the frame `to` makes of its current one, unless it already has it.
fn set_frame(view: &NSView, to: impl Fn(NSRect) -> NSRect) {
    let now = view.frame();
    let want = to(now);
    if want != now {
        view.setFrame(want);
    }
}
