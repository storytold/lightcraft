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

/// Frames, in points, that put the buttons' centre at the middle of a `bar_h` tall bar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The title-bar container's height.
    pub container_h: f32,
    /// The container's bottom edge above the window's bottom edge (AppKit's origin is bottom-left).
    pub container_y: f32,
    /// The buttons' bottom edge above the container's bottom edge.
    pub button_y: f32,
}

/// Where the container and the buttons go for a window `window_h` tall, a bar `bar_h` tall and
/// buttons `button_h` tall. A bar shorter than the buttons is as tall as they are.
pub fn layout(window_h: f32, bar_h: f32, button_h: f32, scale: f32) -> Layout {
    // on whole device pixels: half a pixel off draws the buttons blurred on a 1x screen
    let container_h = (bar_h.max(button_h) * scale).round() / scale;
    let button_y = ((container_h - button_h) / 2.0 * scale).round() / scale;
    Layout { container_h, container_y: window_h - container_h, button_y }
}

/// Moves the buttons of every window with a full-size content view and visible buttons so their
/// centre is the middle of a `bar_h` tall bar. Call it once per frame, on the main thread;
/// anywhere else, or in full screen (the buttons are gone), it does nothing.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// How far below the window's top edge the buttons' centre is.
    fn centre_from_top(window_h: f32, button_h: f32, l: Layout) -> f32 {
        window_h - (l.container_y + l.button_y + button_h / 2.0)
    }

    // Scenario: the traffic lights are centred on the top bar, whatever the window's height, and
    // on whole device pixels (a button half a pixel off draws blurred on a 1x screen)
    #[test]
    fn the_buttons_are_centred_on_the_bar_on_whole_pixels() {
        for scale in [1.0, 2.0] {
            for window_h in [400.0, 1000.0, 2160.0] {
                for bar_h in [42.0, 41.0, 60.0, 61.5, 51.25] {
                    let l = layout(window_h, bar_h, 16.0, scale);
                    let off = (centre_from_top(window_h, 16.0, l) - bar_h / 2.0).abs();
                    assert!(off <= 0.75 / scale, "scale {scale}, window {window_h}, bar {bar_h}: {off} off");
                    let whole = |points: f32| points * scale == (points * scale).round();
                    assert!(whole(l.button_y) && whole(l.container_h), "scale {scale}, bar {bar_h}: {l:?}");
                    assert!((l.container_h - bar_h).abs() <= 0.5 / scale, "the container covers the bar, so a click on a button lands on it");
                    assert_eq!(l.container_y + l.container_h, window_h, "and starts at the window's top edge");
                }
            }
        }
    }

    // Scenario: a bar shorter than the buttons still holds them
    #[test]
    fn a_bar_shorter_than_the_buttons_is_as_tall_as_they_are() {
        let l = layout(1000.0, 10.0, 16.0, 2.0);
        assert_eq!(l.container_h, 16.0);
        assert_eq!(l.button_y, 0.0);
    }

    #[test]
    fn nothing_happens_off_the_main_thread() {
        // AppKit is main-thread only; a test thread must not touch it
        std::thread::spawn(|| centre_on_bar(42.0)).join().unwrap();
    }
}
