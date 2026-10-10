//! Library's Publish Services panel (Classic, left column): each publish service with its
//! published collections and what is waiting to go (new, modified, to remove). A click shows a
//! collection in the grid (it is an album); its context menu publishes it, marks it up to date
//! or deletes it. The status line under the services publishes the shown collection. Set Up adds
//! a Hard Drive, SFTP, Immich or plug-in service (Edit Settings… changes one). Everything goes
//! through the `publish.*` commands; a publish renders and
//! sends on a worker thread.

use egui::{Align2, Sense, pos2, vec2};
use serde_json::{Value, json};

use crate::DacApp;
use crate::module::PanelId;
use crate::theme::Tokens;
use crate::widgets::register;

const TASK: &str = "Publish";

/// The inline forms' text, kept between frames.
#[derive(Clone, Default)]
struct Form {
    /// Set Up / Edit: shown.
    setup: bool,
    /// Editing this service (else a new one).
    editing: Option<String>,
    /// `hardDrive`, `sftp`, `immich` or `plugin:<id>`.
    kind: String,
    name: String,
    /// Hard Drive: the folder.
    dir: String,
    /// SFTP: a saved upload server's name (empty: the inline server below).
    sftp_saved: String,
    sftp_host: String,
    sftp_port: String,
    sftp_user: String,
    sftp_path: String,
    sftp_key: String,
    /// Immich: account id, what to send, trash removed renders.
    immich_account: String,
    immich_send: String,
    delete_removed: bool,
    /// Plug-in services: their settings as JSON.
    plugin_settings: String,
    /// Choices, read when the form opens: saved upload servers, Immich accounts (id, label),
    /// plug-in services (kind, name).
    servers: Vec<String>,
    accounts: Vec<(String, String)>,
    plugins: Vec<(String, String)>,
    /// New collection: the service it goes into.
    coll_for: Option<String>,
    coll_name: String,
}

fn form_id() -> egui::Id {
    egui::Id::new("publish-panel-form")
}

/// `publish.services`, asked again when the catalog changed or every two seconds.
fn services(app: &mut DacApp, ui: &egui::Ui) -> Value {
    let id = egui::Id::new("publish-services-cache");
    let now = ui.ctx().input(|i| i.time);
    let rev = app.session.catalog.revision;
    if let Some((t, r, v)) = ui.data(|d| d.get_temp::<(f64, u64, Value)>(id))
        && r == rev
        && now - t < 2.0
    {
        return v;
    }
    let v = app.session.execute("publish.services", &json!({})).unwrap_or(Value::Null);
    ui.data_mut(|d| d.insert_temp(id, (now, rev, v.clone())));
    v
}

fn refresh(ui: &egui::Ui) {
    ui.data_mut(|d| d.remove::<(f64, u64, Value)>(egui::Id::new("publish-services-cache")));
}

/// The panel: header plus, when open, the services.
pub fn show(app: &mut DacApp, ui: &mut egui::Ui, side: &[PanelId]) {
    let (r, open) = super::classic::header(app, ui, PanelId::Publish, side);
    let _ = r;
    if !open {
        return;
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 12, right: 12, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        body(app, ui);
    });
}

fn body(app: &mut DacApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let v = services(app, ui);
    let mut form: Form = ui.data(|d| d.get_temp(form_id())).unwrap_or_default();
    let list = v["services"].as_array().cloned().unwrap_or_default();
    if v.is_null() {
        ui.label(egui::RichText::new(crate::i18n::tr("Publish services need a library on disk.")).color(t.text_dim).size(11.5));
        return;
    }
    if list.is_empty() && !form.setup {
        ui.label(egui::RichText::new(crate::i18n::tr("Publish collections to a folder kept in sync.")).color(t.text_dim).size(11.5));
    }
    let shown = match app.session.source {
        dac_engine::LibrarySource::Album(a) => Some(a.0),
        _ => None,
    };
    for svc in &list {
        let sid = svc["id"].as_str().unwrap_or_default().to_string();
        let name = svc["name"].as_str().unwrap_or_default();
        let kind = kind_label(svc["kind"].as_str().unwrap_or_default(), &form.plugins);
        let resp = ui.add(egui::Label::new(egui::RichText::new(format!("{name}  ·  {kind}")).strong().size(12.0)).sense(Sense::click()));
        register(ui.ctx(), format!("publishService:{sid}"), resp.rect);
        resp.context_menu(|ui| {
            if ui.button(crate::i18n::tr("Edit Settings…")).clicked() {
                form.open(app, Some(svc));
                ui.close();
            }
            if ui.button(crate::i18n::tr("Create Published Collection…")).clicked() {
                form.coll_for = Some(sid.clone());
                form.coll_name.clear();
                ui.close();
            }
            if ui.button(crate::i18n::tr("Publish All")).clicked() {
                for c in svc["collections"].as_array().into_iter().flatten() {
                    if let Some(a) = c["album"].as_u64() {
                        publish(app, ui.ctx(), a);
                    }
                }
                ui.close();
            }
            if ui.button(crate::i18n::tr("Delete Publish Service")).clicked() {
                if let Err(e) = app.run("publish.deleteService", json!({"service": sid})) {
                    app.toast_error(ui.ctx(), e);
                }
                refresh(ui);
                ui.close();
            }
        });
        for c in svc["collections"].as_array().into_iter().flatten() {
            let Some(album) = c["album"].as_u64() else { continue };
            let pending = ["new", "modified", "toRemove"].iter().filter_map(|k| c[*k].as_u64()).sum::<u64>();
            let cname = c["name"].as_str().unwrap_or_default();
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
            register(ui.ctx(), format!("publishCollection:{album}"), rect);
            let selected = shown == Some(album);
            if selected {
                ui.painter().rect_filled(rect, 3.0, t.accent.gamma_multiply(0.35));
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 3.0, t.hover);
            }
            ui.painter().text(pos2(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, cname, t.font(12.0), t.text);
            let count = if pending > 0 { format!("{pending}") } else { c["published"].as_u64().unwrap_or(0).to_string() };
            let col = if pending > 0 { t.accent } else { t.text_dim };
            ui.painter().text(pos2(rect.right() - 4.0, rect.center().y), Align2::RIGHT_CENTER, count, t.font(11.5), col);
            if resp.clicked() {
                let _ = app.run("library.source", json!({"kind": "album", "id": album}));
            }
            resp.context_menu(|ui| {
                if ui.button(crate::i18n::tr("Publish Now")).clicked() {
                    publish(app, ui.ctx(), album);
                    ui.close();
                }
                if ui.button(crate::i18n::tr("Mark as Up-to-Date")).clicked() {
                    let _ = app.run("publish.markUpToDate", json!({"collection": album}));
                    refresh(ui);
                    ui.close();
                }
                if ui.button(crate::i18n::tr("Delete Published Collection")).clicked() {
                    if let Err(e) = app.run("publish.deleteCollection", json!({"collection": album})) {
                        app.toast_error(ui.ctx(), e);
                    }
                    refresh(ui);
                    ui.close();
                }
            });
        }
        if form.coll_for.as_deref() == Some(sid.as_str()) {
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut form.coll_name).hint_text(crate::i18n::tr("Collection name")).desired_width(120.0));
                register(ui.ctx(), "publishCollectionName", r.rect);
                let go = ui.button(crate::i18n::tr("Create")).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                if go {
                    match app.run("publish.createCollection", json!({"service": sid, "name": form.coll_name.trim(), "addSelected": true})) {
                        Ok(_) => form.coll_for = None,
                        Err(e) => app.toast_error(ui.ctx(), e),
                    }
                    refresh(ui);
                }
                if ui.button(crate::i18n::tr("Cancel")).clicked() {
                    form.coll_for = None;
                }
            });
        }
    }
    // the shown published collection: what's waiting, and Publish
    if let Some(a) = shown.filter(|a| list.iter().any(|s| s["collections"].as_array().into_iter().flatten().any(|c| c["album"].as_u64() == Some(*a))))
    {
        status_line(app, ui, a);
    }
    ui.add_space(2.0);
    if form.setup {
        setup_form(app, ui, &mut form);
    } else {
        let r = ui.button(crate::i18n::tr("Set Up Publish Service…"));
        register(ui.ctx(), "publishSetUp", r.rect);
        if r.clicked() {
            form.open(app, None);
        }
    }
    ui.data_mut(|d| d.insert_temp(form_id(), form));
}

fn status_line(app: &mut DacApp, ui: &mut egui::Ui, album: u64) {
    let t = Tokens::get(ui.ctx());
    let Ok(st) = app.session.execute("publish.status", &json!({"collection": album})) else { return };
    let n = |k: &str| st[k].as_array().map(Vec::len).unwrap_or(0);
    let text = format!(
        "{} {} · {} {} · {} {} · {} {}",
        n("new"),
        crate::i18n::tr("new"),
        n("modified"),
        crate::i18n::tr("modified"),
        n("toRemove"),
        crate::i18n::tr("to remove"),
        n("published"),
        crate::i18n::tr("published")
    );
    ui.label(egui::RichText::new(text).color(t.text_dim).size(11.0));
    let busy = app.tasks.is_running(TASK);
    ui.horizontal(|ui| {
        let r = ui.add_enabled(
            !busy && st["pending"].as_u64().unwrap_or(0) > 0,
            egui::Button::new(crate::i18n::tr(if busy { "Publishing…" } else { "Publish" })),
        );
        register(ui.ctx(), "publishNow", r.rect);
        if r.clicked() {
            publish(app, ui.ctx(), album);
        }
        if n("modified") > 0 {
            let r = ui.button(crate::i18n::tr("Mark as Up-to-Date"));
            if r.clicked() {
                let _ = app.run("publish.markUpToDate", json!({"collection": album}));
            }
        }
    });
}

/// A service kind as people call it.
fn kind_label(kind: &str, plugins: &[(String, String)]) -> String {
    match kind {
        dac_publish::KIND_HARD_DRIVE => crate::i18n::tr("Hard Drive").to_string(),
        dac_publish::KIND_SFTP => crate::i18n::tr("SFTP Server").to_string(),
        dac_publish::KIND_IMMICH => "Immich".to_string(),
        k if k.starts_with("plugin:") => {
            plugins.iter().find(|(pk, _)| pk == k).map(|(_, n)| n.clone()).unwrap_or_else(|| crate::i18n::tr("Plug-in").to_string())
        }
        k => k.to_string(),
    }
}

impl Form {
    /// Show the form for a new service (`svc` None) or to edit `svc`, reading the choices.
    fn open(&mut self, app: &mut DacApp, svc: Option<&Value>) {
        let (coll_for, coll_name) = (self.coll_for.take(), std::mem::take(&mut self.coll_name));
        *self = Form { coll_for, coll_name, setup: true, ..Form::default() };
        let servers = app.session.execute("web.servers", &json!({})).unwrap_or(Value::Null);
        self.servers = servers.as_array().into_iter().flatten().filter_map(|s| s["name"].as_str().map(str::to_string)).collect();
        let st = app.session.execute("immich.status", &json!({})).unwrap_or(Value::Null);
        self.accounts = st["accounts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| {
                let id = a["id"].as_str()?.to_string();
                let who = a["userName"].as_str().filter(|u| !u.is_empty()).unwrap_or_default();
                let url = a["url"].as_str().unwrap_or_default();
                Some((id, if who.is_empty() { url.to_string() } else { format!("{who} · {url}") }))
            })
            .collect();
        let pl = app.session.execute("plugin.publishServices", &json!({})).unwrap_or(Value::Null);
        self.plugins = pl
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| Some((p["kind"].as_str()?.to_string(), p["name"].as_str().unwrap_or("Plug-in").to_string())))
            .collect();
        self.immich_send = "rendered".into();
        self.sftp_port = "22".into();
        self.plugin_settings = "{}".into();
        match svc {
            None => {
                self.kind = dac_publish::KIND_HARD_DRIVE.into();
                self.name = crate::i18n::tr("Hard Drive").to_string();
            }
            Some(svc) => {
                self.editing = svc["id"].as_str().map(str::to_string);
                self.kind = svc["kind"].as_str().unwrap_or_default().to_string();
                self.name = svc["name"].as_str().unwrap_or_default().to_string();
                let st = &svc["settings"];
                let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
                self.dir = s(&st["dir"]);
                match &st["server"] {
                    Value::String(n) => self.sftp_saved = n.clone(),
                    o @ Value::Object(_) => {
                        self.sftp_host = s(&o["host"]);
                        self.sftp_port = o["port"].as_u64().unwrap_or(22).to_string();
                        self.sftp_user = s(&o["user"]);
                        self.sftp_path = s(&o["path"]);
                        self.sftp_key = s(&o["keyFile"]);
                    }
                    _ => {}
                }
                self.immich_account = s(&st["account"]);
                if let Some(x) = st["send"].as_str() {
                    self.immich_send = x.to_string();
                }
                self.delete_removed = st["deleteRemoved"].as_bool().unwrap_or(false);
                if self.kind.starts_with("plugin:") {
                    self.plugin_settings = serde_json::to_string_pretty(st).unwrap_or_else(|_| "{}".into());
                }
            }
        }
        if self.immich_account.is_empty()
            && let Some((id, _)) = self.accounts.first()
        {
            self.immich_account = id.clone();
        }
    }

    /// The kind's settings from the fields.
    fn settings(&self) -> Result<Value, String> {
        Ok(match self.kind.as_str() {
            dac_publish::KIND_HARD_DRIVE => json!({"dir": self.dir.trim()}),
            dac_publish::KIND_SFTP if !self.sftp_saved.is_empty() => json!({"server": self.sftp_saved}),
            dac_publish::KIND_SFTP => {
                let port: u16 = self.sftp_port.trim().parse().map_err(|_| crate::i18n::tr("The port must be a number (1–65535)").to_string())?;
                let mut server = json!({"host": self.sftp_host.trim(), "port": port, "user": self.sftp_user.trim(), "path": self.sftp_path.trim()});
                if !self.sftp_key.trim().is_empty() {
                    server["keyFile"] = json!(self.sftp_key.trim());
                }
                json!({"server": server})
            }
            dac_publish::KIND_IMMICH => json!({"account": self.immich_account, "send": self.immich_send, "deleteRemoved": self.delete_removed}),
            _ => {
                let v: Value =
                    serde_json::from_str(&self.plugin_settings).map_err(|e| format!("{}: {e}", crate::i18n::tr("Settings are not valid JSON")))?;
                if !v.is_object() {
                    return Err(crate::i18n::tr("Settings must be a JSON object").to_string());
                }
                v
            }
        })
    }
}

fn setup_form(app: &mut DacApp, ui: &mut egui::Ui, form: &mut Form) {
    let title = if form.editing.is_some() { "Edit Publish Service" } else { "New Publish Service" };
    ui.label(egui::RichText::new(crate::i18n::tr(title)).strong().size(12.0));
    // the kind: fixed once the service exists
    let mut kinds: Vec<String> =
        [dac_publish::KIND_HARD_DRIVE, dac_publish::KIND_SFTP, dac_publish::KIND_IMMICH].iter().map(|k| k.to_string()).collect();
    kinds.extend(form.plugins.iter().map(|(k, _)| k.clone()));
    let before = form.kind.clone();
    ui.add_enabled_ui(form.editing.is_none(), |ui| {
        let r = egui::ComboBox::from_id_salt("publishSetUpKind").selected_text(kind_label(&form.kind, &form.plugins)).show_ui(ui, |ui| {
            for k in &kinds {
                let label = kind_label(k, &form.plugins);
                ui.selectable_value(&mut form.kind, k.clone(), label);
            }
        });
        register(ui.ctx(), "publishSetUpKind", r.response.rect);
    });
    if form.kind != before && form.editing.is_none() {
        form.name = kind_label(&form.kind, &form.plugins);
    }
    let r = ui.add(egui::TextEdit::singleline(&mut form.name).hint_text(crate::i18n::tr("Name")));
    register(ui.ctx(), "publishSetUpName", r.rect);
    match form.kind.as_str() {
        dac_publish::KIND_HARD_DRIVE => {
            let r = ui.add(egui::TextEdit::singleline(&mut form.dir).hint_text(crate::i18n::tr("Folder (absolute path)")));
            register(ui.ctx(), "publishSetUpDir", r.rect);
        }
        dac_publish::KIND_SFTP => {
            let other = crate::i18n::tr("Other server…").to_string();
            let shown = if form.sftp_saved.is_empty() { other.clone() } else { form.sftp_saved.clone() };
            let r = egui::ComboBox::from_id_salt("publishSetUpServer").selected_text(shown).show_ui(ui, |ui| {
                for s in &form.servers {
                    ui.selectable_value(&mut form.sftp_saved, s.clone(), s);
                }
                ui.selectable_value(&mut form.sftp_saved, String::new(), other);
            });
            register(ui.ctx(), "publishSetUpServer", r.response.rect);
            if form.sftp_saved.is_empty() {
                ui.horizontal(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut form.sftp_host).hint_text(crate::i18n::tr("Host")).desired_width(110.0));
                    register(ui.ctx(), "publishSetUpHost", r.rect);
                    let r = ui.add(egui::TextEdit::singleline(&mut form.sftp_port).hint_text(crate::i18n::tr("Port")).desired_width(40.0));
                    register(ui.ctx(), "publishSetUpPort", r.rect);
                });
                let r = ui.add(egui::TextEdit::singleline(&mut form.sftp_user).hint_text(crate::i18n::tr("User name")));
                register(ui.ctx(), "publishSetUpUser", r.rect);
                let r = ui.add(egui::TextEdit::singleline(&mut form.sftp_path).hint_text(crate::i18n::tr("Remote folder")));
                register(ui.ctx(), "publishSetUpPath", r.rect);
                let r = ui.add(egui::TextEdit::singleline(&mut form.sftp_key).hint_text(crate::i18n::tr("Private key file")));
                register(ui.ctx(), "publishSetUpKey", r.rect);
                let t = Tokens::get(ui.ctx());
                ui.label(
                    egui::RichText::new(crate::i18n::tr("Password logins need a saved server (Web module → Upload Settings)."))
                        .color(t.text_dim)
                        .size(11.0),
                );
            }
        }
        dac_publish::KIND_IMMICH => {
            if form.accounts.is_empty() {
                let t = Tokens::get(ui.ctx());
                ui.label(
                    egui::RichText::new(crate::i18n::tr("Connect an Immich server first (Settings → Connections).")).color(t.text_dim).size(11.0),
                );
            } else {
                let shown = form.accounts.iter().find(|(id, _)| *id == form.immich_account).map(|(_, l)| l.clone()).unwrap_or_default();
                let r = egui::ComboBox::from_id_salt("publishSetUpAccount").selected_text(shown).show_ui(ui, |ui| {
                    for (id, label) in &form.accounts {
                        ui.selectable_value(&mut form.immich_account, id.clone(), label);
                    }
                });
                register(ui.ctx(), "publishSetUpAccount", r.response.rect);
            }
            let sends = [("rendered", "Rendered copies"), ("original", "Originals"), ("both", "Rendered, stacked on originals")];
            let shown = sends.iter().find(|(k, _)| *k == form.immich_send).map(|(_, l)| crate::i18n::tr(l)).unwrap_or_default();
            let r = egui::ComboBox::from_id_salt("publishSetUpSend").selected_text(shown).show_ui(ui, |ui| {
                for (k, l) in sends {
                    ui.selectable_value(&mut form.immich_send, k.to_string(), crate::i18n::tr(l));
                }
            });
            register(ui.ctx(), "publishSetUpSend", r.response.rect);
            let r = ui.checkbox(&mut form.delete_removed, crate::i18n::tr("Trash renders taken out of a collection"));
            register(ui.ctx(), "publishSetUpDeleteRemoved", r.rect);
        }
        _ => {
            let r = ui.add(
                egui::TextEdit::multiline(&mut form.plugin_settings).hint_text(crate::i18n::tr("Settings (JSON)")).desired_rows(3).code_editor(),
            );
            register(ui.ctx(), "publishSetUpSettings", r.rect);
        }
    }
    ui.horizontal(|ui| {
        let r = ui.button(crate::i18n::tr(if form.editing.is_some() { "Save" } else { "Create" }));
        register(ui.ctx(), "publishSetUpCreate", r.rect);
        if r.clicked() {
            let res = form.settings().and_then(|settings| match &form.editing {
                Some(id) => app.run("publish.updateService", json!({"service": id, "name": form.name.trim(), "settings": settings})),
                None => app.run("publish.createService", json!({"kind": form.kind, "name": form.name.trim(), "settings": settings})),
            });
            match res {
                Ok(_) => {
                    let (coll_for, coll_name) = (form.coll_for.take(), std::mem::take(&mut form.coll_name));
                    *form = Form { coll_for, coll_name, plugins: std::mem::take(&mut form.plugins), ..Form::default() };
                }
                Err(e) => app.toast_error(ui.ctx(), e),
            }
            refresh(ui);
        }
        if ui.button(crate::i18n::tr("Cancel")).clicked() {
            form.setup = false;
        }
    });
}

/// Publish one collection: set up here, rendered and sent on a worker thread, recorded when done.
pub fn publish(app: &mut DacApp, ctx: &egui::Context, album: u64) {
    if app.tasks.is_running(TASK) {
        app.toast(ctx, crate::i18n::tr("A publish is already running"));
        return;
    }
    let plan = match dac_engine::cmd::publish::plan(&mut app.session, dac_catalog::AlbumId(album)) {
        Ok(p) => p,
        Err(e) => return app.toast_error(ctx, e.to_string()),
    };
    if plan.is_empty() && plan.failed.is_empty() {
        return app.toast(ctx, crate::i18n::tr("Nothing to publish"));
    }
    let work = move || plan.execute(&mut |_, _| true);
    let done = |app: &mut DacApp, ctx: &egui::Context, out: dac_engine::cmd::publish::Outcome| match dac_engine::cmd::publish::apply(
        &mut app.session,
        out,
    ) {
        Ok(r) => {
            let (p, rm, f) = (
                r["published"].as_array().map(Vec::len).unwrap_or(0),
                r["removed"].as_array().map(Vec::len).unwrap_or(0),
                r["failed"].as_array().map(Vec::len).unwrap_or(0),
            );
            let mut msg = format!("{} {p} · {} {rm}", crate::i18n::tr("Published"), crate::i18n::tr("removed"));
            if f > 0 {
                let first = r["failed"][0]["error"].as_str().unwrap_or_default();
                msg.push_str(&format!(" · {} {f}: {first}", crate::i18n::tr("failed")));
                app.toast_error(ctx, msg);
            } else {
                app.toast(ctx, msg);
            }
        }
        Err(e) => app.toast_error(ctx, e.to_string()),
    };
    if let Err(e) = crate::tasks::spawn(app, TASK, None, work, done) {
        app.toast_error(ctx, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_friendly_labels() {
        let plugins = vec![("plugin:demo".to_string(), "Demo Gallery".to_string())];
        assert_eq!(kind_label("sftp", &plugins), "SFTP Server");
        assert_eq!(kind_label("immich", &plugins), "Immich");
        assert_eq!(kind_label("plugin:demo", &plugins), "Demo Gallery");
        assert_eq!(kind_label("plugin:gone", &plugins), "Plug-in");
    }

    #[test]
    fn form_settings_per_kind() {
        let mut f = Form { kind: "sftp".into(), sftp_host: " h ".into(), sftp_port: "2222".into(), sftp_user: "u".into(), ..Form::default() };
        assert_eq!(f.settings().unwrap()["server"]["port"], 2222);
        f.sftp_port = "x".into();
        assert!(f.settings().is_err());
        f.sftp_saved = "Saved".into();
        assert_eq!(f.settings().unwrap(), json!({"server": "Saved"}));
        let f = Form { kind: "immich".into(), immich_account: "a1".into(), immich_send: "both".into(), ..Form::default() };
        assert_eq!(f.settings().unwrap()["send"], "both");
        let mut f = Form { kind: "plugin:demo".into(), plugin_settings: "[1]".into(), ..Form::default() };
        assert!(f.settings().is_err());
        f.plugin_settings = r#"{"a": 1}"#.into();
        assert_eq!(f.settings().unwrap()["a"], 1);
    }
}
