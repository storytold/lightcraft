//! Window ▸ Bursts: Assisted Culling's bursts as rows of thumbnails, pick first. A photo dragged
//! to the front of its burst becomes the pick; dragged into another burst, onto New Burst or Not
//! in a Burst it moves there; a burst dragged by its title merges into another; photos dragged
//! from the grid drop here too. Each drop is one undo step.

use egui::{Pos2, Rect, pos2};
use lightcraft_catalog::{Analysis, Flag, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::state::RightPanel;
use crate::{LightcraftApp, Services};

/// Burst 1: a (picked by culling), b, c; burst 2: d, e; f never culled. The grid, with the
/// Bursts panel open.
fn app() -> (Headless, [PhotoId; 6]) {
    let mut session = lightcraft_engine::Session::new();
    let mut add = |name: &str, when: &str, culled: Option<(u32, f32, bool)>| {
        let id = session.catalog.alloc_photo_id();
        let mut p = Photo::new(id, Source::File { path: format!("/nowhere/{name}") }, name, "JPEG", 300, 200, "2026-05-01T12:00:00");
        p.captured = Some(when.into());
        p.analysis = culled.map(|(g, sharpness, best)| Analysis { sharpness, group: Some(g), best, ..Default::default() });
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let ids = [
        add("a.jpg", "2026-05-01T10:00:00", Some((1, 80.0, true))),
        add("b.jpg", "2026-05-01T10:00:01", Some((1, 60.0, false))),
        add("c.jpg", "2026-05-01T10:00:02", Some((1, 40.0, false))),
        add("d.jpg", "2026-05-01T11:00:00", Some((2, 70.0, true))),
        add("e.jpg", "2026-05-01T11:00:01", Some((2, 30.0, false))),
        add("f.jpg", "2026-05-01T12:00:00", None),
    ];
    session.catalog.apply(Op::SetFlag { id: ids[0], flag: Flag::Pick }).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": "photoGrid"}), std::time::Duration::from_secs(10));
    assert_eq!(r["ok"], true, "{r}");
    h.app.run("panel.bursts", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(h.app.ui.right, RightPanel::Bursts);
    (h, ids)
}

fn widget(h: &Headless, id: &str) -> Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| {
        panic!("no {id} in {:?}", h.app.widgets.iter().filter(|(w, _)| w.starts_with("burst")).map(|(w, _)| w).collect::<Vec<_>>())
    })
}

fn cell(h: &Headless, id: PhotoId) -> Rect {
    widget(h, &format!("burstPhoto:{}", id.0))
}

fn press(h: &mut Headless, at: Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(at));
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
}

fn glide(h: &mut Headless, from: Pos2, to: Pos2) {
    for i in 1..=8 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        h.step();
    }
    // (synthetic input arrives a frame or two behind)
    for _ in 0..3 {
        h.step();
    }
}

fn release(h: &mut Headless, at: Pos2) {
    h.app.synthetic.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    h.step();
}

fn drag(h: &mut Headless, from: Pos2, to: Pos2) {
    press(h, from);
    glide(h, from, to);
}

fn hint(h: &Headless) -> Option<String> {
    h.view.ctx.data(|d| d.get_temp::<(u64, String)>(egui::Id::unique("drag-photos-hint"))).map(|(_, text)| text)
}

fn best(h: &Headless, id: PhotoId) -> bool {
    h.app.session.catalog.photo(id).unwrap().analysis.unwrap().best
}

#[test]
fn bursts_show_their_pick_first() {
    let (h, [a, b, c, d, e, f]) = app();
    // a row per burst, its pick first
    let (one, two) = (widget(&h, "burst:1"), widget(&h, "burst:2"));
    assert!(one.bottom() <= cell(&h, a).top() && cell(&h, e).top() >= two.bottom(), "each title above its photos");
    assert!(cell(&h, a).left() < cell(&h, b).left() && cell(&h, b).left() < cell(&h, c).left());
    assert!(cell(&h, d).left() < cell(&h, e).left());
    widget(&h, "burstPick:1");
    assert!(widget(&h, "burstPick:1").intersects(cell(&h, a)), "the pick is marked");
    // a photo culling never saw has no row; the drop places for a new burst and for none are there
    assert!(!h.app.widgets.iter().any(|(w, _)| w == &format!("burstPhoto:{}", f.0)));
    widget(&h, "burst:new");
    widget(&h, "burst:none");
}

/// A burst whose photos are all hidden in a collapsed stack of another burst's photo is still
/// listed, so it can be arranged.
#[test]
fn a_burst_hidden_in_another_bursts_stack_is_listed() {
    let (mut h, [a, _, _, d, e, _]) = app();
    h.app.session.selection.ids = vec![a, d, e];
    h.app.session.selection.active = Some(a);
    h.app.session.execute("stack.group", &json!({"collapsed": true})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let visible = h.app.session.visible_cloned();
    assert!(visible.contains(&a) && !visible.contains(&d) && !visible.contains(&e), "d and e are hidden under a: {visible:?}");
    let (one, two) = (widget(&h, "burst:1"), widget(&h, "burst:2"));
    assert!(one.top() < two.top(), "listed after the burst of the stack's top");
    assert!(cell(&h, d).top() >= two.bottom() && cell(&h, e).top() >= two.bottom(), "with its photos");
}

#[test]
fn a_photo_dragged_to_the_front_becomes_the_pick() {
    let (mut h, [a, b, c, ..]) = app();
    let to = pos2(cell(&h, a).left() + 4.0, cell(&h, a).center().y);
    let from = cell(&h, c).center();
    drag(&mut h, from, to);
    assert_eq!(h.app.ui.dragging_photos, Some(vec![c.0]));
    assert_eq!(hint(&h).as_deref(), Some("Make it the pick"));
    release(&mut h, to);
    assert!(h.app.ui.dragging_photos.is_none());
    assert_eq!(h.app.session.catalog.burst(1), vec![c, a, b]);
    assert!(best(&h, c) && !best(&h, a));
    assert_eq!(h.app.session.catalog.photo(c).unwrap().flag, Flag::Pick, "culling's Pick follows the pick");
    // the panel shows the new order
    h.step();
    assert!(cell(&h, c).left() < cell(&h, a).left());
    // one undo step
    h.app.run("edit.undo", json!({})).unwrap();
    assert_eq!(h.app.session.catalog.burst(1), vec![a, b, c]);
    assert!(best(&h, a));
}

#[test]
fn photos_move_between_bursts_and_bursts_merge() {
    let (mut h, [a, b, c, d, e, _]) = app();
    let group = |h: &Headless, id: PhotoId| h.app.session.catalog.photo(id).unwrap().analysis.unwrap().group;
    // e onto burst 1 between a and b; burst 2 is left with d alone: no burst any more
    let to = pos2(cell(&h, b).left() + 4.0, cell(&h, b).center().y);
    let from = cell(&h, e).center();
    drag(&mut h, from, to);
    assert_eq!(hint(&h).as_deref(), Some("Move to this burst"));
    release(&mut h, to);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, e, b, c]);
    assert!(h.app.session.catalog.burst(2).is_empty() && group(&h, d).is_none());
    h.step();
    // d dragged from the grid onto burst 1's title goes last
    let title = widget(&h, "burst:1").center();
    let from = widget(&h, &format!("thumb:{}", d.0)).center();
    drag(&mut h, from, title);
    assert_eq!(hint(&h).as_deref(), Some("Move to this burst"));
    release(&mut h, title);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, e, b, c, d]);
    h.step();
    // b onto New Burst: a burst of its own, which c then joins by its title
    let new = widget(&h, "burst:new").center();
    let from = cell(&h, b).center();
    drag(&mut h, from, new);
    assert_eq!(hint(&h).as_deref(), Some("New burst"));
    release(&mut h, new);
    let g = group(&h, b).expect("a new burst");
    assert_eq!(h.app.session.catalog.burst(g), vec![b]);
    h.step();
    let to = widget(&h, &format!("burst:{g}")).center();
    let from = cell(&h, c).center();
    drag(&mut h, from, to);
    release(&mut h, to);
    assert_eq!(h.app.session.catalog.burst(g), vec![b, c]);
    h.step();
    // e onto Not in a Burst
    let out = widget(&h, "burst:none").center();
    let from = cell(&h, e).center();
    drag(&mut h, from, out);
    assert_eq!(hint(&h).as_deref(), Some("Take out of its burst"));
    release(&mut h, out);
    assert!(group(&h, e).is_none());
    assert_eq!(h.app.session.catalog.burst(1), vec![a, d]);
    h.step();
    // a burst dragged by its title onto another merges into it
    let to = widget(&h, "burst:1").center();
    let from = widget(&h, &format!("burst:{g}")).center();
    drag(&mut h, from, to);
    assert_eq!(hint(&h).as_deref(), Some("Merge into this burst"));
    release(&mut h, to);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, d, b, c]);
    assert!(h.app.session.catalog.burst(g).is_empty());
}

#[test]
fn a_photo_never_culled_is_refused() {
    let (mut h, [_, _, _, _, _, f]) = app();
    let to = widget(&h, "burst:1").center();
    let from = widget(&h, &format!("thumb:{}", f.0)).center();
    drag(&mut h, from, to);
    assert_eq!(hint(&h).as_deref(), Some("Run Assisted Culling on it first"));
    let commits = h.app.session.commits();
    release(&mut h, to);
    assert_eq!(h.app.session.commits(), commits);
    let toast = h.app.ui.toast.as_ref().map(|(t, _, _)| t.clone()).unwrap_or_default();
    assert!(toast.contains("Assisted Culling"), "{toast}");
}

/// A burst dragged by its title takes all its photos along, also those the view leaves out.
#[test]
fn a_burst_dragged_by_its_title_takes_its_photos_out_of_view() {
    let (mut h, [a, b, c, d, e, _]) = app();
    // only the picks are in view: e (of burst 2) is left out
    let r = h.app.session.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "bestOfGroup", "op": "is", "value": true}]}})).unwrap();
    assert!(r.is_object(), "{r}");
    for _ in 0..4 {
        h.step();
    }
    assert!(!h.app.session.visible_cloned().contains(&e), "e is out of view");
    let to = widget(&h, "burst:1").center();
    let from = widget(&h, "burst:2").center();
    drag(&mut h, from, to);
    assert_eq!(hint(&h).as_deref(), Some("Merge into this burst"));
    release(&mut h, to);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, b, c, d, e], "e came along");
    assert!(h.app.session.catalog.burst(2).is_empty());
}

/// A photo dropped after a shown member lands right after it in the whole burst, before the
/// members the view leaves out, not after them.
#[test]
fn a_drop_after_a_shown_photo_goes_before_the_photos_out_of_view() {
    let (mut h, [a, b, c, d, _, _]) = app();
    // only the picks are in view: burst 1 shows a, its b and c are left out
    h.app.session.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "bestOfGroup", "op": "is", "value": true}]}})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let to = pos2(cell(&h, a).right() - 4.0, cell(&h, a).center().y);
    let from = cell(&h, d).center();
    drag(&mut h, from, to);
    assert_eq!(hint(&h).as_deref(), Some("Move to this burst"));
    release(&mut h, to);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, d, b, c], "right after a, before b and c");
}

/// Between two shown photos with one out of view between them, a drop on the left half of the
/// second goes right before it, a drop on the right half of the first right after that one.
#[test]
fn a_drop_keeps_its_side_of_a_photo_out_of_view() {
    let (mut h, [a, b, c, d, _, _]) = app();
    // b is left out: burst 1 shows a, c
    h.app.session.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "fileName", "op": "notContains", "value": "b.jpg"}]}})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(!h.app.session.visible_cloned().contains(&b));
    let before_c = pos2(cell(&h, c).left() + 4.0, cell(&h, c).center().y);
    let from = cell(&h, d).center();
    drag(&mut h, from, before_c);
    release(&mut h, before_c);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, b, d, c], "right before c");
    h.app.run("edit.undo", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    let after_a = pos2(cell(&h, a).right() - 4.0, cell(&h, a).center().y);
    let from = cell(&h, d).center();
    drag(&mut h, from, after_a);
    release(&mut h, after_a);
    assert_eq!(h.app.session.catalog.burst(1), vec![a, d, b, c], "right after a");
}
