//! Library's Publish Services panel (Classic, left column): each publish service with its
//! published collections and what is waiting to go (new, modified, to remove). A click shows a
//! collection in the grid (it is an album); its context menu publishes it, marks it up to date
//! or deletes it. The status line under the services publishes the shown collection. Set Up adds
//! a Hard Drive service. Everything goes through the `publish.*` commands; a publish renders and
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
    /// Set Up: shown.
    setup: bool,
    name: String,
    dir: String,
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
        let kind = match svc["kind"].as_str() {
            Some(dac_publish::KIND_HARD_DRIVE) => crate::i18n::tr("Hard Drive"),
            Some(k) => k,
            None => "",
        };
        let resp = ui.add(egui::Label::new(egui::RichText::new(format!("{name}  ·  {kind}")).strong().size(12.0)).sense(Sense::click()));
        register(ui.ctx(), format!("publishService:{sid}"), resp.rect);
        resp.context_menu(|ui| {
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
        let r = ui.button(crate::i18n::tr("Set Up Hard Drive…"));
        register(ui.ctx(), "publishSetUp", r.rect);
        if r.clicked() {
            form.setup = true;
            if form.name.is_empty() {
                form.name = crate::i18n::tr("Hard Drive").to_string();
            }
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

fn setup_form(app: &mut DacApp, ui: &mut egui::Ui, form: &mut Form) {
    ui.label(egui::RichText::new(crate::i18n::tr("New Hard Drive service")).strong().size(12.0));
    let r = ui.add(egui::TextEdit::singleline(&mut form.name).hint_text(crate::i18n::tr("Name")));
    register(ui.ctx(), "publishSetUpName", r.rect);
    let r = ui.add(egui::TextEdit::singleline(&mut form.dir).hint_text(crate::i18n::tr("Folder (absolute path)")));
    register(ui.ctx(), "publishSetUpDir", r.rect);
    ui.horizontal(|ui| {
        let r = ui.button(crate::i18n::tr("Create"));
        register(ui.ctx(), "publishSetUpCreate", r.rect);
        if r.clicked() {
            match app.run("publish.createService", json!({"name": form.name.trim(), "dir": form.dir.trim()})) {
                Ok(_) => *form = Form::default(),
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
