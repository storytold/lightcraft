//! The number of running jobs on the Dock icon (macOS).
//!
//! The UI says how many jobs the person started are running (`Services::dock_badge`, from
//! `Activity::running_jobs`); this shows it as the badge of the app's Dock tile, and clears it at zero, so an
//! export in a minimized window can be seen from the Dock. A badge is text on the icon: `NSDockTile` has no safe
//! drawing path for a progress bar without a custom view, and the badge needs no `unsafe`.

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;
use objc2_foundation::NSString;

/// The text of the badge for `running` jobs: nothing at zero.
pub fn label(running: usize) -> Option<String> {
    (running > 0).then(|| running.to_string())
}

/// Show `running` on the Dock icon. AppKit is main-thread only: from another thread nothing is shown.
pub fn show(running: usize) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let tile = NSApplication::sharedApplication(mtm).dockTile();
    match label(running) {
        Some(text) => tile.setBadgeLabel(Some(&NSString::from_str(&text))),
        None => tile.setBadgeLabel(None),
    }
}

/// The host service for `Services::dock_badge`.
pub fn service() -> lightcraft_ui_egui::DockBadgeFn {
    Box::new(show)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_badge_is_the_count_and_empty_when_idle() {
        assert_eq!(label(0), None);
        assert_eq!(label(1).as_deref(), Some("1"));
        assert_eq!(label(12).as_deref(), Some("12"));
    }
}
