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
    /// `x64`, `x86`, `arm64`, `x86_64`, `aarch64`, `riscv64`.
    pub arch: &'static str,
}

impl Target {
    /// The running platform, or `None` on a platform the release pipeline does not build for (the
    /// web build has no updater at all).
    pub fn current() -> Option<Target> {
        Target::of(std::env::consts::OS, std::env::consts::ARCH)
    }

    /// [`Target::current`] from a pair of `std::env::consts` strings, so the mapping can be tested
    /// for every platform we ship.
    pub fn of(os: &str, arch: &str) -> Option<Target> {
        let (os, arch) = match (os, arch) {
            ("windows", "x86_64") => (Os::Windows, "x64"),
            ("windows", "x86") => (Os::Windows, "x86"),
            ("windows", "aarch64") => (Os::Windows, "arm64"),
            ("macos", "aarch64") | ("macos", "x86_64") => (Os::Macos, "universal"),
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
        match self.os {
            // the macOS build is one universal app; either arch slug is the same file
            Os::Macos => lower.contains("-macos-universal") || lower.contains("-macos-x86_64") || lower.contains("-macos-aarch64"),
            _ => lower.contains(&format!("-{}-{}", self.os.slug(), self.arch)),
        }
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
            "universal" => "universal",
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_artifacts() {
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

    #[test]
    fn maps_the_shipped_platforms() {
        assert_eq!(Target::of("windows", "x86_64"), Some(Target { os: Os::Windows, arch: "x64" }));
        assert_eq!(Target::of("windows", "x86"), Some(Target { os: Os::Windows, arch: "x86" }));
        assert_eq!(Target::of("windows", "aarch64"), Some(Target { os: Os::Windows, arch: "arm64" }));
        assert_eq!(Target::of("macos", "aarch64"), Some(Target { os: Os::Macos, arch: "universal" }));
        assert_eq!(Target::of("macos", "x86_64"), Some(Target { os: Os::Macos, arch: "universal" }));
        assert_eq!(Target::of("linux", "riscv64"), Some(Target { os: Os::Linux, arch: "riscv64" }));
        assert_eq!(Target::of("freebsd", "x86_64"), Some(Target { os: Os::Freebsd, arch: "x86_64" }));
        assert_eq!(Target::of("android", "aarch64"), None);
        assert_eq!(Target::of("windows", "powerpc"), None);
    }

    #[test]
    fn picks_only_this_platforms_files() {
        let win = Target { os: Os::Windows, arch: "x64" };
        assert!(win.matches_name("app-1.2.3-windows-x64.msi"));
        assert!(!win.matches_name("app-1.2.3-windows-x64-portable.zip"), "the portable zip is a manual install");
        assert!(!win.matches_name("app-1.2.3-windows-arm64.msi"));
        assert!(!win.matches_name("app-1.2.3-linux-x86_64.AppImage"));
        // a Windows version that only ever mentions x64 in prose still matches: the name is ours
        assert!(!win.matches_name("app-1.2.3-linux-x86_64.tar.gz"));
        let mac = Target { os: Os::Macos, arch: "universal" };
        assert!(mac.matches_name("app-1.2.3-macos-universal.dmg"));
        assert!(mac.matches_name("app-1.2.3-macos-aarch64.dmg"), "a per-arch dmg is still the macOS app");
        assert!(!mac.matches_name("app-1.2.3-windows-x64.msi"));
        let lin = Target { os: Os::Linux, arch: "aarch64" };
        assert!(lin.matches_name("app-1.2.3-linux-aarch64.AppImage"));
        assert!(!lin.matches_name("app-1.2.3-linux-x86_64.AppImage"));
    }

    #[test]
    fn describes_what_will_happen() {
        assert!(Target { os: Os::Linux, arch: "x86_64" }.describe().contains("replace this program"));
        assert!(Target { os: Os::Windows, arch: "x64" }.describe().contains("installer"));
        assert!(Target { os: Os::Linux, arch: "riscv64" }.describe().contains("open the folder"));
    }

    #[test]
    fn asset_ranks_installers_above_archives() {
        let msi = Asset::new("a-windows-x64.msi", "https://x/1", 10, None).expect("msi");
        let zip = Asset::new("a-windows-x64-portable.zip", "https://x/2", 10, None).expect("zip");
        let sums = Asset::new("SHA256SUMS.txt", "https://x/3", 10, None).expect("sums");
        assert!(msi.kind.rank() < zip.kind.rank());
        assert!(zip.kind.rank() < sums.kind.rank());
        assert_eq!(Asset::new("app.exe", "https://x/4", 1, None), None);
        assert!(msi.kind.installs_by().contains("installer"));
    }
}
