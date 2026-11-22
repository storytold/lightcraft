//! The Android app: the shared UI ([`lcc_ui_egui::LccApp`]) and engine, started by android-activity
//! inside the Java activity in `packaging/android` (a `NativeActivity` subclass).
//!
//! Where the desktop has a home folder, this host takes the app-private folders the activity
//! knows ([`lcc_brand::set_platform_dirs`]): settings in the internal data folder, the library in
//! the external one (visible to the Files app over USB, removed with the app). Pickers and links
//! go through the activity ([`lcc_android_host::Host`]).
//!
//! Off Android the library is empty, so `cargo build --workspace` stays cheap.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![deny(clippy::indexing_slicing)]

#[cfg(target_os = "android")]
mod app;
#[cfg(any(target_os = "android", test))]
mod copies;
#[cfg(target_os = "android")]
mod prefs;
#[cfg(any(target_os = "android", test))]
mod staging;
#[cfg(target_os = "android")]
mod system_fonts;

/// The entry point android-activity calls on its own thread once the activity has started.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
// SAFETY: `no_mangle` exports the one symbol android-activity looks up (`android_main`); no other
// item in the program has that name.
#[unsafe(no_mangle)]
fn android_main(activity: lcc_android_host::AndroidApp) {
    app::main(activity);
}
