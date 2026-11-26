//! The system's Reduce Motion setting (macOS: System Settings ▸ Accessibility ▸ Display), followed
//! until the user chooses in Settings ▸ Interface (`Services::system_reduce_motion`).

use objc2_app_kit::NSWorkspace;

/// Whether the system asks for reduced motion.
pub fn system() -> Option<bool> {
    Some(NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion())
}
