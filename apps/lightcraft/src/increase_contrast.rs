//! The system's Increase Contrast setting (macOS: System Settings ▸ Accessibility ▸ Display), followed
//! until the user chooses in Settings ▸ Interface (`Services::system_increase_contrast`).

use objc2_app_kit::NSWorkspace;

/// Whether the system asks for increased contrast.
pub fn system() -> Option<bool> {
    Some(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldIncreaseContrast())
}
