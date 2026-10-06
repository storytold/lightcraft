//! People: a card per person named on faces (read from XMP): a close-up of their largest face, the
//! name and how many photos they are in. A click opens that person's page (`person.rs`): only their cropped faces,
//! and the faces that look like them.
//!
//! Only the rows on screen ask for a face render (the engine caches them, memory and disk).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::Person;
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// A card's face picture (points); the name and count sit below it.
const CARD: f32 = 150.0;
const LABEL_H: f32 = 44.0;
const GAP: f32 = 16.0;
const PAD: f32 = 20.0;
const HEADER_H: f32 = 44.0;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if let Some(name) = app.ui.person_page.clone() {
        return super::person::show(app, ui, &name);
    }
    let t = Tokens::get(ui.ctx());
    let people = app.caches.people(&app.session.catalog, &app.session.filter);
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().text(pos2(head.left() + PAD, head.center().y), Align2::LEFT_CENTER, "Named People", t.semibold(15.0), t.text);
    ui.painter().text(pos2(head.right() - PAD, head.center().y), Align2::RIGHT_CENTER, people.len().to_string(), t.font(13.0), t.text_dim);
    // the filters narrowing the list (a date, a keyword…), removable here
    let chips = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog);
    super::chips::show(app, ui, &chips);
    if people.is_empty() {
        let (title, body) = if chips.is_empty() {
            ("No named people yet", "Face names written to XMP by Lightroom and other apps show up here.")
        } else {
            ("No named people in these photos", "Remove a filter above, or choose Clear all")
        };
        super::empty_message(ui, ui.available_rect_before_wrap(), title, body);
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let active = app.session.filter.person.clone();
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let cols = (((width - PAD * 2.0 + GAP) / (CARD + GAP)).floor() as usize).max(1);
        let row_h = CARD + LABEL_H + GAP;
        let rows = people.len().div_ceil(cols);
        let (area, _) = ui.allocate_exact_size(vec2(width, PAD * 2.0 + rows as f32 * row_h), Sense::hover());
        let first = ((viewport.top() - PAD) / row_h).floor().max(0.0) as usize;
        let last = (((viewport.bottom() - PAD) / row_h).ceil().max(0.0) as usize).min(rows);
        for row in first..last {
            for col in 0..cols {
                let Some(person) = people.get(row * cols + col) else { break };
                let min = area.min + vec2(PAD + col as f32 * (CARD + GAP), PAD + row as f32 * row_h);
                let selected = active.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&person.name));
                card(app, ui, person, Rect::from_min_size(min, vec2(CARD, CARD + LABEL_H)), ppp, selected);
            }
        }
    });
}

fn card(app: &mut LightcraftApp, ui: &mut egui::Ui, person: &Person, r: Rect, ppp: f32, selected: bool) {
    let t = Tokens::get(ui.ctx());
    let face = Rect::from_min_size(r.min, vec2(CARD, CARD));
    let resp = ui.interact(r, egui::Id::new(("person-card", &person.name)), Sense::click());
    register(ui.ctx(), format!("person:{}", person.name), r);
    let p = ui.painter();
    p.rect_filled(face, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(person.photo, person.face, (CARD * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.tex.id(), face, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if selected {
        p.rect_stroke(face, 3.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
    } else if resp.hovered() {
        p.rect_stroke(face, 3.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
    }
    // a long name must not run past the card
    let name =
        if person.name.chars().count() > 19 { format!("{}…", person.name.chars().take(18).collect::<String>()) } else { person.name.clone() };
    p.text(pos2(r.left() + 2.0, face.bottom() + 14.0), Align2::LEFT_CENTER, name, t.semibold(13.0), t.text);
    let photos = if person.count == 1 { "1 photo".to_string() } else { format!("{} photos", person.count) };
    p.text(pos2(r.left() + 2.0, face.bottom() + 32.0), Align2::LEFT_CENTER, photos, t.font(12.0), t.text_dim);
    if resp.on_hover_text(format!("{} — their faces", person.name)).clicked() {
        let _ = app.run("view.person", json!({"name": person.name}));
    }
}
