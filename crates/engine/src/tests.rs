use serde_json::{Value, json};

use crate::{Selection, Session, command_specs};

fn demo() -> Session {
    Session::with_demo()
}

fn active_dev(s: &Session) -> lightcraft_develop::DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

#[test]
fn changing_one_wb_control_resolves_as_shot_without_stale_tint() {
    use lightcraft_catalog::{Photo, PhotoId, Source};
    use lightcraft_develop::{DevelopSettings, WbMode};
    for format in ["ARW", "NEF", "NRW", "RAF", "CR2", "PEF", "ORF"] {
        let mut s = demo();
        let id = PhotoId(100);
        let name = format!("synthetic.{format}");
        let mut p = Photo::new(id, Source::File { path: name.clone() }, &name, format, 16, 16, "");
        // A catalog made by the old generic-matrix Kelvin inference.
        p.as_shot_wb = Some((6829.0, -127.0));
        p.develop = std::sync::Arc::new(DevelopSettings::for_raw(6829.0, -127.0));
        s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.execute("library.select", &json!({"ids": [100], "active": 100})).unwrap();
        s.execute("develop.set", &json!({"control": "wb.temp", "value": 8000})).unwrap();
        let d = active_dev(&s);
        assert_eq!(d.wb.mode, WbMode::Custom);
        assert_eq!(d.wb.temp, 8000.0);
        assert_eq!(d.wb.tint, 0.0, "{format}: untouched tint comes from the current as-shot reference");
        s.execute("develop.wb", &json!({"mode": "asShot"})).unwrap();
        s.execute("develop.set", &json!({"control": "wb.tint", "value": 10})).unwrap();
        assert_eq!(active_dev(&s).wb.temp, 6500.0, "{format}");
        s.execute("develop.reset", &json!({})).unwrap();
        assert_eq!((active_dev(&s).wb.temp, active_dev(&s).wb.tint), (6500.0, 0.0), "{format}");
    }
}

#[test]
fn demo_library_loads() {
    let mut s = demo();
    assert_eq!(s.visible().len(), 24);
    assert!(s.active().is_some());
    let st = s.execute("catalog.stats", &json!({})).unwrap();
    assert_eq!(st["photos"], 24);
    let albums = s.execute("albums.list", &json!({})).unwrap();
    assert!(albums.as_array().unwrap().iter().any(|a| a["folder"] == true));
}

#[test]
fn rating_flag_undo_redo() {
    let mut s = demo();
    let id = s.active().unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    s.execute("photo.flag", &json!({"flag": "reject"})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().rating, 5);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_ne!(s.catalog.photo(id).unwrap().flag, lightcraft_catalog::Flag::Reject);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().flag, lightcraft_catalog::Flag::Reject);
    assert!(s.execute("photo.rate", &json!({"rating": 7})).is_err());
}

#[test]
fn develop_set_and_interaction_coalesces() {
    let mut s = demo();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, 0.5);
    let undo_before = s.undo.len();
    s.execute("develop.beginInteraction", &json!({"label": "Exposure"})).unwrap();
    for v in [0.6, 0.8, 1.2] {
        s.execute("develop.set", &json!({"control": "light.exposure", "value": v})).unwrap();
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert_eq!(s.undo.len(), undo_before + 1, "a drag is one undo step");
    assert_eq!(active_dev(&s).light.exposure, 1.2);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, 0.5);
    // unknown control and bad values are rejected
    assert!(s.execute("develop.set", &json!({"control": "nope", "value": 1})).is_err());
    assert!(s.execute("develop.set", &json!({"values": {"light.contrast": "x"}})).is_err());
    // clamped
    s.execute("develop.set", &json!({"values": {"light.contrast": 500}})).unwrap();
    assert_eq!(active_dev(&s).light.contrast, 100.0);
}

#[test]
fn copy_paste_sync_presets() {
    let mut s = demo();
    let vis = s.visible_cloned();
    s.execute("develop.set", &json!({"values": {"effects.clarity": 30, "light.shadows": 20}})).unwrap();
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("library.select", &json!({"ids": [vis[1].0, vis[2].0]})).unwrap();
    s.execute("develop.paste", &json!({})).unwrap();
    assert_eq!(s.develop_of(vis[2]).unwrap().effects.clarity, 30.0);
    s.execute("preset.apply", &json!({"id": "lc.bw-high-contrast", "amount": 100})).unwrap();
    assert_eq!(s.develop_of(vis[1]).unwrap().treatment, lightcraft_develop::Treatment::Bw);
    let r = s.execute("preset.create", &json!({"name": "Mine", "groups": ["light", "effects"]})).unwrap();
    assert!(r["id"].as_str().unwrap().starts_with("user."));
    assert!(s.execute("preset.delete", &json!({"id": "lc.moody"})).is_err());
}

#[test]
fn albums_crud() {
    let mut s = demo();
    let f = s.execute("album.create", &json!({"name": "Trips", "folder": true})).unwrap()["id"].as_u64().unwrap();
    let a = s.execute("album.create", &json!({"name": "Best", "parent": f, "addSelected": true})).unwrap()["id"].as_u64().unwrap();
    s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
    assert_eq!(s.visible().len(), 1);
    s.execute("album.rename", &json!({"id": a, "name": "Best of"})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    s.execute("library.selectAll", &json!({})).unwrap();
    s.execute("album.addPhotos", &json!({"id": a})).unwrap();
    assert_eq!(s.catalog.album(lightcraft_catalog::AlbumId(a)).unwrap().photos.len(), 24);
    s.execute("album.delete", &json!({"id": f})).unwrap();
    assert!(s.catalog.album(lightcraft_catalog::AlbumId(a)).is_none());
    assert!(s.catalog.album(lightcraft_catalog::AlbumId(f)).is_none());
    assert_eq!(s.catalog.photos().count(), 24, "album deletion keeps the photos");
}

/// Given an album and a folder, when something is created inside each, then only the folder
/// accepts it: the sidebar only lists children of folders, so a child of a plain album would
/// be invisible.
#[test]
fn albums_and_smart_albums_are_created_only_inside_folders() {
    let mut s = demo();
    let folder = s.execute("album.create", &json!({"name": "Trips", "folder": true})).unwrap()["id"].as_u64().unwrap();
    let album = s.execute("album.create", &json!({"name": "Best", "parent": folder})).unwrap()["id"].as_u64().unwrap();
    for (cmd, params) in [
        ("album.create", json!({"name": "Nested", "parent": album})),
        ("album.create", json!({"name": "Nested", "folder": true, "parent": album})),
        ("album.createSmart", json!({"name": "Nested", "parent": album})),
        ("album.create", json!({"name": "Nested", "parent": 987_654})),
    ] {
        assert!(s.execute(cmd, &params).is_err(), "{cmd} {params} must be refused");
    }
    let n = s.catalog.albums().count();
    let sub = s.execute("album.create", &json!({"name": "Sub", "folder": true, "parent": folder})).unwrap()["id"].as_u64().unwrap();
    let smart = s.execute("album.createSmart", &json!({"name": "Rated", "parent": sub})).unwrap()["id"].as_u64().unwrap();
    assert_eq!(s.catalog.albums().count(), n + 2);
    assert_eq!(s.catalog.album(lightcraft_catalog::AlbumId(smart)).unwrap().parent, Some(lightcraft_catalog::AlbumId(sub)));
}

#[test]
fn filters_and_delete_restore() {
    let mut s = demo();
    s.execute("library.filter", &json!({"rating": 4})).unwrap();
    let n = s.visible().len();
    assert!(n > 0 && n < 24);
    s.execute("library.clearFilter", &json!({})).unwrap();
    s.execute("photo.delete", &json!({})).unwrap();
    assert_eq!(s.visible().len(), 23);
    s.execute("library.source", &json!({"kind": "recentlyDeleted"})).unwrap();
    assert_eq!(s.visible().len(), 1);
    s.execute("photo.restore", &json!({})).unwrap();
    s.execute("library.source", &json!({"kind": "all"})).unwrap();
    assert_eq!(s.visible().len(), 24);
}

#[test]
fn deleting_photos_selects_a_neighbour_in_the_current_view() {
    for params in [json!({}), json!({"key": "fileName", "ascending": false}), json!({"key": "random", "seed": 123})] {
        for at_end in [false, true] {
            let mut s = demo();
            s.execute("library.sort", &params).unwrap();
            s.execute("library.filter", &json!({"rating": 2})).unwrap();
            let visible = s.visible_cloned();
            assert!(visible.len() > 5);
            let at = if at_end { visible.len() - 2 } else { visible.len() / 2 };
            let deleted = &visible[at..at + 2];
            s.execute("library.select", &json!({"ids": deleted, "active": deleted[0]})).unwrap();
            s.execute("photo.delete", &json!({})).unwrap();
            let expected = if at_end { visible[at - 1] } else { visible[at + 2] };
            assert_eq!(s.selection, Selection::single(expected), "sort {params}, end {at_end}");
            assert!(deleted.iter().all(|id| s.catalog.photo(*id).unwrap().deleted));
            s.execute("edit.undo", &json!({})).unwrap();
            assert!(deleted.iter().all(|id| !s.catalog.photo(*id).unwrap().deleted));
            s.execute("edit.redo", &json!({})).unwrap();
            assert!(deleted.iter().all(|id| s.catalog.photo(*id).unwrap().deleted));
        }
    }
    let mut s = demo();
    s.execute("library.selectAll", &json!({})).unwrap();
    s.execute("photo.delete", &json!({})).unwrap();
    assert!(s.visible().is_empty());
    assert_eq!(s.selection, Selection::default());
}

#[test]
fn deleting_explicit_targets_keeps_the_surviving_selection() {
    let mut s = demo();
    let visible = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [visible[8], visible[9]], "active": visible[9]})).unwrap();
    let before = s.selection.clone();
    s.execute("photo.delete", &json!({"ids": [visible[3]]})).unwrap();
    assert_eq!(s.selection, before, "deleting an unselected photo doesn't change selection");
    s.execute("photo.delete", &json!({"ids": [visible[9]]})).unwrap();
    assert_eq!(s.selection, Selection::single(visible[8]), "partial deletion retains a selected survivor");
}

#[test]
fn permanently_deleting_photos_selects_a_neighbour_in_recently_deleted() {
    for params in [json!({}), json!({"key": "fileName", "ascending": false}), json!({"key": "random", "seed": 123})] {
        for at_end in [false, true] {
            let mut s = demo();
            s.execute("library.selectAll", &json!({})).unwrap();
            s.execute("photo.delete", &json!({})).unwrap();
            s.execute("library.source", &json!({"kind": "recentlyDeleted"})).unwrap();
            s.execute("library.sort", &params).unwrap();
            s.execute("library.filter", &json!({"rating": 2})).unwrap();
            let visible = s.visible_cloned();
            assert!(visible.len() > 5);
            let at = if at_end { visible.len() - 2 } else { visible.len() / 2 };
            let deleted = &visible[at..at + 2];
            s.execute("library.select", &json!({"ids": deleted, "active": deleted[0]})).unwrap();
            let snapshot = s.catalog.to_snapshot();
            let undo_len = s.undo.len();
            let r = s.execute("photo.deletePermanently", &json!({})).unwrap();
            assert_eq!(r["deleted"], 2);
            let expected = if at_end { visible[at - 1] } else { visible[at + 2] };
            assert_eq!(s.selection, Selection::single(expected), "sort {params}, end {at_end}");
            assert!(deleted.iter().all(|id| s.catalog.photo(*id).is_none()));
            assert_eq!(s.undo.len(), undo_len + 1);
            let removed = s.catalog.to_snapshot();
            s.execute("edit.undo", &json!({})).unwrap();
            assert_eq!(s.catalog.to_snapshot(), snapshot);
            s.execute("edit.redo", &json!({})).unwrap();
            assert_eq!(s.catalog.to_snapshot(), removed);
        }
    }
    let mut s = demo();
    s.execute("library.selectAll", &json!({})).unwrap();
    s.execute("photo.deletePermanently", &json!({})).unwrap();
    assert!(s.visible().is_empty());
    assert_eq!(s.selection, Selection::default());
}

#[test]
fn permanently_deleting_explicit_targets_keeps_the_surviving_selection() {
    let mut s = demo();
    let visible = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [visible[8], visible[9]], "active": visible[9]})).unwrap();
    let before = s.selection.clone();
    s.execute("photo.deletePermanently", &json!({"ids": [visible[3]]})).unwrap();
    assert_eq!(s.selection, before, "deleting an unselected photo doesn't change selection");
    s.execute("photo.deletePermanently", &json!({"ids": [visible[9]]})).unwrap();
    assert_eq!(s.selection, Selection::single(visible[8]), "partial deletion retains a selected survivor");
    let before = s.selection.clone();
    let snapshot = s.catalog.to_snapshot();
    assert!(s.execute("photo.deletePermanently", &json!({"ids": [999_999]})).is_err());
    assert_eq!(s.selection, before, "a refused deletion doesn't change selection");
    assert_eq!(s.catalog.to_snapshot(), snapshot);
}

#[test]
fn masks_crop_and_render() {
    let mut s = demo();
    s.execute("mask.add", &json!({"kind": "radial", "center": [0.5, 0.5]})).unwrap();
    s.execute("mask.adjust", &json!({"values": {"exposure": 1.0}})).unwrap();
    s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2], [0.3, 0.3]], "size": 0.05})).unwrap();
    let d = active_dev(&s);
    assert_eq!(d.masks.len(), 1);
    assert_eq!(d.masks[0].components.len(), 2);
    s.execute("crop.aspect", &json!({"aspect": "1x1"})).unwrap();
    s.execute("crop.straighten", &json!({"angle": 5.0})).unwrap();
    let id = s.active().unwrap();
    let r = s.render_now(id, 200, 200).unwrap();
    assert_eq!((r.image.width, r.image.height), (200, 200));
    s.execute("develop.auto", &json!({})).unwrap();
    s.execute("develop.wb", &json!({"mode": "auto"})).unwrap();
    s.execute("develop.wbPick", &json!({"x": 0.5, "y": 0.5})).unwrap();
}

#[test]
fn versions_history_and_reset() {
    let mut s = demo();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    s.execute("version.create", &json!({"name": "bright"})).unwrap();
    s.execute("develop.reset", &json!({})).unwrap();
    assert!(active_dev(&s).is_unedited());
    s.execute("version.restore", &json!({"index": 0})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, 1.0);
    let h = s.execute("history.list", &json!({})).unwrap();
    assert!(h["history"].as_array().unwrap().len() >= 3);
}

#[test]
fn op_log_replay_reproduces_catalog() {
    let mut s = demo();
    let snap = s.catalog.to_snapshot();
    let _ = s.drain_log();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.dehaze", "value": 40})).unwrap();
    s.execute("album.create", &json!({"name": "X"})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    let log: String = s.drain_log().iter().map(lightcraft_catalog::Catalog::op_to_log_line).collect();
    let mut c = lightcraft_catalog::Catalog::from_snapshot(&snap).unwrap();
    c.replay(&log).unwrap();
    assert_eq!(c.to_snapshot(), s.catalog.to_snapshot());
}

/// Every command runs (or fails cleanly with an error) with empty params and has metadata.
#[test]
fn command_sweep() {
    let mut s = demo();
    let mut ids = std::collections::HashSet::new();
    for c in command_specs() {
        assert!(ids.insert(c.id), "duplicate {}", c.id);
        assert!(!c.label.is_empty() && !c.params.is_empty(), "{}", c.id);
        let _ = s.execute(c.id, &Value::Null);
    }
    assert!(s.commands().len() >= 70, "{}", s.commands().len());
}

#[test]
fn upright_and_guides_commands() {
    let mut s = demo();
    let r = s.execute("geometry.upright", &json!({"mode": "level"})).unwrap();
    assert_eq!(r["mode"], "level");
    let d = active_dev(&s);
    assert_eq!(d.geometry.upright, lightcraft_develop::Upright::Level);
    assert!(d.geometry.upright_transform.is_some(), "analysis result is stored");
    s.execute("geometry.upright", &json!({"mode": "off"})).unwrap();
    assert!(active_dev(&s).geometry.upright_transform.is_none());
    for i in 0..5 {
        let x = 0.1 + 0.15 * i as f64;
        s.execute("geometry.guides", &json!({"guides": [[x, 0.2, x + 0.02, 0.8]], "add": true})).unwrap();
    }
    let d = active_dev(&s);
    assert_eq!(d.geometry.upright, lightcraft_develop::Upright::Guided);
    assert_eq!(d.geometry.guides.len(), 4, "at most four guides");
    assert!((d.geometry.guides[0].0.x - 0.25).abs() < 1e-9, "oldest dropped");
    let id = s.active().unwrap();
    assert!(s.render_now(id, 160, 160).is_ok());
    assert!(s.execute("geometry.upright", &json!({"mode": "sideways"})).is_err());
}

#[test]
fn memory_report_counts_decoded_sources_and_renders() {
    let mut s = demo();
    let r = s.execute("library.memory", &json!({})).unwrap();
    assert_eq!(r["previewSources"]["count"], 0);
    assert!(r["gpu"]["allocated"].is_u64() && r.get("engineBytes").is_some());
    let id = s.active().unwrap();
    s.render_now(id, 800, 600).unwrap();
    let m = s.memory_report();
    assert_eq!(m.preview_sources.count, 1);
    // a 2560 px linear float source: 12 bytes per pixel
    assert!(m.preview_sources.bytes >= 12 * 2560 * 1000, "{:?}", m.preview_sources);
    assert_eq!(m.engine_bytes, m.thumb_sources.bytes + m.preview_sources.bytes + m.full_source.bytes + m.rendered.bytes);
    let r = s.thumb_job(id, 200).unwrap().run();
    s.accept(&r);
    let m = s.memory_report();
    assert_eq!(m.thumb_sources.count, 1);
    assert!(m.rendered.count >= 1, "the thumbnail render is cached");
}

#[test]
fn paste_selected_settings_pastes_only_chosen_copied_groups() {
    let mut s = demo();
    let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.0, "color.vibrance": 30, "detail.sharpenAmount": 90}})).unwrap();
    s.execute("develop.copy", &json!({"groups": ["light", "color"]})).unwrap();
    // `detail` was not copied: asking for it pastes nothing for it
    s.execute("develop.paste", &json!({"ids": [ids[1].0], "groups": ["light", "detail"]})).unwrap();
    let d = s.develop_of(ids[1]).unwrap();
    let base = lightcraft_develop::DevelopSettings::default();
    assert_eq!(d.light.exposure, 1.0);
    assert_eq!(d.color.vibrance, base.color.vibrance);
    assert_ne!(d.detail.sharpen_amount, 90.0);
    // without groups: everything copied
    s.execute("develop.paste", &json!({"ids": [ids[1].0]})).unwrap();
    assert_eq!(s.develop_of(ids[1]).unwrap().color.vibrance, 30.0);
}

#[test]
fn masks_reorder_and_duplicate_and_invert() {
    let mut s = demo();
    for kind in ["linear", "radial", "brush"] {
        s.execute("mask.add", &json!({"kind": kind})).unwrap();
    }
    let ids = |s: &Session| active_dev(s).masks.iter().map(|m| m.id).collect::<Vec<_>>();
    let [a, b, c] = ids(&s)[..] else { panic!("{:?}", ids(&s)) };
    s.execute("mask.move", &json!({"id": c, "to": 0})).unwrap();
    assert_eq!(ids(&s), [c, a, b]);
    s.execute("mask.move", &json!({"id": c, "delta": 1})).unwrap();
    assert_eq!(ids(&s), [a, c, b]);
    s.execute("mask.move", &json!({"id": a, "delta": -5})).unwrap();
    assert_eq!(ids(&s), [a, c, b], "clamped at the top");
    assert!(s.execute("mask.move", &json!({"id": a})).is_err());
    // one undo step per move
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ids(&s), [c, a, b]);
    // duplicate and invert: right after the original, inverted, selected
    let r = s.execute("mask.duplicate", &json!({"id": a, "invert": true})).unwrap();
    let copy = r["activeMask"].as_u64().unwrap() as u32;
    let d = active_dev(&s);
    let pos = d.masks.iter().position(|m| m.id == copy).unwrap();
    assert_eq!(d.masks[pos - 1].id, a);
    assert!(d.masks[pos].invert && !d.masks[pos - 1].invert);
    assert_eq!(d.masks[pos].components, d.masks[pos - 1].components);
    assert_eq!(s.active_mask, Some(copy));
}

#[test]
fn before_is_the_import_state_and_can_be_set_copied_and_swapped() {
    let mut s = demo();
    let id = s.active().unwrap();
    let import = s.catalog.photo(id).unwrap().import_defaults();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.5}})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.1, 0.1, 0.9, 0.9]})).unwrap();
    let before = |s: &Session| s.before_settings(s.catalog.photo(s.active().unwrap()).unwrap());
    // default: the import state, with the current crop so both sides line up
    let b = before(&s);
    assert_eq!(b.light.exposure, import.light.exposure);
    assert_eq!(b.crop, active_dev(&s).crop);
    // from the current settings, then a history step
    s.execute("beforeAfter.copyAfterToBefore", &json!({})).unwrap();
    assert_eq!(before(&s).light.exposure, 1.5);
    s.execute("develop.set", &json!({"values": {"light.exposure": -1.0}})).unwrap();
    let steps = s.catalog.photo(id).unwrap().history.len();
    let r = s.execute("beforeAfter.setBefore", &json!({"source": "history", "index": steps - 1})).unwrap();
    assert_eq!(r["before"], "custom");
    assert_eq!(before(&s).light.exposure, -1.0);
    assert!(s.execute("beforeAfter.setBefore", &json!({"source": "history", "index": 999})).is_err());
    // swap: the photo gets the before settings, the before side the old current ones
    s.execute("beforeAfter.setBefore", &json!({"source": "import"})).unwrap();
    s.execute("beforeAfter.swap", &json!({})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, import.light.exposure);
    assert_eq!(before(&s).light.exposure, -1.0);
    // copy before → after is one undoable edit
    s.execute("beforeAfter.copyBeforeToAfter", &json!({})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, -1.0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(active_dev(&s).light.exposure, import.light.exposure);
    // the before render follows (the render key changes with the before settings)
    let k1 = s.render_job(id, 64, 64, true, true).unwrap().key;
    s.execute("beforeAfter.resetBefore", &json!({})).unwrap();
    let k2 = s.render_job(id, 64, 64, true, true).unwrap().key;
    assert_ne!(k1, k2);
}

#[test]
fn presets_versions_history_and_select_by() {
    let mut s = demo();
    s.execute("develop.set", &json!({"values": {"light.exposure": 0.5}})).unwrap();
    let pid = s.execute("preset.create", &json!({"name": "Warm", "groups": ["light"]})).unwrap()["id"].as_str().unwrap().to_string();
    let preset = |s: &Session| s.presets.iter().find(|p| p.id == pid).cloned().unwrap();
    s.execute("preset.rename", &json!({"id": pid, "name": "Warmer"})).unwrap();
    s.execute("preset.move", &json!({"id": pid, "group": "Mine"})).unwrap();
    assert_eq!((preset(&s).name.as_str(), preset(&s).group.as_str()), ("Warmer", "Mine"));
    // update keeps the preset's groups and takes the current values
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.25, "color.vibrance": 40}})).unwrap();
    s.execute("preset.update", &json!({"id": pid})).unwrap();
    assert_eq!(preset(&s).settings["light"]["exposure"], 1.25);
    assert!(preset(&s).settings.get("color").is_none(), "only the groups it already had");
    let builtin = s.presets.iter().find(|p| p.builtin).unwrap().id.clone();
    assert!(s.execute("preset.rename", &json!({"id": builtin, "name": "x"})).is_err());
    // versions
    s.execute("version.create", &json!({"name": "A"})).unwrap();
    s.execute("version.rename", &json!({"index": 0, "name": "First"})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": -2.0}})).unwrap();
    s.execute("version.update", &json!({"index": 0})).unwrap();
    let v = s.catalog.photo(s.active().unwrap()).unwrap().versions[0].clone();
    assert_eq!((v.name.as_str(), v.settings.light.exposure), ("First", -2.0));
    // history
    s.execute("history.clear", &json!({})).unwrap();
    let h = &s.catalog.photo(s.active().unwrap()).unwrap().history;
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].settings.light.exposure, -2.0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(s.active().unwrap()).unwrap().history.len() > 1, "undoable");
    // select by: the demo has rated and picked photos
    let vis = s.visible_cloned();
    let picks = vis.iter().filter(|id| s.catalog.photo(**id).unwrap().flag == lightcraft_catalog::Flag::Pick).count();
    let r = s.execute("library.selectBy", &json!({"flag": "pick"})).unwrap();
    assert_eq!(r["selected"].as_u64(), Some(picks as u64));
    assert!(picks > 0);
    let four = vis.iter().filter(|id| s.catalog.photo(**id).unwrap().rating >= 4).count();
    assert_eq!(s.execute("library.selectBy", &json!({"rating": 4})).unwrap()["selected"].as_u64(), Some(four as u64));
    let r = s.execute("library.selectBy", &json!({"rating": 0, "ratingOp": "eq", "add": true})).unwrap();
    assert!(r["selected"].as_u64().unwrap() >= four as u64);
    assert!(s.execute("library.selectBy", &json!({})).is_err());
    assert!(s.execute("library.selectBy", &json!({"label": "mauve"})).is_err());
}

#[test]
fn crop_aspect_lock_current_and_toggle() {
    let mut s = demo();
    s.execute("crop.set", &json!({"rect": [0.1, 0.2, 0.7, 0.6]})).unwrap();
    let rect = active_dev(&s).crop.geometry.rect;
    s.execute("crop.aspect", &json!({"aspect": "toggle"})).unwrap();
    let d = active_dev(&s);
    assert_eq!(d.crop.geometry.rect, rect, "locking keeps the rectangle");
    let (aw, ah) = d.crop.aspect.expect("locked");
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap();
    let (w, h) = if d.orientation.swaps_axes() { (p.height as f64, p.width as f64) } else { (p.width as f64, p.height as f64) };
    let want = (rect.x1 - rect.x0) * w / ((rect.y1 - rect.y0) * h);
    assert!((aw as f64 / ah as f64 - want).abs() < 0.01, "{aw}:{ah} vs {want}");
    s.execute("crop.aspect", &json!({"aspect": "toggle"})).unwrap();
    assert!(active_dev(&s).crop.aspect.is_none(), "toggle unlocks");
}

// Feature: dragging crop handles (issue 295)
#[test]
fn crop_drag_stops_at_the_image_edge_and_keeps_the_anchor() {
    let mut s = demo();
    s.execute("crop.set", &json!({"rect": [0.2, 0.2, 0.8, 0.8]})).unwrap();
    s.execute("crop.drag", &json!({"handle": "bottomRight", "from": [0.8, 0.8], "to": [1.4, 0.7]})).unwrap();
    let r = active_dev(&s).crop.geometry.rect;
    assert!((r.x0 - 0.2).abs() < 1e-6 && (r.y0 - 0.2).abs() < 1e-6, "anchor moved: {r:?}");
    assert!((r.x1 - 1.0).abs() < 1e-6 && (r.y1 - 0.7).abs() < 1e-6, "{r:?}");
}

#[test]
fn crop_drag_keeps_a_locked_ratio_on_every_handle() {
    let mut s = demo();
    s.execute("crop.aspect", &json!({"aspect": "16x9"})).unwrap();
    let d = active_dev(&s);
    assert!(d.crop.aspect.is_some());
    let want = d.crop.geometry.rect.aspect();
    let r = d.crop.geometry.rect;
    for (handle, to) in [("left", [r.x0 + 0.1, 0.5]), ("top", [0.5, r.y0 + 0.05]), ("bottomRight", [1.3, 1.3]), ("right", [0.9, 0.5])] {
        s.execute("crop.drag", &json!({"handle": handle, "from": [0.5, 0.5], "to": to, "start": [r.x0, r.y0, r.x1, r.y1]})).unwrap();
        let g = active_dev(&s).crop.geometry.rect;
        assert!((g.aspect() - want).abs() < 1e-6, "{handle}: {} vs {want}", g.aspect());
    }
}

#[test]
fn crop_drag_rejects_unknown_handles_and_bad_points() {
    let mut s = demo();
    assert!(s.execute("crop.drag", &json!({"handle": "sideways", "from": [0.5, 0.5], "to": [0.6, 0.6]})).is_err());
    assert!(s.execute("crop.drag", &json!({"handle": "move", "from": [0.5], "to": [0.6, 0.6]})).is_err());
    assert!(s.execute("crop.drag", &json!({"handle": "move", "to": [0.6, 0.6]})).is_err());
}

#[test]
fn crop_aspect_rejects_degenerate_custom_ratios() {
    let mut s = demo();
    let before = active_dev(&s).crop;
    for bad in [json!([0.004, 3]), json!([3, 0]), json!([-1, 2]), json!([1000, 0.01]), json!(["a", 2]), json!([1, 2, 3])] {
        assert!(s.execute("crop.aspect", &json!({"aspect": bad.clone()})).is_err(), "{bad}");
    }
    assert!(s.execute("crop.aspect", &json!({"aspect": "0x5"})).is_err());
    let after = active_dev(&s).crop;
    assert_eq!(before.geometry, after.geometry, "a rejected ratio leaves the crop alone");
    s.execute("crop.aspect", &json!({"aspect": [2.5, 1]})).unwrap();
    assert!(!active_dev(&s).crop.geometry.rect.is_empty());
}

#[test]
fn crop_drag_rejects_non_numeric_points() {
    let mut s = demo();
    assert!(s.execute("crop.drag", &json!({"handle": "move", "from": ["a", 0.5, 0.5], "to": [0.6, 0.6]})).is_err());
    assert!(s.execute("crop.drag", &json!({"handle": "move", "from": [0.5, 0.5], "to": [0.6, 0.6], "start": [0.1, "x", 0.5, 0.5]})).is_err());
}

#[test]
fn paste_from_previous_and_copy_paste_metadata() {
    let mut s = demo();
    let ids: Vec<_> = s.visible().iter().copied().take(3).collect();
    s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
    assert!(s.execute("develop.pastePrevious", &json!({})).is_err(), "nothing before");
    s.execute("develop.set", &json!({"values": {"light.exposure": 0.8}})).unwrap();
    s.execute("photo.setMeta", &json!({"title": "Dawn", "keywords": ["sky", "red"], "creator": "Me"})).unwrap();
    s.execute("photo.copyMetadata", &json!({})).unwrap();
    // move on: the first photo becomes "previous"
    s.execute("library.select", &json!({"ids": [ids[1].0]})).unwrap();
    assert_eq!(s.previous_active, Some(ids[0]));
    let r = s.execute("develop.pastePrevious", &json!({})).unwrap();
    assert_eq!(r["from"].as_u64(), Some(ids[0].0));
    assert_eq!(active_dev(&s).light.exposure, 0.8);
    // metadata: only the chosen fields
    s.execute("photo.pasteMetadata", &json!({"fields": ["title", "keywords"]})).unwrap();
    let m = &s.catalog.photo(ids[1]).unwrap().meta;
    assert_eq!((m.title.as_str(), m.keywords.clone()), ("Dawn", vec!["sky".to_string(), "red".to_string()]));
    assert_ne!(m.creator, "Me");
    s.execute("photo.pasteMetadata", &json!({"ids": [ids[2].0]})).unwrap();
    assert_eq!(s.catalog.photo(ids[2]).unwrap().meta.creator, "Me");
}

#[test]
fn stacks_move_and_split() {
    let mut s = demo();
    let ids: Vec<_> = s.visible().iter().copied().take(5).collect();
    s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "active": ids[0].0})).unwrap();
    s.execute("stack.group", &json!({"collapsed": false})).unwrap();
    let order = |s: &Session| s.catalog.stack_of(ids[0]).or(s.catalog.stack_of(ids[4])).map(|st| st.photos.clone()).unwrap_or_default();
    let before = order(&s);
    assert_eq!(before.len(), 5);
    s.execute("stack.moveUp", &json!({"id": before[2].0})).unwrap();
    assert_eq!(order(&s)[1], before[2]);
    s.execute("stack.moveDown", &json!({"id": before[2].0})).unwrap();
    assert_eq!(order(&s), before);
    // split at the 4th photo: 3 stay, 2 form a new stack
    s.execute("stack.split", &json!({"id": before[3].0})).unwrap();
    assert_eq!(s.catalog.stack_of(before[0]).unwrap().photos, before[..3].to_vec());
    assert_eq!(s.catalog.stack_of(before[3]).unwrap().photos, before[3..].to_vec());
    // splitting at the last photo of a 2-stack leaves both unstacked
    s.execute("stack.split", &json!({"id": before[4].0})).unwrap();
    assert!(s.catalog.stack_of(before[3]).is_none() && s.catalog.stack_of(before[4]).is_none());
    assert!(s.execute("stack.split", &json!({"id": before[0].0})).is_err(), "the top can't split");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.stack_of(before[3]).unwrap().photos.len(), 2);
}

#[test]
fn filter_presets_save_and_apply() {
    let mut s = demo();
    assert!(s.execute("filter.savePreset", &json!({"name": "Empty"})).is_err(), "nothing to save");
    s.execute("library.filter", &json!({"rating": 4})).unwrap();
    let four = s.visible_cloned().len();
    s.execute("filter.savePreset", &json!({"name": "Best"})).unwrap();
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert!(s.visible_cloned().len() > four);
    let r = s.execute("filter.applyPreset", &json!({"name": "best"})).unwrap();
    assert_eq!(r["photos"].as_u64(), Some(four as u64));
    s.execute("filter.deletePreset", &json!({"name": "Best"})).unwrap();
    assert!(s.filter_presets.is_empty());
}

#[test]
fn auto_bw_mix_separates_colours() {
    let mut s = demo();
    s.execute("develop.autoBwMix", &json!({})).unwrap();
    let d = active_dev(&s);
    assert_eq!(d.treatment, lightcraft_develop::Treatment::Bw);
    let m = d.bw_mix.bands();
    assert!(m.iter().any(|v| *v != 0.0), "some band moved: {m:?}");
    assert!(m.iter().all(|v| v.abs() <= 60.0));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_ne!(active_dev(&s).treatment, lightcraft_develop::Treatment::Bw, "one undo step");
}

#[test]
fn auto_sync_carries_only_the_changed_settings() {
    use lightcraft_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    // the second photo has its own contrast, which must survive
    s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.contrast", "value": 25})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.selection.active = Some(PhotoId(ids[0]));
    assert_eq!(s.execute("develop.autoSync", &json!({})).unwrap(), json!({"on": true}));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();
    for id in &ids {
        assert_eq!(s.develop_of(PhotoId(*id)).unwrap().light.exposure, 0.7, "photo {id}");
    }
    assert_eq!(s.develop_of(PhotoId(ids[1])).unwrap().light.contrast, 25.0, "untouched settings stay");
    // a slider drag syncs once, on release
    let shadows: Vec<f64> = ids.iter().map(|id| s.develop_of(PhotoId(*id)).unwrap().light.shadows).collect();
    s.begin_interaction("Shadows").unwrap();
    let mut d = (*s.develop_of(PhotoId(ids[0])).unwrap()).clone();
    d.light.shadows = 40.0;
    s.set_develop(PhotoId(ids[0]), d, "Shadows").unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[2])).unwrap().light.shadows, shadows[2], "not while dragging");
    s.end_interaction().unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[2])).unwrap().light.shadows, 40.0);
    // one undo step reverts every photo
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ids.iter().map(|id| s.develop_of(PhotoId(*id)).unwrap().light.shadows).collect::<Vec<_>>(), shadows);
    // spot removal belongs to one photo
    let mut d = (*s.develop_of(PhotoId(ids[0])).unwrap()).clone();
    d.spots.push(lightcraft_develop::Spot::default());
    s.set_develop(PhotoId(ids[0]), d, "Remove").unwrap();
    assert!(s.develop_of(PhotoId(ids[1])).unwrap().spots.is_empty());
    // off: only the active photo
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": -1})).unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[1])).unwrap().light.exposure, 0.7);
}

#[test]
fn json_delta_keeps_only_changes() {
    let a = json!({"light": {"exposure": 0, "contrast": 5}, "masks": [1], "x": 1});
    let b = json!({"light": {"exposure": 1, "contrast": 5}, "masks": [1, 2], "x": 1});
    assert_eq!(crate::json_delta(&a, &b), Some(json!({"light": {"exposure": 1}, "masks": [1, 2]})));
    assert_eq!(crate::json_delta(&a, &a), None);
}

#[test]
fn build_previews_fills_the_cache() {
    use lightcraft_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    let cached = |s: &mut Session, id: u64| {
        let t = s.thumb_job(PhotoId(id), 512).unwrap();
        let (c, k) = t.cache.clone().unwrap();
        let q = s.quick_view_job(PhotoId(id), 1024, true).unwrap();
        let (vc, vk) = q.cached[0].clone();
        (c.get(k).is_some(), vc.get(vk).is_some())
    };
    assert_eq!(cached(&mut s, ids[0]), (false, false));
    let r = s.execute("library.buildPreviews", &json!({"ids": [ids[0], ids[1]], "size": "standard", "edge": 512, "wait": true})).unwrap();
    assert_eq!((r["total"].as_u64(), r["done"].as_u64(), r["failed"].as_u64(), r["running"].as_bool()), (Some(2), Some(2), Some(0), Some(false)));
    // view previews are written to the cache in the background
    let t0 = std::time::Instant::now();
    while cached(&mut s, ids[1]) != (true, true) && t0.elapsed().as_secs() < 20 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(cached(&mut s, ids[0]), (true, true), "thumbnail and loupe view are ready");
    assert_eq!(cached(&mut s, ids[2]), (false, false), "only the photos asked for");
    // background build: returns at once, progress by command
    let r = s.execute("library.buildPreviews", &json!({"ids": [ids[2]], "edge": 512})).unwrap();
    assert_eq!(r["total"], 1);
    let t0 = std::time::Instant::now();
    loop {
        let p = s.execute("library.previewProgress", &json!({})).unwrap();
        if p["running"] == false {
            assert_eq!(p["done"], 1);
            break;
        }
        assert!(t0.elapsed().as_secs() < 60, "build finishes");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(s.execute("library.buildPreviews", &json!({"size": "huge"})).is_err());
}

/// Wait (at most a minute) until no background task is listed.
fn wait_activity_empty(s: &Session) {
    let t0 = std::time::Instant::now();
    while !s.activity.list().is_empty() {
        assert!(t0.elapsed().as_secs() < 60, "tasks still listed: {:?}", s.activity.list());
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Issue #345: a background preview build is a row in the activity stack while it runs.
#[test]
fn preview_build_shows_in_activity_until_it_finishes() {
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(4).map(|p| p.id.0).collect();
    let r = s.execute("library.buildPreviews", &json!({"ids": ids, "size": "standard", "edge": 256})).unwrap();
    let tasks = s.activity.list();
    assert_eq!(tasks.first().map(|t| (t.kind, t.total, t.label.as_str())), Some(("previews", 4, "Building previews")), "{tasks:?} {r}");
    assert!(tasks[0].cancellable);
    wait_activity_empty(&s);
    assert_eq!(s.execute("library.previewProgress", &json!({})).unwrap()["running"], false);
}

/// Issue #345: `activity.cancel` stops a preview build like `library.cancelPreviews`.
#[test]
fn activity_cancel_stops_a_preview_build() {
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
    s.execute("library.buildPreviews", &json!({"ids": ids, "size": "full"})).unwrap();
    let id = s.activity.list().first().map(|t| t.id).expect("a previews row");
    s.execute("activity.cancel", &json!({"id": id})).unwrap();
    wait_activity_empty(&s);
    let p = s.execute("library.previewProgress", &json!({})).unwrap();
    assert_eq!(p["cancelled"], true, "{p}");
    assert!(p["done"].as_u64() < p["total"].as_u64(), "stopped part-way: {p}");
}

/// Colour range: a click samples the colour under it, so the mask selects that colour (white
/// in the mask view) and not a different one; ⇧ adds samples, up to five.
#[test]
fn color_range_sampling_selects_the_clicked_colour() {
    use lightcraft_pipeline::{MaskView, Overlay};
    let mut s = demo();
    // a flower: magenta petals at the centre, green around them
    let id = s.catalog.photos().find(|p| p.meta.keywords.iter().any(|k| k == "flower")).unwrap().id;
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    s.execute("mask.add", &json!({"kind": "colorRange"})).unwrap();
    let mask = |s: &mut Session| {
        let m = s.develop_of(id).unwrap().masks[0].clone();
        let job = s.render_job(id, 200, 200, false, true).unwrap().with_overlay(Overlay::Mask {
            id: m.id as u16,
            view: MaskView::WhiteOnBlack,
            color: [255, 0, 0],
            opacity: 100,
        });
        job.run().rendered.unwrap().image
    };
    let at =
        |img: &lightcraft_raster::Rgba8, x: f64, y: f64| img.data[(y * img.height as f64) as usize * img.width + (x * img.width as f64) as usize][0];
    let r = s.execute("mask.sampleColor", &json!({"x": 0.5, "y": 0.5})).unwrap();
    assert_eq!(r["samples"].as_array().unwrap().len(), 1);
    let img = mask(&mut s);
    let (centre, corner) = (at(&img, 0.5, 0.5), at(&img, 0.03, 0.03));
    assert!(centre > 200 && corner < centre / 2, "the sampled colour is selected: centre {centre}, corner {corner}");
    // ⇧-click adds, at most five
    for _ in 0..6 {
        s.execute("mask.sampleColor", &json!({"x": 0.03, "y": 0.03, "add": true})).unwrap();
    }
    let n = s.execute("mask.sampleColor", &json!({"x": 0.5, "y": 0.52, "add": true})).unwrap()["samples"].as_array().unwrap().len();
    assert_eq!(n, 5);
    assert!(at(&mask(&mut s), 0.03, 0.03) > 200, "the added colour is selected too");
    // a plain click starts over
    assert_eq!(s.execute("mask.sampleColor", &json!({"x": 0.5, "y": 0.5})).unwrap()["samples"].as_array().unwrap().len(), 1);
    assert!(s.execute("mask.sampleColor", &json!({"x": 1.5, "y": 0.5})).is_err());
}

#[test]
fn leaving_an_edited_photo_keeps_an_auto_version() {
    use lightcraft_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
    let before = s.catalog.photo(PhotoId(ids[0])).unwrap().versions.len();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    let undo = s.undo.len();
    s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
    let v = s.catalog.photo(PhotoId(ids[0])).unwrap().versions.clone();
    assert_eq!(v.len(), before + 1);
    assert!(v.last().unwrap().auto && v.last().unwrap().settings.light.exposure == 0.4);
    assert_eq!(s.undo.len(), undo, "not an undo step");
    // back and away again without changes: nothing new
    s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
    s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
    assert_eq!(s.catalog.photo(PhotoId(ids[0])).unwrap().versions.len(), before + 1);
    // at most AUTO_VERSIONS
    for i in 0..(crate::AUTO_VERSIONS + 5) {
        s.execute("library.select", &json!({"ids": [ids[0]]})).unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.01 * i as f64 + 0.5})).unwrap();
        s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
    }
    let autos = s.catalog.photo(PhotoId(ids[0])).unwrap().versions.iter().filter(|v| v.auto).count();
    assert_eq!(autos, crate::AUTO_VERSIONS);
}

#[test]
fn match_total_exposures() {
    use lightcraft_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    // reference 1/100 f/4 ISO 100, slider +0.5; other 1/50 f/4 ISO 100 (one stop more light)
    for (i, (sh, ap, iso)) in [("1/100", 4.0f32, 100u32), ("1/50", 4.0, 100), ("", 4.0, 100)].iter().enumerate() {
        let mut m = s.catalog.photo(PhotoId(ids[i])).unwrap().meta.clone();
        m.shutter = sh.to_string();
        m.aperture = Some(*ap);
        m.iso = Some(*iso);
        s.commit("t", lightcraft_catalog::Op::SetMeta { id: PhotoId(ids[i]), meta: Box::new(m) }).unwrap();
    }
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.selection.active = Some(PhotoId(ids[0]));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5, "ids": [ids[0]]})).unwrap();
    let r = s.execute("develop.matchExposure", &json!({})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["skipped"].clone()), (Some(1), json!([ids[2]])));
    let e = s.develop_of(PhotoId(ids[1])).unwrap().light.exposure;
    assert!((e - (-0.5)).abs() < 1e-9, "one stop more light → one stop less exposure: {e}");
}

#[test]
fn find_similar_filters_to_look_alikes() {
    let mut s = demo();
    let id = s.catalog.photos().next().unwrap().id;
    let r = s.execute("library.findSimilar", &json!({"id": id.0, "similarity": 0.5})).unwrap();
    let hits: Vec<u64> = r["photos"].as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect();
    assert_eq!(hits[0], id.0, "the photo itself is the closest");
    assert!(hits.len() < s.catalog.len(), "not everything looks alike");
    let vis = s.visible_cloned();
    assert!(!vis.is_empty() && vis.iter().all(|v| hits.contains(&v.0)), "the view shows only them");
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert!(s.visible_cloned().len() > vis.len());
}

#[test]
fn quick_develop_adds_to_each_photo() {
    use lightcraft_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5, "ids": [ids[0]]})).unwrap();
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    let before: Vec<f64> = ids.iter().map(|i| s.develop_of(PhotoId(*i)).unwrap().light.exposure).collect();
    s.execute("develop.quickAdjust", &json!({"control": "light.exposure", "delta": 1.0 / 3.0})).unwrap();
    for (i, id) in ids.iter().enumerate() {
        let e = s.develop_of(PhotoId(*id)).unwrap().light.exposure;
        assert!((e - before[i] - 1.0 / 3.0).abs() < 0.02, "{e} vs {}", before[i]);
    }
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[0])).unwrap().light.exposure, before[0], "one undo step");
}

#[test]
fn mask_components_invert_duplicate_rename_change_mode_and_delete() {
    use lightcraft_develop::MaskOp;
    let mut s = demo();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("mask.addComponent", &json!({"op": "subtract", "kind": "linear"})).unwrap();
    let mask = |s: &Session| active_dev(s).masks[0].clone();
    let id = mask(&s).id;
    s.execute("mask.component", &json!({"component": 1, "action": "invert"})).unwrap();
    assert!(mask(&s).components[1].invert);
    s.execute("mask.component", &json!({"component": 1, "action": "op", "op": "intersect"})).unwrap();
    assert_eq!(mask(&s).components[1].op, MaskOp::Intersect);
    s.execute("mask.component", &json!({"component": 0, "action": "rename", "name": " Face "})).unwrap();
    assert_eq!(mask(&s).components[0].name.as_deref(), Some("Face"));
    s.execute("mask.component", &json!({"component": 0, "action": "duplicate"})).unwrap();
    assert_eq!(mask(&s).components.len(), 3);
    assert_eq!(mask(&s).components[1].name.as_deref(), Some("Face"));
    // a cleared name falls back to the kind; one undo step each
    s.execute("mask.component", &json!({"component": 1, "action": "rename", "name": ""})).unwrap();
    assert_eq!(mask(&s).components[1].name, None);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(mask(&s).components[1].name.as_deref(), Some("Face"));
    assert!(s.execute("mask.component", &json!({"component": 9, "action": "invert"})).is_err());
    assert!(s.execute("mask.component", &json!({"component": 0, "action": "explode"})).is_err());
    // deleting the last component deletes the mask
    for _ in 0..3 {
        s.execute("mask.component", &json!({"id": id, "component": 0, "action": "delete"})).unwrap();
    }
    assert!(active_dev(&s).masks.is_empty());
    assert_eq!(s.active_mask, None);
}

#[test]
fn curve_reset_by_channel_and_whole() {
    let mut s = demo();
    let s_curve = [[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]];
    for ch in ["master", "red", "green", "blue"] {
        s.execute("develop.curve", &json!({"channel": ch, "points": s_curve})).unwrap();
    }
    s.execute("develop.set", &json!({"control": "curve.shadows", "value": 30})).unwrap();
    s.execute("develop.set", &json!({"control": "curve.refineSaturation", "value": 50})).unwrap();
    // one channel
    s.execute("curve.reset", &json!({"channel": "red"})).unwrap();
    let c = active_dev(&s).curve;
    assert!(c.red.is_empty() && c.green.len() == 4 && c.master.len() == 4, "only red resets");
    assert_eq!(c.shadows, 30.0);
    // parametric only
    s.execute("curve.reset", &json!({"channel": "parametric"})).unwrap();
    let c = active_dev(&s).curve;
    assert_eq!(c.shadows, 0.0);
    assert_eq!(c.master.len(), 4, "point curves stay");
    // all point curves, then undo restores them in one step
    s.execute("curve.reset", &json!({"channel": "point"})).unwrap();
    let c = active_dev(&s).curve;
    assert!(c.master.is_empty() && c.green.is_empty() && c.blue.is_empty());
    assert_eq!(c.refine_saturation, 50.0);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(active_dev(&s).curve.master.len(), 4);
    // everything (the default)
    s.execute("curve.reset", &json!({})).unwrap();
    assert_eq!(active_dev(&s).curve, lightcraft_develop::ToneCurve::default());
    assert!(s.execute("curve.reset", &json!({"channel": "alpha"})).is_err());
}

#[test]
fn ai_masks_without_the_model_explain_themselves() {
    let mut s = demo();
    let id = s.active().unwrap();
    // an Object mask that could never be computed is refused, not left empty
    let e = s.execute("mask.add", &json!({"kind": "object"})).unwrap_err().to_string();
    assert!(e.contains("SAM 3") || e.contains("not available"), "{e}");
    assert!(!e.contains("invalid parameters"), "shown as is: {e}");
    assert!(s.develop_of(id).unwrap().masks.is_empty());
    // an empty Object component (as an older document may hold) still takes clicks
    let shape = lightcraft_develop::MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 };
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks.push(lightcraft_develop::Mask {
        id: 1,
        components: vec![lightcraft_develop::MaskComponent { name: None, op: lightcraft_develop::MaskOp::Add, invert: false, shape }],
        ..Default::default()
    });
    s.set_develop(id, d, "test").unwrap();
    s.active_mask = Some(1);
    // clicks and descriptions need the model: an error that says what's missing, nothing changed
    let e = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap_err().to_string();
    assert!(e.contains("SAM 3") || e.contains("not available"), "{e}");
    let e = s.execute("mask.add", &json!({"kind": "prompt", "text": "sky"})).unwrap_err().to_string();
    assert!(e.contains("SAM 3") || e.contains("not available"), "{e}");
    let empty = std::env::temp_dir().join(format!("lc-no-sam3-{}", std::process::id()));
    s.segmenter.dir = Some(empty.clone());
    let e = s.execute("mask.objectPoint", &json!({"x": 0.5, "y": 0.5})).unwrap_err().to_string();
    assert!(e.contains(&empty.display().to_string()) || e.contains("not available"), "names the folder: {e}");
    assert_eq!(s.develop_of(id).unwrap().masks.len(), 1);
    // bad input is an error, not a crash
    assert!(s.execute("mask.objectPoint", &json!({"x": 1.5, "y": 0.5})).is_err());
    assert!(s.execute("mask.add", &json!({"kind": "prompt", "text": "  "})).is_err());
}

/// A People card's close-up is a square (in pixels) inside the photo, around the face, whatever the
/// face rectangle says: corners, oversized and degenerate rectangles never leave the frame or panic.
#[test]
fn face_job_crops_a_square_inside_the_photo() {
    use lightcraft_geom::Rect;
    let mut s = demo();
    let id = s.active().unwrap();
    let (w, h) = {
        let p = s.catalog.photo(id).unwrap();
        (f64::from(p.width), f64::from(p.height))
    };
    let faces = [
        Rect { x0: 0.40, y0: 0.30, x1: 0.50, y1: 0.45 },
        Rect { x0: 0.0, y0: 0.0, x1: 0.05, y1: 0.05 },
        Rect { x0: 0.95, y0: 0.95, x1: 1.0, y1: 1.0 },
        Rect { x0: -5.0, y0: -5.0, x1: 5.0, y1: 5.0 },
        Rect { x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5 },
        Rect { x0: f64::NAN, y0: 0.5, x1: 0.6, y1: f64::INFINITY },
    ];
    for face in faces {
        let job = s.face_job(id, face, 256).expect("a job for a photo in the library");
        let r = job.settings.crop.geometry.rect;
        assert!((0.0..=1.0).contains(&r.x0) && (0.0..=1.0).contains(&r.y0) && r.x1 <= 1.0 + 1e-9 && r.y1 <= 1.0 + 1e-9, "{face:?} → {r:?}");
        assert!(r.x1 > r.x0 && r.y1 > r.y0, "{face:?} → {r:?}");
        let (pw, ph) = ((r.x1 - r.x0) * w, (r.y1 - r.y0) * h);
        assert!((pw - ph).abs() < 1e-6 * w.max(h), "square in pixels: {face:?} → {pw} × {ph}");
        assert_eq!(job.settings.crop.geometry.angle, 0.0);
    }
    // a face around (0.45, 0.375) is centred in its crop (room to spare on every side)
    let r = s.face_job(id, faces[0], 256).unwrap().settings.crop.geometry.rect;
    assert!((((r.x0 + r.x1) / 2.0) - 0.45).abs() < 1e-9 && (((r.y0 + r.y1) / 2.0) - 0.375).abs() < 1e-9, "{r:?}");
    assert!(s.face_job(lightcraft_catalog::PhotoId(u64::MAX), faces[0], 256).is_none());
}

/// Regions are on the upright photo; after Rotate Right the close-up still frames the face: the box
/// is carried into the rotated frame the crop lives in, and the crop stays square in rotated pixels.
#[test]
fn face_job_follows_rotate_right() {
    use lightcraft_geom::Rect;
    let mut s = demo();
    let id = s.active().unwrap();
    let (w, h) = {
        let p = s.catalog.photo(id).unwrap();
        (f64::from(p.width), f64::from(p.height))
    };
    // a small face left of centre on the upright photo
    let face = Rect { x0: 0.30, y0: 0.45, x1: 0.35, y1: 0.50 };
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let r = s.face_job(id, face, 256).unwrap().settings.crop.geometry.rect;
    // a quarter turn clockwise puts the left of the photo at the top: centre (0.325, 0.475) → (0.525, 0.325)
    assert!((((r.x0 + r.x1) / 2.0) - 0.525).abs() < 1e-9 && (((r.y0 + r.y1) / 2.0) - 0.325).abs() < 1e-9, "{r:?}");
    // the rotated photo is h × w pixels
    let (pw, ph) = ((r.x1 - r.x0) * h, (r.y1 - r.y0) * w);
    assert!((pw - ph).abs() < 1e-6 * w.max(h), "square in rotated pixels: {pw} × {ph}");
}

/// Selecting an id that is not in the library is an error, and the selection stays as it was
/// (#182: `select_photos {"ids": [9999]}` answered `selected: 1` and the session lost its photo).
#[test]
fn selecting_an_unknown_photo_is_an_error_and_keeps_the_selection() {
    let mut s = demo();
    let vis = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [vis[0].0]})).unwrap();
    let before = s.selection.clone();
    for p in [
        json!({"ids": [9999]}),
        json!({"ids": [9999], "active": 9999}),
        json!({"ids": [vis[1].0], "active": 9999}),
        json!({"ids": [vis[1].0, 9999]}),
        json!({"ids": [9999], "mode": "add"}),
        json!({"ids": [9999], "mode": "toggle"}),
        json!({"ids": [9999], "mode": "range"}),
    ] {
        let e = s.execute("library.select", &p).unwrap_err().to_string();
        assert!(e.contains("no such photo 9999"), "{p}: {e}");
        assert_eq!(s.selection, before, "{p}");
    }
    assert_eq!(s.active(), Some(vis[0]));
    assert!(s.execute("develop.get", &json!({})).is_ok());
    // queries about an explicit id check it too
    for c in ["develop.get", "photo.inspect", "photo.allMetadata", "history.list"] {
        let e = s.execute(c, &json!({"id": 9999})).unwrap_err().to_string();
        assert!(e.contains("no such photo 9999"), "{c}: {e}");
        assert!(s.execute(c, &json!({"id": vis[1].0})).is_ok(), "{c}");
    }
    // an empty selection is still allowed
    assert_eq!(s.execute("library.select", &json!({"ids": []})).unwrap()["selected"], 0);
}

/// A preset saved by another version may hold keys this one doesn't know: exporting with it keeps
/// working (the stale keys are dropped), while a typo the caller makes is still refused (#181).
#[test]
fn saved_presets_with_unknown_keys_still_export() {
    use crate::export::{ExportOptions, ExportPreset};
    let mut s = demo();
    s.export_presets.push(ExportPreset {
        name: "Old".into(),
        params: json!({"format": "png", "retiredOption": 3, "watermark": {"text": "©", "retiredKey": true}}),
    });
    let p = s.export_params(&json!({"preset": "old", "quality": 80})).unwrap();
    let o = ExportOptions::from_params(&p).unwrap();
    assert_eq!(o.format, crate::export::ExportFormat::Png);
    assert_eq!(o.watermark.map(|w| w.text), Some("©".to_string()));
    assert!(ExportOptions::from_params(&s.export_params(&json!({"preset": "old", "qualty": 80})).unwrap()).is_err());
}

/// Undo and redo bring the photo they change on screen (issue #293): after editing one photo and
/// moving on, Undo makes it the active photo again; a step that changes several photos leaves the
/// selection alone.
#[test]
fn undo_and_redo_show_the_photo_they_change() {
    let mut s = demo();
    let vis = s.visible_cloned();
    let (a, b) = (vis[0], vis[1]);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.active(), Some(a), "undo shows the photo it changed");
    assert_eq!(s.selection.ids, vec![a]);
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.active(), Some(a), "so does redo");
    // rating both selected photos is one step over two photos: undoing it leaves the selection
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": b.0})).unwrap();
    s.execute("photo.rate", &json!({"rating": 3})).unwrap();
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    let before = s.selection.clone();
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.selection, before, "a step over several photos keeps the selection");
}
