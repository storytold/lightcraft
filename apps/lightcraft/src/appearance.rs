//! The desktop's dark/light setting for Settings ▸ Interface ▸ Appearance Mode ▸ Auto, on Linux
//! desktops whose windowing layer reports no theme to egui (many Wayland compositors).
//!
//! The XDG settings portal is read once, then its `SettingChanged` signal is followed on a worker
//! thread that wakes the UI only when the value changes. Nothing is polled. `gsettings` is a
//! one-shot fallback at start-up, run by absolute path, only when the portal gives no answer.
//! Elsewhere egui's own report (`Context::system_theme`) is used.

#[cfg(target_os = "linux")]
const PORTAL_NAMESPACE: &str = "org.freedesktop.appearance";
#[cfg(target_os = "linux")]
const PORTAL_KEY: &str = "color-scheme";

#[cfg(target_os = "linux")]
fn portal_proxy() -> Option<zbus::blocking::Proxy<'static>> {
    let conn = zbus::blocking::connection::Builder::session().ok()?.method_timeout(std::time::Duration::from_secs(2)).build().ok()?;
    zbus::blocking::Proxy::new(&conn, "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.Settings").ok()
}

#[cfg(target_os = "linux")]
fn portal_theme(proxy: &zbus::blocking::Proxy<'_>) -> Option<egui::Theme> {
    let value: zbus::zvariant::OwnedValue = proxy.call("Read", &(PORTAL_NAMESPACE, PORTAL_KEY)).ok()?;
    decode(portal_code(value)?)
}

/// The `u32` inside a portal value. `Settings.Read` answers a variant, and some portals nest
/// another variant inside it.
#[cfg(target_os = "linux")]
fn portal_code(value: zbus::zvariant::OwnedValue) -> Option<u32> {
    let mut value: zbus::zvariant::Value<'_> = value.into();
    for _ in 0..4 {
        match value {
            zbus::zvariant::Value::Value(inner) => value = *inner,
            other => return u32::try_from(other).ok(),
        }
    }
    None
}

/// The portal's `color-scheme`: 0 = no preference, 1 = prefer dark, 2 = prefer light.
#[cfg(target_os = "linux")]
fn decode(code: u32) -> Option<egui::Theme> {
    match code {
        1 => Some(egui::Theme::Dark),
        2 => Some(egui::Theme::Light),
        _ => None,
    }
}

/// One-shot fallback for desktops without the settings portal. It runs once at start-up, by
/// absolute path, and is never repeated.
#[cfg(target_os = "linux")]
fn gtk_theme() -> Option<egui::Theme> {
    let exe = ["/usr/bin/gsettings", "/bin/gsettings", "/usr/local/bin/gsettings"].into_iter().find(|p| std::path::Path::new(p).is_file())?;
    let output = std::process::Command::new(exe).args(["get", "org.gnome.desktop.interface", "color-scheme"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_gtk_scheme(std::str::from_utf8(&output.stdout).ok()?)
}

#[cfg(target_os = "linux")]
fn parse_gtk_scheme(s: &str) -> Option<egui::Theme> {
    match s.trim().trim_matches('\'') {
        "prefer-dark" => Some(egui::Theme::Dark),
        "prefer-light" => Some(egui::Theme::Light),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn code_of(theme: Option<egui::Theme>) -> u8 {
    match theme {
        Some(egui::Theme::Dark) => 1,
        Some(egui::Theme::Light) => 2,
        None => 0,
    }
}

/// Wake the first frame after a slow initial Read as well as subsequent real changes.
#[cfg(target_os = "linux")]
fn publish(value: &std::sync::atomic::AtomicU8, wake: &std::sync::OnceLock<egui::Context>, code: u8) {
    if value.swap(code, std::sync::atomic::Ordering::Relaxed) != code
        && let Some(ctx) = wake.get()
    {
        ctx.request_repaint();
    }
}

/// The host's system-appearance service (Linux), or `None` where egui's report is enough.
pub fn service() -> Option<lightcraft_ui_egui::SystemThemeFn> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::{
            Arc, OnceLock,
            atomic::{AtomicU8, Ordering},
        };
        let value = Arc::new(AtomicU8::new(0));
        let wake: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        let (worker_value, worker_wake) = (Arc::clone(&value), Arc::clone(&wake));
        let (ready, wait) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("appearance-portal".into())
            .spawn(move || {
                let proxy = portal_proxy();
                // Subscribe before Read so a change during startup is queued rather than lost.
                let signals = proxy.as_ref().and_then(|proxy| proxy.receive_signal("SettingChanged").ok());
                let initial = proxy.as_ref().and_then(portal_theme).or_else(gtk_theme);
                publish(&worker_value, &worker_wake, code_of(initial));
                let _ = ready.send(());
                let Some(signals) = signals else { return };
                // Blocks until the portal sends a signal: no polling.
                for message in signals {
                    let Ok((namespace, key, changed)) = message.body().deserialize::<(String, String, zbus::zvariant::OwnedValue)>() else {
                        continue;
                    };
                    if namespace != PORTAL_NAMESPACE || key != PORTAL_KEY {
                        continue;
                    }
                    let code = code_of(portal_code(changed).and_then(decode));
                    publish(&worker_value, &worker_wake, code);
                }
            })
            .is_err()
        {
            return None;
        }
        // The first frame shows the right theme when the portal answers quickly; a slow one
        // only delays the switch (the worker wakes the UI when the value changes).
        let _ = wait.recv_timeout(std::time::Duration::from_millis(250));
        Some(Box::new(move |ctx| {
            let _ = wake.set(ctx.clone());
            decode(u32::from(value.load(Ordering::Relaxed)))
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn delayed_initial_read_repaints_only_on_change() {
        use std::sync::{
            Arc, OnceLock,
            atomic::{AtomicU8, AtomicUsize, Ordering},
        };
        let ctx = egui::Context::default();
        let settle = |ctx: &egui::Context| {
            for _ in 0..8 {
                let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
                output.textures_delta.clear();
            }
        };
        settle(&ctx);
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        ctx.set_request_repaint_callback(move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
        });
        let wake = OnceLock::new();
        assert!(wake.set(ctx.clone()).is_ok());
        let value = AtomicU8::new(0);
        super::publish(&value, &wake, 2);
        assert_eq!(value.load(Ordering::Relaxed), 2);
        let after_change = calls.load(Ordering::Relaxed);
        assert!(after_change > 0);
        settle(&ctx);
        let after_settle = calls.load(Ordering::Relaxed);
        super::publish(&value, &wake, 2);
        assert_eq!(calls.load(Ordering::Relaxed), after_settle);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn portal_values_and_gsettings_output_decode() {
        assert_eq!(super::decode(0), None);
        assert_eq!(super::decode(1), Some(egui::Theme::Dark));
        assert_eq!(super::decode(2), Some(egui::Theme::Light));
        assert_eq!(super::decode(9), None);
        let nested = zbus::zvariant::Value::Value(Box::new(zbus::zvariant::Value::Value(Box::new(zbus::zvariant::Value::U32(2)))));
        assert_eq!(super::portal_code(zbus::zvariant::OwnedValue::try_from(nested).unwrap()), Some(2));
        let plain = zbus::zvariant::OwnedValue::from(1u32);
        assert_eq!(super::portal_code(plain), Some(1));
        let wrong = zbus::zvariant::OwnedValue::try_from(zbus::zvariant::Value::from("dark")).unwrap();
        assert_eq!(super::portal_code(wrong), None);
        assert_eq!(super::parse_gtk_scheme("'prefer-light'\n"), Some(egui::Theme::Light));
        assert_eq!(super::parse_gtk_scheme("'prefer-dark'\n"), Some(egui::Theme::Dark));
        assert_eq!(super::parse_gtk_scheme("'default'\n"), None);
        assert_eq!(super::parse_gtk_scheme(""), None);
        for theme in [None, Some(egui::Theme::Dark), Some(egui::Theme::Light)] {
            assert_eq!(super::decode(u32::from(super::code_of(theme))), theme);
        }
    }
}
