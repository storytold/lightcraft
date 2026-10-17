//! The ICC profile of the screen (Windows).
//!
//! The window surface isn't colour-managed by Windows (only apps that ask, through ICM/WCS, get
//! colour matching), so while no monitor profile is chosen in Settings ▸ Display the previews are
//! shown through the display's default profile (`Services::screen_profile`). Read without `unsafe`
//! in our code, through the registry (`winreg`'s safe API), where Windows Color System keeps the
//! profile associations of each display device (monitor class `{4d36e96e-…}`):
//!
//! 1. **Per user** (when the device's `UsePerUserProfiles` is 1): `HKCU\Software\Microsoft\Windows
//!    NT\CurrentVersion\ICM\ProfileAssociations\Display\{class}\NNNN`, value `ICMProfile`.
//! 2. **System wide**: `HKLM\SYSTEM\CurrentControlSet\Control\Class\{class}\NNNN`, value `ICMProfile`.
//!
//! `ICMProfile` is a `REG_MULTI_SZ` of file names in the system colour directory
//! (`%SystemRoot%\System32\spool\drivers\color`); the default profile is the last one. Finding the
//! monitor the window is on needs `MonitorFromWindow`/`EnumDisplayDevices`, which only exist as
//! `unsafe` FFI, so the first display device (lowest `NNNN`) with a profile is used: right on
//! single-monitor setups and for the main display. The answer is cached for a few seconds.
//!
//! Sources: Microsoft's Windows Color System documentation (profile associations, per-user
//! profiles, the colour directory); own design.

#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
use crate::screen_icc::read_icc_file;

/// How long an answer is reused (the follow-screen loop asks about once a second).
#[cfg(windows)]
const TTL: Duration = Duration::from_secs(10);
/// At most this many display devices are looked at.
#[cfg(windows)]
const MAX_DEVICES: usize = 64;

/// One display device's associations.
#[derive(Clone, Debug, Default, PartialEq)]
struct Device {
    /// Registry subkey, `0000`, `0001`, …
    key: String,
    /// Whether the user chose "use my settings for this device".
    per_user: bool,
    user_profiles: Vec<String>,
    system_profiles: Vec<String>,
}

/// The reader, kept across calls to cache the answer.
#[cfg(windows)]
#[derive(Default)]
pub struct ScreenProfile {
    cache: Option<(Option<Vec<u8>>, Instant)>,
}

#[cfg(windows)]
impl ScreenProfile {
    /// The default profile of the display, or `None` (then previews are sRGB).
    pub fn icc(&mut self) -> Option<Vec<u8>> {
        if let Some((icc, at)) = &self.cache
            && at.elapsed() < TTL
        {
            return icc.clone();
        }
        let icc = read();
        self.cache = Some((icc.clone(), Instant::now()));
        icc
    }
}

#[cfg(windows)]
fn read() -> Option<Vec<u8>> {
    let devices = registry::devices();
    let name = pick_profile(&devices)?;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    let dir = std::path::Path::new(&root).join("System32").join("spool").join("drivers").join("color");
    read_icc_file(&profile_path(&dir, &name)?)
}

#[cfg(windows)]
mod registry {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    use super::{Device, MAX_DEVICES};

    const CLASS: &str = "{4d36e96e-e325-11ce-bfc1-08002be10318}";

    /// The display devices with their associations, sorted by key.
    pub(super) fn devices() -> Vec<Device> {
        let system = RegKey::predef(HKEY_LOCAL_MACHINE).open_subkey(format!("SYSTEM\\CurrentControlSet\\Control\\Class\\{CLASS}")).ok();
        let user = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!("Software\\Microsoft\\Windows NT\\CurrentVersion\\ICM\\ProfileAssociations\\Display\\{CLASS}"))
            .ok();
        let mut keys: Vec<String> = Vec::new();
        for parent in [&system, &user].into_iter().flatten() {
            for k in parent.enum_keys().take(MAX_DEVICES * 4).flatten() {
                if super::is_device_key(&k) && !keys.contains(&k) {
                    keys.push(k);
                }
            }
        }
        keys.sort();
        keys.truncate(MAX_DEVICES);
        keys.into_iter()
            .map(|key| {
                let sys = system.as_ref().and_then(|s| s.open_subkey(&key).ok());
                let usr = user.as_ref().and_then(|u| u.open_subkey(&key).ok());
                Device {
                    per_user: usr.as_ref().and_then(|u| u.get_value::<u32, _>("UsePerUserProfiles").ok()) == Some(1),
                    user_profiles: usr.as_ref().and_then(|u| u.get_value::<Vec<String>, _>("ICMProfile").ok()).unwrap_or_default(),
                    system_profiles: sys.as_ref().and_then(|s| s.get_value::<Vec<String>, _>("ICMProfile").ok()).unwrap_or_default(),
                    key,
                }
            })
            .collect()
    }
}

/// Device subkeys are four digits (`Properties` and others are skipped).
fn is_device_key(key: &str) -> bool {
    key.len() == 4 && key.bytes().all(|b| b.is_ascii_digit())
}

/// The default profile of a device: the last entry of the list that applies to it.
fn default_profile(device: &Device) -> Option<&str> {
    let list = if device.per_user { &device.user_profiles } else { &device.system_profiles };
    list.iter().rev().map(|s| s.trim()).find(|s| !s.is_empty())
}

/// The default profile of the first device (by key) that has one.
fn pick_profile(devices: &[Device]) -> Option<String> {
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by(|a, b| a.key.cmp(&b.key));
    sorted.into_iter().find_map(default_profile).map(str::to_owned)
}

/// Where a profile named in the registry lives: a bare file name in the colour directory, or an
/// absolute path as some calibration tools write. Names that would climb out of the directory are
/// refused.
fn profile_path(color_dir: &std::path::Path, name: &str) -> Option<std::path::PathBuf> {
    let windows_absolute = name.get(1..3) == Some(":\\") || name.starts_with("\\\\");
    if windows_absolute || std::path::Path::new(name).is_absolute() {
        return Some(std::path::PathBuf::from(name));
    }
    if name.is_empty() || name.contains(['\\', '/']) || name == "." || name == ".." {
        return None;
    }
    Some(color_dir.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn dev(key: &str, per_user: bool, user: &[&str], system: &[&str]) -> Device {
        Device {
            key: key.into(),
            per_user,
            user_profiles: user.iter().map(|s| s.to_string()).collect(),
            system_profiles: system.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn device_keys() {
        assert!(is_device_key("0000"));
        assert!(is_device_key("0012"));
        assert!(!is_device_key("Properties"));
        assert!(!is_device_key("00000"));
        assert!(!is_device_key("00a0"));
    }

    #[test]
    fn default_is_the_last_of_the_list_that_applies() {
        assert_eq!(default_profile(&dev("0000", false, &["u.icm"], &["a.icm", "b.icc"])), Some("b.icc"));
        assert_eq!(default_profile(&dev("0000", true, &["u1.icm", "u2.icm"], &["a.icm"])), Some("u2.icm"));
        assert_eq!(default_profile(&dev("0000", true, &[], &["a.icm"])), None, "per-user with no profile is sRGB");
        assert_eq!(default_profile(&dev("0000", false, &[], &["a.icm", " "])), Some("a.icm"));
    }

    #[test]
    fn first_device_with_a_profile() {
        let devices = vec![dev("0002", false, &[], &["c.icm"]), dev("0000", false, &[], &[]), dev("0001", true, &["b.icm"], &[])];
        assert_eq!(pick_profile(&devices).as_deref(), Some("b.icm"));
        assert_eq!(pick_profile(&[]), None);
    }

    #[test]
    fn profile_paths() {
        let dir = Path::new("color");
        assert_eq!(profile_path(dir, "display.icm"), Some(PathBuf::from("color").join("display.icm")));
        assert_eq!(profile_path(dir, "C:\\Profiles\\cal.icc"), Some(PathBuf::from("C:\\Profiles\\cal.icc")));
        assert_eq!(profile_path(dir, "..\\..\\x.icc"), None);
        assert_eq!(profile_path(dir, "../x.icc"), None);
        assert_eq!(profile_path(dir, ".."), None);
        assert_eq!(profile_path(dir, ""), None);
    }
}
