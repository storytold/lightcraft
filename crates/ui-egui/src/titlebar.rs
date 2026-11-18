//! The integrated title bar's window gestures (macOS: the native title bar is hidden and the top
//! bar takes its place, so dragging and double-click-to-zoom have to be done here).
//!
//! Pure policy, no egui widgets: [`command_for`] maps a gesture on the bar's empty space and the
//! window's current state to the viewport command the host should run.

use egui::ViewportCommand;

/// What the user did on the bar's empty space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    /// A primary-button drag began: the window follows the pointer.
    DragStarted,
    /// A primary-button double-click: zoom the window, or restore it when already zoomed.
    DoubleClicked,
}

/// The window as the host reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowState {
    /// Zoomed (macOS) / maximized (elsewhere).
    pub maximized: bool,
    /// In the OS's full-screen mode.
    pub fullscreen: bool,
}

/// The viewport command for `gesture`, if any. Full screen has no title bar to drag or zoom.
pub fn command_for(gesture: Gesture, window: WindowState) -> Option<ViewportCommand> {
    if window.fullscreen {
        return None;
    }
    match gesture {
        Gesture::DragStarted => Some(ViewportCommand::StartDrag),
        Gesture::DoubleClicked => Some(ViewportCommand::Maximized(!window.maximized)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: WindowState = WindowState { maximized: false, fullscreen: false };
    const ZOOMED: WindowState = WindowState { maximized: true, fullscreen: false };
    const FULL: WindowState = WindowState { maximized: false, fullscreen: true };

    /// Dragging the title bar moves the window, zoomed or not (macOS lets a zoomed one be pulled
    /// free); a double-click zooms it and another restores it; full screen ignores both.
    #[test]
    fn gestures_move_zoom_and_restore_the_window_except_in_full_screen() {
        for (gesture, state, want) in [
            (Gesture::DragStarted, NORMAL, Some(ViewportCommand::StartDrag)),
            (Gesture::DragStarted, ZOOMED, Some(ViewportCommand::StartDrag)),
            (Gesture::DoubleClicked, NORMAL, Some(ViewportCommand::Maximized(true))),
            (Gesture::DoubleClicked, ZOOMED, Some(ViewportCommand::Maximized(false))),
            (Gesture::DragStarted, FULL, None),
            (Gesture::DoubleClicked, FULL, None),
        ] {
            assert_eq!(command_for(gesture, state), want, "{gesture:?} on {state:?}");
        }
    }
}
