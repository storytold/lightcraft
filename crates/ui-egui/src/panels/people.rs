//! People: a card per person named on faces (read from XMP): a close-up of their face and how many photos they are in
//! (a small number on the picture). A click opens that person's page (`person.rs`): only their cropped faces, and the
//! faces that look like them. Below the people, the faces nobody has named yet (`unnamed.rs`), to be named a group at a
//! time.
//!
//! The thumbnail-size slider in the bottom bar sizes the faces here too (with its own smaller and larger limits), and
//! every face is cut from the same kind of box (the detector's, once the scan has looked at it), so faces are shown
//! equally close. Only the rows on screen ask for a face render (the engine caches them, memory and disk).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use lightcraft_catalog::Person;
use serde_json::json;

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

const GAP: f32 = 14.0;
pub(super) const PAD: f32 = 20.0;
const HEADER_H: f32 = 44.0;
/// The name under a card's picture.
const NAME_H: f32 = 26.0;
/// The unnamed-faces heading and its hints.
const SECTION_H: f32 = 70.0;

/// `thumb` (the thumbnail slider, 90 to 480) as a position between `low` and `high`.
fn between(thumb: f32, low: f32, high: f32) -> f32 {
    let f = (thumb - 90.0) / (480.0 - 90.0);
    low + if f.is_finite() { f.clamp(0.0, 1.0) } else { 0.4 } * (high - low)
}

/// A person card's face picture (points) for the thumbnail slider's value.
pub fn card_edge(thumb: f32) -> f32 {
    between(thumb, 84.0, 260.0)
}

/// A face tile's picture (points), for the unnamed faces and a person's page.
pub fn tile_edge(thumb: f32) -> f32 {
    between(thumb, 56.0, 180.0)
}

/// How many `edge`-wide cells (with the gap between them) fit across `width`.
pub(super) fn columns(width: f32, edge: f32) -> usize {
    (((width - PAD * 2.0 + GAP) / (edge + GAP)).floor() as usize).max(1)
}

pub(super) const TILE_GAP: f32 = GAP;

/// `text` shortened with "…" until it is no wider than `max_w` in `font` (measured, not guessed).
pub(super) fn fit(painter: &egui::Painter, text: &str, font: egui::FontId, max_w: f32) -> String {
    let width = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    if width(text) <= max_w {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    (1..chars.len()).rev().map(|n| chars[..n].iter().collect::<String>() + "…").find(|s| width(s) <= max_w).unwrap_or_else(|| "…".to_string())
}

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if let Some(name) = app.ui.person_page.clone() {
        return super::person::show(app, ui, &name);
    }
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    let thumb = app.ui.thumb_size;
    let people = app.caches.people(&app.session.catalog, &app.session.filter);
    let (head, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().text(pos2(head.left() + PAD, head.center().y), Align2::LEFT_CENTER, "Named People", t.semibold(15.0), t.text);
    ui.painter().text(pos2(head.right() - PAD, head.center().y), Align2::RIGHT_CENTER, people.len().to_string(), t.font(13.0), t.text_dim);
    // the filters narrowing the list (a date, a keyword…), removable here
    let chips = lightcraft_engine::filter_chips(&app.session.filter, &app.session.catalog);
    super::chips::show(app, ui, &chips);
    // what is selected below, and the box to name it with
    super::unnamed::naming_bar(app, ui);
    let unnamed = super::unnamed::faces(app, now);
    if people.is_empty() && unnamed.faces.is_empty() {
        let (title, body) = if chips.is_empty() {
            ("No people yet", "Face names written to XMP by Lightroom and other apps show up here, and faces found in your photos can be named.")
        } else {
            ("No named people in these photos", "Remove a filter above, or choose Clear all")
        };
        super::empty_message(ui, ui.available_rect_before_wrap(), title, body);
        return;
    }
    let ppp = ui.ctx().pixels_per_point();
    let active = app.session.filter.person.clone();
    let (edge, tile) = (card_edge(thumb), tile_edge(thumb));
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let cols = columns(width, edge);
        let row_h = edge + NAME_H + GAP;
        let rows = people.len().div_ceil(cols);
        let named_h = if people.is_empty() { 40.0 } else { PAD + rows as f32 * row_h };
        let (tile_cols, tile_row) = (columns(width, tile), tile + GAP);
        let tile_rows = unnamed.faces.len().div_ceil(tile_cols);
        let section_top = named_h + 6.0;
        let (area, _) = ui.allocate_exact_size(vec2(width, section_top + SECTION_H + tile_rows as f32 * tile_row + PAD), Sense::hover());
        let visible = |top: f32, rows: usize, row_h: f32| {
            let first = ((viewport.top() - top) / row_h).floor().max(0.0) as usize;
            let last = (((viewport.bottom() - top) / row_h).ceil().max(0.0) as usize).min(rows);
            first..last
        };
        if people.is_empty() {
            ui.painter().text(
                pos2(area.left() + PAD, area.top() + 24.0),
                Align2::LEFT_CENTER,
                "No named people yet. Name the faces below to start.",
                t.font(13.0),
                t.text_dim,
            );
        }
        for row in visible(PAD, rows, row_h) {
            for col in 0..cols {
                let Some(person) = people.get(row * cols + col) else { break };
                let min = area.min + vec2(PAD + col as f32 * (edge + GAP), PAD + row as f32 * row_h);
                let selected = active.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(&person.name));
                card(app, ui, person, Rect::from_min_size(min, vec2(edge, edge + NAME_H)), edge, ppp, selected);
            }
        }
        // the faces nobody has named
        let top = area.top() + section_top;
        ui.painter().line_segment([pos2(area.left() + PAD, top), pos2(area.right() - PAD, top)], Stroke::new(1.0, t.button_border));
        super::unnamed::section_header(app, ui, &unnamed, pos2(area.left() + PAD, top + 14.0), area.right() - PAD);
        let grid_top = section_top + SECTION_H;
        for row in visible(grid_top, tile_rows, tile_row) {
            for col in 0..tile_cols {
                let i = row * tile_cols + col;
                let Some(face) = unnamed.faces.get(i) else { break };
                let min = area.min + vec2(PAD + col as f32 * (tile + GAP), grid_top + row as f32 * tile_row);
                super::unnamed::tile(app, ui, &unnamed, i, Rect::from_min_size(min, vec2(tile, tile)), face, ppp);
            }
        }
    });
}

fn card(app: &mut LightcraftApp, ui: &mut egui::Ui, person: &Person, r: Rect, edge: f32, ppp: f32, selected: bool) {
    let t = Tokens::get(ui.ctx());
    let face = Rect::from_min_size(r.min, vec2(edge, edge));
    let resp = ui.interact(r, egui::Id::new(("person-card", &person.name)), Sense::click());
    register(ui.ctx(), format!("person:{}", person.name), r);
    let view = app.session.face_view(person.photo, person.face);
    let p = ui.painter();
    p.rect_filled(face, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(person.photo, view, (edge * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.tex.id(), face, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // how many photos they are in, on the picture
    let count = if person.count > 999 { "999+".to_string() } else { person.count.to_string() };
    let g = p.layout_no_wrap(count, t.font(11.5), Color32::WHITE);
    let pill = Rect::from_min_size(pos2(face.right() - g.size().x - 14.0, face.bottom() - g.size().y - 11.0), g.size() + vec2(10.0, 6.0));
    p.rect_filled(pill, 4.0, Color32::from_black_alpha(175));
    p.galley(pill.min + vec2(5.0, 3.0), g, Color32::WHITE);
    if selected {
        p.rect_stroke(face, 3.0, Stroke::new(2.0, Color32::WHITE), StrokeKind::Outside);
    } else if resp.hovered() {
        p.rect_stroke(face, 3.0, Stroke::new(1.0, t.text_dim), StrokeKind::Outside);
    }
    // a long name must not run past the card
    let name = fit(p, &person.name, t.semibold(13.0), edge);
    p.text(pos2(r.left() + 2.0, face.bottom() + 14.0), Align2::LEFT_CENTER, name, t.semibold(13.0), t.text);
    let photos = if person.count == 1 { "1 photo".to_string() } else { format!("{} photos", person.count) };
    if resp.on_hover_text(format!("{} · {photos} · click for their faces", person.name)).clicked() {
        let _ = app.run("view.person", json!({"name": person.name}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_follow_the_thumbnail_slider_within_their_own_limits() {
        assert_eq!((card_edge(90.0), card_edge(480.0)), (84.0, 260.0));
        assert_eq!((tile_edge(90.0), tile_edge(480.0)), (56.0, 180.0));
        // between, growing with the slider; beyond its ends, held at the limits; nonsense, a middle size
        assert!(card_edge(220.0) > card_edge(150.0) && tile_edge(300.0) > tile_edge(220.0));
        assert_eq!((card_edge(-5.0), card_edge(10_000.0)), (84.0, 260.0));
        assert!(card_edge(f32::NAN) > 84.0 && card_edge(f32::NAN) < 260.0);
        // there is always at least one column, whatever the width
        assert_eq!((columns(0.0, 260.0), columns(-50.0, 260.0), columns(f32::NAN, 100.0)), (1, 1, 1));
        assert!(columns(1400.0, 84.0) > columns(1400.0, 260.0));
    }
}
