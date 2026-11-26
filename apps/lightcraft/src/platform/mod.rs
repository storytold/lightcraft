//! Platform file reveal, URL opening and dialog services, one module per operating system.
//!
//! Each module holds straight-line code for its own platform; this facade only selects the
//! module and re-exports a uniform surface, so `session` needs no `cfg` dispatch of its own.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
// FreeBSD and any other Unix that is neither Linux nor macOS: `xdg-open` only, no D-Bus.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod fallback;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
pub(crate) use fallback::{open_url, reveal_files, reveal_one};
#[cfg(target_os = "linux")]
pub(crate) use linux::{open_url, reveal_files, reveal_one};
#[cfg(target_os = "macos")]
pub(crate) use macos::{open_url, reveal_one};
#[cfg(windows)]
pub(crate) use windows::{open_url, reveal_one};

/// The file dialogs' parent, set once the window exists (`services`). A dialog with a parent is
/// a child of the app's window; without one, the xdg-desktop-portal picker on Wayland is a stray
/// top-level ("Failed to associate portal window with parent window" in the portal's log): a
/// tiling compositor puts it beside the app, and a fullscreen window hides it entirely, so
/// Import Photos… looks like a hang. rfd exports the Wayland surface through xdg_foreign itself;
/// `FileDialog` is `Clone` and `Send`, so one parented template serves every picker, including
/// the one on its own thread (#191). Only set on Linux and FreeBSD: on macOS a parented dialog
/// becomes a sheet and on Windows a modal, a behaviour change this does not make.
pub(crate) static DIALOG_PARENT: std::sync::OnceLock<rfd::FileDialog> = std::sync::OnceLock::new();

/// A file dialog owned by the app's window, or a plain one before the window exists.
pub(crate) fn file_dialog() -> rfd::FileDialog {
    DIALOG_PARENT.get().cloned().unwrap_or_default()
}
