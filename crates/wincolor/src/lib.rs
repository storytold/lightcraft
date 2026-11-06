//! The colour profile Windows assigns to the monitor showing a window.
//!
//! On an SDR desktop Windows doesn't colour-manage ordinary windows: the values an app draws go to
//! the monitor as they are, so on a wide-gamut monitor sRGB previews look oversaturated, and on a
//! calibrated one the calibration is ignored. Photo apps convert to the monitor's profile themselves.
//! LightCraft reads it here and shows previews through it while no profile is chosen in Settings ▸
//! Display, as it does with the screen's profile on macOS and colord's on Linux.
//!
//! When Windows colour-manages the desktop itself (HDR, or Auto Color Management on Windows 11), it
//! treats what an ordinary app draws as sRGB and converts it for the monitor; converting in the app
//! too would do it twice, so the answer is then [`MonitorColor::ManagedByWindows`].
//!
//! The profile comes from Windows Color System, which holds what Settings ▸ Display ▸ Color profile
//! and Color Management assign, per user or for the system: it is read afresh on every call, so a
//! change shows without restarting. (GDI's `GetICMProfileW` answers what the profile was when the
//! process first asked, and serves only as a fallback when the monitor's device can't be found.)
//!
//! The Win32 calls (`MonitorFromWindow`, `GetMonitorInfoW`, `EnumDisplayDevicesW`, the `Wcs…`
//! profile functions, `QueryDisplayConfig` + `DisplayConfigGetDeviceInfo`) are in the `win32` module,
//! the only place besides `crates/sysmem` where LightCraft allows `unsafe`. The API is safe, and any
//! failure answers [`MonitorColor::Unknown`].
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::PathBuf;

/// What Windows says about colour on the monitor showing a window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorColor {
    /// Windows colour-manages this monitor's desktop (HDR or Auto Color Management): an app draws
    /// sRGB and converts nothing.
    ManagedByWindows,
    /// The ICC profile assigned to the monitor (the user's own when they chose per-user profiles,
    /// else the system's).
    Profile(PathBuf),
    /// No profile is assigned: Windows treats the monitor as sRGB.
    NoProfile,
    /// Not known: not Windows, no such window, or a call failed.
    Unknown,
}

/// What Windows says about colour on the monitor the window `hwnd` is mostly on (the nearest one
/// when it is off screen or minimised). `hwnd` is the window's `HWND` as an integer; anything that
/// isn't a window answers [`MonitorColor::Unknown`].
pub fn monitor_color(hwnd: isize) -> MonitorColor {
    #[cfg(windows)]
    {
        win32::monitor_color(hwnd)
    }
    #[cfg(not(windows))]
    {
        let _ = hwnd;
        MonitorColor::Unknown
    }
}

/// `true` when Windows colour-manages the desktop on a display with these advanced-colour flags
/// (`DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO`'s bit field): advanced colour enabled (bit 1: HDR, or
/// Auto Color Management) or wide colour enforced (bit 2).
#[cfg(any(windows, test))]
fn managed(flags: u32) -> bool {
    flags & 0b110 != 0
}

/// The answer Windows' own colour management settles, `None` when the monitor's profile decides:
/// [`MonitorColor::ManagedByWindows`] while it colour-manages the desktop, and
/// [`MonitorColor::Unknown`] when its state can't be read (`managed` is `None`), since applying the
/// monitor's profile on top of Windows' conversion would convert twice.
#[cfg(any(windows, test))]
fn settled_by_windows(managed: Option<bool>) -> Option<MonitorColor> {
    match managed {
        Some(true) => Some(MonitorColor::ManagedByWindows),
        Some(false) => None,
        None => Some(MonitorColor::Unknown),
    }
}

/// The text of a NUL-terminated UTF-16 buffer (all of it without a NUL).
#[cfg(any(windows, test))]
fn until_nul(units: &[u16]) -> &[u16] {
    units.iter().position(|&u| u == 0).and_then(|n| units.get(..n)).unwrap_or(units)
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win32 {
    use super::{MonitorColor, managed, settled_by_windows, until_nul};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows_sys::Win32::Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
        DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    };
    use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateDCW, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW, DeleteDC, EnumDisplayDevicesW, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO,
        MONITORINFOEXW, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::ColorSystem::{
        CPST_NONE, CPT_ICC, GetColorDirectoryW, GetICMProfileW, WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
        WcsGetDefaultColorProfile, WcsGetDefaultColorProfileSize, WcsGetUsePerUserProfiles,
    };

    /// The most display paths read (a machine has a handful).
    const MAX_PATHS: u32 = 64;
    /// The most monitors looked at on one display output (several in clone mode).
    const MAX_MONITORS: u32 = 16;
    /// The device class of monitors in Windows Color System (`'mntr'`, as in an ICC header).
    const CLASS_MONITOR: u32 = u32::from_be_bytes(*b"mntr");
    /// The longest profile name read, in bytes (names are file names in the colour directory).
    const MAX_NAME_BYTES: u32 = 64 << 10;

    pub fn monitor_color(hwnd: isize) -> MonitorColor {
        let Some(device) = monitor_device(hwnd) else { return MonitorColor::Unknown };
        // (an advanced-colour state that can't be read, as on Windows before 10 1709 or with a
        // driver that doesn't say, is unknown: previews then stay sRGB, as before profiles were read)
        if let Some(answer) = settled_by_windows(managed_by_windows(&device)) {
            return answer;
        }
        match monitor_id(&device) {
            Some(id) => wcs_profile(&id),
            None => icm_profile(&device).map_or(MonitorColor::Unknown, MonitorColor::Profile),
        }
    }

    /// The GDI device name (`\\.\DISPLAY1`) of the monitor showing `hwnd`, NUL-terminated.
    fn monitor_device(hwnd: isize) -> Option<[u16; 32]> {
        if hwnd == 0 {
            return None;
        }
        // SAFETY: `MonitorFromWindow` takes any handle value: window handles are looked up in the
        // system's table, not dereferenced, and one that isn't a window gives a null monitor (or
        // the nearest monitor). It writes nothing.
        let monitor = unsafe { MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
        // SAFETY: `info` is a `MONITORINFOEXW` whose `cbSize` says so, and the pointer is taken from
        // the whole struct, so `GetMonitorInfoW` may write all of it (it writes at most `cbSize`
        // bytes). `monitor` came from `MonitorFromWindow` just above.
        let ok = unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) };
        (ok != 0 && !until_nul(&info.szDevice).is_empty() && info.szDevice.contains(&0)).then_some(info.szDevice)
    }

    /// The device id (`MONITOR\GSM7750\{…}\0002`) of the monitor on display output `device`, as
    /// Windows Color System names it, NUL-terminated: the first active monitor there.
    fn monitor_id(device: &[u16; 32]) -> Option<[u16; 128]> {
        if !device.contains(&0) {
            return None;
        }
        let mut first = None;
        for i in 0..MAX_MONITORS {
            let mut dd = DISPLAY_DEVICEW { cb: size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            // SAFETY: `device` is NUL-terminated (checked above) and `dd` is a `DISPLAY_DEVICEW` whose
            // `cb` says so; `EnumDisplayDevicesW` writes at most that struct and fails past the last
            // monitor.
            if unsafe { EnumDisplayDevicesW(device.as_ptr(), i, &mut dd, 0) } == 0 {
                break;
            }
            if until_nul(&dd.DeviceID).is_empty() || !dd.DeviceID.contains(&0) {
                continue;
            }
            if dd.StateFlags & DISPLAY_DEVICE_ACTIVE != 0 {
                return Some(dd.DeviceID);
            }
            first.get_or_insert(dd.DeviceID);
        }
        first
    }

    /// The profile Windows Color System assigns to monitor `id`: in the user's scope when they chose
    /// per-user profiles for it, else the system's.
    fn wcs_profile(id: &[u16; 128]) -> MonitorColor {
        let mut per_user = 0;
        // SAFETY: `id` is NUL-terminated (from `monitor_id`); one `BOOL` to write.
        let asked = unsafe { WcsGetUsePerUserProfiles(id.as_ptr(), CLASS_MONITOR, &mut per_user) };
        let scope = if asked != 0 && per_user != 0 { WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER } else { WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE };
        let mut bytes = 0u32;
        // SAFETY: as above; one `u32` to write (the name's size in bytes, its NUL included). It fails
        // when no profile is assigned in that scope.
        if unsafe { WcsGetDefaultColorProfileSize(scope, id.as_ptr(), CPT_ICC, CPST_NONE, 0, &mut bytes) } == 0 {
            return MonitorColor::NoProfile;
        }
        if !(4..=MAX_NAME_BYTES).contains(&bytes) {
            return MonitorColor::Unknown;
        }
        let mut name = vec![0u16; bytes.div_ceil(2) as usize];
        // SAFETY: `name` holds at least `bytes` bytes, the size given; Windows writes at most that.
        if unsafe { WcsGetDefaultColorProfile(scope, id.as_ptr(), CPT_ICC, CPST_NONE, 0, bytes, name.as_mut_ptr()) } == 0 {
            return MonitorColor::Unknown;
        }
        let name = PathBuf::from(OsString::from_wide(until_nul(&name)));
        if name.as_os_str().is_empty() {
            return MonitorColor::NoProfile;
        }
        // a name is a file in the colour directory (a full path is kept as it is)
        if name.is_absolute() {
            return MonitorColor::Profile(name);
        }
        color_directory().map_or(MonitorColor::Unknown, |dir| MonitorColor::Profile(dir.join(name)))
    }

    /// The system's colour directory (`…\spool\drivers\color`), where profiles are installed.
    fn color_directory() -> Option<PathBuf> {
        let mut dir = [0u16; 1024];
        let mut bytes = size_of_val(&dir) as u32;
        // SAFETY: a null machine name means this one; `dir` holds `bytes` bytes, and Windows writes at
        // most that many (failing when the path doesn't fit), updating `bytes`.
        let ok = unsafe { GetColorDirectoryW(std::ptr::null(), dir.as_mut_ptr(), &mut bytes) };
        let dir = until_nul(&dir);
        (ok != 0 && !dir.is_empty()).then(|| PathBuf::from(OsString::from_wide(dir)))
    }

    /// GDI's profile for the display `device`: what it was when this process first asked (see the
    /// crate notes), for when the monitor's device isn't found.
    fn icm_profile(device: &[u16; 32]) -> Option<PathBuf> {
        if !device.contains(&0) {
            return None;
        }
        // SAFETY: `device` is NUL-terminated (checked above); the driver, port and mode may be null,
        // which makes a DC for that display. A null result is handled below.
        let dc = unsafe { CreateDCW(std::ptr::null(), device.as_ptr(), std::ptr::null(), std::ptr::null()) };
        if dc.is_null() {
            return None;
        }
        // (profile paths are under the colour directory, well within this)
        let mut name = [0u16; 1024];
        let mut len = name.len() as u32;
        // SAFETY: `dc` is the valid DC created above; `name` holds `len` UTF-16 units and
        // `GetICMProfileW` writes at most that many (failing when the name doesn't fit), updating `len`.
        let ok = unsafe { GetICMProfileW(dc, &mut len, name.as_mut_ptr()) };
        // SAFETY: `dc` came from `CreateDCW` and is deleted exactly once, after its last use.
        unsafe { DeleteDC(dc) };
        let path = until_nul(&name);
        (ok != 0 && !path.is_empty()).then(|| PathBuf::from(OsString::from_wide(path)))
    }

    /// Whether Windows colour-manages the desktop on the display `device`; `None` when it can't be
    /// told.
    fn managed_by_windows(device: &[u16; 32]) -> Option<bool> {
        let (mut paths, mut modes) = (0u32, 0u32);
        // SAFETY: two counts to write; nothing else is passed.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths, &mut modes) } != ERROR_SUCCESS {
            return None;
        }
        if paths == 0 || paths > MAX_PATHS || modes > 2 * MAX_PATHS {
            return None;
        }
        let mut path = vec![DISPLAYCONFIG_PATH_INFO::default(); paths as usize];
        let mut mode = vec![DISPLAYCONFIG_MODE_INFO::default(); modes as usize];
        // SAFETY: the arrays hold `paths` and `modes` elements as the counts say; Windows writes at
        // most that many and lowers the counts to what it wrote. The topology id must be null with
        // `QDC_ONLY_ACTIVE_PATHS`.
        let r =
            unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut paths, path.as_mut_ptr(), &mut modes, mode.as_mut_ptr(), std::ptr::null_mut()) };
        if r != ERROR_SUCCESS {
            return None;
        }
        path.truncate(paths as usize);
        let wanted = until_nul(device);
        for p in &path {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: p.sourceInfo.adapterId,
                    id: p.sourceInfo.id,
                },
                ..Default::default()
            };
            // SAFETY: the header's type and size describe `source`, and the pointer is taken from the
            // whole struct, which `DisplayConfigGetDeviceInfo` fills (at most `size` bytes).
            if unsafe { DisplayConfigGetDeviceInfo((&raw mut source).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>()) } != 0 {
                continue;
            }
            if until_nul(&source.viewGdiDeviceName) != wanted {
                continue;
            }
            let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
                    size: size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>() as u32,
                    adapterId: p.targetInfo.adapterId,
                    id: p.targetInfo.id,
                },
                ..Default::default()
            };
            // SAFETY: as above, for `color`.
            if unsafe { DisplayConfigGetDeviceInfo((&raw mut color).cast::<DISPLAYCONFIG_DEVICE_INFO_HEADER>()) } != 0 {
                return None;
            }
            // SAFETY: the union's two views are a bit field and the same 32 bits as a `u32`; every
            // bit pattern is a valid `u32`.
            let flags = unsafe { color.Anonymous.value };
            return Some(managed(flags));
        }
        None
    }

    #[cfg(test)]
    pub fn desktop_window() -> isize {
        // SAFETY: `GetDesktopWindow` takes nothing and returns the desktop's handle.
        unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetDesktopWindow() as isize }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_is_unknown() {
        assert_eq!(monitor_color(0), MonitorColor::Unknown);
    }

    #[test]
    fn hostile_handles_never_crash() {
        // not windows: unknown, or (Windows answers the nearest monitor) a profile or managed
        for h in [1isize, -1, 0x7fff_ffff, isize::MIN, isize::MAX] {
            let _ = monitor_color(h);
        }
    }

    #[test]
    fn advanced_colour_flags_decide_who_manages_colour() {
        // supported only (an SDR desktop on an HDR-capable monitor): the app converts
        assert!(!managed(0b0001));
        assert!(!managed(0));
        // HDR on / Auto Color Management (advanced colour enabled), or wide colour enforced
        assert!(managed(0b0011));
        assert!(managed(0b0101));
        // forced off (bit 3) alone
        assert!(!managed(0b1000));
    }

    #[test]
    fn an_unreadable_advanced_colour_state_is_unknown_rather_than_a_profile() {
        assert_eq!(settled_by_windows(Some(true)), Some(MonitorColor::ManagedByWindows));
        assert_eq!(settled_by_windows(Some(false)), None, "an SDR desktop: the monitor's profile decides");
        // Windows may be converting already: a profile on top would convert twice
        assert_eq!(settled_by_windows(None), Some(MonitorColor::Unknown));
    }

    #[test]
    fn utf16_text_stops_at_its_nul() {
        assert_eq!(until_nul(&[65, 66, 0, 67]), &[65, 66]);
        assert_eq!(until_nul(&[65, 66]), &[65, 66]);
        assert_eq!(until_nul(&[0, 65]), &[] as &[u16]);
    }

    /// The desktop's monitor has an answer, and a profile it names is a file that exists.
    #[cfg(windows)]
    #[test]
    fn the_desktop_monitor_names_a_profile_that_exists_or_windows_manages_colour() {
        let answer = monitor_color(win32::desktop_window());
        eprintln!("the desktop's monitor: {answer:?}");
        match answer {
            MonitorColor::Profile(p) => assert!(p.is_file(), "{}", p.display()),
            MonitorColor::ManagedByWindows | MonitorColor::NoProfile => {}
            MonitorColor::Unknown => lightcraft_testkit::skip!(System, "no monitor answers for the desktop window (a session without a display)"),
        }
    }
}
