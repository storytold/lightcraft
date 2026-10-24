//! The ICC profile of the screen showing LightCraft's window (macOS).
//!
//! macOS colour-manages windows that say what colour space they draw in. wgpu leaves the window's
//! Metal layer without one for sRGB content, which Core Animation shows without colour matching, so
//! on a wide-gamut (P3) screen the previews looked more saturated than the exported files do in
//! Preview or Photos (issue #787). While no monitor profile is chosen in Settings ▸ Monitor, the
//! previews are shown through this profile instead (`Services::screen_profile`).

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSScreen};

/// The profile of the screen the window is mostly on (the main screen when it is hidden), or `None`
/// when it isn't known: off the main thread, or a screen without a usable profile.
pub fn icc() -> Option<Vec<u8>> {
    let mtm = MainThreadMarker::new()?;
    let app = NSApplication::sharedApplication(mtm);
    let window = app.mainWindow().or_else(|| app.keyWindow()).or_else(|| app.windows().firstObject());
    let screen = window.and_then(|w| w.screen()).or_else(|| NSScreen::mainScreen(mtm))?;
    let icc = screen.colorSpace()?.ICCProfileData()?.to_vec();
    (!icc.is_empty()).then_some(icc)
}

#[cfg(test)]
mod tests {
    #[test]
    fn unknown_off_the_main_thread() {
        // AppKit is main-thread only; a test thread gets nothing rather than undefined behaviour
        assert_eq!(std::thread::spawn(super::icc).join().unwrap(), None);
    }
}
