//! `lightcraft-ios`: the iOS app's executable (bundled by `scripts/ios-sim.sh`). Android loads
//! the cdylib instead; on other platforms this binary only says where to look.
#![forbid(unsafe_code)]

#[cfg(target_os = "ios")]
fn main() {
    lightcraft_mobile::ios_main();
}

#[cfg(not(target_os = "ios"))]
fn main() {
    eprintln!("lightcraft-ios runs on iOS only: see apps/lightcraft-mobile/README.md (desktop: `cargo run -p lightcraft`)");
}
