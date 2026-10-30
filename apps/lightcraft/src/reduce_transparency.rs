//! The system's Reduce Transparency setting (macOS: System Settings ▸ Accessibility ▸ Display), followed
//! until the user chooses in Settings ▸ Interface (`Services::system_reduce_transparency`).

use objc2_app_kit::NSWorkspace;

/// Whether the system asks for reduced transparency.
pub fn system() -> Option<bool> {
    Some(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceTransparency())
}
