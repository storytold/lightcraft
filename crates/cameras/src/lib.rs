//! Cameras and phones that don't mount as a disk: they talk PTP or MTP over USB, and the operating
//! system's own driver owns the connection. LightCraft reads them through that driver instead of
//! talking USB itself, so nothing has to be installed or swapped (a raw-USB client would need
//! Zadig/WinUSB on Windows and would fight `ptpcamerad` on macOS).
//!
//! - **Windows:** Windows Portable Devices (WPD), the API Explorer and the Photos app use
//!   ([`wpd`]). The device is opened read-only (`GENERIC_READ`): nothing on the camera is ever
//!   changed or deleted.
//! - **macOS:** ImageCaptureCore, the framework Image Capture and Photos use (`ica`). The system's
//!   camera daemon keeps the USB connection and serves it to every app; sessions are read-only.
//!   The framework answers on the main thread only, so cameras are read while the main thread runs
//!   an event loop (the app) or is the caller (the CLI).
//! - **Elsewhere:** not yet. [`cameras`] lists none and [`Session::open`] says so, so callers need
//!   no platform checks. Linux (PTP over `nusb`) can implement the same three calls: list cameras,
//!   list a camera's photos, copy one photo.
//!
//! This is LightCraft's second crate allowed `unsafe` (after `crates/sysmem`): the system calls live
//! in the `wpd` (COM) and `ica` (Objective-C) modules, each block with a `// SAFETY:` note, behind
//! the safe API below.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::atomic::AtomicBool;

#[cfg(target_os = "macos")]
mod ica;
mod layout;
#[cfg(windows)]
mod wpd;

/// A connected camera or phone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Camera {
    /// The operating system's id for the device (Windows: its PnP device path; macOS: its
    /// ImageCaptureCore UUID). Stable while it stays plugged in; pass it to [`Session::open`].
    pub id: String,
    /// The name the system shows for it ("X100T", "Pixel 8").
    pub name: String,
    /// Its manufacturer ("FUJIFILM"), when the device reports one.
    pub manufacturer: String,
}

/// A file on a camera.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraFile {
    /// The device's id for the file (opaque; pass it back to [`Session::copy_to`]).
    pub object: String,
    /// Its file name on the card ("DSCF2896.RAF").
    pub name: String,
    /// The folders above it, from the storage down, joined with `/` ("DCIM/122_FUJI").
    pub folder: String,
    /// Its size in bytes, when the device reports it.
    pub size: Option<u64>,
    /// When the camera says it was taken or written, as local `YYYY-MM-DDTHH:MM:SS` (the camera's
    /// clock; the format of a photo's capture time in the catalog). `None` when it reports none.
    pub created: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// This platform can't read cameras yet.
    Unsupported,
    /// Stopped by the caller's cancel flag.
    Cancelled,
    /// The device or the system API refused (unplugged, locked phone, …), with its message.
    Device(String),
    /// Writing the copy failed (disk full, …).
    Write(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => f.write_str("reading cameras that don't mount as a disk isn't supported on this platform yet"),
            Error::Cancelled => f.write_str("cancelled"),
            Error::Device(m) => write!(f, "camera: {m}"),
            Error::Write(m) => write!(f, "writing the copy: {m}"),
        }
    }
}

impl std::error::Error for Error {}

/// The cameras and phones connected now. Empty (not an error) on platforms without support.
pub fn cameras() -> Result<Vec<Camera>, Error> {
    #[cfg(windows)]
    {
        wpd::cameras()
    }
    #[cfg(target_os = "macos")]
    {
        ica::cameras()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(Vec::new())
    }
}

/// An open connection to one camera. Use it on the thread that opened it (on Windows it holds COM
/// objects of that thread's apartment); it is closed when dropped.
pub struct Session {
    #[cfg(windows)]
    inner: wpd::Session,
    #[cfg(target_os = "macos")]
    inner: ica::Session,
}

impl Session {
    /// Open the camera `id` (from [`cameras`]) for reading.
    pub fn open(id: &str) -> Result<Session, Error> {
        #[cfg(windows)]
        {
            Ok(Session { inner: wpd::Session::open(id)? })
        }
        #[cfg(target_os = "macos")]
        {
            Ok(Session { inner: ica::Session::open(id)? })
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = id;
            Err(Error::Unsupported)
        }
    }

    /// Every file under the camera's `DCIM` folders (the DCF layout cameras and phones use), or on
    /// its whole storage when it has none. `found` is called with the running count as folders are
    /// read (a full card can hold thousands of files). Folders nest at most [`MAX_DEPTH`] deep.
    pub fn files(&self, cancel: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.files(cancel, found)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (cancel, found);
            Err(Error::Unsupported)
        }
    }

    /// Copy `file`'s bytes into `out`, calling `progress` with the bytes copied so far. Returns the
    /// byte count. `cancel` is checked between reads.
    pub fn copy_to(&self, file: &CameraFile, out: &mut dyn std::io::Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
        #[cfg(any(windows, target_os = "macos"))]
        {
            self.inner.copy_to(file, out, cancel, progress)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let _ = (file, out, cancel, progress);
            Err(Error::Unsupported)
        }
    }
}

/// How deep [`Session::files`] follows folders below a storage (DCIM/100XXXXX is 2).
pub const MAX_DEPTH: usize = 8;

/// An OLE Automation date (days since 1899-12-30, the fraction is the time of day; what WPD reports
/// for object dates) as `YYYY-MM-DDTHH:MM:SS`. `None` for no date (0, which cameras without a
/// clock report, shows as 1899-12-30), NaN/inf, or anything before 1970 or after 2200.
pub fn ole_date_to_iso(days: f64) -> Option<String> {
    if !days.is_finite() {
        return None;
    }
    // 1970-01-01 is day 25569; 2200-12-31 is day 109939
    if !(25569.0..109940.0).contains(&days) {
        return None;
    }
    let secs = (days * 86_400.0).round() as i64;
    let (day, sec) = (secs.div_euclid(86_400) - 25_569, secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(day);
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", sec / 3600, sec / 60 % 60, sec % 60))
}

/// Seconds since 1970-01-01 on a local clock as `YYYY-MM-DDTHH:MM:SS`. `None` before 1970 or
/// after 2200, like [`ole_date_to_iso`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn unix_to_iso(secs: i64) -> Option<String> {
    // 2201-01-01 is day 84371
    if !(0..84_371 * 86_400).contains(&secs) {
        return None;
    }
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sec = secs.rem_euclid(86_400);
    Some(format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}", sec / 3600, sec / 60 % 60, sec % 60))
}

/// A local `YYYY-MM-DDTHH:MM:SS` timestamp (anything after the seconds, such as a fraction or a zone,
/// is ignored) as seconds since 1970-01-01 on the same clock, for comparing a camera's date with a
/// photo's capture time. `None` when it isn't one.
pub fn local_seconds(iso: &str) -> Option<i64> {
    let b = iso.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let s = b.get(r)?;
        if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(s).ok()?.parse().ok()
    };
    let sep = |i: usize, c: &[u8]| b.get(i).is_some_and(|x| c.contains(x));
    if !(sep(4, b"-:") && sep(7, b"-:") && sep(10, b"T ") && sep(13, b":") && sep(16, b":")) {
        return None;
    }
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, s) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, m as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s)
}

/// (year, month, day) to days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Days since 1970-01-01 to (year, month, day) in the proleptic Gregorian calendar (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ole_dates_become_local_timestamps() {
        // 2026-10-09 18:48:46 (an X100T capture) = 46304 days + 67726 s
        let d = 46304.0 + 67726.0 / 86400.0;
        assert_eq!(ole_date_to_iso(d).as_deref(), Some("2026-10-09T18:48:46"));
        assert_eq!(ole_date_to_iso(25569.0).as_deref(), Some("1970-01-01T00:00:00"));
        // leap day, and the last second of a day rounding into the next
        assert_eq!(ole_date_to_iso(45351.5).as_deref(), Some("2024-02-29T12:00:00"));
        assert_eq!(ole_date_to_iso(45351.0 + 86399.6 / 86400.0).as_deref(), Some("2024-03-01T00:00:00"));
    }

    #[test]
    fn missing_and_hostile_dates_are_none() {
        for d in [0.0, -1.0, 1.0, 25568.99, 109940.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX, f64::MIN] {
            assert_eq!(ole_date_to_iso(d), None, "{d}");
        }
    }

    #[test]
    fn civil_dates_round_trip_known_days() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_735), (2026, 10, 9));
    }

    #[test]
    fn local_timestamps_parse_to_comparable_seconds() {
        let a = local_seconds("2026-10-11T09:17:08").unwrap();
        assert_eq!(local_seconds("2026-10-11T09:17:10"), Some(a + 2));
        // EXIF spelling, fractions and zones are ignored
        assert_eq!(local_seconds("2026:10:11 09:17:08"), Some(a));
        assert_eq!(local_seconds("2026-10-11T09:17:08.250+02:00"), Some(a));
        assert_eq!(local_seconds("1970-01-01T00:00:00"), Some(0));
        assert_eq!(local_seconds("2024-03-01T00:00:00").unwrap() - local_seconds("2024-02-28T00:00:00").unwrap(), 2 * 86_400);
        // round trip with the OLE date conversion
        let iso = ole_date_to_iso(46306.0 + 33428.0 / 86400.0).unwrap();
        assert_eq!(local_seconds(&iso), Some(days_from_civil(2026, 10, 11) * 86_400 + 33428));
    }

    #[test]
    fn non_timestamps_are_none() {
        for s in [
            "",
            "2026",
            "2026-10-11",
            "2026-13-01T00:00:00",
            "2026-10-11T24:00:00",
            "2026-10-11T09:61:00",
            "x026-10-11T09:17:08",
            "2026-10-11T09:17:0",
            "２０２６-10-11T09:17:08",
        ] {
            assert_eq!(local_seconds(s), None, "{s}");
        }
    }

    #[test]
    fn unix_seconds_become_local_timestamps() {
        assert_eq!(unix_to_iso(0).as_deref(), Some("1970-01-01T00:00:00"));
        let t = local_seconds("2026-10-09T18:48:46").unwrap();
        assert_eq!(unix_to_iso(t).as_deref(), Some("2026-10-09T18:48:46"));
        assert_eq!(unix_to_iso(local_seconds("2200-12-31T23:59:59").unwrap()).as_deref(), Some("2200-12-31T23:59:59"));
        for s in [-1, local_seconds("2201-01-01T00:00:00").unwrap(), i64::MIN, i64::MAX] {
            assert_eq!(unix_to_iso(s), None, "{s}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn without_a_served_main_queue_cameras_fail_instead_of_hanging() {
        // the test harness's main thread is parked, so ImageCaptureCore never answers: nothing is
        // listed and opening is an error, each within its time limit
        let t = std::time::Instant::now();
        assert_eq!(cameras(), Ok(Vec::new()));
        assert!(matches!(Session::open("no-such-camera"), Err(Error::Device(_))));
        assert!(t.elapsed() < std::time::Duration::from_secs(20), "{:?}", t.elapsed());
    }

    #[test]
    fn other_platforms_list_no_cameras() {
        if cfg!(not(any(windows, target_os = "macos"))) {
            assert_eq!(cameras(), Ok(Vec::new()));
            assert!(matches!(Session::open("x"), Err(Error::Unsupported)));
        }
    }
}
