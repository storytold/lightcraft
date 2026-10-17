//! Help ▸ Check for Updates…: the auto updater's face ([`lcc_engine::update`]).
//!
//! The dialog is the whole flow in one window: what this build is, whether a newer release exists,
//! what it says, and what the user can do about it — download it, install it and restart, or skip
//! this version. The updater's state lives in the session (a check runs on a worker thread), so the
//! window only reads `update.status` and dispatches `update.*`: nothing here has to survive a frame.
//!
//! When the user turns it on (the checkbox in this dialog, in Settings ▸ General or in the welcome
//! dialog, saved in the app's own settings folder through `update.prefs`), one check runs shortly
//! after the app starts. Only a host that sets [`LccApp::startup_update_check`] (the desktop app
//! with a settings folder of its own) runs it: headless sessions, `--memory` and
//! `<PREFIX>_NO_PREFS` runs never check at start. It never opens the dialog by itself: a found
//! release is announced with a toast, so a start does not interrupt whatever the user is doing.

use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

/// How long after start-up the automatic check runs (the app is busy opening the library).
const STARTUP_DELAY_S: f64 = 4.0;

/// How often a running check or download asks to be redrawn.
const TICK_MS: u64 = 200;

/// Pump the updater once a frame: the start-up check, the worker's result, and a repaint while
/// something is in flight.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    if app.session.updater.poll() {
        // what the dialog shows just changed (a check's answer, a download's outcome)
        ctx.request_repaint();
    }
    let now = ctx.input(|i| i.time);
    if app.session.updater.checking() || app.session.updater.downloading() {
        ctx.request_repaint_after(std::time::Duration::from_millis(TICK_MS));
    }
    if !app.ui.update_checked_at_startup {
        app.ui.update_checked_at_startup = true;
        if app.startup_update_check && app.session.updater.prefs().check_at_startup {
            app.ui.update_checked_at = now + STARTUP_DELAY_S;
        }
    }
    if app.ui.update_checked_at > 0.0 && now >= app.ui.update_checked_at {
        app.ui.update_checked_at = 0.0;
        let _ = app.session.execute("update.check", &json!({}));
    }
    // a start-up check that found something says so once, without opening a window
    if let Some(version) = app.ui.update_pending.clone()
        && let Some(available) = app.session.updater.available()
        && available.version == version
    {
        app.ui.update_pending = None;
        app.toast(ctx, tr_format!("{app} {version} is available (Help ▸ Check for Updates…)", version = version));
    }
    // a staged file whose release is no longer on offer (a newer check replaced it) leaves nothing behind
    if app.session.updater.available().is_none()
        && let Some(staged) = app.session.updater.staged().cloned()
        && !matches!(app.ui.dialog, Some(Dialog::Update))
    {
        lcc_engine::update::discard(&staged);
        app.session.updater.set_progress(lcc_engine::update::Progress::Idle);
    }
}

/// The dialog's body (the window frame is [`crate::panels::dialogs`]'s).
pub fn body(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let status = app.session.updater.status();
    let version = status["version"].as_str().unwrap_or_default().to_string();
    let platform = status["platform"].as_str().unwrap_or_default().to_string();
    let supported = status["supported"].as_bool().unwrap_or(false);
    ui.set_min_width(460.0);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(tr_format!("{app} {version}", version = version)).font(t.semibold(13.5)).color(t.text));
        ui.label(egui::RichText::new(platform).font(t.font(11.5)).color(t.text_dim));
    });

    if !supported {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(tr("This build has no updates of its own: the releases page has the files for the platforms we ship."))
                .color(t.text_dim),
        );
        if link(ui, "link:updateReleases", tr("Open the releases page")) {
            let _ = crate::links::open(app, &format!("{}/releases", lcc_brand::REPOSITORY));
        }
        settings(app, ui, &t);
        return;
    }

    let check = status["check"].as_str().unwrap_or("never");
    let download = status["download"].as_str().unwrap_or("idle");
    ui.add_space(8.0);
    match check {
        "running" => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(egui::RichText::new(tr("Checking for updates…")).color(t.text_label));
            });
        }
        "found" => {
            let a = &status["available"];
            let found = a["version"].as_str().unwrap_or_default().to_string();
            ui.label(egui::RichText::new(tr_format!("Version {version} is available.", version = found)).font(t.semibold(13.0)).color(t.text));
            let mut line = a["file"].as_str().unwrap_or_default().to_string();
            if let Some(published) = a["published"].as_str() {
                line = tr_format!("{file} · {published}", file = line, published = published);
            }
            if let Some(size) = a["size"].as_u64().filter(|s| *s > 0) {
                line = tr_format!("{info} · {size}", info = line, size = megabytes(size));
            }
            ui.label(egui::RichText::new(line).font(t.font(11.5)).color(t.text_dim));
            if let Some(action) = a["action"].as_str() {
                ui.label(egui::RichText::new(tr_format!("Installing it will {action}.", action = action)).color(t.text_label));
            }
            ui.label(
                egui::RichText::new(if a["verified"].as_bool().unwrap_or(false) {
                    tr("The download is checked against the release's SHA-256 before anything is installed.")
                } else {
                    tr("The download is checked against the size the release states.")
                })
                .font(t.font(11.0))
                .color(t.text_dim),
            );
            let notes = a["notes"].as_str().unwrap_or("").trim().to_string();
            if !notes.is_empty() {
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("update-notes")
                    .max_height(crate::panels::dialogs::list_height(ui, 220.0))
                    .auto_shrink([false, true])
                    .show(ui, |ui| notes_body(ui, &t, &notes));
            }
        }
        _ => {
            let last = status["lastChecked"].as_str().unwrap_or_default().to_string();
            ui.label(egui::RichText::new(tr("This is the newest version.")).color(t.text_label));
            if !last.is_empty() {
                ui.label(egui::RichText::new(tr_format!("Last checked {when}", when = last)).font(t.font(11.5)).color(t.text_dim));
            }
        }
    }

    // the download and the install
    ui.add_space(8.0);
    match download {
        "running" => {
            let bytes = status["progress"]["bytes"].as_u64().unwrap_or(0);
            let total = status["progress"]["total"].as_u64().unwrap_or(0);
            let frac = if total > 0 { (bytes as f64 / total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
            let text = if total > 0 {
                tr_format!(
                    "{done} of {total} ({percent} %)",
                    done = megabytes(bytes),
                    total = megabytes(total),
                    percent = (frac * 100.0).round() as u64
                )
            } else {
                tr_format!("{done} downloaded", done = megabytes(bytes))
            };
            let r = ui.add(egui::ProgressBar::new(frac).text(text));
            register(ui.ctx(), "progress:updateDownload", r.rect);
            if button(ui, "button:updateCancel", tr("Cancel Download")) {
                let _ = app.session.execute("update.downloadCancel", &json!({}));
            }
        }
        "ready" => {
            let staged = &status["staged"];
            ui.label(
                egui::RichText::new(tr_format!(
                    "{file} is downloaded and checked; install it to update (the app restarts).",
                    file = staged["file"].as_str().unwrap_or_default()
                ))
                .color(t.text_label),
            );
        }
        "failed" => {
            ui.label(egui::RichText::new(tr("The download could not be finished.")).color(t.text));
            if let Some(why) = status["error"].as_str() {
                ui.label(egui::RichText::new(why).font(t.font(11.5)).color(t.text_dim));
            }
        }
        "cancelled" => {
            ui.label(egui::RichText::new(tr("The download was cancelled.")).color(t.text_dim));
        }
        _ => {}
    }

    // a failed check explains itself
    if check == "never"
        && let Some(why) = status["error"].as_str()
    {
        ui.label(egui::RichText::new(why).font(t.font(11.5)).color(t.text_dim));
    }

    if let Some(installed) = status.get("installed") {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(installed["message"].as_str().unwrap_or_default()).color(t.text_label));
        if installed["exit"].as_bool().unwrap_or(false) {
            ui.label(egui::RichText::new(tr("Quit the app to let the installer finish.")).font(t.font(11.5)).color(t.text_dim));
        }
    }

    // the buttons
    ui.add_space(10.0);
    let checking = check == "running";
    let downloading = download == "running";
    let ready = download == "ready";
    ui.horizontal(|ui| {
        if button(ui, "button:updateCheck", tr("Check Again")) && !checking {
            let _ = app.session.execute("update.check", &json!({}));
        }
        if check == "found" && !ready && !downloading && button(ui, "button:updateDownload", tr("Download")) {
            let _ = app.session.execute("update.download", &json!({}));
        }
        if ready && button(ui, "button:updateInstall", tr("Install and Relaunch")) {
            match app.session.execute("update.install", &json!({"confirm": true})) {
                Ok(v) => {
                    if let Some(message) = v["message"].as_str() {
                        app.toast(ui.ctx(), message);
                    }
                    if v["exit"].as_bool().unwrap_or(false) {
                        // the installer or the replacement script needs this process gone
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
                Err(e) => app.toast(ui.ctx(), e.to_string()),
            }
        }
        if check == "found"
            && let Some(version) = status["available"]["version"].as_str()
            && button(ui, "button:updateSkip", tr("Skip This Version"))
        {
            // the dialog stays open (its own Close or Esc ends it); this only stops the asking
            let _ = app.session.execute("update.dismiss", &json!({"version": version, "skipped": true}));
        }
    });

    settings(app, ui, &t);
}

/// The two settings, and where the download lands.
fn settings(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    ui.add_space(8.0);
    ui.separator();
    let mut check_at_startup = app.session.updater.prefs().check_at_startup;
    let mut prerelease = app.session.updater.prefs().prerelease;
    let r = ui.checkbox(&mut check_at_startup, tr("Check for updates when the app starts"));
    register(ui.ctx(), "check:updateAtStartup", r.rect);
    let r = ui.checkbox(&mut prerelease, tr("Include prereleases (release candidates)"));
    register(ui.ctx(), "check:updatePrerelease", r.rect);
    // not saved: the boxes show the stored values again next frame
    if (check_at_startup != app.session.updater.prefs().check_at_startup || prerelease != app.session.updater.prefs().prerelease)
        && let Err(e) = app.session.execute("update.prefs", &json!({"checkAtStartup": check_at_startup, "prerelease": prerelease}))
    {
        app.toast_error(ui.ctx(), format!("{}: {e}", tr("Update settings not saved")));
    }
    if !app.session.updater.persistent() {
        ui.label(egui::RichText::new(tr("These settings are saved in the app's settings folder.")).font(t.font(11.0)).color(t.text_dim));
    }
}

/// The release notes: the app's Markdown subset (`### Heading`, `- item`, prose).
fn notes_body(ui: &mut egui::Ui, t: &Tokens, notes: &str) {
    ui.scope(|ui| {
        for line in notes.lines().take(400) {
            let l = line.trim_end();
            if let Some(h) = l.strip_prefix("### ") {
                ui.add_space(4.0);
                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(12.5)).color(t.text));
            } else if let Some(h) = l.strip_prefix("## ") {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(crate::i18n::tr(h)).font(t.semibold(13.5)).color(t.text));
            } else if l.starts_with("# ") || l.is_empty() {
            } else if let Some(b) = l.strip_prefix("- ") {
                ui.label(egui::RichText::new(format!("•  {}", crate::i18n::tr(b).replace('`', ""))).color(t.text_label));
            } else {
                ui.label(egui::RichText::new(crate::i18n::tr(l.trim()).replace('`', "")).color(t.text_label));
            }
        }
    });
}

/// `12.3 MB`.
fn megabytes(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb < 10.0 { format!("{mb:.1} MB") } else { format!("{:.0} MB", mb.floor()) }
}

fn button(ui: &mut egui::Ui, id: &str, label: &str) -> bool {
    let r = ui.button(label);
    register(ui.ctx(), id, r.rect);
    r.clicked()
}

fn link(ui: &mut egui::Ui, id: &str, label: &str) -> bool {
    let r = ui.link(label);
    register(ui.ctx(), id, r.rect);
    r.clicked()
}

/// What `update.status` says, for the control channel and tests.
pub fn status(app: &LccApp) -> Value {
    app.session.updater.status()
}
