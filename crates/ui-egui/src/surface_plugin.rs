//! Installing a surface's plugin into its maker's app from Settings ▸ Controls (the LightCraft
//! plugin for Logi Options+), and taking it out again.
//!
//! The host app supplies a [`PluginInstaller`] (`Services::surface_plugin`; desktop only): it
//! knows where the maker's app keeps plugins, where LightCraft's copy of the plugin package is,
//! and how to hand it to the maker's installer. Installing runs off the UI thread with a row in
//! the activity stack. Commands: `surface.plugin` (status), `surface.installPlugin`,
//! `surface.uninstallPlugin`.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use lightcraft_engine::activity::{Cancel, TaskGuard};
use serde::Serialize;
use serde_json::{Value, json};

use crate::LightcraftApp;
use lightcraft_surface as ls;

/// Where a surface plugin stands.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginStatus {
    /// The maker's app, by name ("Logi Options+"), when it is installed on this computer.
    pub host: Option<String>,
    /// The version of the plugin installed there.
    pub installed: Option<String>,
    /// The version of the plugin package that comes with LightCraft (none: this build has none).
    pub package: Option<String>,
    /// Where that package is.
    pub package_path: Option<String>,
}

impl PluginStatus {
    /// The package is newer than what is installed (or nothing is).
    pub fn can_install(&self) -> bool {
        self.host.is_some() && self.package.is_some()
    }

    pub fn update_available(&self) -> bool {
        match (&self.installed, &self.package) {
            (Some(i), Some(p)) => version_key(p) > version_key(i),
            _ => false,
        }
    }
}

fn version_key(v: &str) -> Vec<u64> {
    v.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

/// The host app's side: finds and installs a surface plugin. Calls may block (they run on a
/// worker thread, except `status`, which only looks at files).
pub trait PluginInstaller: Send + Sync {
    /// The plugin's name for messages ("LightCraft plugin for Logi Options+").
    fn name(&self) -> String;
    fn status(&self) -> PluginStatus;
    /// Install (or update) the plugin; it connects to LightCraft on `port`. The text says what
    /// happens next ("Click Continue in Logi Options+…").
    fn install(&self, port: u16) -> Result<String, String>;
    fn uninstall(&self) -> Result<String, String>;
    /// The trust token the installed plugin reads (its data folder), if there is one.
    fn plugin_token(&self) -> Option<String> {
        None
    }
    /// Hand the installed plugin a trust token (`Some`) and the port LightCraft listens on, in
    /// its data folder, which only this user can read. Only what differs is written.
    fn provision(&self, _token: Option<&str>, _port: u16) -> Result<(), String> {
        Ok(())
    }
}

/// The client id of the LightCraft plugin for Logi Options+.
pub const LOGI_CLIENT: &str = "lightcraft-logi";

/// Zero setup for the plugin LightCraft knows how to install: when it is installed for this
/// user, control surfaces turn on by themselves (unless the user turned them off) and the plugin
/// is trusted at once, through a token in its data folder (no pairing code to compare). Runs
/// when the settings load, before the surface listener starts, and after an install.
pub fn auto_setup(app: &mut LightcraftApp) {
    let Some(i) = installer(app) else { return };
    let status = i.status();
    app.surface.plugin.status = Some(status.clone());
    if status.installed.is_none() {
        return;
    }
    let s = app.surface.settings();
    if s.off_by_user {
        return;
    }
    if !s.enabled {
        app.surface.runtime.settings_mut().enabled = true;
        app.surface.changed();
        log::info!("control surfaces turned on: the Logi Options+ plugin is installed");
    }
    let port = app.surface.settings().port;
    let trusted = i.plugin_token().is_some_and(|t| ls::protocol::verify(app.surface.settings(), t.trim()).is_some_and(|c| c.client == LOGI_CLIENT));
    let token = if trusted {
        None
    } else {
        let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mut s = app.surface.settings().clone();
        let t = app.surface.pairing.trust(LOGI_CLIENT, "LightCraft plugin for Logi Options+", &mut s, now);
        app.surface.runtime.settings_mut().paired = s.paired;
        app.surface.changed();
        Some(t)
    };
    if let Err(e) = i.provision(token.as_deref(), port) {
        log::warn!("Logi plugin: {e}");
        app.surface.plugin.message = Some(Err(e));
    }
}

/// Tell the installed plugin the port LightCraft now listens on (after the user changed it), so it
/// connects there without a restart. The trust token and the on/off setting are left alone.
pub fn provision_port(app: &mut LightcraftApp) {
    let Some(i) = installer(app) else { return };
    if i.status().installed.is_none() {
        return;
    }
    if let Err(e) = i.provision(None, app.surface.settings().port) {
        log::warn!("Logi plugin: {e}");
        app.surface.plugin.message = Some(Err(e));
    }
}

/// An install or uninstall on its way.
pub struct PluginJob {
    rx: Receiver<Result<String, String>>,
    _guard: TaskGuard,
    pub installing: bool,
}

/// What Settings ▸ Controls shows about the plugin.
#[derive(Default)]
pub struct PluginState {
    pub status: Option<PluginStatus>,
    /// egui time of the last status check (s).
    checked_at: f64,
    pub job: Option<PluginJob>,
    /// The last install / uninstall result.
    pub message: Option<Result<String, String>>,
}

fn installer(app: &LightcraftApp) -> Option<Arc<dyn PluginInstaller>> {
    app.services.surface_plugin.clone()
}

/// Look at the plugin again (at most every 3 s unless `force`).
pub fn refresh(app: &mut LightcraftApp, force: bool) {
    let now = app.surface.now();
    if !force && app.surface.plugin.status.is_some() && now - app.surface.plugin.checked_at < 3.0 {
        return;
    }
    let Some(i) = installer(app) else { return };
    app.surface.plugin.status = Some(i.status());
    app.surface.plugin.checked_at = now;
}

/// Start installing (`install`) or removing the plugin on a worker thread.
pub fn start(app: &mut LightcraftApp, install: bool) -> Result<Value, String> {
    let Some(i) = installer(app) else { return Err("this LightCraft can't install surface plugins (desktop only)".into()) };
    if app.surface.plugin.job.is_some() {
        return Err("the plugin is already being installed or removed".into());
    }
    let status = i.status();
    if install && !status.can_install() {
        return Err(match (&status.host, &status.package) {
            (None, _) => "Logi Options+ isn't installed on this computer".into(),
            _ => "this LightCraft has no plugin package with it (see docs/control-surfaces.md, Installing the plugin)".into(),
        });
    }
    let port = app.surface.listening.unwrap_or(app.surface.settings().port);
    let label = if install { "Installing the surface plugin" } else { "Removing the surface plugin" };
    let guard = app.session.activity.start("surfacePlugin", label, Cancel::No);
    guard.detail(&i.name());
    let (tx, rx) = channel();
    let repaint = app.tasks.repaint.clone();
    std::thread::Builder::new()
        .name("surface-plugin".into())
        .spawn(move || {
            let r = if install { i.install(port) } else { i.uninstall() };
            let _ = tx.send(r);
            if let Some(c) = repaint {
                c.request_repaint();
            }
        })
        .map_err(|e| format!("can't start the installer: {e}"))?;
    app.surface.plugin.message = None;
    app.surface.plugin.job = Some(PluginJob { rx, _guard: guard, installing: install });
    Ok(json!({"started": true}))
}

/// Once per frame: pick up a finished job.
pub fn poll(app: &mut LightcraftApp) {
    let Some(job) = &app.surface.plugin.job else { return };
    let r = match job.rx.try_recv() {
        Ok(r) => r,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => Err("the installer stopped without an answer".into()),
    };
    let installed = job.installing && r.is_ok();
    app.surface.plugin.job = None;
    app.surface.plugin.message = Some(r);
    refresh(app, true);
    if installed {
        auto_setup(app);
    }
}

/// `surface.plugin*` commands; `None` for other ids.
pub fn command(app: &mut LightcraftApp, id: &str) -> Option<Result<Value, String>> {
    Some(match id {
        "surface.plugin" => {
            refresh(app, true);
            let busy = app.surface.plugin.job.as_ref().map(|j| if j.installing { "installing" } else { "removing" });
            let message = match &app.surface.plugin.message {
                Some(Ok(m)) => json!({"ok": true, "text": m}),
                Some(Err(e)) => json!({"ok": false, "text": e}),
                None => Value::Null,
            };
            Ok(json!({"status": app.surface.plugin.status, "busy": busy, "message": message}))
        }
        "surface.installPlugin" => start(app, true),
        "surface.uninstallPlugin" => start(app, false),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_number() {
        let s = |i: Option<&str>, p: Option<&str>| PluginStatus {
            host: Some("Logi Options+".into()),
            installed: i.map(str::to_string),
            package: p.map(str::to_string),
            package_path: None,
        };
        assert!(s(Some("0.1.0"), Some("0.2.0")).update_available());
        assert!(s(Some("0.9.0"), Some("0.10.0")).update_available());
        assert!(!s(Some("0.2.0"), Some("0.2.0")).update_available());
        assert!(!s(None, Some("0.2.0")).update_available());
        assert!(s(None, Some("0.2.0")).can_install());
        assert!(!s(None, None).can_install());
        assert!(!PluginStatus { host: None, ..s(None, Some("1")) }.can_install());
    }
}
