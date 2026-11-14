//! Keep a Rust panic from a native dialog inside the main queue callback (#1996).
//!
//! On macOS rfd dispatches panel construction to the main queue. A guard on the worker thread
//! cannot catch a panic across that C callback. Enter the main queue ourselves and catch there;
//! rfd then sees the main thread and constructs its panel inline. This catches Rust panics, not
//! Objective-C exceptions or process aborts.

use std::sync::mpsc::Sender;

pub fn run<T: Send>(show: impl FnOnce() -> T + Send) -> Result<T, String> {
    #[cfg(target_os = "macos")]
    {
        dispatch2::run_on_main(move |_| lightcraft_engine::guard::catch("Native dialog", show))
    }
    #[cfg(not(target_os = "macos"))]
    {
        lightcraft_engine::guard::catch("Native dialog", show)
    }
}

/// Older services return paths only. Report a failed chooser separately from their empty answer
/// so the UI can show the error without replacing the dialog or document the user was editing.
pub fn legacy<T: Default + Send>(errors: &Sender<String>, show: impl FnOnce() -> T + Send) -> T {
    match run(show) {
        Ok(answer) => answer,
        Err(error) => {
            let _ = errors.send(error);
            T::default()
        }
    }
}
