//! Immich in the app (IMM-CONNECT, IMM-LINK, IMM-IMPORT, IMM-EXTLIB): Settings → Connections,
//! the "in Immich" grid badge, Info's "Open in Immich" link, the Import dialog's Immich source, and the
//! per-frame `remote.pump` that takes in background work (SHA-1 back-fill, link passes, imports,
//! downloading a link-only photo's original when it is opened in Develop).
//!
//! Everything goes through the engine's `immich.*` commands except the slow listing and thumbnail
//! calls of the Immich import source, which run on worker threads with a client from
//! [`lcc_engine::Session::immich_client`]. The API key typed into the form lives in memory until
//! Connect hands it to the secret store; it is never shown again, logged or saved in settings.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use egui::{Color32, Rect, RichText, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
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
    pub(crate) url: String,
    key: String,
    pinned: Option<String>,
    pending: Option<(bool, Slot<Value>)>,
    result: Option<Value>,
    libraries: HashMap<String, Value>,
    maps: HashMap<String, Vec<(String, String)>>,
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
    /// Waiting for a background `immich.connect` (its result arrives in `immich.status`).
    awaiting_connect: bool,
    /// The key file's passphrase as typed (cleared once handed to `credentials.unlock`), and its
    /// confirmation when the file is new.
    passphrase: String,
    passphrase2: String,
    /// The last `credentials.status`, refreshed every few frames (it is cheap).
    cred: Value,
    cred_at: f64,
    /// Accounts whose Sync section is open, and each one's last `immich.syncStatus` +
    /// `immich.syncConflicts` (refreshed on open, after an action, and while a sync runs).
    sync_open: BTreeSet<String>,
    sync_view: HashMap<String, (Value, Value, f64)>,
    /// Accounts whose XMP-sidecar section is open (IMM-SIDECARS).
    sidecars_open: BTreeSet<String>,
    /// Push the whole XMP for managed assets through a temporary asset (opt-in: `PUT /assets/copy`).
    full_sidecars: bool,
    /// Accounts whose Locked folders section is open, and each one's last `immich.authStatus`
    /// (fetched when the section opens and again after an unlock: the state only changes when a
    /// session is unlocked here or elsewhere, or when the server's elevation ends).
    locked_open: BTreeSet<String>,
    locked_view: HashMap<String, Value>,
    /// The unlock dialog: the account it is open for, the secret as typed (cleared once sent or
    /// cancelled; never stored) and the refusal to show.
    unlock_for: Option<String>,
    unlock_secret: String,
    unlock_error: Option<String>,
    /// The filter bar's Immich smart search: the query, a search in flight (account, results),
    /// and the last message.
    smart_query: String,
    smart_pending: Option<(String, Slot<Result<Vec<String>, String>>)>,
    smart_note: Option<String>,
    /// Server calls this screen made with `background: true` whose answers are still out (their
    /// `immich.answer` keys, `<command>:<account>`): the window never waits for a server.
    asked: Vec<String>,
    /// Accounts whose path-mapping rows are filled from the next `immich.libraries` answer.
    maps_reload: HashSet<String>,
    /// A destructive action of an account card, waiting for the user's yes.
    confirm: Option<Confirm>,
    /// Link-only photos whose original failed to download and was reported; forgotten (with
    /// `fetch_tried`) once the user moves on, so opening the photo again tries again.
    fetch_failed: HashSet<u64>,
}

/// Something that removes or deletes, asked about before it is done. Shown inline in the account
/// card (the Settings screen is the modal layer while it is open).
struct Confirm {
    account: String,
    /// Where the question shows: `card` (under the card's buttons) or `sync` (the Sync section).
    place: &'static str,
    text: String,
    /// The confirm button's label: the label of the button that asked.
    button: &'static str,
    command: &'static str,
    params: Value,
}

#[cfg(test)]
impl ImmichUi {
    /// Fill both passphrase fields (tests: typing into a password field).
    pub(crate) fn type_passphrase(&mut self, p: &str) {
        self.passphrase = p.into();
        self.passphrase2 = p.into();
    }

    /// Type into the unlock dialog's secret field (tests).
    pub(crate) fn type_unlock_secret(&mut self, s: &str) {
        self.unlock_secret = s.into();
    }

    /// A cached locked-folder status (`immich.authStatus`'s result; tests use the one account).
    pub(crate) fn locked_status(&self) -> Value {
        self.locked_view.values().next().cloned().unwrap_or_default()
    }

    /// The unlock dialog's secret field is empty (tests).
    pub(crate) fn unlock_secret_is_empty(&self) -> bool {
        self.unlock_secret.is_empty()
    }
}

// ------------------------------------------------------------------------------------- pump

/// Every frame (cheaply): take in finished background work, keep the window repainting while
/// something runs, and start downloading the original of a link-only photo opened in Develop.
/// RT.C1/C2: the Edit In version waiting for the re-save question (photo id, "replace" picked,
/// "remember my choice" ticked).
#[derive(Clone, Debug, Default)]
struct ResaveAsk {
    photo: u64,
    replace: bool,
    remember: bool,
}

thread_local! {
    static RESAVE: std::cell::RefCell<Vec<ResaveAsk>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// After the external editor saved Edit In versions (`ids` reloaded): versions of Immich-linked
/// photos go to Immich (`immich.uploadVersion`); a version already there asks Keep both / Replace
/// unless the user chose to be remembered.
pub(crate) fn after_edit_saved(app: &mut LccApp, ctx: &egui::Context, ids: &[u64]) {
    for id in ids {
        match app.session.execute("immich.uploadVersion", &json!({"id": id})) {
            Ok(r) if r["ask"] == true => RESAVE.with_borrow_mut(|q| {
                if !q.iter().any(|a| a.photo == *id) {
                    q.push(ResaveAsk { photo: *id, ..Default::default() });
                }
            }),
            Ok(r) if r["started"] == true => app.toast(ctx, crate::i18n::tr("Uploading the edited version to Immich…")),
            Ok(_) => {}
            Err(e) => app.toast(ctx, e.to_string()),
        }
    }
}

/// The re-save question and the finished uploads' toasts (every frame).
fn versions_ui(app: &mut LccApp, ctx: &egui::Context) {
    if let Ok(v) = app.session.execute("immich.versionResults", &json!({})) {
        for r in v["results"].as_array().into_iter().flatten() {
            let text = match r["error"].as_str() {
                Some(e) => trf!("Immich: {}", e),
                None if r["trashed"].is_string() => crate::i18n::tr("The edited version replaced the previous one on Immich").to_string(),
                None => crate::i18n::tr("The edited version is on Immich, stacked on the original").to_string(),
            };
            app.toast(ctx, text);
        }
    }
    let Some(mut ask) = RESAVE.with_borrow(|q| q.first().cloned()) else { return };
    let (mut go, mut open) = (false, true);
    egui::Window::new(crate::i18n::tr("Edited Version Saved Again"))
        .id(egui::Id::new("immich-resave"))
        .title_bar(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            if crate::dialog_frame::card_header(ui, crate::i18n::tr("Edited Version Saved Again"), Some("immich-resave:close")) {
                open = false;
            }
            ui.label(crate::i18n::tr(
                "This version is already on Immich. Immich can't replace a file, so the new save is uploaded as a new asset on top of the stack.",
            ));
            ui.radio_value(&mut ask.replace, false, crate::i18n::tr("Keep both"));
            ui.radio_value(&mut ask.replace, true, crate::i18n::tr("Replace (move the previous upload to Immich's trash)"));
            ui.checkbox(&mut ask.remember, crate::i18n::tr("Remember my choice"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go = crate::button::Button::new("Upload").kind(crate::button::Kind::Primary).widget("button:immichResaveUpload").show(ui).clicked();
            });
        });
    let choice = if ask.replace { "replace" } else { "keepBoth" };
    if go {
        if ask.remember
            && let Err(e) = app.session.execute("immich.versionSettings", &json!({"resave": choice}))
        {
            app.toast(ctx, e.to_string());
        }
        match app.session.execute("immich.uploadVersion", &json!({"id": ask.photo, "resave": choice})) {
            Ok(_) => app.toast(ctx, crate::i18n::tr("Uploading the edited version to Immich…")),
            Err(e) => app.toast(ctx, e.to_string()),
        }
    }
    RESAVE.with_borrow_mut(|q| {
        if go || !open {
            q.retain(|a| a.photo != ask.photo);
        } else if let Some(first) = q.first_mut() {
            *first = ask;
        }
    });
}

/// Make a server call on a worker (the command's `background: true`); its answer comes in
/// through [`answers_ui`]. A call that could not even start answers at once, the same way.
fn ask_bg(app: &mut LccApp, ctx: &egui::Context, command: &str, account: &str, mut params: Value) {
    let key = format!("{command}:{account}");
    params["account"] = json!(account);
    params["background"] = json!(true);
    match app.session.execute(command, &params) {
        Ok(r) if r["started"] == true || r["key"].is_string() => {
            if !app.immich.asked.contains(&key) {
                app.immich.asked.push(key);
            }
        }
        Ok(r) => answer_in(app, ctx, &key, r),
        Err(e) => answer_in(app, ctx, &key, json!({"ok": false, "error": {"message": e.to_string()}})),
    }
}

/// Take in the answers of the background calls this screen made, every frame while one is out.
fn answers_ui(app: &mut LccApp, ctx: &egui::Context) {
    if app.immich.asked.is_empty() {
        return;
    }
    for key in std::mem::take(&mut app.immich.asked) {
        match app.session.execute("immich.answer", &json!({"key": key})) {
            Ok(v) if v["done"] == true => answer_in(app, ctx, &key, v["result"].clone()),
            Ok(v) if v["running"] == true => app.immich.asked.push(key),
            // (neither running nor answered: nothing left to wait for)
            Ok(_) => answer_in(app, ctx, &key, json!({"ok": false, "error": {"message": "Immich"}})),
            Err(e) => answer_in(app, ctx, &key, json!({"ok": false, "error": {"message": e.to_string()}})),
        }
    }
    if !app.immich.asked.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// One background call's answer: into the section that asked, or a toast. A failure is always
/// shown: in its section where there is one, else as a toast.
fn answer_in(app: &mut LccApp, ctx: &egui::Context, key: &str, r: Value) {
    // (`<command>:<account>`; the account id has colons of its own, the command none)
    let Some((command, account)) = key.split_once(':') else { return };
    let failed = r["ok"] == false;
    let message = r["error"]["message"].as_str().unwrap_or("Immich").to_string();
    match command {
        "immich.libraries" => {
            if app.immich.maps_reload.remove(account) {
                let rows = r["pathMaps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|m| (m["container"].as_str().unwrap_or_default().to_string(), m["local"].as_str().unwrap_or_default().to_string()))
                    .collect();
                app.immich.maps.insert(account.to_string(), rows);
            }
            app.immich.libraries.insert(account.to_string(), r);
        }
        "immich.authStatus" => {
            app.immich.locked_view.insert(account.to_string(), r);
        }
        "immich.unlockSession" if failed => app.immich.unlock_error = Some(message),
        "immich.unlockSession" => {
            app.toast(
                ctx,
                crate::i18n::tr("Unlocked: locked-folder photos are now visible to link, import and sync until Immich locks the session again."),
            );
            // the section's status line is stale now
            app.immich.locked_view.remove(account);
            if app.immich.unlock_for.as_deref() == Some(account) {
                app.immich.unlock_for = None;
                app.immich.unlock_secret.clear();
                app.immich.unlock_error = None;
            }
        }
        _ if failed => app.toast(ctx, message),
        "immich.scanLibraries" => app.toast(ctx, trf!("Immich is rescanning {} libraries", r["scanned"].as_array().map(Vec::len).unwrap_or(0))),
        "immich.importPeople" => app.toast(ctx, trf!("Imported the faces of {} photos from Immich", r["photos"].as_u64().unwrap_or(0))),
        "immich.pushPeople" => app.toast(ctx, trf!("Sent {} names to Immich", r["renames"].as_array().map(Vec::len).unwrap_or(0))),
        _ => {}
    }
}

/// Show a command's failure (`{ok: false, error}`, or an error) as a toast → whether it worked.
fn report(app: &mut LccApp, ctx: &egui::Context, r: Result<Value, String>) -> bool {
    match r {
        Ok(r) if r["ok"] == false => {
            app.toast(ctx, r["error"]["message"].as_str().unwrap_or("Immich"));
            false
        }
        Ok(_) => true,
        Err(e) => {
            app.toast(ctx, e);
            false
        }
    }
}

/// The question of a pending [`Confirm`] of this account and place, with its button and Cancel.
/// → true when the action was carried out.
fn confirm_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, id: &str, place: &str) -> bool {
    let Some((text, button)) = app.immich.confirm.as_ref().filter(|c| c.account == id && c.place == place).map(|c| (c.text.clone(), c.button))
    else {
        return false;
    };
    let (mut go, mut cancel) = (false, false);
    let frame = egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.5))).corner_radius(6.0).inner_margin(8);
    let r = frame.show(ui, |ui| {
        let l = ui.label(RichText::new(text).size(11.5).color(t.text));
        register(ui.ctx(), format!("label:immichConfirm-{i}"), l.rect);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            go = crate::button::Button::new(button).kind(crate::button::Kind::Danger).widget("button:immichConfirmGo").show(ui).clicked();
            if text_button(ui, "immichConfirmCancel", crate::i18n::tr("Cancel"), false).clicked() {
                cancel = true;
            }
        });
    });
    register(ui.ctx(), format!("immichConfirmBox-{i}"), r.response.rect);
    if go && let Some(c) = app.immich.confirm.take() {
        match app.run(c.command, c.params) {
            Ok(r) if r["ok"] == false => app.toast(ui.ctx(), r["error"]["message"].as_str().unwrap_or("Immich")),
            // (a deletion the server refused comes back in `failed`)
            Ok(r) => {
                if let Some(m) = r["failed"].as_array().and_then(|f| f.first()).and_then(|f| f["error"]["message"].as_str()) {
                    app.toast(ui.ctx(), m);
                }
            }
            Err(e) => app.toast(ui.ctx(), e),
        }
        if c.command == "immich.disconnect" {
            app.immich.libraries.remove(id);
        }
        return true;
    }
    if cancel {
        app.immich.confirm = None;
    }
    false
}

pub fn pump(app: &mut LccApp, ctx: &egui::Context) {
    versions_ui(app, ctx);
    answers_ui(app, ctx);
    let now = ctx.input(|i| i.time);
    if now - app.immich.last_pump < 0.2 {
        return;
    }
    app.immich.last_pump = now;
    let Ok(v) = app.session.execute("remote.pump", &json!({})) else { return };
    let busy = v["sha1"]["active"] == true
        || v["import"]["active"] == true
        || v["links"].as_object().is_some_and(|m| m.values().any(|l| l["active"] == true))
        || v["fetching"].as_array().is_some_and(|a| !a.is_empty())
        || v["connecting"] == true
        || v["unlocking"] == true
        || v["checking"].as_array().is_some_and(|a| !a.is_empty())
        || v["sidecars"]["active"] == true;
    // an import that just finished: say so
    if app.immich.last["import"]["active"] == true && v["import"]["active"] == false {
        let n = v["import"]["imported"].as_u64().unwrap_or(0);
        let skipped = v["import"]["skipped"].as_u64().unwrap_or(0);
        let failed = v["import"]["failed"].as_array().map(Vec::len).unwrap_or(0);
        app.toast(ctx, trf!("Imported {} from Immich ({} already in the catalog, {} failed)", n, skipped, failed));
    }
    // a sidecar run that just finished: say what it did
    if app.immich.last["sidecars"]["active"] == true && v["sidecars"]["active"] == false {
        let last = &v["sidecars"]["last"];
        let n = |k: &str| last[k].as_u64().unwrap_or(0);
        if last["direction"] == "pull" {
            app.toast(ctx, trf!("Pulled {} XMP sidecars ({} written beside the originals, {} skipped)", n("pulled"), n("written"), n("skipped")));
        } else if last["direction"] == "push" {
            app.toast(ctx, trf!("Pushed {} XMP sidecars ({} written beside the originals, {} skipped)", n("pushed"), n("written"), n("skipped")));
        }
    }
    // an original that could not be downloaded: say why (once per try)
    for e in v["fetchErrors"].as_array().into_iter().flatten() {
        if let Some(id) = e["id"].as_u64()
            && app.immich.fetch_tried.contains(&id)
            && app.immich.fetch_failed.insert(id)
        {
            app.toast(ctx, trf!("Could not download the original from Immich: {}", e["error"].as_str().unwrap_or_default()));
        }
    }
    // …and once the user has moved on from that photo, forget the try: opening it again tries again
    let active = app.session.active().map(|i| i.0);
    let im = &mut app.immich;
    let tried = &mut im.fetch_tried;
    im.fetch_failed.retain(|id| {
        if Some(*id) == active {
            return true;
        }
        tried.remove(id);
        false
    });
    app.immich.last = v;
    if busy {
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
    }
    // Develop on a link-only photo: fetch its original (once per photo; again after a failure
    // when the photo is opened again)
    // (the Modern layout's Develop has no Edit panel: being in Develop is enough there)
    let developing = app.ui.view == crate::state::ViewMode::Detail
        && (app.ui.right.is_edit_tool() || (crate::modern::on(app) && app.ui.module != crate::module::ModuleId::Library));
    if developing
        && let Some(id) = app.session.active()
        && app.session.catalog.photo(id).is_some_and(|p| p.preview_only.as_deref() == Some(lcc_engine::remote::LINK_ONLY))
        && app.immich.fetch_tried.insert(id.0)
    {
        let failed = match app.session.execute("immich.fetchOriginal", &json!({"id": id.0})) {
            Ok(r) if r["started"] == true => {
                app.toast(ctx, crate::i18n::tr("Downloading the original from Immich…"));
                false
            }
            Ok(r) if r["ok"] == false => {
                app.toast(ctx, r["error"]["message"].as_str().unwrap_or("Immich"));
                true
            }
            Ok(_) => false,
            Err(e) => {
                app.toast(ctx, e.to_string());
                true
            }
        };
        // (a download that could not even start is tried again when the photo is opened again)
        if failed {
            app.immich.fetch_failed.insert(id.0);
        }
    }
}

// ----------------------------------------------------------------------------- Info panel

/// "Immich" rows in Info: the link state and an Open in Immich link per account; Confirm for a
/// probable link.
pub fn metadata_rows(app: &mut LccApp, ui: &mut egui::Ui, id: lcc_catalog::PhotoId) {
    if app.session.catalog.remote_links().link_state(id, lcc_immich::link::SERVICE) == "none" {
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

fn open_url(app: &mut LccApp, url: &str) {
    if let Some(f) = app.services.open_url.as_mut()
        && let Err(e) = f(url)
    {
        log::warn!("open {url}: {e}");
    }
}

// --------------------------------------------------------------------- Settings → Connections

fn row<R>(ui: &mut egui::Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    if crate::dialog_frame::in_form(ui) {
        return crate::dialog_frame::form_row(ui, label, add);
    }
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(120.0, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(120.0);
            ui.label(RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

/// Indents a row's content to the control column of the rows above it.
fn control_indent(ui: &mut egui::Ui) {
    let indent = if crate::dialog_frame::in_form(ui) { crate::dialog_frame::control_indent(ui) } else { 124.0 };
    ui.add_space(indent.max(0.0));
}

/// Test the form's server on a worker thread (`immich.test` in a session of its own), or connect
/// in the background (`immich.connect {background}`: the probe runs on a worker, `remote.pump`
/// stores the key; the result shows up in `immich.status`).
fn start_test(app: &mut LccApp, connect: bool) {
    if connect {
        let params = json!({"url": app.immich.url, "apiKey": app.immich.key, "pinned": app.immich.pinned, "background": true});
        app.immich.result = None;
        match app.session.execute("immich.connect", &params) {
            Ok(r) if r["started"] == true => app.immich.awaiting_connect = true,
            Ok(r) => app.immich.result = Some(json!({"ok": false, "error": {"message": r["reason"].as_str().unwrap_or("busy")}})),
            Err(e) => app.immich.result = Some(json!({"ok": false, "error": {"kind": "keyStorage", "message": e.to_string()}})),
        }
        return;
    }
    let slot: Slot<Value> = Arc::new(Mutex::new(None));
    let out = slot.clone();
    let params = json!({"url": app.immich.url, "apiKey": app.immich.key, "pinned": app.immich.pinned});
    app.immich.pending = Some((connect, slot));
    app.immich.result = None;
    std::thread::spawn(move || {
        let mut s = lcc_engine::Session::new();
        let v = s.execute("immich.test", &params).unwrap_or_else(|e| json!({"ok": false, "error": {"kind": "badParams", "message": e.to_string()}}));
        *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(v);
    });
}

pub fn settings_tab(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    // a finished test
    if let Some((_, slot)) = &app.immich.pending
        && let Some(v) = take(slot)
    {
        app.immich.pending = None;
        app.immich.result = Some(v);
    }
    if app.immich.pending.is_some() || app.immich.awaiting_connect {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    key_storage(app, ui, t);
    super::settings::heading_help(ui, t, "Immich servers", "connections");
    let st = app.session.execute("immich.status", &json!({})).unwrap_or_default();
    // a background connect that finished
    if app.immich.awaiting_connect && st["connecting"] == false {
        app.immich.awaiting_connect = false;
        let r = st["connected"].clone();
        if r["ok"] == true {
            app.immich.key.clear();
            app.immich.url.clear();
            app.immich.pinned = None;
            if let Some(a) = r["account"].as_str() {
                let _ = app.session.execute("immich.link", &json!({"account": a}));
            }
        }
        app.immich.result = Some(r);
    }
    let accounts = st["accounts"].as_array().cloned().unwrap_or_default();
    if accounts.is_empty() {
        super::settings::hint(ui, t, "No server connected. Add one below to link your catalog with Immich and import from it.");
    }
    for (i, a) in accounts.iter().enumerate() {
        account_card(app, ui, t, i, a);
    }

    super::settings::heading(ui, t, "Add Immich server");
    row(ui, t, "Server URL", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.immich.url).hint_text("https://photos.example.org").desired_width(300.0));
        crate::access::label(&r, "Server URL");
        register(ui.ctx(), "field:immichUrl", r.rect);
    });
    if let Some(w) = lcc_immich::client::plain_http_warning(&app.immich.url) {
        ui.horizontal_wrapped(|ui| {
            control_indent(ui);
            let l = ui.label(RichText::new(crate::i18n::tr(w)).size(11.0).color(t.caution));
            register(ui.ctx(), "label:immichUrlInsecure", l.rect);
        });
    }
    row(ui, t, "API key", |ui| {
        let r = ui.add(egui::TextEdit::singleline(&mut app.immich.key).password(true).desired_width(300.0));
        crate::access::label(&r, "API key");
        register(ui.ctx(), "field:immichKey", r.rect);
    });
    ui.horizontal(|ui| {
        control_indent(ui);
        let base = lcc_immich::client::normalize_url(&app.immich.url).ok();
        if ui
            .add_enabled_ui(base.is_some(), |ui| text_button(ui, "immichKeyPage", crate::i18n::tr("Create an API key on the server…"), false))
            .inner
            .clicked()
            && let Some(b) = base
        {
            open_url(app, &lcc_immich::client::api_key_page(&b));
        }
    });
    super::settings::hint(
        ui,
        t,
        "The key needs asset.read, asset.view and asset.download to link and import, album.read and person.read to browse, library.read for external libraries. It is kept in the key storage above, never in settings.",
    );
    let ready =
        !app.immich.url.trim().is_empty() && !app.immich.key.trim().is_empty() && app.immich.pending.is_none() && !app.immich.awaiting_connect;
    ui.horizontal(|ui| {
        control_indent(ui);
        if ui.add_enabled_ui(ready, |ui| text_button(ui, "immichTest", crate::i18n::tr("Test"), false)).inner.clicked() {
            start_test(app, false);
        }
        if ui.add_enabled_ui(ready, |ui| text_button(ui, "immichConnect", crate::i18n::tr("Connect"), false)).inner.clicked() {
            start_test(app, true);
        }
        if app.immich.pending.is_some() || app.immich.awaiting_connect {
            ui.spinner();
            let what = if app.immich.awaiting_connect { "Connecting…" } else { "Testing…" };
            ui.label(RichText::new(crate::i18n::tr(what)).color(t.text_dim));
        }
    });
    if let Some(r) = app.immich.result.clone() {
        result_box(app, ui, t, &r);
    }
}

/// `credentials.status`, refreshed at most twice a second.
fn cred_status(app: &mut LccApp, ctx: &egui::Context) -> Value {
    let now = ctx.input(|i| i.time);
    if app.immich.cred.is_null() || now - app.immich.cred_at > 0.5 || app.immich.cred["unlocking"] == true {
        app.immich.cred = app.session.execute("credentials.status", &json!({})).unwrap_or_default();
        app.immich.cred_at = now;
    }
    app.immich.cred.clone()
}

/// Settings → Connections → Key storage: where API keys are kept (the system keychain or an
/// encrypted file), and the passphrase prompt that unlocks the file once per session.
fn key_storage(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens) {
    super::settings::heading(ui, t, "Key storage");
    let st = cred_status(app, ui.ctx());
    let system_ok = st["system"]["state"] == "ok";
    let pref = st["preference"].as_str().unwrap_or("system").to_string();
    row(ui, t, "Keep keys in", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let sys = text_button(ui, "credStoreSystem", crate::i18n::tr("System Keychain"), pref == "system");
        if sys.clicked() {
            let _ = app.session.execute("credentials.useStore", &json!({"store": "system"}));
            app.immich.cred = Value::Null;
        }
        if text_button(ui, "credStoreFile", crate::i18n::tr("Encrypted File"), pref == "file").clicked() {
            let _ = app.session.execute("credentials.useStore", &json!({"store": "file"}));
            app.immich.cred = Value::Null;
        }
    });
    if !system_ok {
        let why = match st["system"]["state"].as_str() {
            Some("locked") => "The system keychain is locked.",
            Some("unsupported") => "This build can't use this system's keychain, so keys go into an encrypted file.",
            _ => "The system keychain can't be reached, so keys go into an encrypted file.",
        };
        super::settings::hint(ui, t, why);
    }
    if st["system"]["state"] == "locked" || st["error"]["kind"] == "locked" {
        ui.horizontal(|ui| {
            control_indent(ui);
            if ui
                .add_enabled_ui(st["unlocking"] != true, |ui| text_button(ui, "credUnlockSystem", crate::i18n::tr("Unlock Keychain…"), false))
                .inner
                .clicked()
            {
                let _ = app.session.execute("credentials.unlockSystem", &json!({}));
                app.immich.cred = Value::Null;
            }
        });
    }
    if st["needsPassphrase"] == true {
        unlock_box(app, ui, t, &st);
    } else {
        let active = st["active"].as_str().unwrap_or("none");
        let label = if active == "encrypted file" {
            crate::i18n::tr("Keys are in the encrypted file (unlocked for this session).").to_string()
        } else {
            trf!("Keys are in the system keychain ({}).", active)
        };
        ui.horizontal(|ui| {
            control_indent(ui);
            let l = ui.label(RichText::new(label).color(t.text_label));
            register(ui.ctx(), "label:credStore", l.rect);
            if st["file"]["unlocked"] == true && text_button(ui, "credLock", crate::i18n::tr("Lock"), false).clicked() {
                let _ = app.session.execute("credentials.lock", &json!({}));
                app.immich.cred = Value::Null;
            }
        });
    }
}

/// The passphrase prompt of the encrypted key file (create it when there is none yet).
pub(crate) fn unlock_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, st: &Value) {
    let exists = st["file"]["exists"] == true;
    let frame = egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.5))).corner_radius(6.0).inner_margin(8);
    let r = frame.show(ui, |ui| {
        let title = if exists { "Unlock the key file" } else { "Create an encrypted key file" };
        ui.label(RichText::new(crate::i18n::tr(title)).font(t.semibold(12.5)).color(t.text));
        let hint = if exists {
            "Your Immich keys are in a file encrypted with your passphrase. It is asked once per session."
        } else {
            "Choose a passphrase (at least 8 characters). Keys you add are encrypted with it; you give it once per session. It can't be recovered."
        };
        ui.label(RichText::new(crate::i18n::tr(hint)).size(11.5).color(t.text_label));
        let mut submit = false;
        row(ui, t, "Passphrase", |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut app.immich.passphrase).password(true).desired_width(240.0));
            crate::access::label(&r, "Passphrase");
            register(ui.ctx(), "field:credPassphrase", r.rect);
            submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        });
        if !exists {
            row(ui, t, "Repeat", |ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut app.immich.passphrase2).password(true).desired_width(240.0));
                crate::access::label(&r, "Repeat");
                register(ui.ctx(), "field:credPassphrase2", r.rect);
                submit |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        }
        let long = app.immich.passphrase.chars().count() >= if exists { 1 } else { 8 };
        let same = exists || app.immich.passphrase == app.immich.passphrase2;
        let busy = st["unlocking"] == true;
        ui.horizontal(|ui| {
            control_indent(ui);
            let label = if exists { "Unlock" } else { "Create" };
            let clicked = ui.add_enabled_ui(long && same && !busy, |ui| text_button(ui, "credUnlock", crate::i18n::tr(label), true)).inner.clicked();
            if (clicked || submit) && long && same && !busy {
                let pass = std::mem::take(&mut app.immich.passphrase);
                app.immich.passphrase2.clear();
                match app.session.execute("credentials.unlock", &json!({"passphrase": pass, "background": true})) {
                    Ok(r) if r["started"] == true => {}
                    Ok(r) => app.toast(ui.ctx(), r["reason"].as_str().unwrap_or("busy")),
                    Err(e) => app.toast(ui.ctx(), e.to_string()),
                }
                app.immich.cred = Value::Null;
            }
            if busy {
                ui.spinner();
            }
        });
        if !exists && !same && !app.immich.passphrase2.is_empty() {
            ui.label(RichText::new(crate::i18n::tr("The passphrases differ.")).size(11.0).color(t.caution));
        }
        let err = match st["error"]["kind"].as_str() {
            Some("wrongPassphrase") => Some(crate::i18n::tr("Wrong passphrase.").to_string()),
            Some("locked") | None => None,
            Some(_) => st["error"]["message"].as_str().map(str::to_string),
        };
        if let Some(e) = err {
            let l = ui.label(RichText::new(e).color(t.caution));
            register(ui.ctx(), "label:credError", l.rect);
        }
    });
    register(ui.ctx(), "credUnlockBox", r.response.rect);
}

fn result_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, r: &Value) {
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
        permissions(ui, t, usize::MAX, r);
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

/// The key's permissions, short: only what the app needs and the key lacks (or one line saying
/// nothing is missing); the full list sits behind a collapsed "Show permissions".
fn permissions(ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    missing_permissions(ui, t, i, a);
    ui.horizontal_wrapped(|ui| permissions_inline(ui, t, i, a));
    all_permissions(ui, t, i, a);
}

/// What the key lacks, one caution line per feature.
fn missing_permissions(ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    for m in a["missingPermissions"].as_array().into_iter().flatten() {
        let perms: Vec<&str> = m["permissions"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        let l = ui.label(
            RichText::new(trf!("Not available with this key: {} (needs {})", m["feature"].as_str().unwrap_or_default(), perms.join(", ")))
                .size(11.0)
                .color(t.caution),
        );
        register(ui.ctx(), format!("label:immichMissing-{i}"), l.rect);
    }
}

fn perms_open_id(i: usize) -> egui::Id {
    egui::Id::new(("immich-perms", i))
}

/// Inside a row: "Key has all needed permissions." and the Show / Hide permissions toggle.
fn permissions_inline(ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    if a["permissions"].as_array().is_none() {
        ui.label(RichText::new(crate::i18n::tr("The key may not read its own permissions.")).size(11.0).color(t.text_dim));
        return;
    }
    if a["missingPermissions"].as_array().is_none_or(Vec::is_empty) {
        let l = ui.label(RichText::new(crate::i18n::tr("Key has all needed permissions.")).size(11.0).color(t.text_dim));
        register(ui.ctx(), format!("label:immichPermsOk-{i}"), l.rect);
    }
    let id = perms_open_id(i);
    let open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let label = if open { crate::i18n::tr("Hide permissions") } else { crate::i18n::tr("Show permissions") };
    let r = ui.add(egui::Label::new(RichText::new(label).size(11.0).color(t.accent).underline()).sense(egui::Sense::click()));
    register(ui.ctx(), format!("button:immichPerms-{i}"), r.rect);
    if r.clicked() {
        ui.data_mut(|d| d.insert_temp(id, !open));
    }
}

/// The key's whole permission list, when opened.
fn all_permissions(ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    let Some(all) = a["permissions"].as_array() else { return };
    if ui.data(|d| d.get_temp::<bool>(perms_open_id(i))).unwrap_or(false) {
        let all: Vec<&str> = all.iter().filter_map(Value::as_str).collect();
        let l = ui.label(RichText::new(all.join(", ")).size(11.0).color(t.text_dim));
        register(ui.ctx(), format!("label:immichPermsAll-{i}"), l.rect);
    }
}

fn account_card(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value) {
    let id = a["id"].as_str().unwrap_or_default().to_string();
    let r = egui::Frame::NONE.fill(t.inset).corner_radius(6.0).inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 3.0;
        // url · version · user on one line
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(a["url"].as_str().unwrap_or_default()).font(t.semibold(12.5)).color(t.text));
            ui.label(RichText::new(trf!("Immich {}", a["version"].as_str().unwrap_or("?"))).color(t.text_dim));
            ui.label(
                RichText::new(trf!("{} ({})", a["userName"].as_str().unwrap_or_default(), a["email"].as_str().unwrap_or_default()))
                    .color(t.text_label),
            );
        });
        if let Some(w) = a["warning"].as_str() {
            let l = ui.label(RichText::new(crate::i18n::tr(w)).size(11.0).color(t.caution));
            register(ui.ctx(), format!("label:immichInsecure-{i}"), l.rect);
        }
        missing_permissions(ui, t, i, a);
        // the server check, the link count and the key's permissions on one line
        let link = &a["link"];
        let status = if link["active"] == true {
            trf!("Linking… {} assets checked", link["seen"].as_u64().unwrap_or(0))
        } else {
            trf!("{} photos linked, {} probable", a["linked"].as_u64().unwrap_or(0), a["probable"].as_u64().unwrap_or(0))
        };
        ui.horizontal_wrapped(|ui| {
            let dot = |ui: &mut egui::Ui| ui.label(RichText::new("·").color(t.text_dim));
            if a["checking"] == true {
                ui.spinner();
                ui.label(RichText::new(crate::i18n::tr("Checking the server…")).color(t.text_dim));
                dot(ui);
            } else if a["server"]["ok"] == false {
                let l = ui.label(RichText::new(a["server"]["error"]["message"].as_str().unwrap_or("failed")).color(t.caution));
                register(ui.ctx(), format!("label:immichCheck-{i}"), l.rect);
                dot(ui);
            } else if a["server"]["ok"] == true {
                let l = ui.label(RichText::new(crate::i18n::tr("The server answers; the key works.")).color(t.text_label));
                register(ui.ctx(), format!("label:immichCheck-{i}"), l.rect);
                dot(ui);
            }
            ui.label(RichText::new(status).color(t.text_label));
            dot(ui);
            permissions_inline(ui, t, i, a);
        });
        all_permissions(ui, t, i, a);
        if let Some(e) = link["error"].as_str() {
            ui.label(RichText::new(e).color(t.caution));
        }
        // (wraps: on a narrow window the buttons take two rows instead of running off the edge)
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if text_button(ui, &format!("immichLink-{i}"), crate::i18n::tr("Link Now"), false).clicked() {
                let r = app.run("immich.link", json!({"account": id}));
                report(app, ui.ctx(), r);
            }
            if text_button(ui, &format!("immichLinkFull-{i}"), crate::i18n::tr("Full Link Pass"), false)
                .on_hover_text(crate::i18n::tr("Looks at every photo on the server again, not only those changed since the last pass."))
                .clicked()
            {
                let r = app.run("immich.link", json!({"account": id, "full": true}));
                report(app, ui.ctx(), r);
            }
            if text_button(ui, &format!("immichRecheck-{i}"), crate::i18n::tr("Check"), false).clicked() {
                let _ = app.session.execute("immich.status", &json!({"check": true, "background": true}));
            }
            if text_button(ui, &format!("immichLibraries-{i}"), crate::i18n::tr("External Libraries…"), false).clicked() {
                // the section opens now and fills in when the server has answered
                app.immich.maps_reload.insert(id.clone());
                app.immich.libraries.entry(id.clone()).or_insert_with(|| json!({"loading": true}));
                ask_bg(app, ui.ctx(), "immich.libraries", &id, json!({}));
            }
            if text_button(ui, &format!("immichSync-{i}"), crate::i18n::tr("Sync Now"), false).clicked() {
                let r = app.run("immich.sync", json!({"account": id, "background": true}));
                report(app, ui.ctx(), r);
                app.immich.sync_open.insert(id.clone());
                app.immich.sync_view.remove(&id);
            }
            let open = app.immich.sync_open.contains(&id);
            if text_button(ui, &format!("immichSyncPanel-{i}"), crate::i18n::tr("Sync…"), open).clicked() {
                if open {
                    app.immich.sync_open.remove(&id);
                } else {
                    app.immich.sync_open.insert(id.clone());
                    app.immich.sync_view.remove(&id);
                }
            }
            if text_button(ui, &format!("immichImport-{i}"), crate::i18n::tr("Import…"), false).clicked() {
                open_import(app, Some(id.clone()));
            }
            let sidecars_open = app.immich.sidecars_open.contains(&id);
            if text_button(ui, &format!("immichSidecars-{i}"), crate::i18n::tr("XMP Sidecars"), sidecars_open).clicked() {
                if sidecars_open {
                    app.immich.sidecars_open.remove(&id);
                } else {
                    app.immich.sidecars_open.insert(id.clone());
                }
            }
            let locked_open = app.immich.locked_open.contains(&id);
            if text_button(ui, &format!("immichLocked-{i}"), crate::i18n::tr("Locked Folders…"), locked_open).clicked() {
                if locked_open {
                    app.immich.locked_open.remove(&id);
                } else {
                    // (a fresh status every time the section opens)
                    app.immich.locked_open.insert(id.clone());
                    app.immich.locked_view.remove(&id);
                }
            }
            if text_button(ui, &format!("immichDisconnect-{i}"), crate::i18n::tr("Disconnect"), false).clicked() {
                app.immich.confirm = Some(Confirm {
                    account: id.clone(),
                    place: "card",
                    text: crate::i18n::tr(
                        "Disconnect this server? Its stored API key is removed. The photos and their Immich links stay in the catalog.",
                    )
                    .to_string(),
                    button: "Disconnect",
                    command: "immich.disconnect",
                    params: json!({"account": id}),
                });
            }
        });
        confirm_box(app, ui, t, i, &id, "card");
        if app.immich.libraries.contains_key(&id) {
            extlib(app, ui, t, &id);
        }
        if app.immich.sidecars_open.contains(&id) {
            sidecar_section(app, ui, t, i, &id);
        }
        if app.immich.locked_open.contains(&id) {
            locked_section(app, ui, t, i, &id);
        }
        if app.immich.sync_open.contains(&id) {
            sync_section(app, ui, t, i, &id);
        }
    });
    register(ui.ctx(), format!("immichAccount:{i}"), r.response.rect);
    ui.add_space(4.0);
}

/// IMM-SIDECARS: push the catalog's metadata to Immich (whole sidecars for external-library
/// folders, the supported fields for managed assets) and pull the sidecars Immich stores back.
fn sidecar_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, id: &str) {
    ui.separator();
    let state = app.immich.last["sidecars"].clone();
    let active = state["active"] == true;
    let direction = state["direction"].as_str().unwrap_or_default().to_string();
    let last = state["last"].clone();
    ui.label(
        RichText::new(crate::i18n::tr("Immich keeps an XMP sidecar for every asset: push yours into it, or pull Immich's into the catalog"))
            .size(11.5)
            .color(t.text_dim),
    );
    ui.checkbox(
        &mut app.immich.full_sidecars,
        crate::i18n::tr("Replace managed sidecars with the whole XMP (creates a temporary asset)"),
    )
    .on_hover_text(crate::i18n::tr(
        "Read the sidecar Immich holds, merge the catalog's metadata and develop settings into it, and copy it back; Immich has no direct write endpoint, so a tiny temporary asset is uploaded and removed",
    ));
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let push = crate::i18n::tr("Push XMP Sidecars");
        if text_button(ui, &format!("immichPushXmp-{i}"), push, false)
            .on_hover_text(crate::i18n::tr(
                "Write the whole sidecar for external-library photos and send the rating, description, date, place and tags of managed photos to Immich",
            ))
            .clicked()
        {
            let full = app.immich.full_sidecars;
            match app.run("immich.pushSidecars", json!({"account": id, "background": true, "full": full})) {
                Ok(r) if r["started"] == true => app.toast(ui.ctx(), crate::i18n::tr("Pushing XMP sidecars to Immich…")),
                Ok(r) if r["ok"] == false => app.toast(ui.ctx(), r["error"]["message"].as_str().unwrap_or("Immich")),
                Ok(_) => {}
                Err(e) => app.toast(ui.ctx(), e.to_string()),
            }
        }
        let pull = crate::i18n::tr("Pull XMP Sidecars");
        if text_button(ui, &format!("immichPullXmp-{i}"), pull, false)
            .on_hover_text(crate::i18n::tr(
                "Read the XMP sidecars Immich stores for the linked photos (metadata and develop settings) into the catalog",
            ))
            .clicked()
        {
            match app.run("immich.pullSidecars", json!({"account": id, "background": true})) {
                Ok(r) if r["started"] == true => app.toast(ui.ctx(), crate::i18n::tr("Pulling XMP sidecars from Immich…")),
                Ok(r) if r["ok"] == false => app.toast(ui.ctx(), r["error"]["message"].as_str().unwrap_or("Immich")),
                Ok(_) => {}
                Err(e) => app.toast(ui.ctx(), e.to_string()),
            }
        }
    });
    if active {
        let line = if direction == "pull" {
            crate::i18n::tr("Pulling XMP sidecars from Immich…")
        } else {
            crate::i18n::tr("Pushing XMP sidecars to Immich…")
        };
        ui.label(RichText::new(line).color(t.text_dim));
    } else if last.is_object() {
        let n = |k: &str| last[k].as_u64().unwrap_or(0);
        let line = if last["direction"] == "pull" {
            trf!("Pulled {} XMP sidecars ({} written beside the originals, {} skipped)", n("pulled"), n("written"), n("skipped"))
        } else {
            trf!("Pushed {} XMP sidecars ({} written beside the originals, {} skipped)", n("pushed"), n("written"), n("skipped"))
        };
        ui.label(RichText::new(line).color(if n("failed") > 0 { t.caution } else { t.text_label }));
    }
}

/// Immich's locked folders: this session's elevation state and the *Unlock…* dialog. While the
/// session is locked the server leaves locked-folder assets out of its listings, so link, import
/// and sync never see them; unlocked, they are listed like any other asset until the server
/// locks the session again.
fn locked_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, id: &str) {
    ui.separator();
    // the status is asked for on a worker; the section shows "checking" until it is here
    if !app.immich.locked_view.contains_key(id) && !app.immich.asked.contains(&format!("immich.authStatus:{id}")) {
        ask_bg(app, ui.ctx(), "immich.authStatus", id, json!({}));
    }
    let st = app.immich.locked_view.get(id).cloned().unwrap_or_default();
    ui.label(RichText::new(crate::i18n::tr("Locked folders")).font(t.semibold(12.0)).color(t.text));
    if st["ok"] == false {
        let l = ui.label(RichText::new(st["error"]["message"].as_str().unwrap_or("failed")).size(11.5).color(t.caution));
        register(ui.ctx(), format!("label:immichLockedStatus-{i}"), l.rect);
        return;
    }
    let line = if st.is_null() {
        crate::i18n::tr("Checking the server…").to_string()
    } else if st["isElevated"] == true {
        match st["pinExpiresAt"].as_str().or_else(|| st["expiresAt"].as_str()) {
            Some(until) => trf!("Unlocked until {}", until.chars().take(16).collect::<String>().replace('T', " ")),
            None => crate::i18n::tr("Unlocked: locked-folder photos are visible to link, import and sync.").to_string(),
        }
    } else if st["pinCode"] == true || st["password"] == true {
        crate::i18n::tr("Locked: the server hides locked-folder photos from link, import and sync.").to_string()
    } else {
        crate::i18n::tr("The account has no PIN or password, so there is nothing to unlock.").to_string()
    };
    let l = ui.label(RichText::new(line).size(11.5).color(t.text_label));
    register(ui.ctx(), format!("label:immichLockedStatus-{i}"), l.rect);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let open = app.immich.unlock_for.as_deref() == Some(id);
        if text_button(ui, &format!("immichLockedUnlock-{i}"), crate::i18n::tr("Unlock…"), open).clicked() {
            app.immich.unlock_for = if open { None } else { Some(id.to_string()) };
            app.immich.unlock_secret.clear();
            app.immich.unlock_error = None;
        }
    });
    if app.immich.unlock_for.as_deref() == Some(id) {
        locked_unlock_box(app, ui, t, i, id);
    }
}

/// The unlock box: the Immich account's PIN or password, sent once (`immich.unlockSession`,
/// never journaled) and forgotten. A PIN is 4–6 digits, everything else goes as the password.
/// Inline in the section (the Settings screen is the modal layer while it is open).
fn locked_unlock_box(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, id: &str) {
    let (mut go, mut submit, mut close) = (false, false, false);
    let frame = egui::Frame::NONE.stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.5))).corner_radius(6.0).inner_margin(8);
    let r = frame.show(ui, |ui| {
        ui.label(RichText::new(crate::i18n::tr("Unlock Locked Folders")).font(t.semibold(12.5)).color(t.text));
        ui.label(
            RichText::new(crate::i18n::tr(
                "Immich hides the photos of its locked folders until this session is unlocked with the account's PIN or password.",
            ))
            .size(11.5)
            .color(t.text_label),
        );
        ui.label(
            RichText::new(crate::i18n::tr(
                "4–6 digits are sent as the PIN code, anything else as the password. The secret is used once and is not stored or journaled; the server locks the session again later.",
            ))
            .size(11.5)
            .color(t.text_label),
        );
        let field = crate::text_field::TextField::singleline("field:immichUnlockSecret", &mut app.immich.unlock_secret)
            .password(true)
            .width(240.0)
            .label(crate::i18n::tr("PIN or password"))
            .hint(crate::i18n::tr("PIN or password"));
        let fr = field.show(ui);
        submit |= fr.ending == Some(crate::text_field::Ending::Return);
        close |= fr.ending == Some(crate::text_field::Ending::Escape);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            go = crate::button::Button::new("Unlock")
                .kind(crate::button::Kind::Primary)
                .widget("button:immichUnlockGo")
                .show(ui)
                .clicked();
            if text_button(ui, "immichUnlockCancel", crate::i18n::tr("Cancel"), false).clicked() {
                close = true;
            }
        });
        if let Some(e) = &app.immich.unlock_error {
            let l = ui.label(RichText::new(e).size(11.5).color(t.caution));
            register(ui.ctx(), format!("label:immichUnlockError-{i}"), l.rect);
        }
    });
    register(ui.ctx(), format!("immichUnlockBox-{i}"), r.response.rect);
    // one unlock at a time; the server answers on a worker ([`answer_in`] closes the box, or
    // shows the refusal in it)
    let pending = app.immich.asked.contains(&format!("immich.unlockSession:{id}"));
    if pending {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
    if (go || submit) && !pending {
        // the typed secret is gone from the field at once, whatever the server says
        let secret = std::mem::take(&mut app.immich.unlock_secret);
        app.immich.unlock_error = None;
        ask_bg(app, ui.ctx(), "immich.unlockSession", id, json!({"secret": secret}));
    }
    if close {
        app.immich.unlock_for = None;
        app.immich.unlock_secret.clear();
        app.immich.unlock_error = None;
    }
}

/// The external-library helper: libraries, the path mapping table, which catalog folders are
/// covered, and Write XMP.
fn extlib(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, id: &str) {
    let Some(v) = app.immich.libraries.get(id).cloned() else { return };
    ui.separator();
    ui.label(RichText::new(crate::i18n::tr("External libraries")).font(t.semibold(12.0)).color(t.text));
    if v["loading"] == true {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label(RichText::new(crate::i18n::tr("Checking the server…")).color(t.text_dim));
        });
        return;
    }
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
        ui.label(RichText::new(format!("{}: {}", l["name"].as_str().unwrap_or_default(), paths.join(", "))).color(t.text_label));
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
            let r = ui.add(egui::TextEdit::singleline(c).hint_text("/mnt/photos").desired_width(170.0));
            crate::access::label(&r, "Immich path");
            ui.label("↔");
            let r = ui.add(egui::TextEdit::singleline(l).hint_text("/home/me/Photos").desired_width(220.0));
            crate::access::label(&r, "Local path");
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
            let r = app.run("immich.setPathMaps", json!({"account": id, "pathMaps": maps}));
            if report(app, ui.ctx(), r) {
                // which folders the libraries cover now (the list stays up meanwhile)
                ask_bg(app, ui.ctx(), "immich.libraries", id, json!({}));
            }
        }
        if text_button(ui, "immichWriteXmp", crate::i18n::tr("Write XMP Sidecars"), false)
            .on_hover_text(crate::i18n::tr("Immich reads ratings, descriptions and keywords from the sidecars on its next library scan"))
            .clicked()
        {
            // (a finished run says what it did: the pump's toast)
            match app.session.execute("immich.pushSidecars", &json!({"account": id, "externalOnly": true, "background": true})) {
                Ok(r) if r["started"] == true => app.toast(ui.ctx(), crate::i18n::tr("Pushing XMP sidecars to Immich…")),
                Ok(r) if r["ok"] == false => app.toast(ui.ctx(), r["error"]["message"].as_str().unwrap_or("Immich")),
                Ok(r) => app.toast(ui.ctx(), r["reason"].as_str().unwrap_or("Immich")),
                Err(e) => app.toast(ui.ctx(), e.to_string()),
            }
        }
        if text_button(ui, "immichRescan", crate::i18n::tr("Rescan in Immich"), false)
            .on_hover_text(crate::i18n::tr("Ask Immich to rescan the external libraries now (needs an admin key)"))
            .clicked()
        {
            ask_bg(app, ui.ctx(), "immich.scanLibraries", id, json!({}));
        }
    });
    app.immich.maps.insert(id.to_string(), rows);
    let cov = v["coverage"].as_array().cloned().unwrap_or_default();
    let covered = cov.iter().filter(|c| c["library"].is_string()).count();
    ui.label(RichText::new(trf!("{} of {} catalog folders are inside an external library", covered, cov.len())).color(t.text_label));
    egui::ScrollArea::vertical().id_salt(("immich-coverage", id)).max_height(120.0).show(ui, |ui| {
        for c in &cov {
            let (mark, color) = if c["library"].is_string() { ("✓", t.text) } else { ("–", t.text_dim) };
            let lib = c["libraryName"].as_str().map(|n| format!("  → {n}")).unwrap_or_default();
            ui.label(RichText::new(format!("{mark} {}{lib}", c["folder"].as_str().unwrap_or_default())).size(11.0).color(color));
        }
    });
}

// ---------------------------------------------------------------------- Import from Immich

/// UI commands: `file.importImmich` opens the Import dialog on its Immich source.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    match id {
        "immich.connections" => Some(app.run("app.settings", json!({"tab": "connections"}))),
        "file.importImmich" => {
            open_import(app, p.get("account").and_then(Value::as_str).map(str::to_string));
            Some(Ok(json!({"open": true, "source": "immich", "account": app.immich.account})))
        }
        _ => None,
    }
}

/// Open the Import dialog with Immich as its source.
fn open_import(app: &mut LccApp, account: Option<String>) {
    let opts = crate::import::ImportDialog { immich: true, ..Default::default() };
    app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(opts) });
    open_source(app, account);
}

/// Switch the Import dialog's Immich source to `account` (else the first one) and list its timeline.
pub fn open_source(app: &mut LccApp, account: Option<String>) {
    let accounts: Vec<String> = app.session.immich_accounts().map(|a| a.immich.iter().map(|x| x.id.clone()).collect()).unwrap_or_default();
    let ui = &mut app.immich;
    ui.account = account
        .filter(|a| accounts.contains(a))
        .or_else(|| ui.account.clone().filter(|a| accounts.contains(a)))
        .or_else(|| accounts.first().cloned());
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

/// How many Immich assets are selected (the dialog's Import button counts them).
pub fn selected_count(app: &LccApp) -> usize {
    app.immich.selected.len()
}

fn set_source(app: &mut LccApp, source: &str, target: Option<(String, String)>) {
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
fn list(app: &mut LccApp) {
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
fn browse(c: &lcc_immich::Client, p: &Value, sums: &HashSet<String>, linked: &HashSet<String>) -> Value {
    use lcc_immich::types::MetadataSearch;
    let fail = |e: lcc_immich::ImmichError| json!({"ok": false, "error": {"message": e.to_string(), "retryable": e.retryable()}});
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
fn load_thumbs(app: &mut LccApp, ids: Vec<String>) {
    let Some(account) = app.immich.account.clone() else { return };
    let Ok((_, client)) = app.session.immich_client(&account) else { return };
    let out = app.immich.thumb_rx.get_or_insert_with(Default::default).clone();
    for i in &ids {
        app.immich.thumbs.insert(i.clone(), Thumb::Loading);
    }
    std::thread::spawn(move || {
        for id in ids {
            let img = client.thumbnail(&id, "thumbnail").ok().and_then(|b| {
                let d = lcc_codecs::decode(&b, lcc_codecs::DecodeOptions { max_size: Some((256, 256)), ..Default::default() }).ok()?;
                let rgba = d.image.to_srgb8();
                Some(egui::ColorImage::from_rgba_unmultiplied([rgba.width, rgba.height], &rgba.as_bytes()))
            });
            out.lock().unwrap_or_else(PoisonError::into_inner).push((id, img));
        }
    });
}

/// Take in finished listings and decoded thumbnails.
fn poll(app: &mut LccApp, ctx: &egui::Context) {
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
}

/// Start importing the selected assets (the Import dialog's Import button).
pub fn start_import(app: &mut LccApp) -> Result<Value, String> {
    if app.immich.selected.is_empty() {
        return Err(crate::i18n::tr("Select photos to import").to_string());
    }
    if !app.immich.link_only && app.immich.destination.trim().is_empty() {
        return Err(crate::i18n::tr("Choose a destination folder").to_string());
    }
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
    let r = app.run("immich.import", params)?;
    app.immich.selected.clear();
    Ok(r)
}

/// The Import dialog's Immich source: accounts, timeline / favourites / albums / people as
/// thumbnails loaded from the server, Copy or Link only.
pub fn import_source(app: &mut LccApp, ui: &mut egui::Ui) {
    poll(app, ui.ctx());
    let t = Tokens::get(ui.ctx());
    let st = cred_status(app, ui.ctx());
    if st["needsPassphrase"] == true {
        unlock_box(app, ui, &t, &st);
        if app.immich.list_error.is_some() && st["unlocking"] != true {
            // once unlocked, list again
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        return;
    }
    let accounts: Vec<(String, String)> = app
        .session
        .immich_accounts()
        .map(|a| a.immich.iter().map(|x| (x.id.clone(), format!("{} ({})", x.url, x.user_name))).collect())
        .unwrap_or_default();
    if accounts.is_empty() {
        ui.label(RichText::new(crate::i18n::tr("No Immich server is connected.")).color(t.text_label));
        if text_button(ui, "immichOpenConnections", crate::i18n::tr("Connect a Server…"), false).clicked() {
            let _ = app.run("app.settings", json!({"tab": "connections"}));
        }
        return;
    }
    if app.immich.account.is_none()
        || (app.immich.list_error.is_some()
            && app.immich.assets.is_empty()
            && app.immich.listing.is_none()
            && st["active"].is_string()
            && app.immich.list_error.as_deref().is_some_and(|e| e.contains("passphrase")))
    {
        // first open, or the key store was just unlocked: list
        open_source(app, None);
    }
    if accounts.len() > 1 {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(crate::i18n::tr("Server")).color(t.text_label));
            for (i, (id, label)) in accounts.iter().enumerate() {
                let on = app.immich.account.as_deref() == Some(id);
                if text_button(ui, &format!("immichAccount-{i}"), label, on).clicked() && !on {
                    open_source(app, Some(id.clone()));
                }
            }
        });
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
            let l = ui.label(RichText::new(e).color(t.caution));
            register(ui.ctx(), "label:immichListError", l.rect);
            if text_button(ui, "immichListRetry", crate::i18n::tr("Retry"), false).clicked() {
                app.immich.list_error = None;
                list(app);
            }
        });
    }
    let grid_h = (ui.available_height() - 110.0).max(240.0);
    let in_groups = matches!(app.immich.source.as_str(), "albums" | "people") && app.immich.target.is_none();
    egui::ScrollArea::vertical().id_salt("immich-import-grid").max_height(grid_h).min_scrolled_height(grid_h).auto_shrink([false, false]).show(
        ui,
        |ui| {
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
        },
    );
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
            crate::access::label(&r, "Destination");
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let n = app.immich.selected.len();
        ui.label(RichText::new(trf!("{} selected", n)).color(t.text_label));
        if text_button(ui, "immichSelectAll", crate::i18n::tr("Select All"), false).clicked() {
            let all: Vec<String> =
                app.immich.assets.iter().filter(|a| a["inCatalog"] != true).filter_map(|a| a["id"].as_str().map(str::to_string)).collect();
            app.immich.selected.extend(all);
        }
        if text_button(ui, "immichSelectNone", crate::i18n::tr("Select None"), false).clicked() {
            app.immich.selected.clear();
        }
        if app.immich.last["import"]["active"] == true {
            ui.spinner();
            ui.label(RichText::new(crate::i18n::tr("An Immich import is running…")).color(t.text_dim));
        }
    });
}

fn asset_cell(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, a: &Value, size: f32) {
    let id = a["id"].as_str().unwrap_or_default().to_string();
    let (r, resp) = ui.allocate_exact_size(vec2(size - 4.0, size - 4.0), Sense::click());
    register(ui.ctx(), format!("immichAsset:{i}"), r);
    let have = a["inCatalog"] == true;
    let selected = app.immich.selected.contains(&id);
    let asset_name = a["originalFileName"].as_str().or_else(|| a["name"].as_str()).unwrap_or(&id).to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, selected, &asset_name));
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
        let g = p.layout_no_wrap(crate::i18n::tr("In catalog").into(), t.semibold(9.0), Color32::WHITE);
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

/// The fields of IMM-SYNC, as `immich.setSync` names them, with their labels.
const SYNC_FIELDS: &[(&str, &str)] = &[
    ("rating", "Star rating ↔ rating"),
    ("favorite", "Pick ↔ favourite"),
    ("archived", "Reject ↔ archived"),
    ("description", "Caption ↔ description"),
    ("keywords", "Keywords ↔ tags"),
    ("location", "GPS ↔ location"),
    ("captured", "Capture time ↔ date"),
];
const DIRECTIONS: &[(&str, &str)] = &[("twoWay", "Two-way"), ("toImmich", "To Immich"), ("fromImmich", "From Immich"), ("off", "Off")];
const POLICIES: &[(&str, &str)] = &[("newest", "Newest wins"), ("catalog", "Catalog wins"), ("immich", "Immich wins"), ("ask", "Ask")];

/// The last album run as one short line (and whether it failed); None before any run.
fn album_summary(v: &Value) -> Option<(String, bool)> {
    if v.is_null() {
        return None;
    }
    if v["ok"] == false {
        let msg = v["error"]["message"].as_str().or_else(|| v["errors"][0].as_str()).unwrap_or("failed");
        return Some((trf!("Albums: {}", msg), true));
    }
    let n = |k: &str| v[k].as_u64().unwrap_or(0);
    Some((
        trf!(
            "Albums: {} linked, {} created, {} renamed, {} changed, {} deleted",
            n("albums"),
            n("created"),
            n("renamed"),
            n("membership"),
            n("deleted")
        ),
        false,
    ))
}

fn label_of(list: &[(&str, &'static str)], v: &str) -> &'static str {
    list.iter().find(|(k, _)| *k == v).map(|(_, l)| *l).unwrap_or("?")
}

/// What Preview Changes found (the last dry run): one summary line, then a line per photo saying
/// which fields would go which way. In the Sync section, where the button is.
fn preview_list(app: &LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, pv: &Value) {
    if !pv.is_object() {
        return;
    }
    if pv["ok"] == false {
        let msg = pv["error"]["message"].as_str().or_else(|| pv["errors"][0].as_str()).unwrap_or("failed");
        let l = ui.label(RichText::new(msg).size(11.5).color(t.caution));
        register(ui.ctx(), format!("label:immichPreview-{i}"), l.rect);
        return;
    }
    let n = |k: &str| pv[k].as_u64().unwrap_or(0);
    let line = trf!("Preview: {} checked, {} from Immich, {} to Immich, {} conflicts", n("checked"), n("pulled"), n("pushed"), n("conflicts"));
    let l = ui.label(RichText::new(line).size(11.5).color(t.text_label));
    register(ui.ctx(), format!("label:immichPreview-{i}"), l.rect);
    let changes = pv["changes"].as_array().cloned().unwrap_or_default();
    if changes.is_empty() {
        ui.label(RichText::new(crate::i18n::tr("Nothing would change.")).size(11.5).color(t.text_dim));
        return;
    }
    let fields = |v: &Value| -> String {
        v.as_object().into_iter().flatten().map(|(f, _)| crate::i18n::tr(label_of(SYNC_FIELDS, f))).collect::<Vec<_>>().join(", ")
    };
    egui::ScrollArea::vertical().id_salt(("immich-preview", i)).max_height(140.0).show(ui, |ui| {
        for c in &changes {
            let name = c["photo"]
                .as_u64()
                .and_then(|p| app.session.catalog.photo(lcc_catalog::PhotoId(p)))
                .map(|p| p.file_name.clone())
                .unwrap_or_default();
            let mut parts = vec![name];
            for (key, label) in [("toImmich", "To Immich"), ("toCatalog", "From Immich")] {
                let f = fields(&c[key]);
                if !f.is_empty() {
                    parts.push(format!("{}: {f}", crate::i18n::tr(label)));
                }
            }
            let conflicts = c["conflicts"].as_array().map(Vec::len).unwrap_or(0);
            if conflicts > 0 {
                parts.push(trf!("Sync conflicts ({})", conflicts));
            }
            ui.label(RichText::new(parts.join(" · ")).size(11.0).color(t.text_label));
        }
    });
}

/// Settings → Connections → an account → Sync…: status and activity, per-field rules, the
/// schedule, Sync Conflicts (both values, keep one) and the deletion queue.
fn sync_section(app: &mut LccApp, ui: &mut egui::Ui, t: &Tokens, i: usize, id: &str) {
    let now = ui.input(|x| x.time);
    let stale = match app.immich.sync_view.get(id) {
        None => true,
        Some((st, _, at)) => (st["running"] == true || st["albumsRunning"] == true) && now - at > 0.5,
    };
    if stale {
        let st = app.session.execute("immich.syncStatus", &json!({"account": id, "log": 1})).unwrap_or_default();
        let cf = app.session.execute("immich.syncConflicts", &json!({"account": id})).unwrap_or_default();
        app.immich.sync_view.insert(id.to_string(), (st, cf, now));
    }
    let Some((st, cf, _)) = app.immich.sync_view.get(id).cloned() else { return };
    let running = st["running"] == true || st["albumsRunning"] == true;
    if running {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
    }
    let mut changed = false;
    ui.separator();
    // heading and the one status line (the latest run only)
    let last = &st["last"];
    let latest = st["log"].as_array().and_then(|l| l.first()).cloned().unwrap_or(Value::Null);
    let line = if running {
        crate::i18n::tr("Syncing…").to_string()
    } else if last["ok"] == false {
        last["error"]["message"].as_str().unwrap_or("failed").to_string()
    } else if !last.is_null() {
        trf!(
            "Last sync: {} checked, {} from Immich, {} to Immich, {} conflicts",
            last["checked"].as_u64().unwrap_or(0),
            last["pulled"].as_u64().unwrap_or(0),
            last["pushed"].as_u64().unwrap_or(0),
            last["conflicts"].as_u64().unwrap_or(0)
        )
    } else if !latest.is_null() {
        trf!(
            "Last sync {}: {} from Immich, {} to Immich, {} conflicts",
            latest["finished"].as_str().unwrap_or_default().chars().take(16).collect::<String>().replace('T', " "),
            latest["pulled"].as_u64().unwrap_or(0),
            latest["pushed"].as_u64().unwrap_or(0),
            latest["conflicts"].as_u64().unwrap_or(0)
        )
    } else {
        crate::i18n::tr("Not synced yet.").to_string()
    };
    let albums_line = album_summary(&st["albumsLast"]);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(crate::i18n::tr("Metadata sync")).font(t.semibold(12.0)).color(t.text));
        let l = ui.label(RichText::new(line).size(11.5).color(if last["ok"] == false { t.caution } else { t.text_label }));
        register(ui.ctx(), format!("label:immichSyncStatus-{i}"), l.rect);
        if let Some((s, bad)) = albums_line {
            let l = ui.label(RichText::new(s).size(11.5).color(if bad { t.caution } else { t.text_label }));
            register(ui.ctx(), format!("label:immichAlbumStatus-{i}"), l.rect);
        }
        // collections the last album run kept although their Immich album was deleted
        let kept: Vec<&str> = st["albumsLast"]["kept"].as_array().into_iter().flatten().filter_map(|k| k["name"].as_str()).collect();
        if !kept.is_empty() {
            let l = ui
                .label(RichText::new(trf!("Kept {} collections that hold photos Immich doesn't have", kept.len())).size(11.5).color(t.caution))
                .on_hover_text(kept.join(", "));
            register(ui.ctx(), format!("label:immichAlbumKept-{i}"), l.rect);
        }
    });
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if text_button(ui, &format!("immichSyncDry-{i}"), crate::i18n::tr("Preview Changes"), false).clicked() {
            // a dry run on a worker; its changes are listed below when it is back
            let r = app.session.execute("immich.sync", &json!({"account": id, "dryRun": true, "background": true})).map_err(|e| e.to_string());
            report(app, ui.ctx(), r);
            changed = true;
        }
        if text_button(ui, &format!("immichSyncAlbums-{i}"), crate::i18n::tr("Sync Albums"), false).clicked() {
            let r = app.run("immich.syncAlbums", json!({"account": id, "background": true}));
            report(app, ui.ctx(), r);
            changed = true;
        }
        if text_button(ui, &format!("immichPeople-{i}"), crate::i18n::tr("Import People"), false).clicked() {
            ask_bg(app, ui.ctx(), "immich.importPeople", id, json!({}));
        }
        if text_button(ui, &format!("immichPushPeople-{i}"), crate::i18n::tr("Send Names"), false).clicked() {
            ask_bg(app, ui.ctx(), "immich.pushPeople", id, json!({}));
        }
    });
    preview_list(app, ui, t, i, &st["preview"]);
    // rules
    let cfg = &st["config"];
    let mut patch = serde_json::Map::new();
    let mut album_patch: Option<Value> = None;
    egui::Grid::new(("immich-sync-rules", i)).num_columns(3).spacing([8.0, 2.0]).show(ui, |ui| {
        for (row, rule) in st["rules"].as_array().into_iter().flatten().enumerate() {
            let field = rule["field"].as_str().unwrap_or_default();
            let dir = rule["rule"]["direction"].as_str().unwrap_or("twoWay").to_string();
            let pol = rule["rule"]["policy"].as_str().unwrap_or("newest").to_string();
            ui.label(RichText::new(crate::i18n::tr(label_of(SYNC_FIELDS, field))).color(t.text_label));
            let (mut d2, mut p2) = (dir.clone(), pol.clone());
            egui::ComboBox::from_id_salt(("immich-dir", i, row)).width(110.0).selected_text(crate::i18n::tr(label_of(DIRECTIONS, &dir))).show_ui(
                ui,
                |ui| {
                    for (k, l) in DIRECTIONS {
                        ui.selectable_value(&mut d2, k.to_string(), crate::i18n::tr(l));
                    }
                },
            );
            egui::ComboBox::from_id_salt(("immich-pol", i, row)).width(110.0).selected_text(crate::i18n::tr(label_of(POLICIES, &pol))).show_ui(
                ui,
                |ui| {
                    for (k, l) in POLICIES {
                        ui.selectable_value(&mut p2, k.to_string(), crate::i18n::tr(l));
                    }
                },
            );
            ui.end_row();
            if d2 != dir || p2 != pol {
                patch.insert(field.to_string(), json!({"direction": d2, "policy": p2}));
            }
        }
        // collections ↔ albums: off = only albums linked by hand
        let ar = &st["albumRule"];
        let (dir, pol) = (ar["direction"].as_str().unwrap_or("off").to_string(), ar["policy"].as_str().unwrap_or("ask").to_string());
        let (mut d2, mut p2) = (dir.clone(), pol.clone());
        ui.label(RichText::new(crate::i18n::tr("Collections ↔ albums")).color(t.text_label)).on_hover_text(crate::i18n::tr(
            "Catalog collections map to the Immich albums of the same name: creates, renames, photos added or removed, and deletes. Deleting a collection or an album never deletes photos. Off: only collections linked by hand.",
        ));
        egui::ComboBox::from_id_salt(("immich-dir-albums", i)).width(110.0).selected_text(crate::i18n::tr(label_of(DIRECTIONS, &dir))).show_ui(
            ui,
            |ui| {
                for (k, l) in DIRECTIONS {
                    ui.selectable_value(&mut d2, k.to_string(), crate::i18n::tr(l));
                }
            },
        );
        egui::ComboBox::from_id_salt(("immich-pol-albums", i)).width(110.0).selected_text(crate::i18n::tr(label_of(POLICIES, &pol))).show_ui(
            ui,
            |ui| {
                for (k, l) in POLICIES {
                    ui.selectable_value(&mut p2, k.to_string(), crate::i18n::tr(l));
                }
            },
        );
        ui.end_row();
        if d2 != dir || p2 != pol {
            album_patch = Some(json!({"direction": d2, "policy": p2}));
        }
    });
    let mut cfg_patch = serde_json::Map::new();
    if !patch.is_empty() {
        cfg_patch.insert("rules".into(), Value::Object(patch));
    }
    if let Some(a) = album_patch {
        cfg_patch.insert("albums".into(), a);
    }
    ui.horizontal(|ui| {
        let mut fav = cfg["favorite"].as_str().unwrap_or("pick").to_string();
        ui.label(RichText::new(crate::i18n::tr("Favourite means")).color(t.text_label));
        let before = fav.clone();
        egui::ComboBox::from_id_salt(("immich-fav", i))
            .width(90.0)
            .selected_text(crate::i18n::tr(if fav == "fiveStars" { "5 stars" } else { "Pick" }))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut fav, "pick".to_string(), crate::i18n::tr("Pick"));
                ui.selectable_value(&mut fav, "fiveStars".to_string(), crate::i18n::tr("5 stars"));
            });
        if fav != before {
            cfg_patch.insert("favorite".into(), json!(fav));
        }
        let mut minutes = cfg["intervalMinutes"].as_u64().unwrap_or(0) as u32;
        ui.label(RichText::new(crate::i18n::tr("Every (minutes, 0 = manual)")).color(t.text_label));
        if ui.add(egui::DragValue::new(&mut minutes).range(0..=1440)).changed() {
            cfg_patch.insert("intervalMinutes".into(), json!(minutes));
        }
    });
    if !cfg_patch.is_empty() {
        let _ = app.run("immich.setSync", json!({"account": id, "config": Value::Object(cfg_patch)}));
        changed = true;
    }
    // conflicts
    let conflicts = cf["conflicts"].as_array().cloned().unwrap_or_default();
    if !conflicts.is_empty() {
        ui.label(RichText::new(trf!("Sync conflicts ({})", conflicts.len())).font(t.semibold(12.0)).color(t.caution));
        egui::ScrollArea::vertical().id_salt(("immich-conflicts", i)).max_height(160.0).show(ui, |ui| {
            for (n, c) in conflicts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let field = c["field"].as_str().unwrap_or_default();
                    ui.label(
                        RichText::new(format!("{} · {}", c["fileName"].as_str().unwrap_or_default(), crate::i18n::tr(label_of(SYNC_FIELDS, field))))
                            .color(t.text),
                    );
                    ui.label(RichText::new(trf!("catalog: {}", c["catalog"])).color(t.text_label));
                    ui.label(RichText::new(trf!("Immich: {}", c["immich"])).color(t.text_label));
                    for (side, label) in [("catalog", "Keep Catalog"), ("immich", "Keep Immich")] {
                        if text_button(ui, &format!("immichConflict-{i}-{n}-{side}"), crate::i18n::tr(label), c["decided"] == side).clicked() {
                            let _ = app.run("immich.resolveConflict", json!({"account": id, "assetId": c["assetId"], "field": field, "keep": side}));
                            changed = true;
                        }
                    }
                });
            }
        });
        ui.horizontal(|ui| {
            if text_button(ui, &format!("immichConflictsApply-{i}"), crate::i18n::tr("Apply Decisions"), false).clicked() {
                let _ = app.run("immich.sync", json!({"account": id, "background": true}));
                changed = true;
            }
        });
    }
    // deletions
    let dels = st["deletions"].as_array().cloned().unwrap_or_default();
    if !dels.is_empty() {
        ui.label(RichText::new(trf!("Deleted on one side ({})", dels.len())).font(t.semibold(12.0)).color(t.caution));
        for (n, d) in dels.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let what = if d["side"] == "immich" { crate::i18n::tr("in Immich's trash") } else { crate::i18n::tr("deleted in the catalog") };
                ui.label(RichText::new(format!("{}: {what}", d["fileName"].as_str().unwrap_or_default())).color(t.text));
                let here = d["side"] == "immich";
                let apply = if here { "Delete Here Too" } else { "Trash in Immich" };
                if text_button(ui, &format!("immichDeletion-{i}-{n}-apply"), crate::i18n::tr(apply), false).clicked() {
                    let name = d["fileName"].as_str().unwrap_or_default();
                    app.immich.confirm = Some(Confirm {
                        account: id.to_string(),
                        place: "sync",
                        text: if here { trf!("Move “{}” to Recently Deleted?", name) } else { trf!("Move “{}” to Immich's trash?", name) },
                        button: apply,
                        command: "immich.resolveDeletion",
                        params: json!({"account": id, "assets": [d["asset"]], "action": "apply"}),
                    });
                }
                if text_button(ui, &format!("immichDeletion-{i}-{n}-keep"), crate::i18n::tr("Keep"), false).clicked() {
                    let r = app.run("immich.resolveDeletion", json!({"account": id, "assets": [d["asset"]], "action": "keep"}));
                    report(app, ui.ctx(), r);
                    changed = true;
                }
            });
        }
    }
    // albums deleted on one side, waiting for the user
    let adels = st["albumDeletions"].as_array().cloned().unwrap_or_default();
    if !adels.is_empty() {
        ui.label(RichText::new(trf!("Albums deleted on one side ({})", adels.len())).font(t.semibold(12.0)).color(t.caution));
        for (n, d) in adels.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let in_catalog = d["deletedOn"] == "catalog";
                let what = if in_catalog { crate::i18n::tr("deleted in the catalog") } else { crate::i18n::tr("deleted in Immich") };
                ui.label(RichText::new(format!("{}: {what}", d["name"].as_str().unwrap_or_default())).color(t.text));
                let apply = if in_catalog { "Delete in Immich Too" } else { "Delete Here Too" };
                if text_button(ui, &format!("immichAlbumDeletion-{i}-{n}-apply"), crate::i18n::tr(apply), false)
                    .on_hover_text(crate::i18n::tr("Deletes the album only; its photos stay."))
                    .clicked()
                {
                    let name = d["name"].as_str().unwrap_or_default();
                    app.immich.confirm = Some(Confirm {
                        account: id.to_string(),
                        place: "sync",
                        text: if in_catalog {
                            trf!("Delete the album “{}” in Immich? Its photos stay.", name)
                        } else {
                            trf!("Delete the collection “{}” here? Its photos stay.", name)
                        },
                        button: apply,
                        command: "immich.resolveAlbumDeletion",
                        params: json!({"account": id, "album": d["album"], "action": "apply"}),
                    });
                }
                if text_button(ui, &format!("immichAlbumDeletion-{i}-{n}-keep"), crate::i18n::tr("Keep"), false).clicked() {
                    let r = app.run("immich.resolveAlbumDeletion", json!({"account": id, "album": d["album"], "action": "keep"}));
                    report(app, ui.ctx(), r);
                    changed = true;
                }
            });
        }
    }
    changed |= confirm_box(app, ui, t, i, id, "sync");
    if changed {
        app.immich.sync_view.remove(id);
    }
}

/// The filter bar's "Immich smart search" box (IMM-SEARCH): a natural-language query sent to the
/// connected server on a worker; the linked photos it finds become the grid's temporary collection
/// (`filter.only`, cleared with the filter bar's other filters). Shown only with one account.
pub fn smart_search_box(app: &mut LccApp, ui: &mut egui::Ui) {
    let account = match app.session.immich_accounts().map(|a| a.immich.iter().map(|x| x.id.clone()).collect::<Vec<_>>()) {
        Ok(v) if v.len() == 1 => v.into_iter().next().unwrap_or_default(),
        _ => return,
    };
    if let Some((acc, slot)) = &app.immich.smart_pending
        && let Some(r) = take(slot)
    {
        let acc = acc.clone();
        app.immich.smart_pending = None;
        match r {
            Ok(ids) => {
                let mut photos: Vec<lcc_catalog::PhotoId> = Vec::new();
                for a in &ids {
                    if let Some(p) = app.session.catalog.photo_of_remote(lcc_immich::link::SERVICE, &acc, a)
                        && !photos.contains(&p)
                    {
                        photos.push(p);
                    }
                }
                app.immich.smart_note = Some(trf!("{} photos", photos.len()));
                app.session.show_only(photos);
            }
            Err(e) => app.immich.smart_note = Some(e),
        }
    }
    let t = Tokens::get(ui.ctx());
    let busy = app.immich.smart_pending.is_some();
    let r = ui.add(egui::TextEdit::singleline(&mut app.immich.smart_query).hint_text(crate::i18n::tr("Immich smart search")).desired_width(170.0));
    crate::access::label(&r, "Immich smart search");
    register(ui.ctx(), "field:immichSmartSearch", r.rect);
    if busy {
        ui.spinner();
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    } else if let Some(n) = &app.immich.smart_note {
        ui.label(RichText::new(n.as_str()).size(11.0).color(t.text_dim));
    }
    let q = app.immich.smart_query.trim().to_string();
    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !busy {
        if q.is_empty() {
            app.session.filter.only.clear();
            app.immich.smart_note = None;
            return;
        }
        let client = match app.session.immich_client(&account) {
            Ok((_, c)) => c,
            Err(e) => {
                app.immich.smart_note = Some(e.to_string());
                return;
            }
        };
        let slot: Slot<Result<Vec<String>, String>> = Arc::new(Mutex::new(None));
        let out = slot.clone();
        std::thread::spawn(move || {
            let q = lcc_immich::types::SmartSearch { query: q, size: Some(250), kind: Some("IMAGE".into()), ..Default::default() };
            let r = client.smart_search(&q).map(|p| p.items.into_iter().map(|a| a.id).collect()).map_err(|e| e.to_string());
            *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
        });
        app.immich.smart_pending = Some((account, slot));
    }
}
