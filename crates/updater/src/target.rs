//! Which release artifact belongs to this machine.
//!
//! The release pipeline names every artifact `<binary>-<version>-<os>-<arch>[-portable]<ext>`
//! (see `docs/releasing.md` and `packaging/`). This module turns the running platform into that
//! shape and classifies the assets a release offers, so the update check can pick the one file
//! that applies — and say so in words first ([`Target::describe`]).

/// The platform a release is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Windows,
    Macos,
    Linux,
    Freebsd,
}

impl Os {
    /// The slug the pipeline puts in artifact names (`windows`, `macos`, `linux`, `freebsd`).
    pub fn slug(self) -> &'static str {
        match self {
            Os::Windows => "windows",
            Os::Macos => "macos",
            Os::Linux => "linux",
            Os::Freebsd => "freebsd",
        }
    }
}

/// What a release asset is, and therefore what installing it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A Windows `.msi`: run it and let the installer replace this installation.
    Msi,
    /// A macOS `.dmg`: mount it, copy the app beside the running one, relaunch.
    Dmg,
    /// A Linux `.AppImage`: replace the running file and start the new one.
    AppImage,
    /// A tarball or zip (`portable`): unpack it yourself over the current installation.
    Archive,
    /// The release's `SHA256SUMS.txt`: integrity, not an install.
    Checksums,
}

impl Kind {
    /// How much we want this artifact, lower first. Only one of each is ever offered per platform.
    pub fn rank(self) -> u8 {
        match self {
            Kind::Msi | Kind::Dmg | Kind::AppImage => 0,
            Kind::Archive => 1,
            Kind::Checksums => 9,
        }
    }

    /// Classify `name` (`<binary>-<version>-windows-x64.msi`) by its extension and shape.
    pub fn of(name: &str) -> Option<Kind> {
        let lower = name.to_ascii_lowercase();
        if lower.ends_with("sha256sums.txt") || lower.ends_with("sha256sums") {
            return Some(Kind::Checksums);
        }
        // `.AppImage.zsync` is delta metadata for AppImageUpdate: not an install by itself
        if lower.ends_with(".zsync") {
            return None;
        }
        if lower.ends_with(".msi") {
            return Some(Kind::Msi);
        }
        if lower.ends_with(".dmg") {
            return Some(Kind::Dmg);
        }
        if lower.ends_with(".appimage") {
            return Some(Kind::AppImage);
        }
        if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") || lower.ends_with(".zip") || lower.ends_with(".flatpak") {
            return Some(Kind::Archive);
        }
        None
    }

    /// One word for a dialog: how this file is installed.
    pub fn installs_by(self) -> &'static str {
        match self {
            Kind::Msi => "runs the installer",
            Kind::Dmg => "copies the app out of the disk image",
            Kind::AppImage => "replaces the running program",
            Kind::Archive => "unpacks over the current installation",
            Kind::Checksums => "lists the checksums",
        }
    }
}

/// One asset of a release, as the feed describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    /// Bytes, when the feed says (0 = unknown).
    pub size: u64,
    /// Lower-case hex SHA-256, when the feed pins one.
    pub sha256: Option<String>,
    pub kind: Kind,
}

impl Asset {
    /// Build an asset from a feed entry, classifying its name (`None`: not an artifact of ours).
    pub fn new(name: impl Into<String>, url: impl Into<String>, size: u64, sha256: Option<String>) -> Option<Asset> {
        let name = name.into();
        let kind = Kind::of(&name)?;
        Some(Asset { name, url: url.into(), size, sha256, kind })
    }
}

/// The platform this build runs on, and which artifacts it can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Target {
    pub os: Os,
    /// `x64`, `x86`, `arm64`, `x86_64`, `aarch64`, `riscv64` (macOS: `arm64`, `x64`).
    pub arch: &'static str,
}

impl Target {
    /// The running platform, or `None` on a platform the release pipeline does not build for (the
    /// web build has no updater at all). An Intel build running under Rosetta counts as the Mac it
    /// runs on, so it updates to the Apple silicon build ([`machine_arch`]).
    pub fn current() -> Option<Target> {
        Target::of(std::env::consts::OS, machine_arch(std::env::consts::ARCH, translated()))
    }

    /// [`Target::current`] from a pair of `std::env::consts` strings, so the mapping can be tested
    /// for every platform we ship.
    pub fn of(os: &str, arch: &str) -> Option<Target> {
        let (os, arch) = match (os, arch) {
            ("windows", "x86_64") => (Os::Windows, "x64"),
            ("windows", "x86") => (Os::Windows, "x86"),
            ("windows", "aarch64") => (Os::Windows, "arm64"),
            ("macos", "aarch64") => (Os::Macos, "arm64"),
            ("macos", "x86_64") => (Os::Macos, "x64"),
            ("linux", "x86_64") => (Os::Linux, "x86_64"),
            ("linux", "aarch64") => (Os::Linux, "aarch64"),
            ("linux", "riscv64") => (Os::Linux, "riscv64"),
            ("freebsd", "x86_64") => (Os::Freebsd, "x86_64"),
            _ => return None,
        };
        Some(Target { os, arch })
    }

    /// Whether `name` names an artifact for this platform: it carries the OS and architecture slug.
    /// The portable Windows zip is deliberately not matched (installing it is a manual unpack).
    pub fn matches_name(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if lower.contains("portable") {
            return false;
        }
        if !lower.contains(&format!("-{}-", self.os.slug())) {
            return false;
        }
        lower.contains(&format!("-{}-{}", self.os.slug(), self.arch)) || self.is_fallback(&lower)
    }

    /// Whether `name` is a file this platform takes only when the release has none for its own
    /// architecture: on macOS, the universal image (Apple silicon and Intel in one app).
    ///
    /// The per-architecture images are `-macos-arm64` and `-macos-x64`, not `-macos-aarch64` and
    /// `-macos-x86_64`: builds before them took any of `-macos-universal`, `-macos-aarch64` and
    /// `-macos-x86_64` as the one macOS file and picked the smallest, so they would install the
    /// Apple silicon image on an Intel Mac.
    pub fn is_fallback(&self, name: &str) -> bool {
        self.os == Os::Macos && name.to_ascii_lowercase().contains("-macos-universal")
    }

    /// [`Target::matches_name`] for a portable copy: the portable zip is its file, and an installer
    /// is not (it would install the app somewhere else and leave this folder as it was).
    pub fn matches_portable(&self, name: &str) -> bool {
        let plain = name.to_ascii_lowercase().replace("-portable", "");
        Kind::of(&plain) != Some(Kind::Msi) && self.matches_name(&plain)
    }

    /// What the update will do, in one line, before anything is downloaded.
    pub fn describe(&self) -> String {
        match self.os {
            Os::Windows => format!("run the {} installer and then start the app again", self.words_for_arch()),
            Os::Macos => "copy the app out of the disk image over this installation and start it again".into(),
            Os::Linux if self.arch == "x86_64" || self.arch == "aarch64" => "replace this program with the new one and start it again".into(),
            _ => "download it and open the folder it is in, so you can install it".into(),
        }
    }

    /// How a sentence names this machine's architecture (`64-bit`, `ARM64`, `x86_64`).
    fn words_for_arch(&self) -> &'static str {
        match self.arch {
            "x64" => "64-bit",
            "x86" => "32-bit",
            "arm64" => "ARM64",
            other => other,
        }
    }
}

/// The architecture to update to: the machine's, not the build's. An `x86_64` build that macOS
/// runs under Rosetta (`translated`) is on Apple silicon.
fn machine_arch(build_arch: &'static str, translated: bool) -> &'static str {
    if translated && build_arch == "x86_64" { "aarch64" } else { build_arch }
}

/// Whether this process runs under Rosetta: `sysctl.proc_translated` is 1 then, 0 for a native
/// process, and missing on an Intel Mac. Asked once; only an Intel macOS build can be translated.
fn translated() -> bool {
    if !(cfg!(target_os = "macos") && cfg!(target_arch = "x86_64")) {
        return false;
    }
    static TRANSLATED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRANSLATED.get_or_init(|| {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "sysctl.proc_translated"])
            .output()
            .is_ok_and(|o| o.status.success() && o.stdout.trim_ascii() == b"1")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifacts_are_classified_matched_and_ranked() {
        // classifies artifacts
        {
            assert_eq!(Kind::of("app-1.2.3-windows-x64.msi"), Some(Kind::Msi));
            assert_eq!(Kind::of("app-1.2.3-windows-x64-portable.zip"), Some(Kind::Archive));
            assert_eq!(Kind::of("app-1.2.3-macos-universal.dmg"), Some(Kind::Dmg));
            assert_eq!(Kind::of("app-1.2.3-linux-x86_64.AppImage"), Some(Kind::AppImage));
            assert_eq!(Kind::of("app-1.2.3-linux-x86_64.tar.gz"), Some(Kind::Archive));
            assert_eq!(Kind::of("app-1.2.3-freebsd-x86_64.tar.gz"), Some(Kind::Archive));
            assert_eq!(Kind::of("app-1.2.3-linux-x86_64.flatpak"), Some(Kind::Archive));
            assert_eq!(Kind::of("SHA256SUMS.txt"), Some(Kind::Checksums));
            assert_eq!(Kind::of("app-1.2.3-linux-x86_64.AppImage.zsync"), None);
            assert_eq!(Kind::of("app-1.2.3-web.zip"), Some(Kind::Archive));
            assert_eq!(Kind::of("notes.md"), None);
            assert_eq!(Kind::of("app.exe"), None);
        }
        // picks only this platforms files
        {
            let win = Target { os: Os::Windows, arch: "x64" };
            assert!(win.matches_name("app-1.2.3-windows-x64.msi"));
            assert!(!win.matches_name("app-1.2.3-windows-x64-portable.zip"), "the portable zip is a manual install");
            assert!(!win.matches_name("app-1.2.3-windows-arm64.msi"));
            assert!(!win.matches_name("app-1.2.3-linux-x86_64.AppImage"));
            // a Windows version that only ever mentions x64 in prose still matches: the name is ours
            assert!(!win.matches_name("app-1.2.3-linux-x86_64.tar.gz"));
            let mac = Target { os: Os::Macos, arch: "arm64" };
            assert!(mac.matches_name("app-1.2.3-macos-arm64.dmg"));
            assert!(mac.matches_name("app-1.2.3-rc.1-macos-arm64.dmg"));
            assert!(!mac.matches_name("app-1.2.3-macos-x64.dmg"), "the Intel image is not Apple silicon's");
            assert!(mac.matches_name("app-1.2.3-macos-universal.dmg"), "the universal image runs on both");
            assert!(mac.is_fallback("app-1.2.3-macos-universal.dmg") && !mac.is_fallback("app-1.2.3-macos-arm64.dmg"));
            assert!(!mac.matches_name("app-1.2.3-windows-x64.msi"));
            let intel = Target { os: Os::Macos, arch: "x64" };
            assert!(intel.matches_name("app-1.2.3-macos-x64.dmg"));
            assert!(!intel.matches_name("app-1.2.3-macos-arm64.dmg"), "an Intel Mac can't run the Apple silicon image");
            assert!(intel.matches_name("app-1.2.3-macos-universal.dmg"));
            assert!(!win.is_fallback("app-1.2.3-macos-universal.dmg"), "only macOS has a universal file");
            let lin = Target { os: Os::Linux, arch: "aarch64" };
            assert!(lin.matches_name("app-1.2.3-linux-aarch64.AppImage"));
            assert!(!lin.matches_name("app-1.2.3-linux-x86_64.AppImage"));
            assert!(win.matches_portable("app-1.2.3-windows-x64-portable.zip"));
            assert!(!win.matches_portable("app-1.2.3-windows-x64.msi"), "the installer is not a portable copy's file");
            assert!(!win.matches_portable("app-1.2.3-windows-arm64-portable.zip"));
            assert!(lin.matches_portable("app-1.2.3-linux-aarch64.AppImage"));
        }
        // each Mac asks for its own architecture; an Intel build under Rosetta asks for the Mac's
        {
            assert_eq!(Target::of("macos", "aarch64"), Some(Target { os: Os::Macos, arch: "arm64" }));
            assert_eq!(Target::of("macos", "x86_64"), Some(Target { os: Os::Macos, arch: "x64" }));
            assert_eq!(Target::of("macos", machine_arch("x86_64", true)).map(|t| t.arch), Some("arm64"), "Rosetta");
            assert_eq!(Target::of("macos", machine_arch("x86_64", false)).map(|t| t.arch), Some("x64"));
            assert_eq!(Target::of("macos", machine_arch("aarch64", false)).map(|t| t.arch), Some("arm64"));
        }
        // asset ranks installers above archives
        {
            let msi = Asset::new("a-windows-x64.msi", "https://x/1", 10, None).expect("msi");
            let zip = Asset::new("a-windows-x64-portable.zip", "https://x/2", 10, None).expect("zip");
            let sums = Asset::new("SHA256SUMS.txt", "https://x/3", 10, None).expect("sums");
            assert!(msi.kind.rank() < zip.kind.rank());
            assert!(zip.kind.rank() < sums.kind.rank());
            assert_eq!(Asset::new("app.exe", "https://x/4", 1, None), None);
            assert!(msi.kind.installs_by().contains("installer"));
        }
    }
}
