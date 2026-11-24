//! Clicks in a background window (macOS): whether the click that brings one of the app's windows
//! forward is also acted on, as a press on whatever is under the pointer.
//!
//! AppKit asks the view under the pointer (`acceptsFirstMouse:`) each time a click lands in a
//! window that isn't frontmost. winit's view answers from a value fixed when the window is made
//! (yes, unless the window was built with `with_accepts_first_mouse(false)`), and eframe offers no
//! way to set it. So [`set_accept_background_clicks`] replaces that method on winit's view class
//! (`WinitView`, found through the windows' content views; never AppKit's own panels) with one that
//! answers from a flag, read on every click: a change applies at once, to every window of the app.
//!
//! The method is replaced only once the user turns background clicks off; until then winit's own
//! answer stands. The replacement answers yes while the flag is on, which is winit's default.
//!
//! This is one of the crates allowed `unsafe` (one `class_replaceMethod` and the function pointer
//! it takes, in `mac`). Its API is safe: the replacement reads only an atomic flag and touches
//! neither of its object arguments. It replaces the method only on a class that defines it itself,
//! so `NSView` keeps its own; no window with winit's view, or a view class that doesn't define the
//! method, is an error, and clicks then work as before. On other platforms every call returns
//! [`Error::Unsupported`].
//!
//! Sources: Apple's AppKit documentation (`NSView.acceptsFirstMouse(for:)`) and Objective-C runtime
//! documentation (`class_replaceMethod`, `method_getTypeEncoding`); winit 0.30's macOS view
//! (`src/platform_impl/macos/view.rs`, read as a reference). Own design otherwise.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::fmt;

#[derive(Debug, PartialEq)]
pub enum Error {
    /// Not macOS.
    Unsupported,
    /// AppKit is main-thread only.
    NotMainThread,
    /// The app has no window of its own (winit's) yet.
    NoWindow,
    /// The window's content view (this class) doesn't decide first clicks itself.
    NoMethod(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => f.write_str("the choice of background clicks is only on macOS"),
            Error::NotMainThread => f.write_str("windows can only be changed on the main thread"),
            Error::NoWindow => f.write_str("the app has no window of its own yet"),
            Error::NoMethod(class) => write!(f, "the window's view ({class}) doesn't decide first clicks itself"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Whether a click that brings a window forward is also acted on (the default) or only brings it
/// forward. Call it on the main thread once the window exists; later windows of the same kind
/// follow it too.
pub fn set_accept_background_clicks(accept: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        mac::set(accept)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = accept;
        Err(Error::Unsupported)
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod mac;

#[cfg(test)]
mod tests;
