//! The ICC profile Windows assigns to the monitor showing LightCraft's window.
//!
//! On an SDR desktop Windows passes what an app draws to the monitor unchanged, so sRGB previews
//! look oversaturated on a wide-gamut monitor and ignore a calibration. While no monitor profile is
//! chosen in Settings ▸ Display, previews are shown through the profile Windows uses for the window's
//! monitor (`Services::screen_profile`, see `lightcraft_wincolor`), followed when the window moves to
//! another monitor or the profile changes in Windows' Color Management. When Windows colour-manages
//! the desktop itself (HDR, Auto Color Management) previews stay sRGB, as Windows expects then.

use lightcraft_wincolor::MonitorColor;

/// The service the UI polls, for the window `cc` belongs to; `None` when its handle isn't a Win32 one.
pub fn service(cc: &eframe::CreationContext<'_>) -> Option<lightcraft_ui_egui::ScreenProfileFn> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::Win32(handle) = cc.window_handle().ok()?.as_raw() else { return None };
    let hwnd = handle.hwnd.get();
    let mut file = crate::profile_file::Cached::default();
    let mut last: Option<MonitorColor> = None;
    Some(Box::new(move || {
        let answer = lightcraft_wincolor::monitor_color(hwnd);
        if last.as_ref() != Some(&answer) {
            log::info!("screen profile: {}", describe(&answer));
            last = Some(answer.clone());
        }
        match answer {
            MonitorColor::Profile(path) => file.read(&path),
            MonitorColor::ManagedByWindows | MonitorColor::NoProfile | MonitorColor::Unknown => None,
        }
    }))
}

/// For the log: what the window's monitor answered.
fn describe(answer: &MonitorColor) -> String {
    match answer {
        MonitorColor::Profile(path) => format!("the monitor's profile, {}", path.display()),
        MonitorColor::ManagedByWindows => "Windows colour-manages this monitor (HDR or Auto Color Management): previews are sRGB".into(),
        MonitorColor::NoProfile => "no profile is assigned to the monitor: previews are sRGB".into(),
        MonitorColor::Unknown => "not known for this monitor: previews are sRGB".into(),
    }
}
