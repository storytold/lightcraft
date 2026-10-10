//! Immich in the app (IMM-CONNECT, IMM-LINK, IMM-IMPORT, IMM-EXTLIB): Settings → Connections,
//! the "in Immich" grid badge, Info's "Open in Immich" link, the Import from Immich window, and the
//! per-frame `remote.pump` that takes in background work (SHA-1 back-fill, link passes, imports,
//! downloading a link-only photo's original when it is opened in Develop).
//!
//! Everything goes through the engine's `immich.*` commands except the slow listing and thumbnail
//! calls of the import window, which run on worker threads with a client from
//! [`dac_engine::Session::immich_client`]. The API key typed into the form lives in memory until
//! Connect hands it to the secret store; it is never shown again, logged or saved in settings.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Rect, RichText, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::DacApp;
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

/// `tr(template)` with each `{}` filled in turn (`tr_format!` only knows catalogued formats).
macro_rules! trf {
    ($t:literal $(, $a:expr)* $(,)?) => {
        fill(crate::i18n::tr($t), &[$(&$a as &dyn std::fmt::Display),*])
    };
}

fn fill(template: &str, args: &[&dyn std::fmt::Display]) -> String {
    let mut out = String::new();
    let mut it = args.iter();
    let mut rest = template;
    while let Some(i) = rest.find("{}") {
        out.push_str(rest.get(..i).unwrap_or_default());
        if let Some(a) = it.next() {
            out.push_str(&a.to_string());
        }
        rest = rest.get(i + 2..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

type Slot<T> = Arc<Mutex<Option<T>>>;

fn take<T>(slot: &Slot<T>) -> Option<T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// A decoded thumbnail, or why not.
enum Thumb {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

/// UI state of the Immich surfaces.
#[derive(Default)]
pub struct ImmichUi {
    url: String,
    key: String,
    pinned: Option<String>,
    pending: Option<(bool, Slot<Value>)>,
    result: Option<Value>,
    libraries: HashMap<String, Value>,
    maps: HashMap<String, Vec<(String, String)>>,
    pub import_open: bool,
    account: Option<String>,
    source: String,
    target: Option<(String, String)>,
    listing: Option<Slot<Value>>,
    assets: Vec<Value>,
    groups: Vec<Value>,
    next_page: Option<u64>,
    page: u64,
    list_error: Option<String>,
    selected: BTreeSet<String>,
    link_only: bool,
    destination: String,
    thumbs: HashMap<String, Thumb>,
    thumb_rx: Option<Arc<Mutex<Vec<(String, Option<egui::ColorImage>)>>>>,
    fetch_tried: HashSet<u64>,
    last_pump: f64,
    last: Value,
}

// ------------------------------------------------------------------------------------- pump

/// Every frame (cheaply): take in finished background work, keep the window repainting while
/// something runs, and start downloading the original of a link-only photo opened in Develop.
pub fn pump(app: &mut DacApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    if now - app.immich.last_pump < 0.2 {
        return;
    }
    app.immich.last_pump = now;
    let Ok(v) = app.session.execute("remote.pump", &json!({})) else { return };
    let busy = v["sha1"]["active"] == true
        || v["import"]["active"] == true
        || v["links"].as_object().is_some_and(|m| m.values().any(|l| l["active"] == true))
        || v["fetching"].as_array().is_some_and(|a| !a.is_empty());
    // an import that just finished: say so
    if app.immich.last["import"]["active"] == true && v["import"]["active"] == false {
        let n = v["import"]["imported"].as_u64().unwrap_or(0);
        let skipped = v["import"]["skipped"].as_u64().unwrap_or(0);
        let failed = v["import"]["failed"].as_array().map(Vec::len).unwrap_or(0);
        app.toast(ctx, trf!("Imported {} from Immich ({} already in the library, {} failed)", n, skipped, failed));
    }
    app.immich.last = v;
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    // Develop on a link-only photo: fetch its original (once per photo per session)
    let developing = app.ui.view == crate::state::ViewMode::Detail && app.ui.right.is_edit_tool();
    if developing
        && let Some(id) = app.session.active()
        && app.session.catalog.photo(id).is_some_and(|p| p.preview_only.as_deref() == Some(dac_engine::remote::LINK_ONLY))
        && app.immich.fetch_tried.insert(id.0)
    {
        match app.session.execute("immich.fetchOriginal", &json!({"id": id.0})) {
            Ok(r) if r["started"] == true => app.toast(ctx, "Downloading the original from Immich…"),
            Ok(r) if r["ok"] == false => app.toast(ctx, r["error"]["message"].as_str().unwrap_or("Immich")),
            Ok(_) => {}
            Err(e) => app.toast(ctx, e.to_string()),
        }
    }
}

// --------------------------------------------------------------------------------- grid badge

/// A small "Immich" pill at the top left of a grid cell for photos linked to Immich (dashed
/// outline when the link is only probable).
pub fn grid_badge(app: &DacApp, ui: &egui::Ui, id: dac_catalog::PhotoId, img: Rect) {
    let state = app.session.catalog.remote_links().link_state(id, dac_immich::link::SERVICE);
    if state == "none" {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let p = ui.painter();
    let g = p.layout_no_wrap(crate::i18n::tr("Immich").into(), t.semibold(9.0), Color32::WHITE);
    let r = Rect::from_min_size(pos2(img.left() + 5.0, img.top() + 5.0), vec2(g.size().x + 10.0, 14.0));
    let fill = if state == "linked" { t.accent.gamma_multiply(0.85) } else { Color32::from_black_alpha(160) };
    p.rect_filled(r, 7.0, fill);
    if state == "probable" {
        p.rect_stroke(r, 7.0, egui::Stroke::new(1.0, t.caution), egui::StrokeKind::Inside);
    }
    p.galley(pos2(r.left() + 5.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
    register(ui.ctx(), format!("badge:immich:{}", id.0), r);
}

// ----------------------------------------------------------------------------- Info panel

/// "Immich" rows in Info: the link state and an Open in Immich link per account; Confirm for a
/// probable link.
pub fn metadata_rows(app: &mut DacApp, ui: &mut egui::Ui, id: dac_catalog::PhotoId) {
    if app.session.catalog.remote_links().link_state(id, dac_immich::link::SERVICE) == "none" {
        return;
    }
    let Ok(v) = app.session.execute("immich.links", &json!({"id": id.0})) else { return };
    let t = Tokens::get(ui.ctx());
    ui.add_space(4.0);
    ui.label(RichText::new(crate::i18n::tr("Immich")).size(11.5).color(t.text_dim));
    for (i, l) in v["links"].as_array().cloned().unwrap_or_default().iter().enumerate() {
        ui.horizontal(|ui| {
            let url = l["url"].as_str().unwrap_or_default().to_string();
            if text_button(ui, &format!("openInImmich-{i}"), crate::i18n::tr("Open in Immich"), false).on_hover_text(&url).clicked() {
                open_url(app, &url);
            }
            if l["state"] == "probable" {
                ui.label(RichText::new(crate::i18n::tr("Probable match")).color(t.caution));
                if text_button(ui, &format!("confirmImmich-{i}"), crate::i18n::tr("Confirm"), false).clicked() {
                    let _ = app.run("immich.confirmLink", json!({"ids": [id.0], "account": l["account"]}));
                }
            }
        });
    }
    if v["linkOnly"] == true {
        ui.label(
            RichText::new(crate::i18n::tr("Link only: the original is downloaded when you open the photo in Develop")).size(11.0).color(t.text_dim),
        );
    }
}

fn open_url(app: &mut DacApp, url: &str) {
    if let Some(f) = app.services.open_url.as_mut()
        && let Err(e) = f(url)
    {
        log::warn!("open {url}: {e}");
    }
}

// --------------------------------------------------------------------- Settings → Connections

fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(120.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(120.0);
            ui.label(RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

/// Run `immich.test` or `immich.connect` on a worker thread with its own session-free client.
/// (`immich.connect` must store the key through the session, so it runs on the UI thread after a
/// successful test, against a server that just answered.)
fn start_test(app: &mut DacApp, connect: bool) {
    let slot: Slot<Value> = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let params = json!({"url": app.immich.url, "apiKey": app.immich.key, "pinned": app.immich.pinned});
    app.immich.pending = Some((connect, slot));
    app.immich.result = None;
    std::thread::spawn(move || {
        let mut s = dac_engine::Session::new();
        let v = s.execute("immich.test", &params).unwrap_or_else(|e| json!({"ok": false, "error": {"kind": "badParams", "message": e.to_string()}}));
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(v);
    });
}

pub fn settings_tab(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens) {
    // a finished test
    if let Some((connect, slot)) = &app.immich.pending
        && let Some(v) = take(slot)
    {
        let connect = *connect;
        app.immich.pending = None;
        if connect && v["ok"] == true {
            let params = json!({"url": app.immich.url, "apiKey": app.immich.key, "pinned": app.immich.pinned});
            let r = app.session.execute("immich.connect", &params).unwrap_or_else(|e| json!({"ok": false, "error": {"message": e.to_string()}}));
            if r["ok"] == true {
                app.immich.key.clear();
                app.immich.url.clear();
                app.immich.pinned = None;
                if let Some(a) = r["account"].as_str() {
                    let _ = app.session.execute("immich.link", &json!({"account": a}));
                }
            }
            app.immich.result = Some(r);
        } else {
            app.immich.result = Some(v);
        }
    }
    if app.immich.pending.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }

    super::settings::heading(ui, t, "Immich servers");
    let st = app.session.execute("immich.status", &json!({})).unwrap_or_default();
    let accounts = st["accounts"].as_array().cloned().unwrap_or_default();
    if accounts.is_empty() {
        super::settings::hint(ui, t, "No server connected. Add one below to link your library with Immich and import from it.");
    }
    for (i, a) in accounts.iter().enumerate() {
        account_card(app, ui, t, i, a);
    }
    if let Some(store) = st["secretStore"].as_str().filter(|s| *s == "unavailable") {
        let _ = store;
        ui.label(RichText::new(crate::i18n::tr("No system keychain is available: API keys can't be stored on this computer.")).color(t.caution));
    }

    ui.add_space(8.0);
    super::settings::heading(ui, t, "Add Immich server");
    row(ui, t, "Server URL", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.immich.url).hint_text("https://photos.example.org").desired_width(300.0));
        register(ui.ctx(), "field:immichUrl", r.rect);
    });
    row(ui, t, "API key", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.immich.key).password(true).desired_width(300.0));
        register(ui.ctx(), "field:immichKey", r.rect);
    });
    ui.horizontal(|ui| {
        ui.add_space(124.0);
        let base = dac_immich::client::normalize_url(&app.immich.url).ok();
        if ui
            .add_enabled_ui(base.is_some(), |ui| text_button(ui, "immichKeyPage", crate::i18n::tr("Create an API key on the server…"), false))
            .inner
            .clicked()
            && let Some(b) = base
        {
            open_url(app, &dac_immich::client::api_key_page(&b));
        }
    });
    super::settings::hint(
        ui,
        t,
        "The key needs asset.read, asset.view and asset.download to link and import, album.read and person.read to browse, library.read for external libraries. It is kept in the system keychain.",
    );
    let ready = !app.immich.url.trim().is_empty() && !app.immich.key.trim().is_empty() && app.immich.pending.is_none();
    ui.horizontal(|ui| {
        ui.add_space(124.0);
        if ui.add_enabled_ui(ready, |ui| text_button(ui, "immichTest", crate::i18n::tr("Test"), false)).inner.clicked() {
            start_test(app, false);
        }
        if ui.add_enabled_ui(ready, |ui| text_button(ui, "immichConnect", crate::i18n::tr("Connect"), false)).inner.clicked() {
            start_test(app, true);
        }
        if app.immich.pending.is_some() {
            ui.spinner();
        }
    });
    if let Some(r) = app.immich.result.clone() {
        result_box(app, ui, t, &r);
    }
}

fn result_box(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, r: &Value) {
    ui.add_space(4.0);
    if r["ok"] == true {
        let user = &r["user"];
        let text = trf!(
            "Connected to Immich {} as {} ({})",
            r["version"].as_str().unwrap_or("?"),
            user["name"].as_str().unwrap_or_default(),
            user["email"].as_str().unwrap_or_default()
        );
        let l = ui.label(RichText::new(text).color(t.text));
        register(ui.ctx(), "label:immichResult", l.rect);
        permissions(ui, t, r);
        return;
    }
    let e = &r["error"];
    let l = ui.label(RichText::new(e["message"].as_str().unwrap_or("failed")).color(t.caution));
    register(ui.ctx(), "label:immichResult", l.rect);
    if e["kind"] == "untrustedCertificate"
        && let Some(fp) = e["fingerprint"].as_str()
    {
        // trust on first use: show the fingerprint and let the user accept it
        egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.5))).corner_radius(6.0).inner_margin(8).show(ui, |ui| {
            ui.label(RichText::new(crate::i18n::tr("Unknown certificate")).font(t.semibold(12.5)).color(t.text));
            ui.label(RichText::new(crate::i18n::tr("The server uses a certificate no authority vouches for (common for home servers). Trust it only if this SHA-256 fingerprint matches your server's certificate:")).size(11.5).color(t.text_label));
            let f = ui.label(RichText::new(fp).monospace().size(11.0).color(t.text));
            register(ui.ctx(), "label:immichFingerprint", f.rect);
            ui.horizontal(|ui| {
                if text_button(ui, "immichTrust", crate::i18n::tr("Trust This Certificate"), false).clicked() {
                    app.immich.pinned = Some(fp.to_string());
                    start_test(app, false);
                }
                if text_button(ui, "immichTrustCancel", crate::i18n::tr("Cancel"), false).clicked() {
                    app.immich.result = None;
                }
            });
        });
    } else if e["retryable"] == true && text_button(ui, "immichRetry", crate::i18n::tr("Retry"), false).clicked() {
        start_test(app, false);
    }
}

fn permissions(ui: &mut egui::Ui, t: &Tokens, a: &Value) {
    match a["permissions"].as_array() {
        Some(p) => {
            let all: Vec<&str> = p.iter().filter_map(Value::as_str).collect();
            ui.label(RichText::new(trf!("Key permissions: {}", all.join(", "))).size(11.0).color(t.text_dim));
        }
        None => {
            ui.label(RichText::new(crate::i18n::tr("The key may not read its own permissions.")).size(11.0).color(t.text_dim));
        }
    }
    for m in a["missingPermissions"].as_array().into_iter().flatten() {
        let perms: Vec<&str> = m["permissions"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        ui.label(
            RichText::new(trf!("Not available with this key: {} (needs {})", m["feature"].as_str().unwrap_or_default(), perms.join(", ")))
                .size(11.0)
                .color(t.caution),
        );
    }
}

fn account_card(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    let id = a["id"].as_str().unwrap_or_default().to_string();
    let r = egui::Frame::NONE.fill(t.inset).corner_radius(6.0).inner_margin(8).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(a["url"].as_str().unwrap_or_default()).font(t.semibold(12.5)).color(t.text));
            ui.label(RichText::new(trf!("Immich {}", a["version"].as_str().unwrap_or("?"))).color(t.text_dim));
        });
        ui.label(
            RichText::new(trf!("{} ({})", a["userName"].as_str().unwrap_or_default(), a["email"].as_str().unwrap_or_default())).color(t.text_label),
        );
        permissions(ui, t, a);
        let link = &a["link"];
        let status = if link["active"] == true {
            trf!("Linking… {} assets checked", link["seen"].as_u64().unwrap_or(0))
        } else {
            trf!("{} photos linked, {} probable", a["linked"].as_u64().unwrap_or(0), a["probable"].as_u64().unwrap_or(0))
        };
        ui.label(RichText::new(status).color(t.text_label));
        if let Some(e) = link["error"].as_str() {
            ui.label(RichText::new(e).color(t.caution));
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if text_button(ui, &format!("immichLink-{i}"), crate::i18n::tr("Link Now"), false).clicked() {
                let _ = app.run("immich.link", json!({"account": id}));
            }
            if text_button(ui, &format!("immichRecheck-{i}"), crate::i18n::tr("Check"), false).clicked() {
                let _ = app.run("immich.status", json!({"check": true}));
            }
            if text_button(ui, &format!("immichLibraries-{i}"), crate::i18n::tr("External Libraries…"), false).clicked() {
                let v = app
                    .session
                    .execute("immich.libraries", &json!({"account": id}))
                    .unwrap_or_else(|e| json!({"ok": false, "error": {"message": e.to_string()}}));
                let rows = v["pathMaps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| (m["container"].as_str().unwrap_or_default().to_string(), m["local"].as_str().unwrap_or_default().to_string()))
                    .collect();
                app.immich.maps.insert(id.clone(), rows);
                app.immich.libraries.insert(id.clone(), v);
            }
            if text_button(ui, &format!("immichImport-{i}"), crate::i18n::tr("Import…"), false).clicked() {
                open_import(app, Some(id.clone()));
            }
            if text_button(ui, &format!("immichDisconnect-{i}"), crate::i18n::tr("Disconnect"), false).clicked() {
                let _ = app.run("immich.disconnect", json!({"account": id}));
                app.immich.libraries.remove(&id);
            }
        });
        if app.immich.libraries.contains_key(&id) {
            extlib(app, ui, t, &id);
        }
    });
    register(ui.ctx(), format!("immichAccount:{i}"), r.response.rect);
    ui.add_space(4.0);
}

/// The external-library helper: libraries, the path mapping table, which catalog folders are
/// covered, and Write XMP.
fn extlib(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, id: &str) {
    let Some(v) = app.immich.libraries.get(id).cloned() else { return };
    ui.separator();
    ui.label(RichText::new(crate::i18n::tr("External libraries")).font(t.semibold(12.0)).color(t.text));
    if v["ok"] == false {
        ui.label(RichText::new(v["error"]["message"].as_str().unwrap_or("failed")).color(t.caution));
        return;
    }
    let libs = v["libraries"].as_array().cloned().unwrap_or_default();
    if libs.is_empty() {
        super::settings::hint(
            ui,
            t,
            "No external library on this server (or the key can't read them). An admin adds one in Immich → Administration → External Libraries.",
        );
    }
    for l in &libs {
        let paths: Vec<&str> = l["importPaths"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        ui.label(RichText::new(format!("{} — {}", l["name"].as_str().unwrap_or_default(), paths.join(", "))).color(t.text_label));
    }
    ui.label(RichText::new(crate::i18n::tr("Path mapping (Immich container path ↔ folder on this computer)")).size(11.5).color(t.text_dim));
    let mut rows = app.immich.maps.get(id).cloned().unwrap_or_default();
    if rows.is_empty() {
        rows = v["suggested"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|m| (m["container"].as_str().unwrap_or_default().to_string(), m["local"].as_str().unwrap_or_default().to_string()))
            .collect();
    }
    let mut remove = None;
    for (n, (c, l)) in rows.iter_mut().enumerate() {
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(c).hint_text("/mnt/photos").desired_width(170.0));
            ui.label("↔");
            ui.add(egui::TextEdit::singleline(l).hint_text("/home/me/Photos").desired_width(220.0));
            if text_button(ui, &format!("immichMapRemove-{n}"), "×", false).clicked() {
                remove = Some(n);
            }
        });
    }
    if let Some(n) = remove {
        rows.remove(n);
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if text_button(ui, "immichMapAdd", crate::i18n::tr("Add Row"), false).clicked() {
            rows.push((String::new(), String::new()));
        }
        if text_button(ui, "immichMapSave", crate::i18n::tr("Save Mapping"), false).clicked() {
            let maps: Vec<Value> = rows
                .iter()
                .filter(|(c, l)| !c.trim().is_empty() && !l.trim().is_empty())
                .map(|(c, l)| json!({"container": c.trim(), "local": l.trim()}))
                .collect();
            let _ = app.run("immich.setPathMaps", json!({"account": id, "pathMaps": maps}));
            if let Ok(nv) = app.session.execute("immich.libraries", &json!({"account": id})) {
                app.immich.libraries.insert(id.to_string(), nv);
            }
        }
        if text_button(ui, "immichWriteXmp", crate::i18n::tr("Write XMP Sidecars"), false)
            .on_hover_text(crate::i18n::tr("Immich reads ratings, descriptions and keywords from the sidecars on its next library scan"))
            .clicked()
        {
            match app.session.execute("immich.writeSidecars", &json!({"account": id})) {
                Ok(r) => app.toast(ui.ctx(), trf!("Wrote {} XMP sidecars", r["written"].as_u64().unwrap_or(0))),
                Err(e) => app.toast(ui.ctx(), e.to_string()),
            }
        }
    });
    app.immich.maps.insert(id.to_string(), rows);
    let cov = v["coverage"].as_array().cloned().unwrap_or_default();
    let covered = cov.iter().filter(|c| c["library"].is_string()).count();
    ui.label(RichText::new(trf!("{} of {} library folders are inside an external library", covered, cov.len())).color(t.text_label));
    egui::ScrollArea::vertical().id_salt(("immich-coverage", id)).max_height(120.0).show(ui, |ui| {
        for c in &cov {
            let (mark, color) = if c["library"].is_string() { ("✓", t.text) } else { ("–", t.text_dim) };
            let lib = c["libraryName"].as_str().map(|n| format!("  → {n}")).unwrap_or_default();
            ui.label(RichText::new(format!("{mark} {}{lib}", c["folder"].as_str().unwrap_or_default())).size(11.0).color(color));
        }
    });
}

// ---------------------------------------------------------------------- Import from Immich

/// UI commands: `file.importImmich` opens the Import from Immich window.
pub fn run(app: &mut DacApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        "file.importImmich" => {
            open_import(app, p.get("account").and_then(Value::as_str).map(str::to_string));
            Some(Ok(json!({"open": app.immich.import_open})))
        }
        _ => None,
    }
}

fn open_import(app: &mut DacApp, account: Option<String>) {
    let accounts: Vec<String> = app.session.immich_accounts().map(|a| a.immich.iter().map(|x| x.id.clone()).collect()).unwrap_or_default();
    let ui = &mut app.immich;
    ui.import_open = true;
    ui.account = account.filter(|a| accounts.contains(a)).or_else(|| accounts.first().cloned());
    if ui.destination.is_empty() {
        ui.destination = app
            .session
            .library
            .as_ref()
            .filter(|l| l.on_disk)
            .map(|l| l.dir.join("Originals").join("Immich").to_string_lossy().to_string())
            .unwrap_or_default();
    }
    set_source(app, "timeline", None);
}

fn set_source(app: &mut DacApp, source: &str, target: Option<(String, String)>) {
    let ui = &mut app.immich;
    ui.source = source.to_string();
    ui.target = target;
    ui.assets.clear();
    ui.groups.clear();
    ui.selected.clear();
    ui.page = 1;
    ui.next_page = None;
    ui.list_error = None;
    list(app);
}

/// List the current source's next page on a worker thread.
fn list(app: &mut DacApp) {
    let Some(account) = app.immich.account.clone() else { return };
    let (src, id) = match (&app.immich.target, app.immich.source.as_str()) {
        (Some((kind, id)), _) => (kind.clone(), Some(id.clone())),
        (None, s) => (s.to_string(), None),
    };
    let slot: Slot<Value> = Arc::new(Mutex::new(None));
    app.immich.listing = Some(slot.clone());
    let params = json!({"account": account, "source": src, "id": id, "page": app.immich.page, "size": 120});
    // the client (with the key) is made here, the request runs on the worker
    let client = app.session.immich_client(&account);
    let catalog_sums: HashSet<String> = app.session.catalog.photos().filter_map(|p| p.sha1.clone()).collect();
    let linked: HashSet<String> =
        app.session.catalog.remote_links().iter().filter(|r| r.account_id == account).map(|r| r.remote_id.clone()).collect();
    std::thread::spawn(move || {
        let v = match client {
            Err(e) => json!({"ok": false, "error": {"message": e.to_string()}}),
            Ok((_, c)) => browse(&c, &params, &catalog_sums, &linked),
        };
        *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(v);
    });
}

/// The worker half of `immich.browse` (the engine command needs the session).
fn browse(c: &dac_immich::Client, p: &Value, sums: &HashSet<String>, linked: &HashSet<String>) -> Value {
    use dac_immich::types::MetadataSearch;
    let fail = |e: dac_immich::ImmichError| json!({"ok": false, "error": {"message": e.to_string(), "retryable": e.retryable()}});
    match p["source"].as_str().unwrap_or("timeline") {
        "albums" => match c.albums() {
            Ok(a) => {
                json!({"ok": true, "groups": a.iter().map(|x| json!({"id": x.id, "name": x.album_name, "count": x.asset_count, "thumb": x.album_thumbnail_asset_id})).collect::<Vec<_>>()})
            }
            Err(e) => fail(e),
        },
        "people" => match c.people() {
            Ok(a) => {
                json!({"ok": true, "groups": a.iter().filter(|x| !x.is_hidden).map(|x| json!({"id": x.id, "name": if x.name.is_empty() { crate::i18n::tr("Unnamed").to_string() } else { x.name.clone() }})).collect::<Vec<_>>()})
            }
            Err(e) => fail(e),
        },
        src => {
            let mut q = MetadataSearch {
                page: Some(p["page"].as_u64().unwrap_or(1).clamp(1, 100_000) as u32),
                size: Some(120),
                with_exif: Some(true),
                order: Some("desc".into()),
                ..Default::default()
            };
            let id = p["id"].as_str().map(str::to_string);
            match src {
                "favorites" => q.is_favorite = Some(true),
                "album" => q.album_ids = id.map(|i| vec![i]),
                "person" => q.person_ids = id.map(|i| vec![i]),
                _ => {}
            }
            match c.search(&q) {
                Ok(pg) => json!({
                    "ok": true,
                    "assets": pg.items.iter().map(|a| json!({
                        "id": a.id,
                        "fileName": a.original_file_name,
                        "captured": a.local_capture(),
                        "inCatalog": linked.contains(&a.id) || a.sha1_hex().is_some_and(|h| sums.contains(&h)),
                    })).collect::<Vec<_>>(),
                    "nextPage": pg.next_page.and_then(|n| n.parse::<u64>().ok()),
                }),
                Err(e) => fail(e),
            }
        }
    }
}

/// Load thumbnails for `ids` on one worker thread (decoded to small images there).
fn load_thumbs(app: &mut DacApp, ids: Vec<String>) {
    let Some(account) = app.immich.account.clone() else { return };
    let Ok((_, client)) = app.session.immich_client(&account) else { return };
    let out = app.immich.thumb_rx.get_or_insert_with(Default::default).clone();
    for i in &ids {
        app.immich.thumbs.insert(i.clone(), Thumb::Loading);
    }
    std::thread::spawn(move || {
        for id in ids {
            let img = client.thumbnail(&id, "thumbnail").ok().and_then(|b| {
                let d = dac_codecs::decode(&b, dac_codecs::DecodeOptions { max_size: Some((256, 256)), ..Default::default() }).ok()?;
                let rgba = d.image.to_srgb8();
                Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width, rgba.height], &rgba.as_bytes()))
            });
            out.lock().unwrap_or_else(PoisonError::into_inner).push((id, img));
        }
    });
}

/// The Import from Immich window (when open).
pub fn import_window(app: &mut DacApp, ctx: &egui::Context) {
    if !app.immich.import_open {
        return;
    }
    // finished listing
    if let Some(slot) = app.immich.listing.clone()
        && let Some(v) = take(&slot)
    {
        app.immich.listing = None;
        if v["ok"] == true {
            if let Some(g) = v["groups"].as_array() {
                app.immich.groups = g.clone();
            }
            let new: Vec<Value> = v["assets"].as_array().cloned().unwrap_or_default();
            let ids: Vec<String> = new.iter().filter_map(|a| a["id"].as_str().map(str::to_string)).collect();
            app.immich.assets.extend(new);
            app.immich.next_page = v["nextPage"].as_u64();
            load_thumbs(app, ids);
        } else {
            app.immich.list_error = v["error"]["message"].as_str().map(str::to_string);
        }
    }
    if let Some(rx) = &app.immich.thumb_rx {
        let done: Vec<_> = std::mem::take(&mut *rx.lock().unwrap_or_else(PoisonError::into_inner));
        for (id, img) in done {
            let t = match img {
                Some(img) => Thumb::Ready(ctx.load_texture(format!("immich-{id}"), img, egui::TextureOptions::LINEAR)),
                None => Thumb::Failed,
            };
            app.immich.thumbs.insert(id, t);
        }
    }
    if app.immich.listing.is_some() || app.immich.thumbs.values().any(|t| matches!(t, Thumb::Loading)) {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut open = true;
    egui::Window::new(crate::i18n::tr("Import from Immich"))
        .id(egui::Id::new("immich-import"))
        .open(&mut open)
        .default_size(vec2(820.0, 600.0))
        .show(ctx, |ui| {
            import_body(app, ui);
        });
    if !open {
        app.immich.import_open = false;
    }
}

fn import_body(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let accounts: Vec<String> = app.session.immich_accounts().map(|a| a.immich.iter().map(|x| x.id.clone()).collect()).unwrap_or_default();
    if accounts.is_empty() {
        ui.label(RichText::new(crate::i18n::tr("No Immich server is connected.")).color(t.text_label));
        if text_button(ui, "immichOpenConnections", crate::i18n::tr("Connect a Server…"), false).clicked() {
            app.immich.import_open = false;
            let _ = app.run("app.settings", json!({"tab": "connections"}));
        }
        return;
    }
    // sources
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for (src, label) in [("timeline", "Timeline"), ("favorites", "Favourites"), ("albums", "Albums"), ("people", "People")] {
            let active = app.immich.source == src && app.immich.target.is_none();
            if text_button(ui, &format!("immichSource-{src}"), crate::i18n::tr(label), active).clicked() {
                set_source(app, src, None);
            }
        }
        if let Some((_, name)) = app.immich.target.clone().map(|(k, id)| {
            (k, app.immich.groups.iter().find(|g| g["id"] == id.as_str()).and_then(|g| g["name"].as_str()).unwrap_or_default().to_string())
        }) {
            ui.label(RichText::new(format!("› {name}")).color(t.text));
        }
        if app.immich.listing.is_some() {
            ui.spinner();
        }
    });
    if let Some(e) = app.immich.list_error.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(e).color(t.caution));
            if text_button(ui, "immichListRetry", crate::i18n::tr("Retry"), false).clicked() {
                app.immich.list_error = None;
                list(app);
            }
        });
    }
    ui.separator();
    let grid_h = (ui.available_height() - 90.0).max(160.0);
    let in_groups = matches!(app.immich.source.as_str(), "albums" | "people") && app.immich.target.is_none();
    egui::ScrollArea::vertical().id_salt("immich-import-grid").max_height(grid_h).auto_shrink([false, false]).show(ui, |ui| {
        if in_groups {
            let kind = if app.immich.source == "albums" { "album" } else { "person" };
            for (i, g) in app.immich.groups.clone().iter().enumerate() {
                let name = g["name"].as_str().unwrap_or_default().to_string();
                let label = match g["count"].as_u64() {
                    Some(n) => format!("{name} ({n})"),
                    None => name.clone(),
                };
                if text_button(ui, &format!("immichGroup-{i}"), &label, false).clicked() {
                    let id = g["id"].as_str().unwrap_or_default().to_string();
                    set_source(app, kind, Some((kind.to_string(), id)));
                }
            }
            return;
        }
        let cell = 132.0;
        let cols = ((ui.available_width() / cell).floor() as usize).max(1);
        let assets = app.immich.assets.clone();
        for (r, chunk) in assets.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                for (c, a) in chunk.iter().enumerate() {
                    asset_cell(app, ui, &t, r * cols + c, a, cell);
                }
            });
        }
        if app.immich.next_page.is_some()
            && app.immich.listing.is_none()
            && text_button(ui, "immichMore", crate::i18n::tr("Load More"), false).clicked()
        {
            app.immich.page = app.immich.next_page.unwrap_or(app.immich.page + 1);
            list(app);
        }
    });
    ui.separator();
    // options
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.label(RichText::new(crate::i18n::tr("Transfer")).color(t.text_label));
        if text_button(ui, "immichCopy", crate::i18n::tr("Copy"), !app.immich.link_only)
            .on_hover_text(crate::i18n::tr("Download the originals into a dated folder, then import them"))
            .clicked()
        {
            app.immich.link_only = false;
        }
        if text_button(ui, "immichLinkOnly", crate::i18n::tr("Link only"), app.immich.link_only)
            .on_hover_text(crate::i18n::tr(
                "Catalog the photos with a preview from the server; the original is downloaded when you open a photo in Develop",
            ))
            .clicked()
        {
            app.immich.link_only = true;
        }
        if !app.immich.link_only {
            ui.add_space(8.0);
            ui.label(RichText::new(crate::i18n::tr("Destination")).color(t.text_label));
            let r = ui.add(egui::TextEdit::singleline(&mut app.immich.destination).desired_width(280.0));
            register(ui.ctx(), "field:immichDestination", r.rect);
        }
    });
    ui.horizontal(|ui| {
        let n = app.immich.selected.len();
        ui.label(RichText::new(trf!("{} selected", n)).color(t.text_label));
        if text_button(ui, "immichSelectAll", crate::i18n::tr("Select All"), false).clicked() {
            let all: Vec<String> =
                app.immich.assets.iter().filter(|a| a["inCatalog"] != true).filter_map(|a| a["id"].as_str().map(str::to_string)).collect();
            app.immich.selected.extend(all);
        }
        let busy = app.immich.last["import"]["active"] == true;
        let ok = n > 0 && !busy && (app.immich.link_only || !app.immich.destination.trim().is_empty());
        if ui.add_enabled_ui(ok, |ui| text_button(ui, "immichImportStart", crate::i18n::tr("Import"), true)).inner.clicked() {
            let album = match &app.immich.target {
                Some((k, id)) if k == "album" => {
                    app.immich.groups.iter().find(|g| g["id"] == id.as_str()).and_then(|g| g["name"].as_str()).map(str::to_string)
                }
                _ => None,
            };
            let params = json!({
                "account": app.immich.account,
                "assets": app.immich.selected.iter().collect::<Vec<_>>(),
                "mode": if app.immich.link_only { "link" } else { "copy" },
                "destination": (!app.immich.link_only).then(|| app.immich.destination.trim().to_string()),
                "albumName": album,
            });
            match app.run("immich.import", params) {
                Ok(_) => {
                    app.immich.selected.clear();
                    app.immich.import_open = false;
                    app.toast(ui.ctx(), "Importing from Immich…");
                }
                Err(e) => app.toast(ui.ctx(), &e),
            }
        }
        if busy {
            ui.spinner();
        }
    });
}

fn asset_cell(app: &mut DacApp, ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value, size: f32) {
    let id = a["id"].as_str().unwrap_or_default().to_string();
    let (r, resp) = ui.allocate_exact_size(vec2(size - 4.0, size - 4.0), Sense::click());
    register(ui.ctx(), format!("immichAsset:{i}"), r);
    let have = a["inCatalog"] == true;
    let selected = app.immich.selected.contains(&id);
    let p = ui.painter();
    p.rect_filled(r, 3.0, if selected { t.cell_selected } else { t.cell });
    let img = r.shrink(6.0);
    match app.immich.thumbs.get(&id) {
        Some(Thumb::Ready(tex)) => {
            let [w, h] = tex.size();
            let s = (img.width() / w as f32).min(img.height() / h as f32);
            let fit = Rect::from_center_size(img.center(), vec2(w as f32 * s, h as f32 * s));
            let tint = if have { Color32::from_gray(110) } else { Color32::WHITE };
            p.image(tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        Some(Thumb::Failed) => {
            p.text(img.center(), egui::Align2::CENTER_CENTER, "?", t.font(14.0), t.text_dim);
        }
        _ => {}
    }
    if have {
        let g = p.layout_no_wrap(crate::i18n::tr("In library").into(), t.semibold(9.0), Color32::WHITE);
        let br = Rect::from_min_size(pos2(r.left() + 4.0, r.top() + 4.0), vec2(g.size().x + 8.0, 14.0));
        p.rect_filled(br, 7.0, Color32::from_black_alpha(170));
        p.galley(pos2(br.left() + 4.0, br.center().y - g.size().y / 2.0), g, Color32::WHITE);
    }
    if selected {
        p.rect_stroke(r, 3.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
    }
    let name = a["fileName"].as_str().unwrap_or_default();
    let resp = resp.on_hover_text(format!("{name}\n{}", a["captured"].as_str().unwrap_or_default()));
    if resp.clicked() && !have && !app.immich.selected.remove(&id) {
        app.immich.selected.insert(id);
    }
}
