//! Settings ▸ Sync: this device's name, the devices it is paired with (finding, pairing with a
//! code both people confirm, removing), syncing the open catalog with them (the whole catalog or
//! collections only, invitations, Sync Now, stopping) and, per collection, what this device allows
//! or takes. Everything goes through the engine's `sync.*` commands ([`lcc_engine::device_sync`]);
//! nothing here touches the network or the disk.
//!
//! [`pump`] runs every frame, Settings open or not: it starts the service once when devices are
//! paired, keeps the status known (slowly while the tab is closed) and runs `sync.tick` once a
//! second while sync runs. [`dialogs`] asks how to share a collection created while the catalog
//! syncs, and [`waiting`] counts what needs an answer (the top bar's cloud shows it).

use std::collections::{BTreeMap, BTreeSet};

use egui::RichText;
use lcc_sync::model::{Grant, Take};
use lcc_sync::pill::Pill;
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

/// How long after the tab was last drawn the app stops looking for devices (seconds).
const STOP_AFTER: f64 = 1.0;
/// How often the tab asks for the status (seconds).
const POLL_EVERY: f64 = 0.4;
/// How often the status is asked for while the tab is closed (seconds).
const POLL_CLOSED: f64 = 2.0;
/// How often `sync.tick` runs while sync is running (seconds).
const TICK_EVERY: f64 = 1.0;
/// After this long looking with nothing found, the tab says what to check (seconds).
const HINT_AFTER: f64 = 10.0;

/// "Sync with <device>…": the choices made so far.
#[derive(Clone, Debug, PartialEq)]
struct Setup {
    device: String,
    name: String,
    catalogue: bool,
    keep_mine: bool,
}

/// What the tab keeps between frames.
#[derive(Default)]
pub struct PeerSyncUi {
    status: Value,
    polled: f64,
    /// When the tab was last drawn.
    seen: f64,
    /// `sync.find {on: true}` was sent and not yet undone; when.
    finding: Option<f64>,
    /// `sync.start` was tried this run.
    started: bool,
    name: String,
    name_loaded: bool,
    /// Errors by the control that caused them (`name`, `pair`, `dev:<id>`, `setup`, …).
    errors: BTreeMap<String, String>,
    /// When `sync.tick` last ran.
    ticked: f64,
    setup: Option<Setup>,
    /// A paired device waiting for "Remove?" to be confirmed.
    confirm_remove: Option<String>,
    confirm_leave: bool,
    /// New collections the person put off deciding about (asked again next run).
    later: BTreeSet<u64>,
}

impl PeerSyncUi {
    /// Show `status` and don't poll for a while (headless tests).
    #[cfg(test)]
    pub(crate) fn set_status_for_test(&mut self, status: Value) {
        self.status = status;
        self.polled = f64::INFINITY;
        self.finding = Some(f64::NEG_INFINITY);
        self.started = true;
    }
}

/// Runs `cmd`; an error is kept under `key` (shown next to that control), success clears it and
/// takes a returned status.
fn run(app: &mut LccApp, key: &str, cmd: &str, params: Value) -> Option<Value> {
    match app.session.execute(cmd, &params) {
        Ok(v) => {
            app.peer_sync.errors.remove(key);
            if v.get("catalogue").is_some() || v.get("paired").is_some() {
                app.peer_sync.status = v.clone();
            }
            Some(v)
        }
        Err(e) => {
            app.peer_sync.errors.insert(key.to_string(), e.to_string());
            None
        }
    }
}

/// The error kept under `key`, in red, where the control is.
fn error_line(app: &LccApp, ui: &mut egui::Ui, t: &Tokens, key: &str) {
    if let Some(e) = app.peer_sync.errors.get(key) {
        let r = crate::dialog_frame::note(ui, RichText::new(e).size(11.0).color(t.reject));
        register(ui.ctx(), format!("error:sync-{key}"), r.rect);
    }
}

/// Every frame: stop looking once the tab is closed; start listening for paired devices once;
/// keep the status known; tick while sync runs.
pub fn pump(app: &mut LccApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let closed = now - app.peer_sync.seen > STOP_AFTER;
    if app.peer_sync.finding.is_some() && closed {
        app.peer_sync.finding = None;
        let _ = app.session.execute("sync.find", &json!({"on": false}));
    }
    if !app.peer_sync.started {
        app.peer_sync.started = true;
        match app.session.execute("sync.start", &json!({})) {
            Ok(v) => {
                if running(&v) {
                    app.peer_sync.status["running"] = json!(true);
                    app.peer_sync.ticked = f64::NEG_INFINITY;
                }
            }
            Err(e) => log::info!("sync: {e}"),
        }
    }
    if closed && now - app.peer_sync.polled >= POLL_CLOSED {
        app.peer_sync.polled = now;
        if let Ok(v) = app.session.execute("sync.status", &json!({})) {
            app.peer_sync.status = v;
        }
    }
    if running(&app.peer_sync.status) && now - app.peer_sync.ticked >= TICK_EVERY {
        app.peer_sync.ticked = now;
        if let Err(e) = app.session.execute("sync.tick", &json!({})) {
            log::info!("sync tick: {e}");
        }
    }
    if running(&app.peer_sync.status) {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(TICK_EVERY));
    }
}

fn running(v: &Value) -> bool {
    v.get("running").and_then(Value::as_bool).unwrap_or(false)
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn arr(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).cloned().unwrap_or_default()
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// New collections waiting to be asked about (not put off this run).
fn undecided(app: &LccApp) -> Vec<(u64, String)> {
    arr(&app.peer_sync.status["catalogue"], "undecided")
        .iter()
        .filter_map(|u| Some((u.get("album")?.as_u64()?, s(u, "name").to_string())))
        .filter(|(a, _)| !app.peer_sync.later.contains(a))
        .collect()
}

/// What waits for an answer: invitations from other devices and new collections to decide about.
pub fn waiting(app: &LccApp) -> usize {
    arr(&app.peer_sync.status, "offers").len() + undecided(app).len()
}

/// "Last synced …" for a unix time, relative to now.
fn last_synced(last: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let ago = now.saturating_sub(last).max(0);
    if ago < 60 {
        tr("Last synced just now").to_string()
    } else if ago < 3600 {
        tr_format!("Last synced {n} min ago", n = ago / 60)
    } else if ago < 86_400 {
        tr_format!("Last synced {n} h ago", n = ago / 3600)
    } else {
        let stamp = lcc_catalog::dates::local(last).unwrap_or_else(|| lcc_catalog::dates::civil(last));
        let date = stamp.get(..10).unwrap_or(&stamp).to_string();
        tr_format!("Last synced on {date}", date = date)
    }
}

pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    let now = ui.input(|i| i.time);
    app.peer_sync.seen = now;
    if app.peer_sync.finding.is_none() {
        app.peer_sync.finding = Some(now);
        if run(app, "find", "sync.find", json!({"on": true})).is_some() {
            app.peer_sync.polled = now;
        }
    }
    if now - app.peer_sync.polled >= POLL_EVERY {
        app.peer_sync.polled = now;
        if let Ok(v) = app.session.execute("sync.status", &json!({})) {
            app.peer_sync.status = v;
        }
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(POLL_EVERY));
    let st = app.peer_sync.status.clone();
    if !app.peer_sync.name_loaded && !s(&st, "name").is_empty() {
        app.peer_sync.name = s(&st, "name").to_string();
        app.peer_sync.name_loaded = true;
    }

    this_device(app, ui, t, &st);
    devices(app, ui, t, &st, now);
    catalogue(app, ui, t, &st);
}

// ── this device ────────────────────────────────────────────────────────────────────────────

fn this_device(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value) {
    super::settings::heading_help(ui, t, "This Device", "sync");
    let mut rename = false;
    super::settings::row(ui, t, "Name", |ui| {
        let r = crate::text_field::TextField::singleline("field:syncDeviceName", &mut app.peer_sync.name).width(240.0).show(ui);
        rename = r.committed();
    });
    super::settings::hint(ui, t, "Other devices see this name. Every device needs its own.");
    if rename && app.peer_sync.name.trim() != s(st, "name") {
        let name = app.peer_sync.name.clone();
        if let Some(v) = run(app, "name", "sync.rename", json!({"name": name})) {
            app.peer_sync.name = s(&v, "name").to_string();
        }
    }
    error_line(app, ui, t, "name");
    if flag(st, "nameCollides") {
        crate::dialog_frame::note(
            ui,
            RichText::new(tr("Another device nearby has this name too: give this one another.")).size(11.0).color(t.caution),
        );
    }
    if flag(st, "pairingsLost") {
        crate::dialog_frame::note(
            ui,
            RichText::new(tr("The saved pairings couldn't be read and were set aside: pair the devices again.")).size(11.0).color(t.caution),
        );
    }
}

// ── devices ────────────────────────────────────────────────────────────────────────────────

fn devices(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value, now: f64) {
    super::settings::heading(ui, t, "Devices");
    // a pairing under way takes the place of the list of devices found
    match st.get("pairing").filter(|p| !p.is_null()).cloned() {
        Some(p) => pairing_box(app, ui, t, &p),
        None => found_list(app, ui, t, st, now),
    }
    error_line(app, ui, t, "pair");

    let paired = arr(st, "paired");
    if paired.is_empty() {
        super::settings::hint(ui, t, "No paired devices yet.");
        return;
    }
    let links = st.get("links").cloned().unwrap_or(Value::Null);
    for d in &paired {
        let id = s(d, "deviceId").to_string();
        let name = s(d, "name").to_string();
        crate::dialog_frame::control_row(ui, |ui| {
            let online = flag(d, "online");
            ui.label(RichText::new(if online { "●" } else { "○" }).color(if online { t.accent } else { t.text_dim }));
            let r = ui.label(RichText::new(&name).color(t.text));
            register(ui.ctx(), format!("text:syncPaired-{id}"), r.rect);
            let state = if flag(d, "notPairedThere") {
                tr("Not paired on the other device: pair again")
            } else if online {
                tr("Online")
            } else {
                tr("Offline")
            };
            ui.label(RichText::new(state).size(11.0).color(if flag(d, "notPairedThere") { t.caution } else { t.text_dim }));
            if app.peer_sync.confirm_remove.as_deref() == Some(id.as_str()) {
                ui.label(RichText::new(tr("Remove this device? It also stops syncing this catalog with it.")).size(11.0).color(t.text));
                if text_button(ui, &format!("syncUnpairYes-{id}"), "Remove", true).clicked() {
                    app.peer_sync.confirm_remove = None;
                    run(app, &format!("dev:{id}"), "sync.unpair", json!({"device": id}));
                }
                if text_button(ui, &format!("syncUnpairNo-{id}"), "Cancel", false).clicked() {
                    app.peer_sync.confirm_remove = None;
                }
            } else if text_button(ui, &format!("syncUnpair-{id}"), "Remove…", false).clicked() {
                app.peer_sync.confirm_remove = Some(id.clone());
            }
        });
        link_line(app, ui, t, &links, &id);
    }
}

/// Under a device: its last exchange and its error.
fn link_line(app: &LccApp, ui: &mut egui::Ui, t: &Tokens, links: &Value, id: &str) {
    if let Some(x) = links.get(id) {
        crate::dialog_frame::in_control_column(ui, |ui| {
            ui.indent(format!("syncLink-{id}"), |ui| {
                if flag(x, "running") {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(10.0));
                        ui.label(RichText::new(tr("Syncing…")).size(11.0).color(t.text_dim));
                    });
                } else if let Some(last) = x.get("last").and_then(Value::as_i64).filter(|l| *l > 0) {
                    let mut line = last_synced(last);
                    let (sent, got, files) = (x["sent"].as_u64().unwrap_or(0), x["received"].as_u64().unwrap_or(0), x["files"].as_u64().unwrap_or(0));
                    if sent + got + files > 0 {
                        line.push_str(" · ");
                        line.push_str(&tr_format!("{sent} sent, {got} received, {files} files", sent = sent, got = got, files = files));
                    }
                    ui.label(RichText::new(line).size(11.0).color(t.text_dim));
                }
                let err = s(x, "error");
                if !err.is_empty() {
                    let r = ui.label(RichText::new(err).size(11.0).color(t.reject));
                    register(ui.ctx(), format!("error:sync-link-{id}"), r.rect);
                }
            });
        });
    }
    error_line(app, ui, t, &format!("dev:{id}"));
}

fn found_list(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value, now: f64) {
    crate::dialog_frame::control_row(ui, |ui| {
        ui.add(egui::Spinner::new().size(12.0));
        ui.label(RichText::new(tr("Looking for devices on this network…")).color(t.text_label));
    });
    super::settings::hint(ui, t, "Open the Sync tab of Settings on the other device too.");
    let found = arr(st, "found");
    let since = app.peer_sync.finding.unwrap_or(now);
    if found.is_empty() && now - since > HINT_AFTER {
        let r = crate::dialog_frame::note(
            ui,
            RichText::new(tr(
                "Nothing found yet. Check that both devices are on the same network (not a guest Wi-Fi), that a firewall lets the app through (UDP port 5353 for finding devices), and that no VPN is on.",
            ))
            .size(11.0)
            .color(t.text_dim),
        );
        register(ui.ctx(), "text:syncNothingFound", r.rect);
    }
    for d in &found {
        let id = s(d, "deviceId").to_string();
        let compatible = d.get("compatible").and_then(Value::as_bool).unwrap_or(true);
        crate::dialog_frame::control_row(ui, |ui| {
            ui.label(RichText::new(s(d, "name")).color(t.text));
            let label = if flag(d, "paired") { "Pair Again" } else { "Pair" };
            let b = ui.add_enabled_ui(compatible, |ui| text_button(ui, &format!("syncPair-{id}"), label, false)).inner;
            if b.clicked() {
                run(app, "pair", "sync.pair", json!({"device": id}));
            }
            if !compatible {
                ui.label(RichText::new(tr("Needs the same version of the app on both devices")).size(11.0).color(t.caution));
            } else if flag(d, "paired") {
                ui.label(RichText::new(tr("It no longer has this device paired")).size(11.0).color(t.text_dim));
            }
        });
    }
}

fn pairing_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, p: &Value) {
    let stage = s(p, "stage");
    crate::dialog_frame::control_row(ui, |ui| {
        let what = if flag(p, "incoming") { tr("Pairing asked by") } else { tr("Pairing with") };
        ui.label(RichText::new(what).color(t.text_label));
        ui.label(RichText::new(s(p, "name")).color(t.text));
    });
    crate::dialog_frame::in_control_column(ui, |ui| match stage {
        "connecting" | "confirm" | "waiting" => {
            if stage == "connecting" {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(tr("Connecting…"));
                });
            } else {
                ui.label(tr("Check that both screens show this code:"));
                let code = s(p, "code");
                let shown = match (code.get(..3), code.get(3..)) {
                    (Some(a), Some(b)) if code.len() == 6 => format!("{a} {b}"),
                    _ => code.to_string(),
                };
                let r = ui.label(RichText::new(shown).font(t.semibold(28.0)).color(t.text));
                register(ui.ctx(), "syncCode", r.rect);
            }
            ui.horizontal(|ui| {
                if stage == "confirm" && text_button(ui, "syncCodesMatch", "Codes Match", true).clicked() {
                    run(app, "pair", "sync.confirm", json!({"accept": true}));
                }
                if stage == "waiting" {
                    ui.add(egui::Spinner::new().size(12.0));
                    ui.label(tr("Waiting for the other device…"));
                }
                if text_button(ui, "syncCancelPairing", "Cancel", false).clicked() {
                    run(app, "pair", "sync.cancelPairing", json!({}));
                }
            });
        }
        "paired" | "failed" => {
            if stage == "paired" {
                ui.label(RichText::new(tr("Paired.")).color(t.accent));
            } else {
                ui.label(RichText::new(tr("Pairing failed.")).color(t.reject));
                let why = s(p, "message");
                if !why.is_empty() {
                    ui.label(RichText::new(why).size(11.0).color(t.text_dim));
                }
            }
            if text_button(ui, "syncPairingDone", "OK", false).clicked() {
                run(app, "pair", "sync.clearPairing", json!({}));
            }
        }
        _ => {}
    });
}

// ── this catalog ───────────────────────────────────────────────────────────────────────────

fn catalogue(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value) {
    super::settings::heading(ui, t, "This Catalog");
    let cat = st.get("catalogue").filter(|c| !c.is_null()).cloned();
    let Some(cat) = cat else {
        super::settings::hint(ui, t, "Open a catalog saved on this computer to sync it.");
        return;
    };
    let unkeyed = cat["unkeyed"].as_u64().unwrap_or(0);
    if unkeyed > 0 {
        let r = crate::dialog_frame::note(
            ui,
            RichText::new(tr_format!("{n} photos can't sync yet (their files haven't been read).", n = unkeyed)).size(11.0).color(t.caution),
        );
        register(ui.ctx(), "text:syncUnkeyed", r.rect);
    }
    let offers = arr(st, "offers");
    let photos = cat["photos"].as_u64().unwrap_or(0);
    for o in &offers {
        offer_box(app, ui, t, o, photos);
    }
    match cat.get("group").filter(|g| !g.is_null()).cloned() {
        Some(g) => {
            group_box(app, ui, t, st, &g);
            collections(app, ui, t, &cat, &g);
        }
        None => start_box(app, ui, t, st, photos),
    }
}

fn mode_words(mode: &str) -> &'static str {
    if mode == "catalogue" { tr("the whole catalog") } else { tr("collections only") }
}

/// "This catalog isn't empty" with New Catalog… (joining a shared catalog starts from an empty one).
fn needs_empty(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, key: &str) {
    crate::dialog_frame::note(
        ui,
        RichText::new(tr("The catalog is shared from the other device, so this device starts from a new, empty catalog. This one has photos."))
            .size(11.0)
            .color(t.caution),
    );
    crate::dialog_frame::control_row(ui, |ui| {
        if text_button(ui, &format!("syncNewCatalog-{key}"), "New Catalog…", false).clicked()
            && let Err(e) = app.run("catalog.new", json!({}))
        {
            app.peer_sync.errors.insert(key.to_string(), e.to_string());
        }
    });
}

fn offer_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, o: &Value, photos: u64) {
    let id = s(o, "deviceId").to_string();
    let key = format!("offer:{id}");
    crate::dialog_frame::note(
        ui,
        RichText::new(tr_format!(
            "{name} asks this catalog to sync with its catalog “{catalog}”: {what}.",
            name = s(o, "name"),
            catalog = s(o, "catalogue"),
            what = mode_words(s(o, "mode"))
        ))
        .color(t.text),
    );
    let blocked = flag(o, "needsEmptyCatalogue") && photos > 0;
    if blocked {
        needs_empty(app, ui, t, &key);
    }
    crate::dialog_frame::control_row(ui, |ui| {
        let join = ui.add_enabled_ui(!blocked, |ui| text_button(ui, &format!("syncOfferJoin-{id}"), "Join", true)).inner;
        if join.clicked() {
            run(app, &key, "sync.answer", json!({"device": id, "accept": true}));
        }
        if text_button(ui, &format!("syncOfferDecline-{id}"), "Decline", false).clicked() {
            run(app, &key, "sync.answer", json!({"device": id, "accept": false}));
        }
    });
    error_line(app, ui, t, &key);
}

/// Not syncing yet: "Sync with <device>…" per paired device, then the choices.
fn start_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value, photos: u64) {
    let paired = arr(st, "paired");
    if paired.is_empty() {
        super::settings::hint(ui, t, "Pair a device above to sync this catalog with it.");
        return;
    }
    super::settings::hint(ui, t, "This catalog isn't syncing.");
    if app.peer_sync.setup.is_none() {
        crate::dialog_frame::control_row(ui, |ui| {
            for d in &paired {
                let id = s(d, "deviceId").to_string();
                let name = s(d, "name").to_string();
                if text_button(ui, &format!("syncWith-{id}"), &tr_format!("Sync with {name}…", name = name), false).clicked() {
                    app.peer_sync.setup = Some(Setup { device: id, name, catalogue: false, keep_mine: true });
                }
            }
        });
        return;
    }
    let Some(mut setup) = app.peer_sync.setup.clone() else { return };
    crate::dialog_frame::in_control_column(ui, |ui| {
        ui.label(RichText::new(tr_format!("Sync this catalog with {name}:", name = setup.name)).color(t.text));
        let r = ui.radio_value(&mut setup.catalogue, true, tr("The whole catalog"));
        register(ui.ctx(), "radio:syncModeCatalogue", r.rect);
        ui.label(
            RichText::new(tr("One catalog on both devices: every collection is listed everywhere, and you choose which ones bring their photos."))
                .size(11.0)
                .color(t.text_dim),
        );
        let r = ui.radio_value(&mut setup.catalogue, false, tr("Collections only"));
        register(ui.ctx(), "radio:syncModeCollections", r.rect);
        ui.label(
            RichText::new(tr("Each device keeps its own catalog and sees only the collections it is allowed and chooses to take."))
                .size(11.0)
                .color(t.text_dim),
        );
        if setup.catalogue {
            ui.add_space(4.0);
            ui.label(RichText::new(tr("Which catalog is kept?")).color(t.text));
            let r = ui.radio_value(&mut setup.keep_mine, true, tr("This one: the other device starts from a new, empty catalog"));
            register(ui.ctx(), "radio:syncKeepThis", r.rect);
            let r =
                ui.radio_value(&mut setup.keep_mine, false, tr_format!("{name}'s: this device starts from a new, empty catalog", name = setup.name));
            register(ui.ctx(), "radio:syncKeepOther", r.rect);
        }
    });
    let blocked = setup.catalogue && !setup.keep_mine && photos > 0;
    if blocked {
        needs_empty(app, ui, t, "setup");
    }
    let mut close = false;
    crate::dialog_frame::control_row(ui, |ui| {
        let go = ui.add_enabled_ui(!blocked, |ui| text_button(ui, "syncStart", "Start Syncing", true)).inner;
        if go.clicked() {
            let mode = if setup.catalogue { "catalogue" } else { "collections" };
            let keep = if setup.keep_mine { "this" } else { "other" };
            close = run(app, "setup", "sync.setup", json!({"device": setup.device, "mode": mode, "keep": keep})).is_some();
        }
        if text_button(ui, "syncStartCancel", "Cancel", false).clicked() {
            close = true;
        }
    });
    error_line(app, ui, t, "setup");
    app.peer_sync.setup = if close { None } else { Some(setup) };
}

/// Syncing: the mode, the devices with their last exchange, Sync Now, inviting, stopping.
fn group_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value, g: &Value) {
    let mode = s(g, "mode").to_string();
    let r = crate::dialog_frame::note(ui, RichText::new(tr_format!("Syncing: {what}.", what = mode_words(&mode))).color(t.text));
    register(ui.ctx(), "text:syncMode", r.rect);
    let links = st.get("links").cloned().unwrap_or(Value::Null);
    let members = arr(g, "devices");
    for d in members.iter().filter(|d| !flag(d, "me") && !flag(d, "left")) {
        let id = s(d, "deviceId").to_string();
        crate::dialog_frame::control_row(ui, |ui| {
            let online = flag(d, "online");
            ui.label(RichText::new(if online { "●" } else { "○" }).color(if online { t.accent } else { t.text_dim }));
            let r = ui.label(RichText::new(s(d, "name")).color(t.text));
            register(ui.ctx(), format!("text:syncMember-{id}"), r.rect);
            if !flag(d, "paired") {
                ui.label(RichText::new(tr("Not paired with this device")).size(11.0).color(t.caution));
            }
        });
        link_line(app, ui, t, &links, &id);
    }
    if g.get("invited").is_some_and(|i| i.as_array().is_some_and(|a| !a.is_empty()) || i.as_object().is_some_and(|o| !o.is_empty())) {
        super::settings::hint(ui, t, "Waiting for the invited devices to answer.");
    }
    // paired devices not in the sync yet can be asked into it
    let inside: BTreeSet<String> = members.iter().filter(|d| !flag(d, "left")).map(|d| s(d, "deviceId").to_string()).collect();
    let others: Vec<Value> = arr(st, "paired").into_iter().filter(|p| !inside.contains(s(p, "deviceId"))).collect();
    crate::dialog_frame::control_row(ui, |ui| {
        if text_button(ui, "syncNow", "Sync Now", true).clicked() {
            run(app, "now", "sync.now", json!({}));
        }
        for p in &others {
            let id = s(p, "deviceId").to_string();
            if text_button(ui, &format!("syncInvite-{id}"), &tr_format!("Invite {name}", name = s(p, "name")), false).clicked() {
                run(app, "now", "sync.setup", json!({"device": id, "mode": mode, "keep": "this"}));
            }
        }
        if app.peer_sync.confirm_leave {
            ui.label(RichText::new(tr("Stop syncing? Nothing is deleted on any device.")).size(11.0).color(t.text));
            if text_button(ui, "syncLeaveYes", "Stop Syncing", true).clicked() {
                app.peer_sync.confirm_leave = false;
                run(app, "leave", "sync.leave", json!({}));
            }
            if text_button(ui, "syncLeaveNo", "Cancel", false).clicked() {
                app.peer_sync.confirm_leave = false;
            }
        } else if text_button(ui, "syncLeave", "Stop Syncing This Catalog…", false).clicked() {
            app.peer_sync.confirm_leave = true;
        }
    });
    error_line(app, ui, t, "now");
    error_line(app, ui, t, "leave");
}

// ── collections ────────────────────────────────────────────────────────────────────────────

/// One row of the collections table.
#[derive(Clone, Debug)]
struct Row {
    /// Widget/error key.
    key: String,
    name: String,
    owner: String,
    mine: bool,
    grant: Grant,
    take: Take,
    /// The most this device may take.
    most: Take,
    letters: String,
    pill: Option<Pill>,
    photos: u64,
    /// An Immich album (listed under "Immich").
    immich: bool,
    /// How to address it in `sync.allow`/`sync.take`.
    target: Value,
}

const GRANTS: [(Grant, &str, &str); 4] = [
    (Grant::None, "None", "Not shared with the other devices."),
    (Grant::Preview, "Allow Preview", "Others get previews to develop; only this device adds photos."),
    (Grant::Serve, "Allow Serve", "Others may also fetch the originals; only this device adds photos."),
    (Grant::Sync, "Allow Sync", "Every device keeps the originals and can add photos."),
];

const TAKES: [(Take, &str, &str); 4] = [
    (Take::None, "None", "Nothing of it is kept on this device."),
    (Take::Previews, "Previews", "Previews to develop here; edits go back."),
    (Take::FullQuality, "Full Quality", "The originals come here; edits go back."),
    (Take::FullSync, "Full Sync", "The originals, and photos added here go to the other devices."),
];

fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> Option<T> {
    serde_json::from_value(v.clone()).ok()
}

/// The table's rows: the shared collections, the photos in no collection, and this catalog's own
/// collections not shared yet (so they can be allowed).
fn rows(app: &LccApp, cat: &Value, g: &Value) -> Vec<Row> {
    let me_name = arr(g, "devices").iter().find(|d| flag(d, "me")).map(|d| s(d, "name").to_string()).unwrap_or_default();
    let mut out = Vec::new();
    let mut listed: BTreeSet<u64> = BTreeSet::new();
    let mut unfiled = false;
    for c in arr(cat, "collections") {
        if s(&c, "kind") == "set" {
            continue;
        }
        if let Some(a) = c.get("album").and_then(Value::as_u64) {
            listed.insert(a);
        }
        let mine = flag(&c, "mine");
        let id = s(&c, "id").to_string();
        if s(&c, "kind") == "unfiled" && mine {
            unfiled = true;
        }
        let name = if s(&c, "kind") == "unfiled" { tr("Not in a collection").to_string() } else { s(&c, "name").to_string() };
        let owner = if flag(&c, "immich") { "Immich".to_string() } else { s(&c, "ownerName").to_string() };
        out.push(Row {
            key: id.clone(),
            name,
            owner,
            mine,
            grant: parse(&c["grant"]).unwrap_or_default(),
            take: parse(&c["take"]).unwrap_or_default(),
            most: parse(&c["most"]).unwrap_or_default(),
            letters: s(&c, "letters").to_string(),
            pill: parse(&c["pill"]),
            photos: c["photos"].as_u64().unwrap_or(0),
            immich: flag(&c, "immich"),
            target: json!({"collection": id}),
        });
    }
    if !unfiled {
        out.push(Row {
            key: "unfiled".into(),
            name: tr("Not in a collection").to_string(),
            owner: me_name.clone(),
            mine: true,
            grant: Grant::None,
            take: Take::None,
            most: Take::None,
            letters: "LN".into(),
            pill: None,
            photos: 0,
            immich: false,
            target: json!({"unfiled": true}),
        });
    }
    let mut own: Vec<&lcc_catalog::Album> =
        app.session.catalog.albums().filter(|a| a.smart.is_none() && !a.quick && !a.folder && !listed.contains(&a.id.0)).collect();
    own.sort_by_key(|a| a.name.to_lowercase());
    for a in own {
        out.push(Row {
            key: format!("album{}", a.id.0),
            name: a.name.clone(),
            owner: me_name.clone(),
            mine: true,
            grant: Grant::None,
            take: Take::None,
            most: Take::None,
            letters: "LN".into(),
            pill: None,
            photos: app.session.catalog.album_count(a.id) as u64,
            immich: false,
            target: json!({"album": a.id.0}),
        });
    }
    // the Immich albums last, under their own heading
    out.sort_by_key(|r| r.immich);
    out
}

fn collections(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, cat: &Value, g: &Value) {
    super::settings::heading(ui, t, "Collections");
    super::settings::hint(
        ui,
        t,
        "On the device that owns a collection, choose what the others may have; on the others, choose what this device takes.",
    );
    let rows = rows(app, cat, g);
    let mut picked: Option<(Row, Value, &'static str)> = None;
    egui::Grid::new("syncCollections").num_columns(5).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
        for h in ["Collection", "Owner", "Sync", "Pill", "Photos"] {
            ui.label(RichText::new(tr(h)).size(11.0).color(t.text_label));
        }
        ui.end_row();
        let mut immich_heading = false;
        for r in &rows {
            if r.immich && !immich_heading {
                immich_heading = true;
                let h = ui.label(RichText::new(tr("Immich")).strong().color(t.text));
                register(ui.ctx(), "text:syncImmichGroup", h.rect);
                ui.label(RichText::new(tr("Albums on your Immich server: choose what this device keeps of each.")).size(11.0).color(t.text_dim));
                ui.end_row();
            }
            let n = ui.label(RichText::new(&r.name).color(t.text));
            register(ui.ctx(), format!("text:syncCollection-{}", r.key), n.rect);
            ui.label(RichText::new(&r.owner).size(11.0).color(t.text_dim));
            if let Some((v, cmd)) = picker(ui, r) {
                picked = Some((r.clone(), v, cmd));
            }
            let text = r.pill.map(|p| p.text()).unwrap_or_else(|| r.letters.clone());
            let l = ui.label(RichText::new(text).monospace().size(11.0).color(t.text_dim));
            if let Some(p) = &r.pill {
                l.on_hover_text(super::sync_pill::tooltip(p));
            }
            ui.label(RichText::new(r.photos.to_string()).size(11.0).color(t.text_dim));
            ui.end_row();
        }
    });
    if let Some((r, v, cmd)) = picked {
        let mut params = r.target.clone();
        if let Some(o) = params.as_object_mut() {
            o.insert(if cmd == "sync.allow" { "allow".into() } else { "take".into() }, v);
        }
        run(app, &format!("col:{}", r.key), cmd, params);
    }
    for r in &rows {
        error_line(app, ui, t, &format!("col:{}", r.key));
    }
}

/// The choice for one row: what it allows (owner) or takes (others), each with its line of
/// explanation; the new value and the command to send when it changes.
fn picker(ui: &mut egui::Ui, r: &Row) -> Option<(Value, &'static str)> {
    let mut out = None;
    let current = if r.mine {
        GRANTS.iter().find(|(g, _, _)| *g == r.grant).map_or("None", |x| x.1)
    } else {
        TAKES.iter().find(|(k, _, _)| *k == r.take).map_or("None", |x| x.1)
    };
    let resp = egui::ComboBox::from_id_salt(format!("syncPick-{}", r.key)).selected_text(tr(current)).width(130.0).show_ui(ui, |ui| {
        let small = |ui: &mut egui::Ui, text: &str| {
            let t = Tokens::get(ui.ctx());
            ui.label(RichText::new(tr(text)).size(10.5).color(t.text_dim));
        };
        if r.mine {
            for (g, label, why) in GRANTS {
                if ui.selectable_label(g == r.grant, tr(label)).clicked() && g != r.grant {
                    out = Some((json!(g.id()), "sync.allow"));
                }
                small(ui, why);
            }
        } else {
            for (k, label, why) in TAKES.iter().filter(|(k, _, _)| *k <= r.most) {
                let why = match (r.immich, k) {
                    (true, Take::Previews) => "Previews from Immich to develop here; edits go to your other devices.",
                    (true, Take::FullQuality) => "The originals come from Immich; edits go to your other devices.",
                    _ => why,
                };
                if ui.selectable_label(*k == r.take, tr(label)).clicked() && *k != r.take {
                    out = Some((json!(k.id()), "sync.take"));
                }
                small(ui, why);
            }
        }
    });
    register(ui.ctx(), format!("combo:syncPick-{}", r.key), resp.response.rect);
    out
}

// ── the new-collection question ────────────────────────────────────────────────────────────

/// A collection created while the catalog syncs: how should it be shared? Asked at once, only
/// when there is one to ask about and no other dialog is open.
pub fn dialogs(app: &mut LccApp, ctx: &egui::Context) {
    if app.ui.dialog.is_some() {
        return;
    }
    let Some((album, name)) = undecided(app).into_iter().next() else { return };
    let t = Tokens::get(ctx);
    egui::Window::new(tr("Share the New Collection?"))
        .id(egui::Id::new("syncUndecided"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            register(ui.ctx(), "dialog:syncUndecided", ui.max_rect());
            ui.label(
                RichText::new(tr_format!("“{name}” is new. What may the other devices syncing this catalog have of it?", name = name)).color(t.text),
            );
            ui.add_space(6.0);
            for (g, label, why) in GRANTS {
                ui.horizontal(|ui| {
                    let label = if g == Grant::None { "Keep on This Device" } else { label };
                    if text_button(ui, &format!("syncUndecided-{}", g.id()), label, g == Grant::Preview).clicked() {
                        let res = if g == Grant::None {
                            app.session.execute("sync.skip", &json!({"album": album}))
                        } else {
                            app.session.execute("sync.allow", &json!({"album": album, "allow": g.id()}))
                        };
                        match res {
                            Ok(v) => app.peer_sync.status = v,
                            Err(e) => {
                                app.toast(ui.ctx(), e.to_string());
                                app.peer_sync.later.insert(album);
                            }
                        }
                    }
                    ui.label(RichText::new(tr(why)).size(11.0).color(t.text_dim));
                });
            }
            ui.add_space(4.0);
            if text_button(ui, "syncUndecidedLater", "Decide Later", false).clicked() {
                app.peer_sync.later.insert(album);
            }
        });
}
