//! Remove tool commands: add (automatic source), select, move target/source, edit, refresh, delete.

use serde_json::json;

use crate::Session;

/// `library.up` / `library.down` (#1171): move by the given number of photos, clamped at the ends.
#[test]
fn up_and_down_move_by_rows_and_stop_at_the_ends() {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    assert!(vis.len() >= 6);
    s.selection = crate::Selection::single(vis[0]);
    s.execute("library.down", &json!({"rows": 4})).unwrap();
    assert_eq!(s.selection.active, Some(vis[4]));
    s.execute("library.up", &json!({})).unwrap();
    assert_eq!(s.selection.active, Some(vis[3]), "default is one photo (the filmstrip)");
    s.execute("library.up", &json!({"rows": 99})).unwrap();
    assert_eq!(s.selection.active, Some(vis[0]), "clamped at the first photo");
    s.execute("library.down", &json!({"rows": u64::MAX})).unwrap();
    assert_eq!(s.selection.active, vis.last().copied(), "hostile counts clamp, never overflow");
}

fn spots(s: &Session) -> Vec<lcc_develop::Spot> {
    s.develop_of(s.active().unwrap()).unwrap().spots.clone()
}

#[test]
fn spots_select_move_edit_refresh_and_delete() {
    let mut s = Session::with_demo();
    // a heal spot gets a resolved source (a pin to show) and is selected (a Remove spot too, but it
    // chooses again when moved: `remove_picks_its_own_source_and_heal_keeps_its_own`)
    let r = s.execute("spot.add", &json!({"mode": "heal", "points": [[0.4, 0.5]], "size": 0.02})).unwrap();
    assert_eq!(r["index"], 0);
    assert_eq!(s.active_spot, Some(0));
    let src0 = spots(&s)[0].source_offset.expect("automatic source resolved");
    assert!(src0.x.hypot(src0.y) > 0.02, "the source is outside the spot: {src0:?}");
    s.execute("spot.add", &json!({"mode": "clone", "points": [[0.7, 0.3]], "source": [0.1, 0.0]})).unwrap();
    assert_eq!(s.active_spot, Some(1));
    assert_eq!(s.execute("library.state", &json!({})).unwrap()["activeSpot"], 1);
    // select
    s.execute("spot.select", &json!({"index": 0})).unwrap();
    assert_eq!(s.active_spot, Some(0));
    assert!(s.execute("spot.select", &json!({"index": 9})).is_err());
    // move the target (the source follows: its offset is relative), then the source alone
    s.execute("spot.update", &json!({"move": [0.05, -0.1]})).unwrap();
    let sp = &spots(&s)[0];
    assert!((sp.points[0].x - 0.45).abs() < 1e-9 && (sp.points[0].y - 0.4).abs() < 1e-9);
    assert_eq!(sp.source_offset, Some(src0));
    s.execute("spot.update", &json!({"moveSource": [0.01, 0.02]})).unwrap();
    let o = spots(&s)[0].source_offset.unwrap();
    assert!((o.x - src0.x - 0.01).abs() < 1e-9 && (o.y - src0.y - 0.02).abs() < 1e-9);
    // size / feather / opacity of the selected spot (clamped), one undo step each
    s.execute("spot.update", &json!({"size": 0.03, "feather": 20, "opacity": 150})).unwrap();
    let sp = &spots(&s)[0];
    assert_eq!((sp.size, sp.feather, sp.opacity), (0.03, 20.0, 100.0));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(spots(&s)[0].size, 0.02);
    // refresh: a different source, at least a spot radius away
    let before = spots(&s)[0].source_offset.unwrap();
    let r = s.execute("spot.refreshSource", &json!({})).unwrap();
    let after = spots(&s)[0].source_offset.unwrap();
    assert!((after.x - before.x).hypot(after.y - before.y) > 0.01, "{r}");
    // delete the selected spot: the selection clears; deleting before a selected one shifts it
    s.execute("spot.delete", &json!({})).unwrap();
    assert_eq!((spots(&s).len(), s.active_spot), (1, None));
    assert_eq!(spots(&s)[0].mode, lcc_develop::SpotMode::Clone);
    assert!(s.execute("spot.delete", &json!({})).is_err(), "nothing selected");
    s.execute("spot.add", &json!({"points": [[0.2, 0.2]], "source": [0.05, 0.0]})).unwrap();
    s.execute("spot.delete", &json!({"index": 0})).unwrap();
    assert_eq!(s.active_spot, Some(0));
    // another photo: no selection
    s.execute("library.next", &json!({})).unwrap();
    assert_eq!(s.active_spot, None);
}

#[test]
fn a_content_aware_spot_has_no_source() {
    let mut s = Session::with_demo();
    s.execute("spot.add", &json!({"mode": "fill", "points": [[0.4, 0.5]], "size": 0.03})).unwrap();
    let sp = &spots(&s)[0];
    assert_eq!((sp.mode, sp.source_offset), (lcc_develop::SpotMode::Fill, None));
    assert_eq!(serde_json::to_value(sp).unwrap()["mode"], "fill");
    assert!(s.execute("spot.update", &json!({"mode": "nonsense"})).is_err());
}

fn target_session() -> Session {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "colorRange"})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("mask.addComponent", &json!({"kind": "colorRange"})).unwrap();
    s.execute("mask.saveEffect", &json!({"id": 1, "name": "Existing"})).unwrap();
    s.execute("mask.adjust", &json!({"values": {"exposure": 1.0}})).unwrap();
    for x in [0.3, 0.7] {
        s.execute("spot.add", &json!({"points": [[x, 0.5]], "source": [0.1, 0.0]})).unwrap();
    }
    // A rejected command must preserve a redo branch as well as existing undo steps.
    s.execute("mask.rename", &json!({"name": "Temporary"})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    s
}

fn target_state(s: &Session) -> serde_json::Value {
    let photo = s.catalog.photo(s.active().unwrap()).unwrap();
    json!({
        "develop": photo.develop,
        "history": photo.history,
        "selection": s.selection,
        "activeMask": s.active_mask,
        "activeSpot": s.active_spot,
        "undo": format!("{:?}", s.undo),
        "redo": format!("{:?}", s.redo),
        "commits": s.commits(),
        "effects": s.mask_effects,
        "rememberedEffects": s.mask_tool_effects,
        "journal": s.journal,
        "revision": s.catalog.revision,
    })
}

fn rejects_target(s: &mut Session, command: &str, params: &serde_json::Value) {
    let before = target_state(s);
    let result = s.execute(command, params);
    let after = target_state(s);
    assert!(result.is_err(), "{command} {params} returned {result:?}; state unchanged: {}", before == after);
    assert_eq!(after, before, "{command} {params} changed edits, selection or history on rejection");
    for field in ["id", "component", "index"] {
        if let Some(value) = params.get(field)
            && !value.is_null()
            && value.as_u64().is_none()
        {
            assert!(
                result.unwrap_err().to_string().contains(&format!("`{field}`")),
                "{command} must reject the malformed {field} before doing other work"
            );
            break;
        }
    }
}

fn malformed_targets() -> Vec<serde_json::Value> {
    vec![json!(-1), json!(0.5), json!("1"), json!(true), json!([]), json!({}), json!(u64::MAX)]
}

/// Malformed mask ids, mask components, effect targets and spot indices (and ids past `u32`, which
/// must not wrap to an existing mask) are refused by every command that takes one, with edits,
/// selection and history left as they were.
#[test]
fn malformed_mask_and_spot_targets_are_rejected_without_changes() {
    let mut s = target_session();
    // mask ids
    let commands = [
        ("mask.delete", json!({})),
        ("mask.rename", json!({"name": "Wrong mask"})),
        ("mask.invert", json!({})),
        ("mask.visible", json!({"visible": false})),
        ("mask.duplicate", json!({})),
        ("mask.move", json!({"to": 0})),
        ("mask.adjust", json!({"values": {"exposure": 1.0}})),
        ("mask.update", json!({"shape": {"kind": "brush", "strokes": []}})),
        ("mask.addComponent", json!({"kind": "brush"})),
        ("mask.component", json!({"component": 0, "action": "invert"})),
        ("mask.brushStroke", json!({"points": [[0.5, 0.5]]})),
        ("mask.sampleColor", json!({"x": 0.5, "y": 0.5})),
        ("mask.objectPoint", json!({"x": 0.5, "y": 0.5})),
        ("mask.refineDetail", json!({})),
    ];
    for (command, mut params) in commands {
        for target in malformed_targets().into_iter().chain([json!(4_294_967_297_u64)]) {
            params["id"] = target;
            rejects_target(&mut s, command, &params);
        }
    }
    // the mask selection
    for target in malformed_targets().into_iter().filter(|v| !v.is_null()).chain([json!(4_294_967_297_u64)]) {
        rejects_target(&mut s, "mask.select", &json!({"id": target}));
    }
    // mask effects
    for (command, mut params) in [
        ("mask.saveEffect", json!({"name": "Wrong effect"})),
        ("mask.updateEffect", json!({"name": "Existing"})),
        ("mask.applyEffect", json!({"name": "Existing"})),
    ] {
        for target in malformed_targets().into_iter().chain([json!(4_294_967_297_u64)]) {
            params["id"] = target;
            rejects_target(&mut s, command, &params);
        }
    }
    for target in malformed_targets().into_iter().chain([json!([1, -1]), json!([1, null]), json!([1, 4_294_967_297_u64])]) {
        rejects_target(&mut s, "mask.applyEffect", &json!({"name": "Existing", "ids": target}));
    }
    // mask components
    for (command, mut params) in [
        ("mask.update", json!({"shape": {"kind": "brush", "strokes": []}})),
        ("mask.component", json!({"action": "invert"})),
        ("mask.sampleColor", json!({"x": 0.5, "y": 0.5})),
        ("mask.refineDetail", json!({})),
    ] {
        for target in malformed_targets().into_iter().chain([json!(4_294_967_296_u64)]) {
            params["component"] = target;
            rejects_target(&mut s, command, &params);
        }
    }
    // spot indices
    for (command, mut params) in [("spot.delete", json!({})), ("spot.update", json!({"size": 0.1})), ("spot.refreshSource", json!({}))] {
        for target in malformed_targets().into_iter().chain([json!(4_294_967_296_u64)]) {
            params["index"] = target;
            rejects_target(&mut s, command, &params);
        }
    }
    // the spot selection
    for target in malformed_targets().into_iter().filter(|v| !v.is_null()).chain([json!(4_294_967_296_u64)]) {
        rejects_target(&mut s, "spot.select", &json!({"index": target}));
    }
}

#[test]
fn mask_effect_targets_preserve_explicit_defaults_and_lists() {
    let mut s = target_session();
    s.execute("mask.saveEffect", &json!({"id": 1, "name": "Explicit"})).unwrap();
    assert_eq!(s.mask_effects.last().unwrap().adjust.exposure, 0.0);
    for (name, target) in [("Omitted", json!({})), ("Null", json!({"id": null}))] {
        let mut params = target;
        params["name"] = json!(name);
        s.execute("mask.saveEffect", &params).unwrap();
        assert_eq!(s.mask_effects.last().unwrap().adjust.exposure, 1.0);
        params["name"] = json!("Existing");
        s.execute("mask.updateEffect", &params).unwrap();
        assert_eq!(s.mask_effects[0].adjust.exposure, 1.0);
    }
    s.execute("mask.updateEffect", &json!({"id": 1, "name": "Existing"})).unwrap();
    assert_eq!(s.mask_effects[0].adjust.exposure, 0.0);
    for mut params in [json!({}), json!({"id": null}), json!({"ids": null})] {
        s.execute("mask.adjust", &json!({"values": {"exposure": 1.0}})).unwrap();
        params["name"] = json!("Existing");
        s.execute("mask.applyEffect", &params).unwrap();
        assert_eq!(s.develop_of(s.active().unwrap()).unwrap().masks[1].adjust.exposure, 0.0);
    }
    s.execute("mask.adjust", &json!({"id": 1, "values": {"exposure": 1.0}})).unwrap();
    s.execute("mask.adjust", &json!({"id": 2, "values": {"exposure": 2.0}})).unwrap();
    s.execute("mask.applyEffect", &json!({"name": "Existing", "ids": [1, 2]})).unwrap();
    assert!(s.develop_of(s.active().unwrap()).unwrap().masks.iter().all(|m| m.adjust.exposure == 0.0));
    rejects_target(&mut s, "mask.applyEffect", &json!({"name": "Existing", "ids": []}));
}

#[test]
fn mask_and_spot_targets_preserve_explicit_defaults_and_selection_clearing() {
    use lcc_develop::MaskShape;
    let mut s = target_session();
    for target in [json!({}), json!({"id": null})] {
        let mut params = target;
        params["name"] = json!("Selected mask");
        s.execute("mask.rename", &params).unwrap();
        assert_eq!(s.develop_of(s.active().unwrap()).unwrap().masks[1].name, "Selected mask");
    }
    s.execute("mask.rename", &json!({"id": 1, "name": "Explicit mask"})).unwrap();
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().masks[0].name, "Explicit mask");
    assert_eq!(s.active_mask, Some(2));
    for params in [json!({"shape": {"kind": "brush", "strokes": []}}), json!({"component": null, "shape": {"kind": "brush", "strokes": []}})] {
        s.execute("mask.update", &params).unwrap();
        let d = s.develop_of(s.active().unwrap()).unwrap();
        assert!(matches!(d.masks[1].components[0].shape, MaskShape::Brush { .. }));
        assert!(matches!(d.masks[1].components[1].shape, MaskShape::ColorRange { .. }));
    }
    s.execute("mask.update", &json!({"id": 1, "component": 0, "shape": {"kind": "sky"}})).unwrap();
    assert!(matches!(s.develop_of(s.active().unwrap()).unwrap().masks[0].components[0].shape, MaskShape::Sky));
    for params in [json!({"points": [[0.2, 0.2]]}), json!({"id": null, "points": [[0.4, 0.4]]})] {
        s.execute("mask.brushStroke", &params).unwrap();
    }
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().masks.len(), 2);
    for params in [json!({"x": 0.5, "y": 0.5}), json!({"component": null, "x": 0.5, "y": 0.5})] {
        s.execute("mask.sampleColor", &params).unwrap();
        let d = s.develop_of(s.active().unwrap()).unwrap();
        let MaskShape::ColorRange { samples, .. } = &d.masks[1].components[1].shape else { panic!("color range") };
        assert_eq!(samples.len(), 1);
    }
    for params in [json!({"size": 0.04}), json!({"index": null, "size": 0.04})] {
        s.execute("spot.update", &params).unwrap();
        assert_eq!(spots(&s)[1].size, 0.04);
    }
    s.execute("spot.update", &json!({"index": 0, "size": 0.03})).unwrap();
    assert_eq!(spots(&s)[0].size, 0.03);
    assert_eq!(s.active_spot, Some(1));
    rejects_target(&mut s, "mask.component", &json!({"component": null, "action": "delete"}));
    for params in [json!({"id": null}), json!({})] {
        s.execute("mask.select", &json!({"id": 1})).unwrap();
        assert_eq!(s.active_mask, Some(1));
        s.execute("mask.select", &params).unwrap();
        assert_eq!(s.active_mask, None);
    }
    for params in [json!({"index": null}), json!({})] {
        s.execute("spot.select", &json!({"index": 0})).unwrap();
        assert_eq!(s.active_spot, Some(0));
        s.execute("spot.select", &params).unwrap();
        assert_eq!(s.active_spot, None);
    }
    rejects_target(&mut s, "mask.delete", &json!({}));
    rejects_target(&mut s, "spot.delete", &json!({}));
    // An omitted mask still creates a brush mask when no mask is selected.
    s.execute("mask.brushStroke", &json!({"points": [[0.5, 0.5]]})).unwrap();
    assert_eq!(s.develop_of(s.active().unwrap()).unwrap().masks.len(), 3);
}

/// Deleting a mask other than the selected one (the Masks list's per-row Delete passes its id) moved the selection
/// onto the last mask; only deleting the selected mask moves it (upstream #3863).
#[test]
fn deleting_another_mask_keeps_the_selection() {
    let mut s = Session::with_demo();
    for _ in 0..5 {
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    }
    let ids: Vec<u32> = s.develop_of(s.active().unwrap()).unwrap().masks.iter().map(|m| m.id).collect();
    let &[a, b, c, d, e] = ids.as_slice() else { panic!("five masks: {ids:?}") };
    assert_eq!(s.active_mask, Some(e));
    s.execute("mask.delete", &json!({"id": a})).unwrap();
    assert_eq!(s.active_mask, Some(e), "the selected last mask stays selected");
    s.execute("mask.select", &json!({"id": b})).unwrap();
    s.execute("mask.delete", &json!({"id": d})).unwrap();
    assert_eq!(s.active_mask, Some(b), "the selected first mask stays selected");
    s.execute("mask.select", &json!({"id": null})).unwrap();
    s.execute("mask.delete", &json!({"id": c})).unwrap();
    assert_eq!(s.active_mask, None, "no selection stays no selection");
    // deleting the selected mask still selects the last one left
    s.execute("mask.select", &json!({"id": b})).unwrap();
    s.execute("mask.delete", &json!({})).unwrap();
    assert_eq!(s.active_mask, Some(e));
}

/// Deleting the last component of another mask removes that mask but keeps the selection, like `mask.delete`.
#[test]
fn deleting_another_masks_last_component_keeps_the_selection() {
    let mut s = Session::with_demo();
    for _ in 0..3 {
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    }
    let ids: Vec<u32> = s.develop_of(s.active().unwrap()).unwrap().masks.iter().map(|m| m.id).collect();
    let &[a, b, c] = ids.as_slice() else { panic!("three masks: {ids:?}") };
    s.execute("mask.select", &json!({"id": b})).unwrap();
    s.execute("mask.component", &json!({"id": a, "component": 0, "action": "delete"})).unwrap();
    let left: Vec<u32> = s.develop_of(s.active().unwrap()).unwrap().masks.iter().map(|m| m.id).collect();
    assert_eq!(left, vec![b, c], "the emptied mask is removed");
    assert_eq!(s.active_mask, Some(b), "the selected mask stays selected");
    s.execute("mask.component", &json!({"component": 0, "action": "delete"})).unwrap();
    assert_eq!(s.active_mask, Some(c), "emptying the selected mask selects the last one left");
}
