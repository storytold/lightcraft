//! The Profile Manager (LR-PROF-MANAGE, ported from OneSlips' Profile Manager): every profile in one
//! table, drawn like the Presets Manager ([`super::manager_table`]): the user's camera profiles
//! (`.dcp`) grouped by camera model, the user's LUT profiles by their groups, the built-in looks,
//! and the bundled camera profiles of each camera.
//!
//! Per profile: Favorite (looks and LUTs), Rename (double-click the name, or the pencil; looks and
//! LUTs: a camera profile keeps the name its file gives it), Group (looks and LUTs), Hide / Show
//! (hidden profiles leave the profile menu, the browser and the camera profile choices but stay
//! here) and Delete for the user's own files (asked in its row; built-ins are hidden, not
//! deleted). The details column says what it is, how many photos of this library use it and the
//! camera bodies it is the default of. Per group: fold, Rename (look and LUT groups), Hide / Show
//! all, and Delete of the user's own profiles in it (asked at the group). Import Profiles… and
//! Import Folder… sit above the list, with a search over names, groups and camera models.
//!
//! A photo that uses a deleted profile falls back safely: a gone LUT renders without it (its settings
//! stay), a gone camera profile goes back to the camera's default (`profile.deleteImported`, `profile.deleteCamera`).
//!
//! The window opens at a size that fits the screen, never grows from its content, and keeps the
//! size the user drags it to, as the Presets Manager's does
//! ([`super::presets_manager::window_size`]).
#![allow(clippy::indexing_slicing, reason = "not yet converted to checked indexing: tracked in docs/gaps.md (indexing lint)")]

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde_json::{Value, json};

use super::manager_table::{self as table, Col, Columns, Dragged, GroupAction, GroupHead};
use crate::LccApp;
use crate::i18n::{tr, tr_format};
use crate::icons::Icon;
use crate::theme::Tokens;
use crate::widgets::{icon_button, register};

/// The manager's window id (its size lives under it).
pub const WINDOW: &str = "app-profile-manager";
/// The list's key (its group headers' widgets, and its drag and drop).
const KEY: &str = "profileManager";
const COLS: Columns = Columns { info_w: 230.0 };

/// What a profile is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Look,
    Lut,
    Camera,
}

/// One profile as the manager shows it.
#[derive(Clone, Debug)]
struct Item {
    id: String,
    /// The name as shown (a built-in's in the UI language) and as the engine keeps it.
    shown: String,
    name: String,
    kind: Kind,
    builtin: bool,
    hidden: bool,
    favorite: bool,
    used: u64,
    /// Camera bodies it is the default camera profile of.
    defaults: Vec<String>,
}

/// One group: `key` is unique in the list (`look:<group>` or `camera:<model>`), `name` the
/// engine's group name (or the camera model).
#[derive(Clone, Debug)]
struct Group {
    key: String,
    name: String,
    shown: String,
    camera: bool,
    /// Holds built-in profiles only.
    builtin: bool,
    items: Vec<Item>,
}

/// One row of the list.
#[derive(Clone, Debug)]
enum Row {
    Group {
        g: usize,
        open: bool,
    },
    /// Asking before the user's profiles of group `g` are deleted (under its header).
    Confirm(usize),
    Item {
        g: usize,
        i: usize,
        n: usize,
    },
}

#[derive(Default)]
struct Built {
    groups: Vec<Group>,
    removed: usize,
}

/// The command that deletes one of the user's own profiles (built-ins are never offered one).
fn delete_action(i: &Item) -> (&'static str, Value) {
    match i.kind {
        Kind::Camera => ("profile.deleteCamera", json!({"id": i.id})),
        _ => ("profile.deleteImported", json!({"id": i.id})),
    }
}

fn fold_key(key: &str) -> String {
    format!("profileManager:{key}")
}

/// What the profile list is built from: changes whenever a profile, its marks or a photo's
/// settings change.
fn source_key(app: &LccApp) -> u64 {
    let s = &app.session;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.commits().hash(&mut h);
    s.profile_hidden.hash(&mut h);
    s.profile_favorites.hash(&mut h);
    s.profile_edits.names.hash(&mut h);
    s.profile_edits.groups.hash(&mut h);
    s.profile_edits.order.hash(&mut h);
    s.profile_edits.removed.hash(&mut h);
    s.profile_edits.group_order.hash(&mut h);
    s.body_profiles.hash(&mut h);
    for p in &s.lut_profiles {
        (&p.id, &p.name, &p.group).hash(&mut h);
    }
    for (id, _) in lcc_raw::dcp::library() {
        id.hash(&mut h);
    }
    h.finish()
}

/// The groups from `profiles.manager`: the user's camera profiles first (by camera model), then
/// the user's LUT groups, the built-in looks, and the bundled camera profiles of each camera.
fn build(m: &Value) -> Built {
    let str_of = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
    let mut looks: Vec<Group> = Vec::new();
    for p in m["profiles"].as_array().into_iter().flatten() {
        let (id, name, group) = (str_of(p, "id"), str_of(p, "name"), str_of(p, "group"));
        if id.is_empty() {
            continue;
        }
        let kind = if p["kind"] == "lut" { Kind::Lut } else { Kind::Look };
        let builtin = kind == Kind::Look;
        let shown = if p["builtinLabel"] == true { crate::i18n::profile_label(&id, &name).to_string() } else { name.clone() };
        let item = Item {
            id,
            shown,
            name,
            kind,
            builtin,
            hidden: p["hidden"] == true,
            favorite: false,
            used: p["usedBy"].as_u64().unwrap_or(0),
            defaults: Vec::new(),
        };
        let key = format!("look:{group}");
        match looks.iter_mut().find(|g| g.key == key) {
            Some(g) => {
                g.builtin &= builtin;
                g.items.push(item);
            }
            None => {
                let shown = crate::i18n::builtin_label(&group, builtin).to_string();
                looks.push(Group { key, name: group, shown, camera: false, builtin, items: vec![item] });
            }
        }
    }
    // the groups in the user's order, empty ones too (a place to move profiles to)
    let order: Vec<String> = m["groups"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
    for g in &order {
        if !looks.iter().any(|x| x.name == *g) {
            looks.push(Group { key: format!("look:{g}"), name: g.clone(), shown: g.clone(), camera: false, builtin: false, items: Vec::new() });
        }
    }
    looks.sort_by_key(|x| order.iter().position(|o| *o == x.name).unwrap_or(usize::MAX));
    let mut cameras: Vec<Group> = Vec::new();
    for p in m["cameras"].as_array().into_iter().flatten() {
        let (id, name, model) = (str_of(p, "id"), str_of(p, "name"), str_of(p, "model"));
        if id.is_empty() {
            continue;
        }
        let builtin = p["builtin"] == true;
        let defaults: Vec<String> = p["bodyDefaults"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect();
        let item = Item {
            id,
            shown: name.clone(),
            name,
            kind: Kind::Camera,
            builtin,
            hidden: p["hidden"] == true,
            favorite: false,
            used: p["usedBy"].as_u64().unwrap_or(0),
            defaults,
        };
        let key = format!("camera:{model}");
        match cameras.iter_mut().find(|g| g.key == key) {
            Some(g) => {
                g.builtin &= builtin;
                g.items.push(item);
            }
            None => {
                let shown = tr_format!("Camera Profiles · {model}", model = model);
                cameras.push(Group { key, name: model, shown, camera: true, builtin, items: vec![item] });
            }
        }
    }
    // the user's own profiles first in each camera group
    for g in &mut cameras {
        g.items.sort_by_key(|i| i.builtin);
    }
    // the user's own groups first, built-in ones after (a stable sort keeps each part's order)
    let mut groups: Vec<Group> = Vec::new();
    groups.extend(cameras.iter().filter(|g| !g.builtin).cloned());
    groups.extend(looks.iter().filter(|g| !g.builtin).cloned());
    groups.extend(looks.into_iter().filter(|g| g.builtin));
    groups.extend(cameras.into_iter().filter(|g| g.builtin));
    Built { groups, removed: m["removed"].as_array().map_or(0, Vec::len) }
}

/// `profiles.manager`'s answer `m` with what the table shows besides: how many photos of this
/// library use each look, and every camera profile as `cameras` (the imported ones `m["camera"]`
/// lists, then the bundled calibrated looks), each with its photo count, whether it is hidden and
/// the bodies it starts.
fn with_counts_and_cameras(app: &LccApp, m: &mut Value) {
    let s = &app.session;
    let mut looks: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut cameras: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for p in s.catalog.photos() {
        let n = looks.entry(p.develop.profile.id.as_str()).or_default();
        *n = n.saturating_add(1);
        let n = cameras.entry(p.develop.profile.camera.as_str()).or_default();
        *n = n.saturating_add(1);
    }
    let hidden = |id: &str| s.profile_hidden.iter().any(|h| h == id);
    let defaults = |id: &str| -> Vec<String> { s.body_profiles.iter().filter(|(_, v)| v.as_str() == id).map(|(k, _)| k.clone()).collect() };
    let mut list: Vec<Value> = Vec::new();
    for c in m["camera"].as_array().into_iter().flatten() {
        let Some(id) = c["id"].as_str() else { continue };
        let name = c["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id.trim_start_matches("dcp:"));
        list.push(json!({"id": id, "name": name, "model": c["model"], "builtin": false, "hidden": hidden(id),
            "usedBy": cameras.get(id).copied().unwrap_or(0), "bodyDefaults": defaults(id), "cameraDefault": c["cameraDefault"]}));
    }
    for cal in lcc_raw::dcp::calibrated() {
        for (id, d) in &cal.looks {
            list.push(json!({"id": id, "name": d.name, "model": d.model, "builtin": true, "hidden": hidden(id),
                "usedBy": cameras.get(id.as_str()).copied().unwrap_or(0), "bodyDefaults": defaults(id)}));
        }
    }
    if let Some(profiles) = m.get_mut("profiles").and_then(Value::as_array_mut) {
        for p in profiles.iter_mut() {
            let used = p.get("id").and_then(Value::as_str).and_then(|id| looks.get(id)).copied().unwrap_or(0);
            if let Some(o) = p.as_object_mut() {
                o.insert("usedBy".into(), json!(used));
            }
        }
    }
    if let Some(o) = m.as_object_mut() {
        o.insert("cameras".into(), Value::Array(list));
    }
}

/// The groups, rebuilt only when what they are built from changed.
fn groups(ui: &egui::Ui, app: &mut LccApp) -> Arc<Built> {
    let key = source_key(app);
    let id = egui::Id::new("profileManagerGroups");
    if let Some((k, built)) = ui.ctx().data(|d| d.get_temp::<(u64, Arc<Built>)>(id))
        && k == key
    {
        return built;
    }
    let mut m = app.session.execute("profiles.manager", &json!({})).unwrap_or(Value::Null);
    with_counts_and_cameras(app, &mut m);
    let mut built = build(&m);
    for g in &mut built.groups {
        for i in &mut g.items {
            i.favorite = app.session.profile_favorites.contains(&i.id);
        }
    }
    let built = Arc::new(built);
    ui.ctx().data_mut(|d| d.insert_temp(id, (key, built.clone())));
    built
}

/// The list's rows for `needle` (lowercase): a search opens what it finds; `confirm`: the group
/// asking before a delete.
fn rows(app: &LccApp, built: &Built, needle: &str, confirm: Option<&str>) -> Vec<Row> {
    let mut out = Vec::new();
    for (gi, g) in built.groups.iter().enumerate() {
        let hits: Vec<usize> = g
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| {
                needle.is_empty()
                    || i.shown.to_lowercase().contains(needle)
                    || i.name.to_lowercase().contains(needle)
                    || g.shown.to_lowercase().contains(needle)
            })
            .map(|(n, _)| n)
            .collect();
        if hits.is_empty() && !needle.is_empty() {
            continue;
        }
        // the user's own groups start open, the bundled camera profiles folded
        let default_open = !(g.camera && g.builtin);
        let open = !needle.is_empty() || app.ui.group_is_open(&fold_key(&g.key), default_open);
        out.push(Row::Group { g: gi, open });
        if confirm == Some(g.key.as_str()) {
            out.push(Row::Confirm(gi));
        }
        if open {
            out.extend(hits.into_iter().enumerate().map(|(n, i)| Row::Item { g: gi, i, n }));
        }
    }
    out
}

/// The rows drawn last frame (for the tests: only those in view are laid out).
pub fn drawn_rows(ctx: &egui::Context) -> Option<(usize, usize)> {
    ctx.data(|d| d.get_temp::<(usize, usize)>(egui::Id::new("profileManagerDrawn")))
}

/// The dialog's body. `query`: the search; `confirm`: the row whose Delete waits for a yes
/// (`profile:<id>`, `group:<key>`); `rename`: the row renamed in place and its text
/// (`profile:<id>`, `group:<key>`).
pub fn body(app: &mut LccApp, ui: &mut egui::Ui, query: &mut String, confirm: &mut Option<String>, rename: &mut Option<(String, String)>) {
    let t = Tokens::get(ui.ctx());
    let mut actions: Vec<(&'static str, Value)> = Vec::new();
    let built = groups(ui, app);
    toolbar(app, ui, &built, &mut actions);
    ui.horizontal_wrapped(|ui| {
        crate::text_field::TextField::singleline("field:profileManagerSearch", query)
            .hint(tr("Search by name, group or camera"))
            .width(260.0_f32.min(ui.available_width()).max(60.0))
            .show(ui);
        let n: usize = built.groups.iter().map(|g| g.items.len()).sum();
        let h: usize = built.groups.iter().map(|g| g.items.iter().filter(|i| i.hidden).count()).sum();
        ui.label(egui::RichText::new(tr_format!("{n} profiles, {h} hidden", n = n, h = h)).color(t.text_dim));
    });
    ui.add(
        egui::Label::new(
            egui::RichText::new(tr(
                "Hidden profiles leave the profile menu, the browser and the camera profile choices. Delete removes your own profile files; built-in profiles can be hidden, not deleted. Photos using a deleted LUT keep their settings and render without it; photos using a deleted camera profile go back to their camera's default (one undo step).",
            ))
            .color(t.text_dim)
            .size(11.5),
        )
        .wrap(),
    );
    let needle = query.trim().to_lowercase();
    let confirm_group = confirm.as_deref().and_then(|c| c.strip_prefix("group:")).map(str::to_string);
    let list = rows(app, &built, &needle, confirm_group.as_deref());
    let look_groups: Vec<String> = built.groups.iter().filter(|g| !g.camera).map(|g| g.name.clone()).collect();
    // the rest of the window, less the footer below (measured last frame); with room to spare, so
    // the body never makes the window grow
    let footer = ui.ctx().data(|d| d.get_temp::<f32>(egui::Id::new(WINDOW).with("dialog-footer"))).unwrap_or(48.0).clamp(0.0, 200.0);
    let spacing = ui.spacing().item_spacing.y;
    let height = (ui.max_rect().bottom() - ui.cursor().top() - footer - 2.0 * spacing - 12.0).max(120.0);
    let confirm_h = table::confirm_h(ui.ctx());
    let hs: Vec<f32> = list.iter().map(|r| if matches!(r, Row::Confirm(_)) { confirm_h } else { table::ROW_H }).collect();
    let drawn = table::virtual_list(ui, "profileManagerList", &hs, height, |ui, ri| {
        let Some(row) = list.get(ri) else { return };
        match *row {
            Row::Group { g, open } => {
                let Some(group) = built.groups.get(g) else { return };
                group_row(app, ui, &t, group, open, confirm, rename, &mut actions);
            }
            Row::Confirm(g) => {
                let Some(group) = built.groups.get(g) else { return };
                if let Some(yes) = confirm_group_delete(ui, &t, group) {
                    if yes {
                        actions.extend(group.items.iter().filter(|i| !i.builtin).map(delete_action));
                    }
                    *confirm = None;
                }
            }
            Row::Item { g, i, n } => {
                let Some(group) = built.groups.get(g) else { return };
                let Some(item) = group.items.get(i) else { return };
                item_row(ui, &t, group, item, n, &look_groups, confirm, rename, &mut actions);
            }
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("profileManagerDrawn"), (drawn.start, drawn.end)));
    table::drag_tick(ui.ctx(), &t, KEY, |d| match d {
        Dragged::Items(ids) if ids.len() == 1 => ids
            .first()
            .and_then(|id| built.groups.iter().flat_map(|g| g.items.iter()).find(|i| &i.id == id))
            .map(|i| i.shown.clone())
            .unwrap_or_default(),
        Dragged::Items(ids) => tr_format!("{n} profiles", n = ids.len()),
        Dragged::Group(g) => g.clone(),
    });
    for (cmd, p) in actions {
        if matches!(cmd, "profile.deleteImported" | "profile.deleteCamera") {
            *confirm = None;
        }
        match app.run(cmd, p) {
            Ok(r) if cmd == "profile.deleteCamera" && r["wasCameraDefault"] == true => {
                app.toast(ui.ctx(), tr("Deleted. It was its camera's default: that camera's raws now start from its built-in colour."));
            }
            Ok(_) => {}
            Err(e) => {
                app.toast_error(ui.ctx(), e);
                break;
            }
        }
    }
}

/// Import Profiles…, Import Folder… and, after built-in looks were removed, Restore Built-in Looks.
fn toolbar(app: &mut LccApp, ui: &mut egui::Ui, built: &Built, actions: &mut Vec<(&'static str, Value)>) {
    ui.horizontal_wrapped(|ui| {
        let b = ui
            .button(tr("Import Profiles…"))
            .on_hover_text(tr("DNG camera profiles (.dcp), 3D LUTs (.cube) and XMP looks: files, folders or .zip packs"));
        register(ui.ctx(), "button:profileManagerImport", b.rect);
        if b.clicked() {
            actions.push(("file.importPresets", json!({})));
        }
        let b = ui.add_enabled(app.services.pick_folder.is_some(), egui::Button::new(tr("Import Folder…")));
        register(ui.ctx(), "button:profileManagerImportFolder", b.rect);
        if b.clicked()
            && let Some(dir) = app.services.pick_folder.as_mut().and_then(|f| f())
        {
            actions.push(("file.importPresets", json!({"paths": [dir]})));
        }
        if built.removed > 0 {
            let b = ui.button(tr_format!("Restore Built-in Looks ({n})", n = built.removed));
            register(ui.ctx(), "button:profileManagerRestore", b.rect);
            if b.clicked() {
                actions.push(("profile.restoreBuiltins", json!({})));
            }
        }
    });
}

/// A group's header row.
#[allow(clippy::too_many_arguments)]
fn group_row(
    app: &mut LccApp,
    ui: &mut egui::Ui,
    t: &Tokens,
    g: &Group,
    open: bool,
    confirm: &mut Option<String>,
    rename: &mut Option<(String, String)>,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let all_hidden = !g.items.is_empty() && g.items.iter().all(|i| i.hidden);
    let head = GroupHead {
        key: KEY,
        name: &g.key,
        shown: g.shown.clone(),
        count: g.items.len(),
        open,
        all_hidden,
        deletable: g.items.iter().any(|i| !i.builtin),
        renamable: !g.camera,
    };
    let mut renaming = rename.as_ref().and_then(|(k, text)| k.strip_prefix("group:").map(|g| (g.to_string(), text.clone())));
    let was = renaming.clone();
    let act = table::group_header(ui, t, &COLS, &head, &mut renaming);
    if renaming != was {
        *rename = renaming.map(|(k, text)| {
            // a rename just started edits the group's name (the table offers the key's last part)
            let text = if was.is_none() && k == g.key { g.name.clone() } else { text };
            (format!("group:{k}"), text)
        });
    }
    match act {
        Some(GroupAction::Fold(o)) => {
            app.ui.group_open.insert(fold_key(&g.key), o);
        }
        Some(GroupAction::Hide(h)) if g.camera => actions.extend(g.items.iter().map(|i| ("profile.hide", json!({"id": i.id, "hidden": h})))),
        Some(GroupAction::Hide(h)) => actions.push(("profile.groupHide", json!({"group": g.name, "hidden": h}))),
        Some(GroupAction::Rename(n)) if !g.camera => actions.push(("profile.groupRename", json!({"group": g.name, "name": n}))),
        Some(GroupAction::Delete) => *confirm = Some(format!("group:{}", g.key)),
        Some(GroupAction::DropItems(ids)) if !g.camera => {
            actions.extend(
                ids.into_iter()
                    .filter(|id| !id.starts_with("dcp:") && !id.starts_with("cal:"))
                    .map(|id| ("profile.setGroup", json!({"id": id, "group": g.name}))),
            );
        }
        _ => {}
    }
}

/// Asking before the user's own profiles of group `g` are deleted, in a box under its header.
/// `Some(true)`: delete them; `Some(false)`: cancelled.
fn confirm_group_delete(ui: &mut egui::Ui, t: &Tokens, g: &Group) -> Option<bool> {
    let own = g.items.iter().filter(|i| !i.builtin).count();
    let used: u64 = g.items.iter().filter(|i| !i.builtin).map(|i| i.used).sum();
    let mut out = None;
    let framed = egui::Frame::NONE
        .fill(t.inset)
        .stroke(egui::Stroke::new(1.0, t.caution.gamma_multiply(0.7)))
        .corner_radius(4.0)
        .inner_margin(egui::Margin { left: 30, right: 8, top: 6, bottom: 6 })
        .show(ui, |ui| {
            ui.set_width(ui.available_width().max(0.0));
            ui.spacing_mut().item_spacing.y = 6.0;
            let mut text = tr_format!("Delete your {n} profiles in “{name}” and their files?", n = own, name = g.shown);
            if used > 0 {
                text.push(' ');
                text.push_str(&tr_format!("{n} photos use them; they fall back safely.", n = used));
            }
            let r = ui.add(egui::Label::new(egui::RichText::new(text).color(t.text)).wrap());
            register(ui.ctx(), format!("label:profileManagerGroupDeleteAsk:{}", g.key), r.rect);
            ui.horizontal_wrapped(|ui| {
                let r = ui.button(tr("Delete"));
                register(ui.ctx(), format!("button:profileManagerGroupDeleteConfirm:{}", g.key), r.rect);
                if r.clicked() {
                    out = Some(true);
                }
                let r = ui.button(tr("Cancel"));
                register(ui.ctx(), format!("button:profileManagerGroupDeleteCancel:{}", g.key), r.rect);
                if r.clicked() {
                    out = Some(false);
                }
            });
        });
    table::note_confirm_h(ui.ctx(), framed.response.rect.height() + 4.0);
    out
}

/// One profile's row: favourite, name, details, then Rename, Group, Hide and Delete in their columns.
#[allow(clippy::too_many_arguments)]
fn item_row(
    ui: &mut egui::Ui,
    t: &Tokens,
    g: &Group,
    p: &Item,
    n: usize,
    look_groups: &[String],
    confirm: &mut Option<String>,
    rename: &mut Option<(String, String)>,
    actions: &mut Vec<(&'static str, Value)>,
) {
    let id = p.id.clone();
    let row_key = format!("profile:{id}");
    let sz = egui::vec2(24.0, 24.0);
    // asking before a delete takes the row
    if confirm.as_deref() == Some(row_key.as_str()) {
        let question = if p.used > 0 {
            tr_format!("Delete “{name}” and its file? {n} photos use it; they fall back safely.", name = p.shown, n = p.used)
        } else {
            tr_format!("Delete “{name}” and its file?", name = p.shown)
        };
        match table::confirm_item(ui, t, &COLS, &format!("profileManager:{id}"), &question) {
            Some(true) => actions.push(delete_action(p)),
            Some(false) => *confirm = None,
            None => {}
        }
        return;
    }
    let (rect, resp, rid) = table::row(ui, table::ROW_H, ("profile", &id));
    table::paint_item(ui, t, rect, n, resp.contains_pointer());
    let renamable = p.kind != Kind::Camera;
    let renaming = rename.as_ref().is_some_and(|(k, _)| *k == row_key);
    let rr = table::RowRects::new(rect, rid, &COLS, table::ITEM_NAME_LEFT);
    if renamable && resp.double_clicked() && !renaming {
        *rename = Some((row_key.clone(), p.name.clone()));
    }
    if renamable && resp.drag_started() && !renaming {
        table::drag_start(ui.ctx(), KEY, Dragged::Items(vec![id.clone()]));
    }
    // dropped on a profile of another look group: into that group
    if !g.camera
        && let Some((Dragged::Items(ids), true)) = table::drop_target(ui, t, KEY, rect)
        && !ids.contains(&id)
    {
        actions.extend(
            ids.into_iter()
                .filter(|i| !i.starts_with("dcp:") && !i.starts_with("cal:"))
                .map(|i| ("profile.setGroup", json!({"id": i, "group": g.name}))),
        );
    }
    if p.kind != Kind::Camera {
        let fav = rr.in_lead(ui, table::item_star(rect), |ui| {
            icon_button(
                ui,
                &format!("profileManagerStar:{id}"),
                if p.favorite { Icon::StarFilled } else { Icon::Star },
                sz,
                p.favorite,
                true,
                "Favorite",
            )
        });
        if fav.clicked() {
            actions.push(("profile.favorite", json!({"id": id, "favorite": !p.favorite})));
        }
    }
    if renaming {
        let mut done = None;
        if let Some((_, text)) = rename.as_mut() {
            rr.in_name(ui, |ui| {
                let r = crate::text_field::TextField::singleline(&format!("field:profileManagerName:{id}"), text)
                    .width(rr.name.width().clamp(40.0, 380.0))
                    .select_on_focus(true)
                    .show(ui);
                if !r.editing && r.ending.is_none() {
                    r.response.request_focus();
                }
                if r.committed() {
                    done = Some(Some(text.trim().to_string()));
                } else if r.cancelled() {
                    done = Some(None);
                }
            });
        }
        if let Some(name) = done {
            *rename = None;
            if let Some(name) = name.filter(|x| !x.is_empty() && *x != p.name) {
                actions.push(("profile.rename", json!({"id": id, "name": name})));
            }
        }
    } else {
        table::text_in(ui, rr.name, &p.shown, t.font(13.0), if p.hidden { t.text_disabled } else { t.text });
        register(ui.ctx(), format!("profileManagerRow:{id}"), rr.name);
    }
    let mut info: Vec<String> = vec![match (p.kind, p.builtin) {
        (Kind::Camera, false) => tr("Camera profile (.dcp)").to_string(),
        (Kind::Camera, true) => tr("Built-in camera profile").to_string(),
        (Kind::Lut, _) => tr("LUT profile").to_string(),
        (Kind::Look, _) => tr("Built-in look").to_string(),
    }];
    if !p.defaults.is_empty() {
        let bodies: Vec<String> = p.defaults.iter().map(|b| b.split_once('|').map_or(b.as_str(), |(_, s)| s).to_string()).collect();
        info.push(tr_format!("default for {bodies}", bodies = bodies.join(", ")));
    }
    if p.used > 0 {
        info.push(tr_format!("{n} photos", n = crate::panels::grid::group_digits(p.used as usize)));
    }
    if p.hidden {
        info.push(tr("Hidden").to_string());
    }
    table::info_in(ui, t, rr.info, &info.join(" · "));
    let tip = if renamable { "Rename" } else { "A camera profile keeps the name its file gives it" };
    if rr.in_col(ui, Col::Rename, |ui| icon_button(ui, &format!("profileManagerRename:{id}"), Icon::Pencil, sz, false, renamable, tip)).clicked() {
        *rename = Some((row_key.clone(), p.name.clone()));
    }
    if p.kind != Kind::Camera {
        let r = rr.in_col(ui, Col::Group, |ui| {
            crate::widgets::menu_button(ui, tr("Group"), |ui| {
                ui.set_min_width(200.0);
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    for other in look_groups {
                        if ui.selectable_label(*other == g.name, other.as_str()).clicked() {
                            actions.push(("profile.setGroup", json!({"id": id, "group": other})));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Back to Its Own Group")).clicked() {
                        actions.push(("profile.setGroup", json!({"id": id, "group": ""})));
                        ui.close();
                    }
                });
            })
        });
        register(ui.ctx(), format!("profileManagerGroupMenu:{id}"), r.response.rect);
    }
    let (icon, tip) = if p.hidden { (Icon::EyeOff, "Show in the profile menus") } else { (Icon::Eye, "Hide from the profile menus") };
    if rr.in_col(ui, Col::Hide, |ui| icon_button(ui, &format!("profileManagerHide:{id}"), icon, sz, p.hidden, true, tip)).clicked() {
        actions.push(("profile.hide", json!({"id": id, "hidden": !p.hidden})));
    }
    let tip = if p.builtin { "Built-in profiles can be hidden, not deleted" } else { "Delete" };
    if rr.in_col(ui, Col::Delete, |ui| icon_button(ui, &format!("profileManagerDelete:{id}"), Icon::Trash, sz, false, !p.builtin, tip)).clicked() {
        *confirm = Some(row_key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_users_own_groups_come_first() {
        let m = json!({
            "profiles": [
                {"id": "lc.color", "name": "Craft Colour", "group": "Basic", "kind": "look", "builtinLabel": true, "usedBy": 2},
                {"id": "lut:mine", "name": "Mine", "group": "My LUTs", "kind": "lut", "usedBy": 0},
            ],
            "cameras": [
                {"id": "cal:x", "name": "Bundled", "model": "Cam A", "builtin": true},
                {"id": "dcp:own", "name": "Own", "model": "Cam B", "builtin": false, "bodyDefaults": ["Cam B|71"], "usedBy": 3},
            ],
            "removed": ["lc.vivid"],
        });
        let b = build(&m);
        let keys: Vec<&str> = b.groups.iter().map(|g| g.key.as_str()).collect();
        assert_eq!(keys, ["camera:Cam B", "look:My LUTs", "look:Basic", "camera:Cam A"]);
        assert_eq!(b.removed, 1);
        let own = &b.groups[0].items[0];
        assert!(!own.builtin && own.used == 3 && own.defaults == ["Cam B|71"]);
        assert!(b.groups[2].builtin && b.groups[3].builtin && !b.groups[1].builtin);
        // nonsense reads as nothing, never a panic
        assert!(build(&json!({"profiles": [{"id": 4}], "cameras": "x"})).groups.is_empty());
        assert!(build(&Value::Null).groups.is_empty());
    }
}
