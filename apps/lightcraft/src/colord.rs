//! The ICC profile the desktop assigns to the display (Linux, through colord).
//!
//! GNOME, KDE and most other desktops keep each monitor's profile in colord
//! (`org.freedesktop.ColorManager` on the system bus): Settings ▸ Color writes it there, and a
//! calibrator (DisplayCAL, gnome-color-manager) installs its profile there. While no monitor profile
//! is chosen in Settings ▸ Display, previews are shown through that profile
//! (`Services::screen_profile`). A background thread asks colord every few seconds, so the UI never
//! waits on D-Bus; without colord, or a display with no profile, the previews stay sRGB.
//!
//! With several displays, the window's own one isn't known here (eframe doesn't say which monitor a
//! window is on), so the profile is used only when every display that has one has the same: a
//! mixed set stays sRGB rather than guess.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// How often colord is asked again (a profile change in the desktop's settings shows within this).
const POLL: Duration = Duration::from_secs(3);
/// A wait before trying again when colord isn't there (not installed, no system bus).
const RETRY: Duration = Duration::from_secs(30);
/// The largest profile file read (real display profiles are a few KiB to a few MiB).
const MAX_PROFILE: u64 = 16 << 20;

const SERVICE: &str = "org.freedesktop.ColorManager";
const DEVICE: &str = "org.freedesktop.ColorManager.Device";
const PROFILE: &str = "org.freedesktop.ColorManager.Profile";

/// Start the background reader and return the service the UI polls (`Services::screen_profile`).
pub fn service() -> Box<dyn FnMut() -> Option<Vec<u8>>> {
    let shared: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
    let writer = Arc::clone(&shared);
    let spawned = std::thread::Builder::new().name("colord".into()).spawn(move || {
        loop {
            let wait = match connect() {
                Some(conn) => {
                    let icc = read(&conn);
                    *writer.lock().unwrap_or_else(PoisonError::into_inner) = icc;
                    POLL
                }
                None => RETRY,
            };
            std::thread::sleep(wait);
            // the app is gone: nobody reads the profile any more
            if Arc::strong_count(&writer) == 1 {
                return;
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("screen profile: no colord reader: {e}");
    }
    Box::new(move || shared.lock().unwrap_or_else(PoisonError::into_inner).clone())
}

fn connect() -> Option<zbus::blocking::Connection> {
    zbus::blocking::connection::Builder::system().ok()?.method_timeout(Duration::from_secs(2)).build().ok()
}

/// The profile colord assigns to the displays, as file bytes (see the module notes for several displays).
fn read(conn: &zbus::blocking::Connection) -> Option<Vec<u8>> {
    let manager = zbus::blocking::Proxy::new(conn, SERVICE, "/org/freedesktop/ColorManager", SERVICE).ok()?;
    let devices: Vec<zbus::zvariant::OwnedObjectPath> = manager.call("GetDevicesByKind", &("display",)).ok()?;
    let mut files = Vec::new();
    for device in devices.iter().take(64) {
        let Some(proxy) = zbus::blocking::Proxy::new(conn, SERVICE, device.as_str(), DEVICE).ok() else { continue };
        let profiles: Vec<zbus::zvariant::OwnedObjectPath> = proxy.get_property("Profiles").unwrap_or_default();
        // colord lists a device's profiles default first
        let Some(first) = profiles.first() else { continue };
        let Some(profile) = zbus::blocking::Proxy::new(conn, SERVICE, first.as_str(), PROFILE).ok() else { continue };
        if let Ok(file) = profile.get_property::<String>("Filename") {
            files.push(file);
        }
    }
    let file = pick(&files)?;
    load(std::path::Path::new(file))
}

/// The profile to use for these displays' profile files: the one they share, `None` with none or
/// with different ones (empty names are ignored).
fn pick(files: &[String]) -> Option<&str> {
    let mut named = files.iter().map(String::as_str).filter(|f| !f.is_empty());
    let first = named.next()?;
    named.all(|f| f == first).then_some(first)
}

/// The bytes of profile file `path`, `None` when it can't be read or is implausibly large.
fn load(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(MAX_PROFILE + 1).read_to_end(&mut bytes).ok()?;
    (!bytes.is_empty() && bytes.len() as u64 <= MAX_PROFILE).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_shared_profile_is_used_and_a_mixed_set_is_not() {
        let s = |v: &[&str]| v.iter().map(|f| f.to_string()).collect::<Vec<_>>();
        assert_eq!(pick(&s(&[])), None);
        assert_eq!(pick(&s(&["/a.icc"])), Some("/a.icc"));
        assert_eq!(pick(&s(&["/a.icc", "", "/a.icc"])), Some("/a.icc"));
        assert_eq!(pick(&s(&["/a.icc", "/b.icc"])), None, "which one the window is on isn't known");
        assert_eq!(pick(&s(&[""])), None);
    }

    #[test]
    fn profile_files_are_read_with_a_size_cap() {
        let dir = std::env::temp_dir().join(format!("colord-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("small.icc");
        std::fs::write(&small, b"acsp").unwrap();
        assert_eq!(load(&small).as_deref(), Some(&b"acsp"[..]));
        let empty = dir.join("empty.icc");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(load(&empty), None);
        assert_eq!(load(&dir.join("missing.icc")), None);
        let big = dir.join("big.icc");
        std::fs::File::create(&big).unwrap().set_len(MAX_PROFILE + 1).unwrap();
        assert_eq!(load(&big), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
