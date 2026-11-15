//! The unnamed faces in the People view: every face nobody has named, as cropped pictures, with look-alikes next to each
//! other (once recognition has looked at them). Click faces to select them (Shift-click selects a range), type a name in
//! the bar above, press Enter: they are all named at once, in one undo step. A face the named ones recognise carries the
//! name they suggest; clicking that name accepts it for that face alone.
//!
//! The engine does the looking (`faces.unnamed`) and the naming (`faces.nameFaces`); this draws and selects.

use std::sync::Arc;

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// One unnamed face: which photo and region, the box to show it by, and a suggested name.
#[derive(Clone, Debug, PartialEq)]
pub struct UnnamedFace {
    pub photo: u64,
    pub index: usize,
    /// The face's own box in the photo: what a command about it sends, so the engine can refuse a face that changed.
    pub rect: lcc_geom::Rect,
    /// The box to show it by (a little wider than `rect`, a recognition model's own crop where it has one).
    pub view: lcc_geom::Rect,
    pub suggestion: Option<String>,
    /// Its look-alike group (#259), once recognition has compared it: faces sharing one can be selected at once.
    pub group: Option<usize>,
}

/// One person's suggested faces (issue #1050): the faces the named ones recognise as them, most alike first, so one
/// person's suggestions are reviewed (confirmed or rejected) together.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuggestionGroup {
    /// The person suggested, as the faces named them.
    pub name: String,
    /// How many of the listed faces are in this group (`faces.len()`, a face never in two).
    pub total: usize,
    pub faces: Vec<UnnamedFace>,
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
    /// The listed faces that carry a suggestion, grouped under the person it suggests (`faces.unnamed {bySuggestion:
    /// true}`), the most suggested faces first. Empty when it was not asked for.
    pub groups: Vec<SuggestionGroup>,
    /// How many listed faces carry no suggestion because the user rejected that person for them.
    pub rejected: usize,
}

fn rect_of(v: &Value) -> Option<lcc_geom::Rect> {
    Some(lcc_geom::Rect { x0: v["x0"].as_f64()?, y0: v["y0"].as_f64()?, x1: v["x1"].as_f64()?, y1: v["y1"].as_f64()? })
}

fn face_of(v: &Value) -> Option<UnnamedFace> {
    Some(UnnamedFace {
        photo: v["photo"].as_u64()?,
        index: usize::try_from(v["index"].as_u64()?).ok()?,
        rect: rect_of(&v["rect"]).or_else(|| rect_of(&v["view"]))?,
        view: rect_of(&v["view"]).or_else(|| rect_of(&v["rect"]))?,
        suggestion: v["suggestion"]["name"].as_str().map(str::to_string),
        group: v["group"].as_u64().and_then(|g| usize::try_from(g).ok()),
    })
}

fn count(v: &Value) -> usize {
    v.as_u64().map_or(0, |n| usize::try_from(n).unwrap_or(0))
}

/// The engine's answer as a list (anything that does not make sense is left out).
pub fn parse(v: &Value) -> Unnamed {
    let groups = v["groups"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|g| {
                    let name = g["name"].as_str().map(str::trim).filter(|n| !n.is_empty())?;
                    let mut faces: Vec<UnnamedFace> = g["faces"].as_array().map(|a| a.iter().filter_map(face_of).collect()).unwrap_or_default();
                    // a group says who it is about, even where the faces carry only a score
                    for face in &mut faces {
                        face.suggestion.get_or_insert_with(|| name.to_string());
                    }
                    Some(SuggestionGroup { name: name.to_string(), total: faces.len().max(count(&g["total"])), faces })
                })
                .collect()
        })
        .unwrap_or_default();
    Unnamed {
        total: count(&v["total"]),
        faces: v["faces"].as_array().map(|a| a.iter().filter_map(face_of).collect()).unwrap_or_default(),
        ordered: v["ordered"] == true,
        ready: v["ready"] == true,
        groups,
        rejected: count(&v["rejected"]),
    }
}


/// The unnamed faces, asked for again only when the catalog changed, or (while the scan runs and learns more faces) at
/// most every few seconds, since putting look-alikes together looks at every pair.
pub fn faces(app: &mut LccApp, now: f64) -> Arc<Unnamed> {
    super::person::follow_filter(app);
    let (rev, indexed) = (app.session.catalog.revision, app.caches.faces_indexed);
    if let Some((r, i, at, list)) = &app.caches.unnamed
        && *r == rev
        && (*i == indexed || now - *at < 3.0)
    {
        return list.clone();
    }
    let v = app.session.execute("faces.unnamed", &json!({"scope": "filter", "bySuggestion": true})).unwrap_or(Value::Null);
    let list = Arc::new(parse(&v));
    // what was selected and has since been named (or removed) is no longer selected; nor is a decision about a face
    // that has left the list (a confirmed one is named by now, and the name is what keeps it out)
    let present: std::collections::HashSet<(u64, usize)> = list.faces.iter().map(|f| (f.photo, f.index)).collect();
    app.ui.unnamed_selected.retain(|k| present.contains(k));
    app.ui.confirmed_suggestions.retain(|k| present.contains(k));
    app.ui.rejected_suggestions.retain(|k| present.contains(k));
    app.caches.unnamed = Some((rev, indexed, now, list.clone()));
    list
}

/// A click on face number `i` of the faces on screen (`visible`, a category's own slice of the listing, or the whole
/// listing): select it (or deselect it), or with Shift every face from the last one clicked.
fn click(app: &mut LccApp, visible: &[UnnamedFace], i: usize, shift: bool) {
    let Some(f) = visible.get(i) else { return };
    match app.ui.unnamed_anchor.filter(|_| shift) {
        Some(a) => {
            for f in visible.get(a.min(i)..=a.max(i)).unwrap_or_default() {
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
fn name(app: &mut LccApp, faces: &[(u64, usize)], name: &str) {
    let list: Vec<Value> = faces.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
    let _ = app.run("faces.nameFaces", json!({"faces": list, "name": name}));
}

/// The bar above the faces while some are selected: how many, a box to name them all, and the people already named.
pub fn naming_bar(app: &mut LccApp, ui: &mut egui::Ui) {
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
    let mut ignore = false;
    ui.horizontal(|ui| {
        ui.add_space(super::people::PAD - 4.0);
        let n = selected.len();
        ui.label(RichText::new(if n == 1 { "1 face selected".to_string() } else { format!("{n} faces selected") }).color(t.text));
        let hint = suggested.clone().unwrap_or_else(|| "Name…".to_string());
        let field = crate::text_field::TextField::singleline("field:unnamedName", &mut app.ui.unnamed_name).hint(hint).width(240.0).show(ui);
        let edit = &field.response;
        if std::mem::take(&mut app.ui.unnamed_focus) {
            edit.request_focus();
        }
        let typed = app.ui.unnamed_name.trim().to_string();
        let chosen = if typed.is_empty() { suggested.clone().unwrap_or_default() } else { typed.clone() };
        if field.ending == Some(crate::text_field::Ending::Return) && !chosen.is_empty() {
            submit = Some(chosen.clone());
        }
        let label = if n == 1 {
            crate::i18n::tr("Name face").to_string()
        } else {
            crate::i18n::tr_format!("Name {} faces", n).to_string()
        };
        let b = ui.add_enabled(!chosen.is_empty(), egui::Button::new(label));
        register(ui.ctx(), "unnamed:name", b.rect);
        if b.clicked() {
            submit = Some(chosen);
        }
        let ig = ui.button(crate::i18n::tr("Ignore")).on_hover_text(crate::i18n::tr("Leave these faces out of People; they are not found again"));
        register(ui.ctx(), "unnamed:ignore", ig.rect);
        ignore = ig.clicked();
        let c = ui.button(crate::i18n::tr("Clear"));
        register(ui.ctx(), "unnamed:clear", c.rect);
        clear = c.clicked() || field.cancelled();
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
    } else if ignore {
        let list: Vec<Value> = selected.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
        let _ = app.run("faces.ignore", json!({"faces": list}));
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    } else if clear {
        app.ui.unnamed_selected.clear();
        app.ui.unnamed_name.clear();
        app.ui.unnamed_anchor = None;
    }
}

/// One face tile: face `i` of the faces on screen (`visible` — a category's slice of `list`, or all of it), drawn at `r`.
pub fn tile(app: &mut LccApp, ui: &mut egui::Ui, list: &Unnamed, visible: &[UnnamedFace], i: usize, r: Rect, ppp: f32) -> egui::Response {
    let keep;
    let f = match visible.get(i) {
        Some(f) => f,
        None => {
            keep = UnnamedFace {
                photo: 0,
                index: 0,
                rect: lcc_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
                view: lcc_geom::Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 },
                suggestion: None,
                group: None,
            };
            &keep
        }
    };
    let t = Tokens::get(ui.ctx());
    let key = (f.photo, f.index);
    let selected = app.ui.unnamed_selected.contains(&key);
    let resp = ui.interact(r, egui::Id::new(("unnamed-face", f.photo, f.index)), Sense::click());
    crate::widgets::announce_selected(&resp, crate::i18n::tr("Unnamed face"), selected);
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
    if let Some(job) = app.session.face_job(lcc_catalog::PhotoId(f.photo), f.view, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // which face this tile is about, when another shares it (#1147)
    super::face_preview::outline(app, p, r, f.photo, f.view);
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
        && a.clone().on_hover_text(crate::i18n::tr_format!("Name this face {who}", who = who)).clicked()
    {
        name(app, &[key], who);
        return resp;
    }
    // hovered: the whole photo, this face marked (#1147)
    super::face_preview::on_hover(app, &resp, f.photo, f.view, None);
    // a double click opens the photo, as in the grid; its second click must not undo the first one's selection
    if resp.double_clicked() {
        let _ = app.run("library.select", json!({"ids": [f.photo], "active": f.photo}));
        let _ = app.run("view.detail", json!({}));
        // Escape comes back to the faces
        app.ui.person_from = Some((None, f.photo));
    } else if resp.clicked() {
        let shift = ui.input(|i| i.modifiers.shift);
        click(app, visible, i, shift);
    }
    // its look-alikes, selected at once to be named or ignored together (#259); or this face, ignored
    let alikes = look_alikes(list, f);
    crate::widgets::context_menu(&resp, |ui| {
        let n = alikes.len();
        let label = crate::i18n::tr_format!("Select {n} look-alikes", n = n);
        let b = ui.add_enabled(n > 1, egui::Button::new(label));
        register(ui.ctx(), "unnamedMenu:selectGroup", b.rect);
        if b.clicked() {
            app.ui.unnamed_selected.extend(alikes.iter().copied());
            app.ui.unnamed_focus = true;
            ui.close();
        }
        let b = ui.button(crate::i18n::tr("Ignore this face"));
        register(ui.ctx(), "unnamedMenu:ignore", b.rect);
        if b.clicked() {
            let _ = app.run("faces.ignore", json!({"faces": [{"photo": f.photo, "index": f.index}]}));
            app.ui.unnamed_selected.remove(&key);
            ui.close();
        }
    });
    resp
}
/// One face of a category of the People view: [`tile`], for a category showing its own faces (its own order and
/// selection) in place of the whole unnamed list.
pub fn tile_at(app: &mut LccApp, ui: &mut egui::Ui, list: &Unnamed, visible: &[UnnamedFace], i: usize, r: Rect, ppp: f32) {
    let _ = tile(app, ui, list, visible, i, r, ppp);
}

/// The faces in `f`'s look-alike group (`f` among them), or just `f` when it has none yet.
fn look_alikes(list: &Unnamed, f: &UnnamedFace) -> Vec<(u64, usize)> {
    match f.group {
        Some(g) => list.faces.iter().filter(|o| o.group == Some(g)).map(|o| (o.photo, o.index)).collect(),
        None => vec![(f.photo, f.index)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_faces_look_alikes_are_its_group() {
        let v = json!({"total": 4, "faces": [
            {"photo": 1, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "group": 0},
            {"photo": 2, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "group": 0},
            {"photo": 3, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}, "group": 1},
            {"photo": 4, "index": 0, "rect": {"x0": 0.1, "y0": 0.1, "x1": 0.3, "y1": 0.4}},
        ]});
        let list = parse(&v);
        assert_eq!(look_alikes(&list, &list.faces[0]), vec![(1, 0), (2, 0)]);
        assert_eq!(look_alikes(&list, &list.faces[2]), vec![(3, 0)]);
        assert_eq!(look_alikes(&list, &list.faces[3]), vec![(4, 0)], "no group yet: the face alone");
    }

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
        assert_eq!(list.faces[0].rect.x0, 0.1, "the face's own box is kept for the commands that check it");
        assert_eq!((list.faces[1].view.x0, list.faces[1].suggestion.clone()), (0.1, None), "else the face's own box");
        assert_eq!(list.faces[0].group, None);
        for bad in [Value::Null, json!([]), json!({"faces": 5, "total": -1})] {
            assert_eq!(parse(&bad), Unnamed::default(), "{bad}");
        }
    }
}
