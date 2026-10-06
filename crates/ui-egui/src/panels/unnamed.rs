//! The unnamed faces in the People view: every face nobody has named, as cropped pictures, with look-alikes next to each
//! other (once recognition has looked at them). Click faces to select them (Shift-click selects a range), type a name in
//! the bar above, press Enter: they are all named at once, in one undo step. A face the named ones recognise carries the
//! name they suggest; clicking that name accepts it for that face alone.
//!
//! The engine does the looking (`faces.unnamed`) and the naming (`faces.nameFaces`); this draws and selects.

use std::sync::Arc;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// One unnamed face: which photo and region, the box to show it by, and a suggested name.
#[derive(Clone, Debug, PartialEq)]
pub struct UnnamedFace {
    pub photo: u64,
    pub index: usize,
    pub view: lightcraft_geom::Rect,
    pub suggestion: Option<String>,
}

/// What `faces.unnamed` said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Unnamed {
    /// Unnamed faces in the library (more than are listed when the list was cut).
    pub total: usize,
    pub faces: Vec<UnnamedFace>,
    /// Look-alikes are next to each other.
    pub ordered: bool,
    /// Recognition is running.
    pub ready: bool,
}

fn rect_of(v: &Value) -> Option<lightcraft_geom::Rect> {
    Some(lightcraft_geom::Rect { x0: v["x0"].as_f64()?, y0: v["y0"].as_f64()?, x1: v["x1"].as_f64()?, y1: v["y1"].as_f64()? })
}

fn face_of(v: &Value) -> Option<UnnamedFace> {
    Some(UnnamedFace {
        photo: v["photo"].as_u64()?,
        index: usize::try_from(v["index"].as_u64()?).ok()?,
        view: rect_of(&v["view"]).or_else(|| rect_of(&v["rect"]))?,
        suggestion: v["suggestion"]["name"].as_str().map(str::to_string),
    })
}

/// The engine's answer as a list (anything that does not make sense is left out).
pub fn parse(v: &Value) -> Unnamed {
    Unnamed {
        total: v["total"].as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0)),
        faces: v["faces"].as_array().map(|a| a.iter().filter_map(face_of).collect()).unwrap_or_default(),
        ordered: v["ordered"] == true,
        ready: v["ready"] == true,
    }
}

/// The unnamed faces, asked for again only when the catalog changed, or (while the scan runs and learns more faces) at
/// most every few seconds, since putting look-alikes together looks at every pair.
pub fn faces(app: &mut LightcraftApp, now: f64) -> Arc<Unnamed> {
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((r, i, at, list)) = &app.caches.unnamed
        && *r == rev
        && (*i == indexed || now - *at < 3.0)
    {
        return list.clone();
    }
    let v = app.session.execute("faces.unnamed", &json!({})).unwrap_or(Value::Null);
    let list = Arc::new(parse(&v));
    // what was selected and has since been named (or removed) is no longer selected
    let present: std::collections::HashSet<(u64, usize)> = list.faces.iter().map(|f| (f.photo, f.index)).collect();
    app.ui.unnamed_selected.retain(|k| present.contains(k));
    app.caches.unnamed = Some((rev, indexed, now, list.clone()));
    list
}

/// A click on face number `i` of `list`: select it (or deselect it), or with Shift every face from the last one clicked.
fn click(app: &mut LightcraftApp, list: &Unnamed, i: usize, shift: bool) {
    let Some(f) = list.faces.get(i) else { return };
    match app.ui.unnamed_anchor.filter(|_| shift) {
        Some(a) => {
            for f in list.faces.get(a.min(i)..=a.max(i)).unwrap_or_default() {
                app.ui.unnamed_selected.insert((f.photo, f.index));
            }
        }
        None => {
            let key = (f.photo, f.index);
            if !app.ui.unnamed_selected.remove(&key) {
                app.ui.unnamed_selected.insert(key);
            }
            app.ui.unnamed_anchor = Some(i);
        }
    }
    // typing goes to the name box at once
    app.ui.unnamed_focus = !app.ui.unnamed_selected.is_empty();
}

/// Name `faces` (photo, region) `name`, as one undo step.
fn name(app: &mut LightcraftApp, faces: &[(u64, usize)], name: &str) {
    let list: Vec<Value> = faces.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    let _ = app.run("faces.nameFaces", json!({"faces": list, "name": name}));
}

/// The bar above the faces while some are selected: how many, a box to name them all, and the people already named.
pub fn naming_bar(app: &mut LightcraftApp, ui: &mut egui::Ui) {
    if app.ui.unnamed_selected.is_empty() {
        app.ui.unnamed_name.clear();
        return;
    }
    let t = Tokens::get(ui.ctx());
    let selected: Vec<(u64, usize)> = {
        let mut v: Vec<_> = app.ui.unnamed_selected.iter().copied().collect();
        v.sort_unstable();
        v
    };
    // the name the named faces suggest, when every selected face has the same one
    let suggested: Option<String> = {
        let list = app.caches.unnamed.as_ref().map(|c| c.3.clone());
        list.and_then(|l| {
            let names: std::collections::BTreeSet<Option<&str>> =
                l.faces.iter().filter(|f| app.ui.unnamed_selected.contains(&(f.photo, f.index))).map(|f| f.suggestion.as_deref()).collect();
            (names.len() == 1).then(|| names.into_iter().next().flatten().map(str::to_string)).flatten()
        })
    };
    let mut submit: Option<String> = None;
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.add_space(super::people::PAD - 4.0);
        let n = selected.len();
        ui.label(RichText::new(if n == 1 { "1 face selected".to_string() } else { format!("{n} faces selected") }).color(t.text));
        let hint = suggested.clone().unwrap_or_else(|| "Name…".to_string());
        let edit = ui.add(egui::TextEdit::singleline(&mut app.ui.unnamed_name).hint_text(hint).desired_width(240.0));
        register(ui.ctx(), "field:unnamedName", edit.rect);
        if std::mem::take(&mut app.ui.unnamed_focus) {
            edit.request_focus();
        }
        let typed = app.ui.unnamed_name.trim().to_string();
        let chosen = if typed.is_empty() { suggested.clone().unwrap_or_default() } else { typed.clone() };
        if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !chosen.is_empty() {
            submit = Some(chosen.clone());
        }
        let label = if n == 1 { "Name face".to_string() } else { format!("Name {n} faces") };
        let b = ui.add_enabled(!chosen.is_empty(), egui::Button::new(label));
        register(ui.ctx(), "unnamed:name", b.rect);
        if b.clicked() {
            submit = Some(chosen);
        }
        let c = ui.button("Clear");
        register(ui.ctx(), "unnamed:clear", c.rect);
        clear = c.clicked() || (edit.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)));
    });
    // the people already named, matching what is typed (a click names the faces)
    let typed = app.ui.unnamed_name.trim().to_lowercase();
    if !typed.is_empty() {
        let people = app.caches.person_names(&app.session.catalog);
        let matching: Vec<&String> = people.iter().filter(|p| p.to_lowercase().starts_with(&typed) && p.to_lowercase() != typed).take(6).collect();
        if !matching.is_empty() {
            ui.horizontal(|ui| {
                ui.add_space(super::people::PAD - 4.0);
                for p in matching {
                    let b = ui.add(egui::Button::new(RichText::new(p.as_str()).color(t.text_label)).frame(false));
                    register(ui.ctx(), format!("unnamed:pick:{p}"), b.rect);
                    if b.clicked() {
                        submit = Some(p.clone());
                    }
                }
            });
        }
    }
    if let Some(who) = submit {
        name(app, &selected, &who);
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    } else if clear {
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    }
}

/// The heading of the section, at `at`, and what it says about where the faces come from.
pub fn section_header(app: &mut LightcraftApp, ui: &mut egui::Ui, list: &Unnamed, at: egui::Pos2, right: f32) {
    let t = Tokens::get(ui.ctx());
    let count = if list.total > list.faces.len() { format!("{} of {}", list.faces.len(), list.total) } else { list.total.to_string() };
    let p = ui.painter();
    p.text(at, Align2::LEFT_CENTER, "Unnamed faces", t.semibold(15.0), t.text);
    p.text(pos2(right, at.y), Align2::RIGHT_CENTER, count, t.font(13.0), t.text_dim);
    let sub = if !list.ready && list.faces.is_empty() {
        "Turn on face recognition in Settings to find the faces in your photos.".to_string()
    } else if list.faces.is_empty() && app.caches.faces_pending > 0 {
        format!("Looking through your photos… {} left", app.caches.faces_pending)
    } else if list.faces.is_empty() {
        "No unnamed faces. Faces found in your photos appear here until you name them.".to_string()
    } else if list.ordered {
        "Faces that look alike are next to each other. Click to select, Shift-click for a range, then type a name.".to_string()
    } else {
        "Click faces to select them, Shift-click for a range, then type a name.".to_string()
    };
    p.text(pos2(at.x, at.y + 24.0), Align2::LEFT_CENTER, sub, t.font(12.0), t.text_dim);
    if !list.ready && list.faces.is_empty() {
        let r = Rect::from_min_size(pos2(at.x, at.y + 38.0), vec2(120.0, 22.0));
        let b = ui.put(r, egui::Button::new("Open Settings"));
        register(ui.ctx(), "unnamed:openSettings", b.rect);
        if b.clicked() {
            let _ = app.run("app.settings", json!({"tab": "faces"}));
        }
    } else if !list.faces.is_empty() {
        let r = Rect::from_min_size(pos2(right - 130.0, at.y + 12.0), vec2(130.0, 22.0));
        let all = app.ui.unnamed_selected.len() == list.faces.len();
        let b = ui.put(r, egui::Button::new(if all { "Select none" } else { "Select all" }));
        register(ui.ctx(), "unnamed:selectAll", b.rect);
        if b.clicked() {
            app.ui.unnamed_selected.clear();
            if !all {
                app.ui.unnamed_selected.extend(list.faces.iter().map(|f| (f.photo, f.index)));
                app.ui.unnamed_focus = true;
            }
        }
    }
}

/// One face tile, number `i` of `list`.
pub fn tile(app: &mut LightcraftApp, ui: &mut egui::Ui, list: &Unnamed, i: usize, r: Rect, f: &UnnamedFace, ppp: f32) {
    let t = Tokens::get(ui.ctx());
    let key = (f.photo, f.index);
    let selected = app.ui.unnamed_selected.contains(&key);
    let resp = ui.interact(r, egui::Id::new(("unnamed-face", f.photo, f.index)), Sense::click());
    register(ui.ctx(), format!("unnamed-face:{}:{}", f.photo, f.index), r);
    // a suggested name along the bottom: a click accepts it for this face (added after the tile, so it wins there)
    let strip = Rect::from_min_max(pos2(r.left(), r.bottom() - 20.0), r.right_bottom());
    let accept = f.suggestion.as_ref().map(|_| ui.interact(strip, egui::Id::new(("unnamed-accept", f.photo, f.index)), Sense::click()));
    if let Some(a) = &accept {
        register(ui.ctx(), format!("unnamed-accept:{}:{}", f.photo, f.index), a.rect);
    }
    let hovered = resp.hovered() || accept.as_ref().is_some_and(|a| a.hovered());
    let p = ui.painter();
    p.rect_filled(r, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lightcraft_catalog::PhotoId(f.photo), f.view, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    if selected {
        p.rect_filled(r, 3.0, Color32::from_rgba_unmultiplied(70, 130, 255, 55));
        p.rect_stroke(r, 3.0, Stroke::new(2.0, Color32::from_rgb(110, 160, 255)), StrokeKind::Inside);
        // a check at the top left
        let c = pos2(r.left() + 13.0, r.top() + 13.0);
        p.circle_filled(c, 9.0, Color32::from_rgb(70, 130, 255));
        p.line_segment([pos2(c.x - 4.0, c.y), pos2(c.x - 1.0, c.y + 3.5)], Stroke::new(1.8, Color32::WHITE));
        p.line_segment([pos2(c.x - 1.0, c.y + 3.5), pos2(c.x + 4.5, c.y - 3.5)], Stroke::new(1.8, Color32::WHITE));
    } else if hovered {
        p.rect_stroke(r, 3.0, Stroke::new(1.0, Color32::WHITE), StrokeKind::Inside);
    }
    if let Some(name) = &f.suggestion {
        p.rect_filled(strip, 0.0, Color32::from_black_alpha(165));
        let label = super::people::fit(p, name, t.font(11.5), r.width() - 18.0);
        p.text(pos2(r.left() + 5.0, strip.center().y), Align2::LEFT_CENTER, format!("{label}?"), t.font(11.5), Color32::from_gray(225));
    }
    if let (Some(a), Some(who)) = (&accept, &f.suggestion)
        && a.clone().on_hover_text(format!("Name this face {who}")).clicked()
    {
        name(app, &[key], who);
        return;
    }
    if resp.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        click(app, list, i, shift);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engines_answer_becomes_a_list_and_odd_entries_are_left_out() {
        let v = json!({
            "total": 9, "ordered": true, "ready": true,
            "faces": [
                {"photo": 3, "index": 1, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "view": {"x0": 0.12, "y0": 0.12, "x1": 0.28, "y1": 0.38}, "suggestion": {"name": "Jane Doe", "score": 0.7}},
                {"photo": 4, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "suggestion": null},
                {"photo": 5, "index": 0},
                {"index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}},
            ],
        });
        let list = parse(&v);
        assert_eq!((list.total, list.ordered, list.ready, list.faces.len()), (9, true, true, 2));
        assert_eq!(list.faces[0].suggestion.as_deref(), Some("Jane Doe"));
        assert_eq!(list.faces[0].view.x0, 0.12, "the view box is used when there is one");
        assert_eq!((list.faces[1].view.x0, list.faces[1].suggestion.clone()), (0.1, None), "else the face's own box");
        for bad in [Value::Null, json!([]), json!({"faces": 5, "total": -1})] {
            assert_eq!(parse(&bad), Unnamed::default(), "{bad}");
        }
    }
}
