//! Linked masks (test notes 2026-10-28 #12, with the user's corrections of 29 Oct): the copies of
//! a mask applied to several photos in one go (Auto Sync, a preset on a selection, Paste, Sync
//! Settings) share a link. The link acts only through Auto Sync: with it on, an edit of one copy
//! reaches the copies on the other *selected* photos; a copy on a photo that isn't selected is never
//! changed, and with Auto Sync off an edit stays on its own photo. AI parts keep what was found on
//! each photo (their settings follow, never their shape); hand-made parts (gradients, brush) follow
//! exactly. There is no unlink.

use lcc_catalog::PhotoId;
use lcc_develop::{Mask, MaskComponent, MaskOp, MaskShape, SegMask};
use lcc_geom::Point;
use serde_json::json;

use crate::Session;

fn masks(s: &Session, id: PhotoId) -> Vec<Mask> {
    s.develop_of(id).unwrap().masks.clone()
}

fn photos<const N: usize>() -> (Session, [PhotoId; N]) {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(N).map(|p| p.0).collect();
    assert_eq!(ids.len(), N, "enough demo photos");
    (s, std::array::from_fn(|i| PhotoId(ids[i])))
}

fn select(s: &mut Session, ids: &[PhotoId], active: PhotoId) {
    let ids: Vec<u64> = ids.iter().map(|x| x.0).collect();
    s.execute("library.select", &json!({"ids": ids, "active": active.0})).unwrap();
}

fn seg() -> SegMask {
    SegMask::from_logits(4, &[2.0; 16])
}

/// The gradient drag the photo does: an interaction, `mask.update`s, its end.
fn drag(s: &mut Session, mask: u32, comp: usize, start: [f64; 2], end: [f64; 2]) {
    s.execute("develop.beginInteraction", &json!({"label": "Edit Mask"})).unwrap();
    for t in [0.5, 1.0] {
        let shape = MaskShape::Linear { start: Point::new(start[0] * t, start[1] * t), end: Point::new(end[0] * t, end[1] * t) };
        s.execute("mask.update", &json!({"id": mask, "component": comp, "shape": shape})).unwrap();
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
}

fn linear(m: &Mask, comp: usize) -> (Point, Point) {
    match &m.components[comp].shape {
        MaskShape::Linear { start, end } => (*start, *end),
        o => panic!("{o:?}"),
    }
}

/// Put mask `m` (the same link) on every photo of `ids` as it is, without an undo step.
fn put(s: &mut Session, ids: &[PhotoId], m: &Mask) {
    for id in ids {
        let mut d = (*s.develop_of(*id).unwrap()).clone();
        d.masks = vec![m.clone()];
        s.set_develop(*id, d, "Test").unwrap();
    }
}

/// A plain Radial mask with Auto Sync, made with `ids` selected: linked on all of them.
fn radial_on(s: &mut Session, ids: &[PhotoId]) -> String {
    select(s, ids, ids[0]);
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let link = masks(s, ids[0])[0].link.clone();
    assert!(!link.is_empty());
    for id in ids {
        assert_eq!(masks(s, *id).len(), 1);
        assert_eq!(masks(s, *id)[0].link, link, "one linked group");
    }
    link
}

/// The failing case of OneSlips' note 12: a smart mask (an AI selection minus a Linear gradient)
/// applied with Auto Sync on to a selection. With Auto Sync on, dragging the
/// gradient on one photo puts it in exactly the same place (start, end) on every *selected*
/// linked photo, not relative to each photo's own people box, and from then on it is placed by
/// hand. A linked copy on a photo that isn't selected stays as it was; the copies keep their own AI
/// selection; one undo step takes the drag back.
#[test]
fn a_gradient_drag_reaches_the_selected_linked_copies_only() {
    let (mut s, [a, b, c, d]) = photos::<4>();
    select(&mut s, &[a, b, c, d], a);
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("mask.add", &json!({"kind": "prompt", "text": "the couple", "seg": seg(), "name": "Sun Kissed"})).unwrap();
    let mid = masks(&s, a)[0].id;
    s.execute("mask.select", &json!({"id": mid})).unwrap();
    s.execute("mask.addComponent", &json!({"kind": "linear", "op": "subtract", "start": [0.5, 1.0], "end": [0.5, 0.55]})).unwrap();
    let link = masks(&s, a)[0].link.clone();
    assert!(!link.is_empty(), "a mask Auto Sync carries is linked");
    for id in [b, c, d] {
        let m = masks(&s, id);
        assert_eq!(m.len(), 1, "one copy each: {m:#?}");
        assert_eq!(m[0].link, link, "the copies share the link");
        assert_eq!(m[0].components.len(), 2);
    }
    // each photo has its gradient somewhere else (as one placed through its own people box)
    for (id, y) in [(a, 0.9), (b, 0.8), (c, 0.7), (d, 0.6)] {
        let mut dev = (*s.develop_of(id).unwrap()).clone();
        dev.masks[0].components[1].shape = MaskShape::Linear { start: Point::new(0.5, y), end: Point::new(0.5, y - 0.3) };
        s.set_develop(id, dev, "Test").unwrap();
    }
    let d_before = masks(&s, d);

    // a, b and c selected, Auto Sync on (the user's case): the drag reaches them exactly
    select(&mut s, &[a, b, c], a);
    drag(&mut s, mid, 1, [0.552, 0.549], [0.261, 0.259]);
    let want = (Point::new(0.552, 0.549), Point::new(0.261, 0.259));
    for id in [a, b, c] {
        let m = &masks(&s, id)[0];
        assert_eq!(linear(m, 1), want, "the master's exact place on every selected photo");
        assert!(m.components[1].anchor.is_none(), "placed by hand: no longer through the people box");
        assert!(m.components[0].shape.is_ai(), "the AI part stays");
    }
    assert!(masks(&s, a)[0].components[0].shape.has_seg(), "a keeps its own selection");
    assert_eq!(masks(&s, d), d_before, "d is linked but not selected: never changed");

    // one undo step takes the drag back on every photo it reached
    s.execute("edit.undo", &json!({})).unwrap();
    for id in [a, b, c] {
        assert_ne!(linear(&masks(&s, id)[0], 1), want, "undone on every photo");
    }
}

/// With Auto Sync off an edit of a linked copy stays on its own photo, whatever is selected:
/// sliders, a gradient drag, a rename. Deleting a copy then deletes only that copy.
#[test]
fn with_auto_sync_off_an_edit_stays_on_its_photo() {
    let (mut s, [a, b, c]) = photos::<3>();
    radial_on(&mut s, &[a, b, c]);
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    select(&mut s, &[a, b, c], b);
    let bid = masks(&s, b)[0].id;
    let before = [masks(&s, a), masks(&s, c)];
    s.execute("mask.adjust", &json!({"id": bid, "values": {"exposure": 0.8}})).unwrap();
    s.execute("mask.rename", &json!({"id": bid, "name": "Only Here"})).unwrap();
    assert!((masks(&s, b)[0].adjust.exposure - 0.8).abs() < 1e-9);
    assert_eq!(masks(&s, b)[0].name, "Only Here");
    assert_eq!([masks(&s, a), masks(&s, c)], before, "Auto Sync off: the other copies stay as they are");

    s.execute("mask.delete", &json!({"id": bid})).unwrap();
    assert!(masks(&s, b).is_empty());
    assert_eq!([masks(&s, a), masks(&s, c)], before, "deleting one copy deletes only it");
}

/// AI masks: settings sync, shape never. A linked mask of People + Object + Linear, where photo
/// b's AI parts are its own (another person picked, its own clicks and selections). An Edge change
/// of the Object and a slider on a reach b's copy, but b keeps its person, its clicks and its
/// selections; a Linear drag on a then moves b's gradient and still leaves b's AI parts alone.
#[test]
fn ai_parts_keep_their_own_shape_while_their_settings_sync() {
    let (mut s, [a, b]) = photos::<2>();
    let people = |person| MaskComponent {
        name: None,
        anchor: None,
        op: MaskOp::Add,
        invert: false,
        shape: MaskShape::People { person, parts: vec![], who: None, seg: None },
    };
    let object = |x: f64| MaskComponent {
        name: None,
        anchor: None,
        op: MaskOp::Add,
        invert: false,
        shape: MaskShape::Object { hint: vec![Point::new(x, 0.5)], exclude: vec![], seg: Some(seg()), detail: vec![], edge: 0.0 },
    };
    let lin = MaskComponent {
        name: None,
        anchor: None,
        op: MaskOp::Subtract,
        invert: false,
        shape: MaskShape::Linear { start: Point::new(0.5, 1.0), end: Point::new(0.5, 0.6) },
    };
    let base = Mask { id: 1, name: "Couple".into(), link: "linkai".into(), components: vec![people(1), object(0.2), lin.clone()], ..Mask::default() };
    put(&mut s, &[a], &base);
    let mine = Mask { components: vec![people(2), object(0.7), lin], ..base.clone() };
    put(&mut s, &[b], &mine);

    select(&mut s, &[a, b], a);
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let mut edged = object(0.2).shape;
    if let MaskShape::Object { edge, .. } = &mut edged {
        *edge = 30.0;
    }
    s.execute("mask.update", &json!({"id": 1, "component": 1, "shape": edged})).unwrap();
    s.execute("mask.adjust", &json!({"id": 1, "values": {"exposure": 0.6}})).unwrap();
    let m = &masks(&s, b)[0];
    assert!((m.adjust.exposure - 0.6).abs() < 1e-9, "the slider syncs");
    match &m.components[1].shape {
        MaskShape::Object { hint, seg, edge, .. } => {
            assert_eq!(hint, &vec![Point::new(0.7, 0.5)], "b keeps its own clicks");
            assert!(seg.is_some(), "and what was found on it");
            assert_eq!(*edge, 30.0, "the Edge setting syncs");
        }
        o => panic!("{o:?}"),
    }
    assert_eq!(m.components[0], mine.components[0], "b keeps its own person and selection");

    drag(&mut s, 1, 2, [0.3, 0.9], [0.7, 0.2]);
    let m = &masks(&s, b)[0];
    assert_eq!(linear(m, 2), (Point::new(0.3, 0.9), Point::new(0.7, 0.2)), "the hand-made gradient follows exactly");
    assert_eq!(m.components[0], mine.components[0], "a gradient drag leaves b's People alone");
    assert!(matches!(&m.components[1].shape, MaskShape::Object { hint, seg: Some(_), .. } if hint == &vec![Point::new(0.7, 0.5)]));
}

/// Adding to or subtracting from a linked mask syncs: a component added on one photo (a hand-made
/// one exactly, an AI one as a recipe found on each photo) and one removed reach the selected linked
/// copies, never a linked copy on a photo that isn't selected.
#[test]
fn an_added_or_removed_component_reaches_the_selected_linked_copies_only() {
    let (mut s, [a, b, c]) = photos::<3>();
    radial_on(&mut s, &[a, b, c]);
    let c_before = masks(&s, c);
    select(&mut s, &[a, b], a);
    let mid = masks(&s, a)[0].id;
    s.execute("mask.select", &json!({"id": mid})).unwrap();
    s.execute("mask.addComponent", &json!({"kind": "linear", "op": "subtract", "start": [0.1, 0.9], "end": [0.4, 0.3]})).unwrap();
    s.execute("mask.addComponent", &json!({"kind": "background", "op": "intersect", "seg": seg()})).unwrap();
    let m = &masks(&s, b)[0];
    assert_eq!(m.components.len(), 3, "{m:#?}");
    assert_eq!(linear(m, 1), (Point::new(0.1, 0.9), Point::new(0.4, 0.3)), "the subtracted gradient at the same place");
    assert_eq!(m.components[1].op, MaskOp::Subtract);
    assert!(m.components[2].shape.is_ai(), "the added AI part, found on b");
    assert_eq!(m.components[2].op, MaskOp::Intersect);
    assert_eq!(masks(&s, c), c_before, "c isn't selected: untouched");

    s.execute("mask.component", &json!({"id": mid, "component": 1, "action": "delete"})).unwrap();
    let m = &masks(&s, b)[0];
    assert_eq!(m.components.len(), 2, "removed on b too");
    assert!(m.components.iter().all(|x| !matches!(x.shape, MaskShape::Linear { .. })));
    assert!(m.components[1].shape.is_ai());
    assert_eq!(masks(&s, c), c_before, "c still untouched");
}

/// Auto Sync copies a mask, and the copies are linked (rule 5): the photos have no mask; one of
/// them gets one; selecting the group with it active, Auto Sync on, one slider change gives all
/// of them the mask, linked with the original; a second change reaches all of them. Once one
/// photo leaves the selection, a change no longer reaches it.
#[test]
fn auto_sync_copies_a_mask_to_the_selected_photos_and_links_them() {
    let (mut s, [a, b, c]) = photos::<3>();
    for id in [a, b, c] {
        assert!(masks(&s, id).is_empty());
    }
    select(&mut s, &[a], a);
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    let mid = masks(&s, a)[0].id;
    assert!(masks(&s, a)[0].link.is_empty(), "one photo: nothing to link yet");

    select(&mut s, &[a, b, c], a);
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("mask.adjust", &json!({"id": mid, "values": {"exposure": 0.3}})).unwrap();
    let link = masks(&s, a)[0].link.clone();
    assert!(!link.is_empty(), "the original joins the group");
    for id in [b, c] {
        let m = masks(&s, id);
        assert_eq!(m.len(), 1, "the mask was copied: {m:#?}");
        assert_eq!(m[0].link, link, "linked with the original");
        assert!((m[0].adjust.exposure - 0.3).abs() < 1e-9);
        assert_eq!(m[0].components, masks(&s, a)[0].components);
    }

    s.execute("mask.adjust", &json!({"id": mid, "values": {"contrast": 20}})).unwrap();
    for id in [a, b, c] {
        assert_eq!(masks(&s, id)[0].adjust.contrast, 20.0, "the second change reaches all of them");
    }

    select(&mut s, &[a, b], a);
    s.execute("mask.adjust", &json!({"id": mid, "values": {"shadows": 10}})).unwrap();
    assert_eq!(masks(&s, b)[0].adjust.shadows, 10.0);
    assert_eq!(masks(&s, c)[0].adjust.shadows, 0.0, "c is linked but not selected: unchanged");
}

/// Renaming a linked mask names its copies on the selected photos the same (Auto Sync on), in one
/// undo step; a copy on a photo that isn't selected keeps its name.
#[test]
fn a_rename_reaches_the_selected_linked_copies_in_one_step() {
    let (mut s, [a, b, c]) = photos::<3>();
    radial_on(&mut s, &[a, b, c]);
    let was = masks(&s, a)[0].name.clone();
    select(&mut s, &[a, b], a);
    let mid = masks(&s, a)[0].id;
    s.execute("mask.rename", &json!({"id": mid, "name": "Warmth"})).unwrap();
    assert_eq!(masks(&s, a)[0].name, "Warmth");
    assert_eq!(masks(&s, b)[0].name, "Warmth", "the selected copy takes the name");
    assert_eq!(masks(&s, c)[0].name, was, "an unselected copy keeps its own");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(masks(&s, a)[0].name, was);
    assert_eq!(masks(&s, b)[0].name, was, "one undo step for both");
}

/// Deleting with Auto Sync on and the photos selected removes the mask on all of them, but not a
/// linked copy on a photo that isn't selected.
#[test]
fn delete_with_auto_sync_removes_the_selected_copies() {
    let (mut s, [a, b, c]) = photos::<3>();
    radial_on(&mut s, &[a, b, c]);
    select(&mut s, &[a, b], a);
    s.execute("mask.delete", &json!({"id": masks(&s, a)[0].id})).unwrap();
    assert!(masks(&s, a).is_empty());
    assert!(masks(&s, b).is_empty(), "the selected copy goes too");
    assert_eq!(masks(&s, c).len(), 1, "c isn't selected: it keeps its copy");
}

/// A preset on a selection, Paste onto a selection, Sync Settings and Paste from Previous link
/// the masks they copy (with the source photo's original for Paste and Sync); a preset on one
/// photo links nothing. A pasted copy's later edit reaches the original only through Auto Sync.
#[test]
fn presets_paste_and_sync_link_the_masks_they_copy() {
    let (mut s, [a, b, c]) = photos::<3>();
    let settings = json!({"masks": [{"id": 1, "name": "Glow", "components": [{"shape": {"kind": "radial", "center": {"x": 0.5, "y": 0.5}, "rx": 0.2, "ry": 0.2, "angle": 0.0, "feather": 50.0, "invert": false}}], "adjust": {"exposure": 0.3}}]});
    s.presets.push(lcc_develop::Preset { id: "glow".into(), name: "Glow".into(), group: "Test".into(), settings, favorite: false, builtin: false });
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("preset.apply", &json!({"id": "glow"})).unwrap();
    assert!(masks(&s, a)[0].link.is_empty(), "one photo: nothing to link");
    s.execute("mask.deleteAll", &json!({})).unwrap();

    s.execute("library.select", &json!({"ids": [a.0, b.0, c.0], "active": a.0})).unwrap();
    s.execute("preset.apply", &json!({"id": "glow"})).unwrap();
    let link = masks(&s, a)[0].link.clone();
    assert!(!link.is_empty());
    assert!([b, c].iter().all(|id| masks(&s, *id)[0].link == link), "a preset on a selection links its masks");
    for id in [a, b, c] {
        s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
        s.execute("mask.deleteAll", &json!({})).unwrap();
    }

    // Paste: copied from a, pasted onto b and c
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["masks"]})).unwrap();
    s.execute("library.select", &json!({"ids": [b.0, c.0], "active": b.0})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    let link = masks(&s, a)[0].link.clone();
    assert!(!link.is_empty(), "the original joins the group");
    assert!([b, c].iter().all(|id| masks(&s, *id)[0].link == link));
    // an edit of c's copy alone stays on c; with a and c selected and Auto Sync on it reaches a
    s.execute("library.select", &json!({"ids": [c.0]})).unwrap();
    s.execute("mask.adjust", &json!({"id": masks(&s, c)[0].id, "values": {"shadows": 40}})).unwrap();
    assert_eq!(masks(&s, a)[0].adjust.shadows, 0.0, "one photo selected: stays on c");
    select(&mut s, &[c, a], c);
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("mask.adjust", &json!({"id": masks(&s, c)[0].id, "values": {"shadows": 50}})).unwrap();
    assert_eq!(masks(&s, a)[0].adjust.shadows, 50.0, "a pasted copy's edit reaches the selected original");
    assert_eq!(masks(&s, b)[0].adjust.shadows, 0.0, "b isn't selected");
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();

    // Sync Settings from b to c (b's mask already linked: c's copy joins that group)
    s.execute("library.select", &json!({"ids": [b.0, c.0], "active": b.0})).unwrap();
    s.execute("develop.sync", &json!({"groups": ["masks"]})).unwrap();
    assert_eq!(masks(&s, c)[0].link, link);
}

/// Masks saved before links existed read as unlinked, and a link survives a save.
#[test]
fn old_masks_read_unlinked() {
    let m: Mask = serde_json::from_value(json!({"id": 1, "name": "Mask 1", "components": []})).unwrap();
    assert!(m.link.is_empty());
    assert!(serde_json::to_value(&m).unwrap().get("link").is_none(), "not written while unlinked");
    let linked = Mask { link: "00ff".into(), ..m };
    let back: Mask = serde_json::from_value(serde_json::to_value(&linked).unwrap()).unwrap();
    assert_eq!(back.link, "00ff");
}

/// A ⇧-click range or ⌘-click that keeps the active photo keeps the selected mask; a click that
/// moves the active photo selects the linked copy there (another id), and on a photo without it
/// none.
#[test]
fn the_selected_mask_follows_the_active_photo_to_its_linked_copy() {
    let (mut s, [a, b, c]) = photos::<3>();
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    let on_a = s.active_mask.expect("selected");
    let link = masks(&s, a)[0].link.clone();
    let on_b = masks(&s, b).iter().find(|m| m.link == link).map(|m| m.id).expect("b's copy");
    assert_ne!(on_a, on_b);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    assert_eq!(s.active_mask, Some(on_a), "the same active photo keeps its mask");
    s.execute("library.select", &json!({"ids": [c.0], "mode": "range"})).unwrap();
    assert_eq!(s.active_mask, Some(on_a), "a range keeps it");
    s.execute("library.select", &json!({"ids": [c.0], "mode": "toggle"})).unwrap();
    assert_eq!(s.active_mask, Some(on_a), "a ⌘-click keeps it");
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    assert_eq!(s.active(), Some(b));
    assert_eq!(s.active_mask, Some(on_b), "the linked copy on the new active photo");
    s.execute("library.select", &json!({"ids": [c.0]})).unwrap();
    assert_eq!(s.active_mask, None, "a photo without it: none");
}
