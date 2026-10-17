//! The ICC profile of the screen showing the app's window (Linux).
//!
//! Neither X11 nor Wayland colour-manages the window, so while no monitor profile is chosen in
//! Settings ▸ Display the previews are shown through the screen's profile
//! (`Services::screen_profile`), read about once a second so moving the window to another monitor
//! is followed. Two sources, both without `unsafe`:
//!
//! 1. **X11** (also XWayland): the ICC Profiles in X specification. The profile of Xinerama screen
//!    `n` is the `_ICC_PROFILE_n` property of the root window (`_ICC_PROFILE` for screen 0). The
//!    monitor is the RandR monitor the app's window overlaps most.
//! 2. **colord** over D-Bus: the default profile of the display device whose `XRANDR_name` is the
//!    window's monitor (the first display device when the monitor isn't known, e.g. on Wayland,
//!    where a client can't learn its position).
//!
//! Sources: the ICC Profiles in X specification 0.4 (freedesktop.org), the colord D-Bus API
//! reference; own design.

use std::time::{Duration, Instant};

use crate::screen_icc::{MAX_ICC, is_icc, read_icc_file};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{self, AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

/// How long a colord answer is reused (colord is a D-Bus round trip per device).
const COLORD_TTL: Duration = Duration::from_secs(10);
/// After a failed connection, how long until the next try.
const RETRY: Duration = Duration::from_secs(30);

/// A monitor: RandR output name and rectangle in root coordinates.
#[derive(Clone, Debug, PartialEq)]
struct Monitor {
    name: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// The reader, kept across calls so connections are opened once.
#[derive(Default)]
pub struct ScreenProfile {
    x11: Option<(RustConnection, usize)>,
    x11_failed: Option<Instant>,
    colord: Option<zbus::blocking::Connection>,
    colord_failed: Option<Instant>,
    /// (monitor name asked for, answer, when)
    colord_cache: Option<(Option<String>, Option<Vec<u8>>, Instant)>,
}

impl ScreenProfile {
    /// The profile of the monitor the window is mostly on, or `None` (then previews are sRGB).
    pub fn icc(&mut self) -> Option<Vec<u8>> {
        let monitor = match self.x11_profile() {
            (Some(icc), _) => return Some(icc),
            (None, monitor) => monitor,
        };
        self.colord_profile(monitor)
    }

    fn x11_conn(&mut self) -> Option<&(RustConnection, usize)> {
        if self.x11.is_none() {
            if std::env::var_os("DISPLAY").is_none() || self.x11_failed.is_some_and(|t| t.elapsed() < RETRY) {
                return None;
            }
            match RustConnection::connect(None) {
                Ok(c) => self.x11 = Some(c),
                Err(e) => {
                    log::debug!("screen profile: no X11 connection: {e}");
                    self.x11_failed = Some(Instant::now());
                }
            }
        }
        self.x11.as_ref()
    }

    /// The X11 root-window profile of the window's monitor, and the monitor's output name.
    fn x11_profile(&mut self) -> (Option<Vec<u8>>, Option<String>) {
        let Some((conn, screen)) = self.x11_conn() else { return (None, None) };
        match x11_read(conn, *screen) {
            Ok(found) => found,
            Err(e) => {
                // the server went away: reconnect later
                log::debug!("screen profile: X11: {e}");
                self.x11 = None;
                self.x11_failed = Some(Instant::now());
                (None, None)
            }
        }
    }

    fn colord_profile(&mut self, monitor: Option<String>) -> Option<Vec<u8>> {
        if let Some((name, icc, at)) = &self.colord_cache
            && *name == monitor
            && at.elapsed() < COLORD_TTL
        {
            return icc.clone();
        }
        if self.colord.is_none() {
            if self.colord_failed.is_some_and(|t| t.elapsed() < RETRY) {
                return None;
            }
            match zbus::blocking::connection::Builder::system().and_then(|b| b.method_timeout(Duration::from_millis(500)).build()) {
                Ok(c) => self.colord = Some(c),
                Err(e) => {
                    log::debug!("screen profile: no system bus: {e}");
                    self.colord_failed = Some(Instant::now());
                    return None;
                }
            }
        }
        let conn = self.colord.as_ref()?;
        let icc = match colord_read(conn, monitor.as_deref()) {
            Ok(icc) => icc,
            Err(e) => {
                log::debug!("screen profile: colord: {e}");
                None
            }
        };
        self.colord_cache = Some((monitor, icc.clone(), Instant::now()));
        icc
    }
}

type XResult<T> = Result<T, Box<dyn std::error::Error>>;

fn x11_read(conn: &RustConnection, screen: usize) -> XResult<(Option<Vec<u8>>, Option<String>)> {
    let root = conn.setup().roots.get(screen).ok_or("no X11 screen")?.root;
    let monitors = conn.randr_get_monitors(root, true)?.reply()?.monitors;
    let mut list = Vec::with_capacity(monitors.len().min(64));
    for m in monitors.iter().take(64) {
        let name = conn.get_atom_name(m.name)?.reply().map(|r| String::from_utf8_lossy(&r.name).into_owned()).unwrap_or_default();
        list.push(Monitor { name, x: m.x.into(), y: m.y.into(), w: m.width.into(), h: m.height.into() });
    }
    let index = own_window_rect(conn, root)?.map(|r| monitor_index(&list, r)).unwrap_or(0);
    let name = list.get(index).map(|m| m.name.clone()).filter(|n| !n.is_empty());
    for atom_name in profile_atoms(index) {
        let atom = conn.intern_atom(true, atom_name.as_bytes())?.reply()?.atom;
        if atom == u32::from(AtomEnum::NONE) {
            continue;
        }
        let prop = conn.get_property(false, root, atom, AtomEnum::CARDINAL, 0, MAX_ICC / 4)?.reply()?;
        if prop.format == 8 && is_icc(&prop.value) {
            return Ok((Some(prop.value), name));
        }
    }
    Ok((None, name))
}

/// The root properties to try for Xinerama screen `index`: its own, then screen 0's.
fn profile_atoms(index: usize) -> Vec<String> {
    if index == 0 { vec!["_ICC_PROFILE".into()] } else { vec![format!("_ICC_PROFILE_{index}"), "_ICC_PROFILE".into()] }
}

/// The app's top-level window in root coordinates: the active window when it is ours, otherwise
/// the first of ours in the window manager's client list (matched by `_NET_WM_PID`).
fn own_window_rect(conn: &RustConnection, root: xproto::Window) -> XResult<Option<(i32, i32, i32, i32)>> {
    let pid = std::process::id();
    let atom = |name: &[u8]| -> XResult<u32> { Ok(conn.intern_atom(true, name)?.reply()?.atom) };
    let (active, clients, wm_pid) = (atom(b"_NET_ACTIVE_WINDOW")?, atom(b"_NET_CLIENT_LIST")?, atom(b"_NET_WM_PID")?);
    if wm_pid == 0 {
        return Ok(None);
    }
    let mut candidates: Vec<u32> = Vec::new();
    for list in [active, clients] {
        if list == 0 {
            continue;
        }
        let prop = conn.get_property(false, root, list, AtomEnum::WINDOW, 0, 1024)?.reply()?;
        candidates.extend(prop.value32().into_iter().flatten());
    }
    for win in candidates.into_iter().take(1024) {
        let Ok(cookie) = conn.get_property(false, win, wm_pid, AtomEnum::CARDINAL, 0, 1) else { continue };
        let Ok(prop) = cookie.reply() else { continue };
        if prop.value32().and_then(|mut v| v.next()) != Some(pid) {
            continue;
        }
        let geo = conn.get_geometry(win)?.reply()?;
        let at = conn.translate_coordinates(win, root, 0, 0)?.reply()?;
        return Ok(Some((at.dst_x.into(), at.dst_y.into(), geo.width.into(), geo.height.into())));
    }
    Ok(None)
}

/// The monitor the rectangle overlaps most (0 when it overlaps none).
fn monitor_index(monitors: &[Monitor], (x, y, w, h): (i32, i32, i32, i32)) -> usize {
    let overlap = |m: &Monitor| -> i64 {
        let dx = (x.saturating_add(w)).min(m.x.saturating_add(m.w)) - x.max(m.x);
        let dy = (y.saturating_add(h)).min(m.y.saturating_add(m.h)) - y.max(m.y);
        i64::from(dx.max(0)) * i64::from(dy.max(0))
    };
    let mut best = (0, 0i64);
    for (i, m) in monitors.iter().enumerate() {
        let o = overlap(m);
        if o > best.1 {
            best = (i, o);
        }
    }
    best.0
}

fn colord_read(conn: &zbus::blocking::Connection, monitor: Option<&str>) -> zbus::Result<Option<Vec<u8>>> {
    use zbus::zvariant::OwnedObjectPath;
    const DEST: &str = "org.freedesktop.ColorManager";
    let manager = zbus::blocking::Proxy::new(conn, DEST, "/org/freedesktop/ColorManager", DEST)?;
    let devices: Vec<OwnedObjectPath> = manager.call("GetDevicesByKind", &("display",))?;
    let mut found: Vec<(Option<String>, Vec<OwnedObjectPath>)> = Vec::new();
    for path in devices.into_iter().take(32) {
        let device = zbus::blocking::Proxy::new(conn, DEST, path, "org.freedesktop.ColorManager.Device")?;
        let meta: std::collections::HashMap<String, String> = device.get_property("Metadata").unwrap_or_default();
        let profiles: Vec<OwnedObjectPath> = device.get_property("Profiles").unwrap_or_default();
        found.push((meta.get("XRANDR_name").cloned(), profiles));
    }
    let Some(profile) = pick_device(&found, monitor) else { return Ok(None) };
    let profile = zbus::blocking::Proxy::new(conn, DEST, profile.clone(), "org.freedesktop.ColorManager.Profile")?;
    let file: String = profile.get_property("Filename")?;
    Ok(read_icc_file(std::path::Path::new(&file)))
}

/// The default (first) profile of the device for `monitor`, else of the first device with one.
fn pick_device<'a, P>(devices: &'a [(Option<String>, Vec<P>)], monitor: Option<&str>) -> Option<&'a P> {
    let named = monitor.and_then(|m| devices.iter().find(|(name, _)| name.as_deref() == Some(m)));
    match named {
        Some((_, profiles)) => profiles.first(),
        None => devices.iter().find_map(|(_, p)| p.first()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icc(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(&(len as u32).to_be_bytes());
        b[36..40].copy_from_slice(b"acsp");
        b
    }

    fn mon(name: &str, x: i32, w: i32) -> Monitor {
        Monitor { name: name.into(), x, y: 0, w, h: 1080 }
    }

    #[test]
    fn window_on_the_monitor_it_overlaps_most() {
        let ms = [mon("DP-1", 0, 1920), mon("HDMI-1", 1920, 2560)];
        assert_eq!(monitor_index(&ms, (100, 100, 800, 600)), 0);
        assert_eq!(monitor_index(&ms, (1800, 100, 800, 600)), 1, "mostly on the second");
        assert_eq!(monitor_index(&ms, (-5000, 0, 10, 10)), 0, "off every monitor");
        assert_eq!(monitor_index(&[], (0, 0, 10, 10)), 0);
        assert_eq!(monitor_index(&ms, (i32::MAX - 1, i32::MAX - 1, i32::MAX, i32::MAX)), 0, "hostile sizes saturate");
    }

    #[test]
    fn xinerama_screen_atoms() {
        assert_eq!(profile_atoms(0), ["_ICC_PROFILE"]);
        assert_eq!(profile_atoms(2), ["_ICC_PROFILE_2", "_ICC_PROFILE"]);
    }

    #[test]
    fn icc_header_checked() {
        assert!(is_icc(&icc(128)));
        assert!(is_icc(&icc(4096)));
        assert!(!is_icc(&[]));
        assert!(!is_icc(&icc(64)[..]), "too short");
        let mut lying = icc(200);
        lying[0..4].copy_from_slice(&10_000u32.to_be_bytes());
        assert!(!is_icc(&lying), "size beyond the data");
        let mut wrong = icc(200);
        wrong[36] = b'x';
        assert!(!is_icc(&wrong));
    }

    #[test]
    fn colord_device_for_the_monitor() {
        let devs = vec![(None, vec![]), (Some("DP-1".to_string()), vec![1, 2]), (Some("HDMI-1".to_string()), vec![3])];
        assert_eq!(pick_device(&devs, Some("HDMI-1")), Some(&3));
        assert_eq!(pick_device(&devs, Some("DP-1")), Some(&1), "the default is the first profile");
        assert_eq!(pick_device(&devs, None), Some(&1), "unknown monitor: first device with a profile");
        assert_eq!(pick_device(&devs, Some("eDP-1")), Some(&1));
        assert_eq!(pick_device::<u8>(&[], None), None);
    }

    #[test]
    fn profile_files_validated() {
        let dir = std::env::temp_dir().join(format!("screen-profile-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.icc");
        std::fs::write(&good, icc(512)).unwrap();
        let bad = dir.join("bad.icc");
        std::fs::write(&bad, b"not a profile").unwrap();
        assert_eq!(read_icc_file(&good).map(|b| b.len()), Some(512));
        assert_eq!(read_icc_file(&bad), None);
        assert_eq!(read_icc_file(&dir.join("missing.icc")), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_display_no_profile() {
        // without an X server or colord the reader answers None rather than failing
        let mut sp = ScreenProfile { x11_failed: Some(Instant::now()), colord_failed: Some(Instant::now()), ..Default::default() };
        assert_eq!(sp.icc(), None);
    }
}
