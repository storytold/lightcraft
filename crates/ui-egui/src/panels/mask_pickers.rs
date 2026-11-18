//! The People and Remembered pickers under Create New Mask's tiles, and Remember as… on a mask:
//! who a People mask is for (everyone, or one of the faces found on the photo, by name when it
//! has one) and which parts; a remembered object (`concepts.*`) to select on this photo.
//!
//! Behaviour ported from OneSlips' fork (`smart_people.rs`), drawn in the app's own widgets.

use lcc_catalog::PhotoId;
use serde_json::json;

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

/// The People parts offered, in Lightroom's order (none ticked: Entire Person).
pub(crate) const PEOPLE_PARTS: [&str; 10] =
    ["Entire Person", "Face Skin", "Body Skin", "Hair", "Clothes", "Eyebrows", "Eye Sclera", "Iris and Pupil", "Lips", "Teeth"];

/// The name of the photo's Face region over the face `rect` (normalized), if it has one.
fn name_at(app: &LccApp, photo: PhotoId, rect: [f64; 4]) -> Option<String> {
    let (cx, cy) = ((rect[0] + rect[2]) / 2.0, (rect[1] + rect[3]) / 2.0);
    let p = app.session.catalog.photo(photo)?;
    p.meta
        .regions
        .iter()
        .filter(|r| r.kind == lcc_meta::RegionKind::Face)
        .find(|r| (r.rect.x0..=r.rect.x1).contains(&cx) && (r.rect.y0..=r.rect.y1).contains(&cy))
        .and_then(|r| r.name.clone())
        .filter(|n| !n.trim().is_empty())
}

/// The People picker (the People tile was clicked): Everyone or one of the faces on the photo
/// (left to right, with their names), the parts, and Create Mask. Without the face model it
/// offers Person 1–4, counted from the left.
pub fn people_picker(app: &mut LccApp, ui: &mut egui::Ui, photo: PhotoId) {
    let Some((mut person, mut parts)) = app.ui.people_pick.clone() else { return };
    let t = Tokens::get(ui.ctx());
    let tr = crate::i18n::tr;
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tr("People")).font(t.semibold(12.5)).color(t.text_label));
    // the faces: known, or asked for once (the AI worker finds them; the panel repaints)
    let asked = app.ui.people_asked.filter(|(p, _)| *p == photo).map(|(_, at)| at.elapsed());
    let faces = match app.session.known_appearance(photo) {
        Some(f) => Some(Ok(f)),
        // asked and still not there after a while (the worker failed: its message was shown):
        // people counted from the left
        None if asked.is_some_and(|a| a > std::time::Duration::from_secs(20)) => Some(Err(String::new())),
        None if asked.is_some() => None,
        None => {
            app.ui.people_asked = Some((photo, web_time::Instant::now()));
            match app.session.photo_appearance(photo, false) {
                Ok(Some(f)) => Some(Ok(f)),
                Ok(None) => None,
                Err(e) => Some(Err(e)),
            }
        }
    };
    let mut names: Vec<Option<String>> = Vec::new();
    ui.horizontal_wrapped(|ui| {
        let r = ui.selectable_label(person == 0, tr("Everyone"));
        register(ui.ctx(), "maskPeople:person:0", r.rect);
        if r.clicked() {
            person = 0;
        }
        let count = match &faces {
            Some(Ok(f)) => f.len(),
            // no face model: people counted from the left
            Some(Err(_)) => 4,
            None => 0,
        };
        if let Some(Ok(f)) = &faces {
            names = f.iter().map(|f| name_at(app, photo, f.rect)).collect();
        }
        for n in 1..=count.min(64) {
            let k = u32::try_from(n).unwrap_or(u32::MAX);
            let label = match names.get(n - 1).cloned().flatten() {
                Some(name) => name,
                None => crate::i18n::tr_format!("Person {n}", n = n),
            };
            let r = ui.selectable_label(person == k, label);
            register(ui.ctx(), format!("maskPeople:person:{n}"), r.rect);
            if r.clicked() {
                person = k;
            }
        }
    });
    match &faces {
        None => {
            ui.horizontal(|ui| {
                crate::progress::spinner(ui, 14.0);
                ui.label(egui::RichText::new(tr("Finding faces…")).color(t.text_dim));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        }
        Some(Ok(f)) if f.is_empty() => {
            ui.label(egui::RichText::new(tr("No faces were found on this photo.")).color(t.text_dim));
        }
        _ => {}
    }
    ui.scope(|ui| {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 3.0;
        let check_radius = egui::CornerRadius::same(t.radius(3.0) as u8);
        let w = &mut ui.style_mut().visuals.widgets;
        for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = check_radius;
        }
        for part in PEOPLE_PARTS {
            let whole = part == "Entire Person";
            let mut on = if whole { parts.is_empty() } else { parts.iter().any(|p| p == part) };
            let r = ui.checkbox(&mut on, tr(part));
            register(ui.ctx(), format!("maskPeople:part:{part}"), r.rect);
            if r.changed() {
                if whole {
                    parts.clear();
                } else if on {
                    parts.push(part.to_string());
                } else {
                    parts.retain(|p| p != part);
                }
            }
        }
        ui.add_space(4.0);
    });
    ui.horizontal(|ui| {
        let create = text_button(ui, "maskPeople:create", tr("Create Mask"), false);
        let cancel = text_button(ui, "maskPeople:cancel", tr("Cancel"), false);
        if cancel.clicked() {
            app.ui.people_pick = None;
            app.mask_pick_op = None;
            return;
        }
        if create.clicked() {
            let mut p = json!({"kind": "people", "person": person, "parts": parts});
            // a named face: by name, so a preset made from this mask finds them on other photos
            if let Some(name) = person.checked_sub(1).and_then(|i| names.get(i as usize).cloned().flatten()) {
                p["who"] = json!({"name": name});
                p["person"] = json!(0);
            }
            app.ui.people_pick = None;
            if let Err(e) = add_picked(app, p) {
                app.ai_error(ui.ctx(), e, None);
            }
            return;
        }
        app.ui.people_pick = Some((person, parts));
    });
}

/// The Remembered picker: each remembered object, selected on this photo with a click.
pub fn concept_picker(app: &mut LccApp, ui: &mut egui::Ui) {
    if !app.ui.concept_pick {
        return;
    }
    let t = Tokens::get(ui.ctx());
    let tr = crate::i18n::tr;
    ui.add_space(6.0);
    ui.label(egui::RichText::new(tr("Remembered")).font(t.semibold(12.5)).color(t.text_label));
    let names: Vec<String> = app.session.concept_store().concepts.iter().map(|c| c.name.clone()).collect();
    if names.is_empty() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(tr("Nothing remembered yet. Select a thing with any mask, then choose Remember as… in the mask's menu."))
                    .color(t.text_dim),
            )
            .wrap(),
        );
    }
    ui.horizontal_wrapped(|ui| {
        for (k, n) in names.iter().enumerate() {
            let b = ui.button(n);
            register(ui.ctx(), format!("maskConcept:{k}"), b.rect);
            if b.clicked() {
                app.ui.concept_pick = false;
                if let Err(e) = add_picked(app, json!({"kind": "concept", "name": n})) {
                    app.ai_error(ui.ctx(), e, None);
                }
            }
        }
    });
    if text_button(ui, "maskConcept:cancel", tr("Cancel"), false).clicked() {
        app.ui.concept_pick = false;
        app.mask_pick_op = None;
    }
}

/// A picker's mask: a new one, or (opened from Add / Subtract / Intersect, OneSlips 1138a5edd) a component the
/// selected mask combines by that op.
fn add_picked(app: &mut LccApp, mut p: serde_json::Value) -> Result<serde_json::Value, String> {
    match app.mask_pick_op.take().filter(|_| app.session.active_mask.is_some()) {
        Some(op) => {
            p["op"] = json!(op);
            app.run("mask.addComponent", p)
        }
        None => app.run("mask.add", p),
    }
}

/// Remember as… in a mask's menu: name what the mask selects (`concepts.teach`).
pub fn remember_item(app: &mut LccApp, ui: &mut egui::Ui, mask: u32) {
    let b = ui.button(crate::i18n::tr("Remember as…"));
    register(ui.ctx(), format!("maskMenu:remember:{mask}"), b.rect);
    if b.clicked() {
        crate::panels::dialogs::prompt(app, "Remember as", "Name, e.g. Wedding dress", "", "concepts.teach", json!({"mask": mask}), "name");
        ui.close();
    }
}
