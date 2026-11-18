//! `cargo xtask android check`: the crates the Android app is built from must keep building for
//! Android (`aarch64-linux-android`). Run by `ci` next to the wasm check. `cargo xtask android
//! package` builds the APK / AAB (`apk.rs`).
//!
//! Android counts as `unix`, so a cfg written as "unix but not macOS" to mean Linux silently
//! takes it in; this check catches that the day it happens. The C parts of the graph (`ring`, via
//! SFTP publishing) need the NDK's clang, found through `ANDROID_NDK_HOME` / `ANDROID_NDK_ROOT`
//! or the newest `~/Android/android-ndk-*`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{layers, metadata};

/// The Rust target the Android app ships for.
pub const TARGET: &str = "aarch64-linux-android";

/// A processor the app is built for: phones (arm64) and the x86_64 emulator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Abi {
    Arm64,
    X86_64,
}

impl Abi {
    pub fn parse(s: &str) -> Option<Abi> {
        match s {
            "arm64" | "arm64-v8a" | "aarch64" => Some(Abi::Arm64),
            "x86_64" | "x86-64" => Some(Abi::X86_64),
            _ => None,
        }
    }

    /// The Rust target triple.
    pub fn triple(self) -> &'static str {
        match self {
            Abi::Arm64 => TARGET,
            Abi::X86_64 => "x86_64-linux-android",
        }
    }

    /// The APK's `lib/<abi>/` folder name.
    pub fn dir(self) -> &'static str {
        match self {
            Abi::Arm64 => "arm64-v8a",
            Abi::X86_64 => "x86_64",
        }
    }
}
/// The oldest Android API level the app supports (Android 9); picks the NDK's clang wrapper.
pub const MIN_API: u32 = 28;

pub fn run(args: &[&str]) -> Result<(), String> {
    match args.first().copied() {
        Some("check") | None => check(false),
        Some("package") => crate::apk::package(args.get(1..).unwrap_or_default()),
        Some(other) => Err(format!("android: unknown subcommand `{other}` (expected `check` or `package`)")),
    }
}

/// Workspace packages that must build for Android: every L0–L5 and standalone crate except the
/// web app, plus the Android app itself.
fn android_set() -> Result<Vec<String>, String> {
    let crates = layers::from_metadata(&metadata()?)?;
    Ok(crates
        .into_iter()
        .filter(|c| c.name != "lcc-web")
        .filter(|c| match layers::classify(&c.name) {
            Some(layers::Class::Layer(l)) => l <= 5,
            Some(layers::Class::Standalone) => true,
            _ => false,
        })
        .map(|c| c.name)
        .chain(std::iter::once("lcc-android".to_string()))
        .collect())
}

/// The NDK root: `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT`, else the newest `~/Android/android-ndk-*`.
pub fn ndk_root() -> Option<PathBuf> {
    for var in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
        if let Some(p) = std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_dir()) {
            return Some(p);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let mut found: Vec<PathBuf> = std::fs::read_dir(home.join("Android"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("android-ndk-")))
        .collect();
    found.sort();
    found.pop()
}

/// The NDK's prebuilt LLVM `bin` directory for this host.
pub fn ndk_bin(ndk: &Path) -> Option<PathBuf> {
    let prebuilt = ndk.join("toolchains/llvm/prebuilt");
    std::fs::read_dir(prebuilt).ok()?.filter_map(|e| e.ok().map(|e| e.path().join("bin"))).find(|p| p.is_dir())
}

/// Points cargo and the `cc` crate at the NDK's clang for `abi`.
pub fn configure(c: &mut Command, bin: &Path, abi: Abi) {
    let clang = bin.join(format!("{}{MIN_API}-clang", abi.triple()));
    let triple = abi.triple().replace('-', "_");
    c.env(format!("CC_{triple}"), &clang)
        .env(format!("AR_{triple}"), bin.join("llvm-ar"))
        .env(format!("CARGO_TARGET_{}_LINKER", triple.to_uppercase()), &clang);
}

pub fn target_installed(triple: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim() == triple))
}

/// A missing toolchain is a skip on a developer machine and a failure in CI (`CI` is set there).
fn missing(what: &str) -> Result<(), String> {
    if std::env::var_os("CI").is_some() {
        Err(format!("android: {what}"))
    } else {
        eprintln!("android: SKIPPED, {what}; CI runs it (see docs/android.md)");
        Ok(())
    }
}

/// The processors a check covers: the fast gate (`together`) only the phones' arm64; the full run and
/// `cargo xtask android check` the emulator's and Chromebooks' x86_64 as well, which releases ship too.
fn abis_to_check(together: bool) -> &'static [Abi] {
    if together { &[Abi::Arm64] } else { &[Abi::Arm64, Abi::X86_64] }
}

/// `together`: one cargo run per target for the whole set and arm64 only (the fast gate), see
/// [`crate::check_for_target`] and [`abis_to_check`].
pub fn check(together: bool) -> Result<(), String> {
    let abis = abis_to_check(together);
    if let Some(abi) = abis.iter().find(|a| !target_installed(a.triple())) {
        let t = abi.triple();
        return missing(&format!("the Rust target is not installed (`rustup target add {t}`)"));
    }
    let Some(bin) = ndk_root().as_deref().and_then(ndk_bin) else {
        return missing("the Android NDK was not found (set ANDROID_NDK_HOME)");
    };
    let set = android_set()?;
    let mut failed = 0;
    for abi in abis {
        let target = abi.triple();
        let results = crate::check_for_target(target, &set, together, |c| configure(c, &bin, *abi));
        println!("\n{target} check:");
        for (p, ok) in &results {
            println!("  {:<6} {p}", if *ok { "ok" } else { "FAIL" });
        }
        failed += results.iter().filter(|r| !r.1).count();
    }
    if failed == 0 { Ok(()) } else { Err(format!("{failed} crate check(s) failed the Android check")) }
}
