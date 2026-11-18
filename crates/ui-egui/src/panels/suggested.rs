//! The Suggested category of the People view (issue #1050): the unnamed faces recognition has matched to someone, under
//! the person the named faces suggest, so one person's suggestions are reviewed together instead of one face at a time.
//!
//! Each person suggested is a section: their name, how many faces wait under it, **Confirm all** and **Reject all**, and
//! then their faces as tiles, most alike first. On a tile **✓** confirms that face (it is named the person, in one undo
//! step, and remembered as confirmed), **✗** rejects it (it stays unknown — not ignored, still detected — and that person
//! is never suggested for it again), and **−** ignores the face entirely (`faces.ignore`, so a scan does not find it
//! again). The person's name opens their page, where the same decisions are offered face by face.
//!
//! Every decision is a suggestion confirmed or not: nothing is named without the user saying so. A confirmation is a
//! name on a face, so the catalog's undo takes it back; a rejection changes no photo, so the toast offers the way back
//! (`faces.undoSuggestionDecision`).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::json;

use super::people::{self, PAD};
use super::unnamed::{SuggestionGroup, Unnamed, UnnamedFace};
use crate::LccApp;
use crate::theme::Tokens;
use crate::widgets::register;

/// What a decision does with the faces it covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// They are them: name the faces.
    Confirm,
    /// They are not: keep the faces unknown, and never suggest this person for them again.
    Reject,
    /// Leave the faces out of People altogether.
    Ignore,
}

/// A decision about one person's suggested faces: all of them, or one.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub verdict: Verdict,
    pub person: String,
    pub faces: Vec<UnnamedFace>,
}

/// The suggested faces of `list` under the person suggested, the most faces first; a face in `decided` (about which the
/// session has already run a decision) is left out, since a confirmed one is named by now and a rejected one is not this
/// person's suggestion any more.
pub fn groups(list: &Unnamed, decided: &[(u64, usize)]) -> Vec<SuggestionGroup> {
    let mut out: Vec<SuggestionGroup> = Vec::new();
    for face in &list.faces {
        if decided.contains(&(face.photo, face.index)) {
            continue;
        }
        let Some(name) = face.suggestion.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
        match out.iter_mut().find(|g| g.name.eq_ignore_ascii_case(name)) {
            Some(group) => {
                group.faces.push(face.clone());
                group.total = group.faces.len();
            }
            None => out.push(SuggestionGroup { name: name.to_string(), total: 1, faces: vec![face.clone()] }),
        }
    }
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    out
}

/// The command `d` runs, with the faces it covers and the box each has in its photo (so a face that changed in the
/// meantime is refused rather than decided about unseen).
pub fn command(d: &Decision) -> (&'static str, serde_json::Value) {
    let faces: Vec<serde_json::Value> = d
        .faces
        .iter()
        .map(|f| {
            // the face's own box, which is what the engine checks against the region it has: a face that changed since
            // the list was read is refused, rather than named or rejected unseen
            let (x0, y0, x1, y1) = (f.rect.x0, f.rect.y0, f.rect.x1, f.rect.y1);
            json!({"photo": f.photo, "index": f.index, "rect": {"x0": x0, "y0": y0, "x1": x1, "y1": y1}})
        })
        .collect();
    let params = json!({"faces": faces});
    match d.verdict {
        Verdict::Confirm => ("faces.confirmSuggestions", params),
        Verdict::Reject => ("faces.rejectSuggestions", params),
        Verdict::Ignore => ("faces.ignore", params),
    }
}

/// What the toast after `d` says.
pub fn said(d: &Decision) -> String {
    let n = d.faces.len();
    let who = &d.person;
    // a decision over everybody suggested at once (the heading's Confirm all) names no one person
    if who.is_empty() {
        return match d.verdict {
            Verdict::Confirm => crate::i18n::tr_format!("Confirmed {} faces", n).to_string(),
            Verdict::Reject => crate::i18n::tr_format!("Rejected {} faces", n).to_string(),
            Verdict::Ignore => crate::i18n::tr_format!("Ignored {} faces", n).to_string(),
        };
    }
    match d.verdict {
        Verdict::Confirm if n == 1 => crate::i18n::tr_format!("Confirmed as {}", who).to_string(),
        Verdict::Confirm => crate::i18n::tr_format!("Confirmed {} faces as {}", n, who).to_string(),
        Verdict::Reject if n == 1 => crate::i18n::tr_format!("Not {}", who).to_string(),
        Verdict::Reject => crate::i18n::tr_format!("Rejected {} faces as {}", n, who).to_string(),
        Verdict::Ignore if n == 1 => crate::i18n::tr_format!("Ignored {} face", n).to_string(),
        Verdict::Ignore => crate::i18n::tr_format!("Ignored {} faces", n).to_string(),
    }
}

/// Runs `d`, then records it for the session (so the groups change at once, before the engine's next answer) and says
/// what happened; a rejection's toast offers to undo it, since a rejection is no photo edit and has no undo step.
pub fn apply(app: &mut LccApp, ctx: &egui::Context, d: Decision) {
    let (cmd, params) = command(&d);
    let keys: Vec<(u64, usize)> = d.faces.iter().map(|f| (f.photo, f.index)).collect();
    let reply = match app.run(cmd, params) {
        Ok(reply) => reply,
        Err(error) => {
            app.toast(ctx, error);
            return;
        }
    };
    // an ignored face leaves the unnamed list at the next answer, and the selection here at once
    if d.verdict == Verdict::Ignore {
        for key in &keys {
            app.ui.unnamed_selected.remove(key);
        }
        let text = said(&d);
        app.toast(ctx, text);
        ctx.request_repaint();
        return;
    }
    // how many faces the command changed: a face the named faces no longer suggest that person for is left unnamed
    // (`confirmed`) or armed with no decision (`rejected`), and one it passed over is not taken off the screen as though
    // it had been decided
    let field = if d.verdict == Verdict::Confirm { "confirmed" } else { "rejected" };
    let decided = reply[field].as_u64().map_or(d.faces.len(), |n| usize::try_from(n).unwrap_or(d.faces.len()));
    // the whole batch went through: it leaves the groups here at once, before the engine's next answer; a batch it
    // passed faces over for waits for the next answer, since a face whose suggestion changed still has one
    if decided == d.faces.len() {
        for key in &keys {
            if d.verdict == Verdict::Confirm {
                app.ui.confirmed_suggestions.insert(*key);
                app.ui.rejected_suggestions.remove(key);
            } else {
                app.ui.rejected_suggestions.insert(*key);
                app.ui.confirmed_suggestions.remove(key);
            }
        }
    }
    // and the toast says how many it took, rather than claiming them all
    let text = if decided < d.faces.len() {
        match d.verdict {
            Verdict::Reject => crate::i18n::tr_format!("Rejected {} of {} suggested faces", decided, d.faces.len()).to_string(),
            _ => crate::i18n::tr_format!("Named {} of {} suggested faces", decided, d.faces.len()).to_string(),
        }
    } else {
        said(&d)
    };
    if d.verdict == Verdict::Reject && decided > 0 {
        // a rejection changes no photo, so there is no undo step to reach for: the toast carries the way back
        let faces: Vec<serde_json::Value> = keys.iter().map(|(photo, index)| json!({"photo": photo, "index": index})).collect();
        app.toast_with_action(ctx, text, crate::i18n::tr("Undo"), "faces.undoSuggestionDecision", json!({"faces": faces}), 10.0);
    } else {
        app.toast(ctx, text);
    }
    ctx.request_repaint();
}

/// The Suggested category: a section per person suggested, their faces waiting under it.
pub fn show(app: &mut LccApp, ui: &mut egui::Ui) {
    let now = ui.input(|i| i.time);
    let ppp = ui.ctx().pixels_per_point();
    // until face recognition is set up: what it takes, and a button that does the next step
    super::faces::setup_banner(app, ui);
    // the filters narrowing the list (a date, a keyword…), removable here
    let chips = lcc_engine::filter_chips(&app.session.filter, &app.session.catalog);
    super::chips::show(app, ui, &chips);
    let list = super::unnamed::faces(app, now);
    let mut decided: Vec<(u64, usize)> = app.ui.confirmed_suggestions.iter().copied().collect();
    decided.extend(app.ui.rejected_suggestions.iter().copied());
    let groups = groups(&list, &decided);
    let waiting: usize = groups.iter().map(|g| g.total).sum();
    // the heading: how many people, how many faces; its one button confirms every suggestion shown
    let note = if groups.is_empty() {
        String::new()
    } else if groups.len() == 1 {
        crate::i18n::tr_format!("{} faces wait under 1 person", waiting).to_string()
    } else {
        crate::i18n::tr_format!("{} faces wait under {} people", waiting, groups.len()).to_string()
    };
    let action = (waiting > 0).then(|| {
        let label = if waiting == 1 {
            crate::i18n::tr("Confirm the suggestion").to_string()
        } else {
            crate::i18n::tr_format!("Confirm all {} suggestions", waiting).to_string()
        };
        (label, "suggested:confirmEvery".to_string())
    });
    let row = people::heading_row(ui);
    let mut decisions: Vec<Decision> = Vec::new();
    if people::heading(ui, row, &people::Heading { title: "Suggested people", count: groups.len(), note, action }).is_some() {
        // every suggested face at once, each named what it was suggested as, in one undo step
        let faces: Vec<UnnamedFace> = groups.iter().flat_map(|g| g.faces.iter().cloned()).collect();
        decisions.push(Decision { verdict: Verdict::Confirm, person: String::new(), faces });
    }
    if groups.is_empty() {
        // nothing to review: why, and where the faces are instead (the message box translates its own two texts)
        if !list.ready {
            super::empty_message(
                ui,
                ui.available_rect_before_wrap(),
                "No suggested faces",
                "Turn on face recognition in Settings ▸ Faces: the faces that look like the people you have named then wait here.",
            );
        } else if list.rejected > 0 {
            super::empty_message(
                ui,
                ui.available_rect_before_wrap(),
                "No suggested faces",
                "Every suggestion was rejected: a rejected face stays unknown, and is not suggested as that person again.",
            );
        } else {
            super::empty_message(
                ui,
                ui.available_rect_before_wrap(),
                "No suggested faces",
                "The faces in your photos that look like the people you have named wait here, to be confirmed one by one.",
            );
        }
        return;
    }
    let tile = people::tile_edge(app.ui.thumb_size);
    egui::ScrollArea::vertical().auto_shrink(false).show_viewport(ui, |ui, viewport| {
        let width = ui.available_width();
        let cols = people::columns(width, tile);
        // what each person's section takes: its heading, then their faces
        let mut tops = Vec::with_capacity(groups.len());
        let mut y = 0.0;
        for g in &groups {
            tops.push(y);
            let rows = g.faces.len().div_ceil(cols);
            y += people::SECTION_H + rows as f32 * (tile + people::TILE_GAP) + PAD;
        }
        let (area, _) = ui.allocate_exact_size(vec2(width, y), Sense::hover());
        let mut shown = 0;
        for (gi, group) in groups.iter().enumerate() {
            let top = area.top() + tops.get(gi).copied().unwrap_or(0.0);
            let rows = group.faces.len().div_ceil(cols);
            // only the sections on screen (a library can hold hundreds of them)
            if top + people::SECTION_H + rows as f32 * (tile + people::TILE_GAP) < viewport.top() + area.top() || top - area.top() > viewport.bottom()
            {
                continue;
            }
            let heading_rect = Rect::from_min_size(pos2(area.left(), top), vec2(width, people::SECTION_H));
            let h = people::Heading {
                title: &group.name,
                count: group.total,
                note: crate::i18n::tr_format!("Suggested as {}. Confirm the faces that are them.", group.name).to_string(),
                action: Some((crate::i18n::tr_format!("Confirm all {}", group.total).to_string(), format!("suggested:confirmAll:{}", group.name))),
            };
            if people::heading(ui, heading_rect, &h).is_some() {
                decisions.push(Decision { verdict: Verdict::Confirm, person: group.name.clone(), faces: group.faces.clone() });
            }
            // Reject all for every face of theirs, left of the heading's Confirm all (so neither covers a count or a name)
            let reject_all = Rect::from_min_size(
                pos2(heading_rect.right() - PAD - people::HEADING_BUTTON_W * 2.0 - 8.0, heading_rect.top() + 11.0),
                vec2(people::HEADING_BUTTON_W, 22.0),
            );
            let b = ui.put(reject_all, egui::Button::new(crate::i18n::tr_format!("Reject all {}", group.total)));
            register(ui.ctx(), format!("suggested:rejectAll:{}", group.name), b.rect);
            let b = b.on_hover_text(crate::i18n::tr_format!("None of these {} faces is {}", group.total, group.name));
            if b.clicked() {
                decisions.push(Decision { verdict: Verdict::Reject, person: group.name.clone(), faces: group.faces.clone() });
            }
            for (i, face) in group.faces.iter().enumerate() {
                let (row, col) = (i / cols, i % cols);
                let min = pos2(
                    area.left() + PAD + col as f32 * (tile + people::TILE_GAP),
                    top + people::SECTION_H + row as f32 * (tile + people::TILE_GAP),
                );
                shown += 1;
                if let Some(verdict) = self::tile(app, ui, group, face, Rect::from_min_size(min, vec2(tile, tile)), ppp) {
                    decisions.push(Decision { verdict, person: group.name.clone(), faces: vec![face.clone()] });
                }
            }
        }
        // the picture cache keeps all of them (a fixed budget would evict and re-request the same few every frame)
        app.renderer.want_variants(shown);
    });
    for d in decisions {
        apply(app, ui.ctx(), d);
    }
}

/// One suggested face: its picture, who it was suggested as along the bottom, and, while it is pointed at, **✓** at the
/// right, **✗** under it and **−** at the left. Returns what was clicked, if anything.
fn tile(app: &mut LccApp, ui: &mut egui::Ui, group: &SuggestionGroup, f: &UnnamedFace, r: Rect, ppp: f32) -> Option<Verdict> {
    let t = Tokens::get(ui.ctx());
    let resp = ui.interact(r, egui::Id::new(("suggested-face", f.photo, f.index)), Sense::hover());
    crate::widgets::announce(&resp, crate::i18n::tr_format!("Suggested as {}", group.name));
    register(ui.ctx(), format!("suggested:face:{}:{}", f.photo, f.index), r);
    // the three controls, each found by a script whether it is drawn yet or not (they show while the tile is pointed at)
    let check = Rect::from_min_size(pos2(r.right() - 32.0, r.top() + 5.0), vec2(26.0, 26.0));
    let cross = Rect::from_min_size(pos2(r.right() - 32.0, r.top() + 36.0), vec2(26.0, 26.0));
    let ignore = Rect::from_min_size(pos2(r.left() + 5.0, r.top() + 5.0), vec2(22.0, 22.0));
    for (id, rect) in [
        (format!("suggested:confirm:{}:{}", f.photo, f.index), check),
        (format!("suggested:reject:{}:{}", f.photo, f.index), cross),
        (format!("suggested:ignore:{}:{}", f.photo, f.index), ignore),
    ] {
        register(ui.ctx(), id, rect);
    }
    let check_r = ui.interact(check, egui::Id::new(("suggested-confirm", f.photo, f.index)), Sense::click());
    let cross_r = ui.interact(cross, egui::Id::new(("suggested-reject", f.photo, f.index)), Sense::click());
    let ignore_r = ui.interact(ignore, egui::Id::new(("suggested-ignore", f.photo, f.index)), Sense::click());
    let hovered = resp.hovered() || check_r.hovered() || cross_r.hovered() || ignore_r.hovered();
    let p = ui.painter();
    p.rect_filled(r, 3.0, t.canvas);
    if let Some(job) = app.session.face_job(lcc_catalog::PhotoId(f.photo), f.view, (r.width() * ppp).ceil() as usize)
        && let Some(tex) = app.renderer.variant(job)
    {
        p.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    }
    // which face this tile is about, when another shares it (#1147)
    super::face_preview::outline(app, p, r, f.photo, f.view);
    if hovered {
        p.rect_stroke(r, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
    }
    // who it was suggested as, along the bottom edge
    let strip = Rect::from_min_max(pos2(r.left(), r.bottom() - 20.0), r.right_bottom());
    p.rect_filled(strip, 0.0, Color32::from_black_alpha(165));
    let label = people::fit(p, &group.name, t.font(11.5), r.width() - 18.0);
    p.text(pos2(r.left() + 5.0, strip.center().y), Align2::LEFT_CENTER, label, t.font(11.5), Color32::from_gray(225));
    if hovered {
        // ✓
        let c = check.center();
        p.circle_filled(c, 12.0, Color32::from_rgba_unmultiplied(30, 30, 30, 225));
        let marks = [pos2(c.x - 4.5, c.y + 0.2), pos2(c.x - 1.2, c.y + 3.6), pos2(c.x + 5.0, c.y - 3.8)];
        p.line_segment([marks[0], marks[1]], Stroke::new(2.0, Color32::from_rgb(120, 220, 140)));
        p.line_segment([marks[1], marks[2]], Stroke::new(2.0, Color32::from_rgb(120, 220, 140)));
        // ✗
        let x = cross.center();
        p.circle_filled(x, 12.0, Color32::from_rgba_unmultiplied(30, 30, 30, 225));
        p.line_segment([pos2(x.x - 4.0, x.y - 4.0), pos2(x.x + 4.0, x.y + 4.0)], Stroke::new(1.8, Color32::from_rgb(240, 140, 130)));
        p.line_segment([pos2(x.x - 4.0, x.y + 4.0), pos2(x.x + 4.0, x.y - 4.0)], Stroke::new(1.8, Color32::from_rgb(240, 140, 130)));
        // −
        let i = ignore.center();
        p.circle_filled(i, 10.0, Color32::from_rgba_unmultiplied(30, 30, 30, 225));
        p.line_segment([pos2(i.x - 4.5, i.y), pos2(i.x + 4.5, i.y)], Stroke::new(1.8, Color32::WHITE));
    }
    // what each control does, said on hover
    let tip = |r: &egui::Response, text: String| {
        let _ = r.clone().on_hover_text(text);
    };
    tip(&check_r, crate::i18n::tr_format!("It is {}: name this face", group.name));
    tip(&cross_r, crate::i18n::tr_format!("Not {}: keep this face unknown", group.name));
    tip(&ignore_r, crate::i18n::tr("Ignore this face, so it is not found again").to_string());
    // pointed at: the whole photo, this face marked (#1147)
    super::face_preview::on_hover(app, &resp, f.photo, f.view, None);
    if check_r.clicked() {
        return Some(Verdict::Confirm);
    }
    if cross_r.clicked() {
        return Some(Verdict::Reject);
    }
    if ignore_r.clicked() {
        return Some(Verdict::Ignore);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(photo: u64, index: usize, suggestion: Option<&str>) -> UnnamedFace {
        UnnamedFace {
            photo,
            index,
            rect: lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.4 },
            view: lcc_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.4 },
            suggestion: suggestion.map(str::to_string),
            group: None,
        }
    }

    fn listing(faces: Vec<UnnamedFace>) -> Unnamed {
        Unnamed { total: faces.len(), faces, ordered: true, ready: true, groups: Vec::new(), rejected: 0 }
    }

    /// Issue #1050: the groups hold the suggested faces under the person suggested, the most faces first and one
    /// person's spellings together; a face decided about this session is out of them, and a face with no suggestion is
    /// in none.
    #[test]
    fn the_groups_hold_each_persons_suggested_faces_until_they_are_decided() {
        let l = listing(vec![face(1, 0, Some("Ann")), face(2, 0, Some("ann")), face(3, 0, Some("Bob")), face(4, 0, None)]);
        let g = groups(&l, &[]);
        assert_eq!(g.len(), 2, "{g:?}");
        assert_eq!((g[0].name.as_str(), g[0].total), ("Ann", 2), "one name's spellings are one person");
        assert_eq!((g[1].name.as_str(), g[1].total), ("Bob", 1));
        // the biggest group first, then by name
        let l = listing(vec![face(1, 0, Some("Bob")), face(2, 0, Some("ann")), face(3, 0, Some("Ann")), face(4, 0, Some("Cy"))]);
        let names: Vec<String> = groups(&l, &[]).into_iter().map(|g| g.name).collect();
        assert_eq!(names, ["Ann", "Bob", "Cy"], "ann and Ann are one person, before Bob");
        // a confirmed face is named, a rejected one is not Ann's any more
        let g = groups(&l, &[(2, 0), (3, 0)]);
        assert!(g.iter().all(|group| group.name != "Ann"), "{g:?}");
        assert!(groups(&l, &[(1, 0), (2, 0), (3, 0), (4, 0)]).is_empty(), "every face decided");
    }

    /// Issue #1050: a decision runs the command its verdict names, with every face it covers and the box each is shown
    /// by, so the engine can refuse a face that changed since.
    #[test]
    fn each_verdict_runs_its_own_command_over_the_faces_it_covers() {
        let faces = vec![face(1, 0, Some("Ann")), face(2, 3, Some("Ann"))];
        for (verdict, cmd) in
            [(Verdict::Confirm, "faces.confirmSuggestions"), (Verdict::Reject, "faces.rejectSuggestions"), (Verdict::Ignore, "faces.ignore")]
        {
            let (id, params) = command(&Decision { verdict, person: "Ann".into(), faces: faces.clone() });
            assert_eq!(id, cmd);
            assert_eq!(params["faces"].as_array().map(Vec::len), Some(2));
            assert_eq!(params["faces"][1]["photo"], 2);
            assert_eq!(params["faces"][1]["index"], 3);
            assert_eq!(params["faces"][0]["rect"]["x0"], 0.1);
            assert_eq!(params["faces"][0]["rect"]["x1"], 0.3, "the face's own box, not the wider view of it");
        }
    }

    /// Issue #1050: the toast counts the faces and names the person.
    #[test]
    fn the_toast_says_who_and_how_many_faces() {
        let one = |verdict| Decision { verdict, person: "Ann".into(), faces: vec![face(1, 0, None)] };
        let many = |verdict| Decision { verdict, person: "Ann".into(), faces: vec![face(1, 0, None), face(2, 0, None)] };
        assert_eq!(said(&one(Verdict::Confirm)), "Confirmed as Ann");
        assert_eq!(said(&one(Verdict::Reject)), "Not Ann");
        assert_eq!(said(&many(Verdict::Confirm)), "Confirmed 2 faces as Ann");
        assert_eq!(said(&many(Verdict::Reject)), "Rejected 2 faces as Ann");
        assert_eq!(said(&one(Verdict::Ignore)), "Ignored 1 face");
        assert_eq!(said(&many(Verdict::Ignore)), "Ignored 2 faces");
        // every suggestion at once (the heading's button) names no one person
        let every = Decision { verdict: Verdict::Confirm, person: String::new(), faces: vec![face(1, 0, None), face(2, 0, None)] };
        assert_eq!(said(&every), "Confirmed 2 faces");
    }
}
