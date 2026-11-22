use serde_json::{Value, json};

use crate::{Selection, Session, command_specs};

fn demo() -> Session {
    Session::with_demo()
}

#[test]
fn nested_catalog_operations_invalidate_sources_after_success_and_on_undo_redo() {
    use crate::media::SourceLevel;
    use lcc_catalog::{Op, PhotoId};
    let mut s = demo();
    let id = s.active().unwrap();
    let image = std::sync::Arc::new(lcc_raster::Rgb32f::new(1, 1));
    let cache = |s: &mut Session| {
        for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
            s.media.insert(id, level, image.clone());
        }
    };
    cache(&mut s);
    // An invalid operation must not throw away usable sources.
    assert!(s.apply_catalog(Op::Batch { ops: vec![Op::RemovePhoto { id }, Op::RemovePhoto { id: PhotoId(u64::MAX) }] }).is_err());
    assert!(s.catalog.photo(id).is_some(), "the failed batch restores the removed photo");
    for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
        assert!(s.media.has_source(id, level), "the failed batch keeps usable sources");
    }
    s.commit("Remove", Op::Batch { ops: vec![Op::Batch { ops: vec![Op::RemovePhoto { id }] }] }).unwrap();
    for level in [SourceLevel::Thumb, SourceLevel::Preview, SourceLevel::Full] {
        assert!(!s.media.has_source(id, level));
    }
    cache(&mut s);
    s.undo_step().unwrap();
    assert!(s.catalog.photo(id).is_some());
    assert!(!s.media.has_source(id, SourceLevel::Full), "undo AddPhoto must invalidate the source too");
    cache(&mut s);
    s.redo_step().unwrap();
    assert!(s.catalog.photo(id).is_none());
    assert!(!s.media.has_source(id, SourceLevel::Full));
}

fn active_dev(s: &Session) -> lcc_develop::DevelopSettings {
    (*s.develop_of(s.active().unwrap()).unwrap()).clone()
}

#[test]
fn changing_one_wb_control_resolves_as_shot_without_stale_tint() {
    use lcc_catalog::{Photo, PhotoId, Source};
    use lcc_develop::{DevelopSettings, WbMode};
    for format in ["ARW", "NEF", "NRW", "RAF", "CR2", "PEF", "ORF"] {
        let mut s = demo();
        let id = PhotoId(100);
        let name = format!("synthetic.{format}");
        let mut p = Photo::new(id, Source::File { path: name.clone() }, &name, format, 16, 16, "");
        // A catalog made by the old generic-matrix Kelvin inference.
        p.as_shot_wb = Some((6829.0, -127.0));
        p.develop = std::sync::Arc::new(DevelopSettings::for_raw(6829.0, -127.0));
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
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

/// Reset Edits on a raw goes back to its import defaults, capture sharpening and colour noise
/// reduction included (#763: reset left both at 0 while import sets 40 / 25); a rendered file
/// keeps none.
#[test]
fn reset_edits_restores_a_raws_import_defaults() {
    use lcc_catalog::{Photo, PhotoId, Source};
    use lcc_develop::DevelopSettings;
    let mut s = demo();
    for (id, format, raw) in [(200u64, "NEF", true), (201, "JPG", false)] {
        let name = format!("p{id}.{format}");
        let mut p = Photo::new(PhotoId(id), Source::File { path: name.clone() }, &name, format, 16, 16, "");
        if raw {
            p.as_shot_wb = Some((5200.0, 4.0));
            p.develop = std::sync::Arc::new(DevelopSettings::for_raw(5200.0, 4.0));
        }
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for (id, want) in [(200, (40.0, 25.0)), (201, (0.0, 0.0))] {
        s.execute("library.select", &json!({"ids": [id], "active": id})).unwrap();
        s.execute("develop.set", &json!({"control": "detail.sharpenAmount", "value": 90})).unwrap();
        s.execute("develop.set", &json!({"control": "detail.nrColor", "value": 70})).unwrap();
        s.execute("develop.reset", &json!({})).unwrap();
        let d = active_dev(&s);
        assert_eq!((d.detail.sharpen_amount, d.detail.nr_color), want, "photo {id}");
    }
}

#[test]
fn rating_flag_undo_redo() {
    {
        let mut s = demo();
        let id = s.active().unwrap();
        s.execute("photo.rate", &json!({"rating": 5})).unwrap();
        s.execute("photo.flag", &json!({"flag": "reject"})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().rating, 5);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_ne!(s.catalog.photo(id).unwrap().flag, lcc_catalog::Flag::Reject);
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().flag, lcc_catalog::Flag::Reject);
    }
    {
        let mut s = demo();
        let id = s.active().unwrap();
        let before = s.catalog.photo(id).unwrap().rating;
        for bad in [5.9, -0.9, 0.5] {
            let err = s.execute("photo.rate", &json!({"rating": bad})).unwrap_err();
            assert!(matches!(&err, crate::EngineError::BadParams { cmd, .. } if cmd == "photo.rate"), "{bad}: {err}");
            assert_eq!(s.catalog.photo(id).unwrap().rating, before, "{bad} changed the rating");
        }
        s.execute("photo.rate", &json!({"rating": 5.0})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().rating, 5);
        s.execute("photo.rate", &json!({"rating": 2})).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().rating, 2);
        assert!(s.execute("photo.rate", &json!({"rating": 7})).is_err());
    }
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
fn albums_crud() {
    {
        let mut s = demo();
        let f = s.execute("album.create", &json!({"name": "Trips", "folder": true})).unwrap()["id"].as_u64().unwrap();
        let a = s.execute("album.create", &json!({"name": "Best", "parent": f, "addSelected": true})).unwrap()["id"].as_u64().unwrap();
        s.execute("library.source", &json!({"kind": "album", "id": a})).unwrap();
        assert_eq!(s.visible().len(), 1);
        s.execute("album.rename", &json!({"id": a, "name": "Best of"})).unwrap();
        s.execute("library.source", &json!({"kind": "all"})).unwrap();
        s.execute("library.selectAll", &json!({})).unwrap();
        s.execute("album.addPhotos", &json!({"id": a})).unwrap();
        assert_eq!(s.catalog.album(lcc_catalog::AlbumId(a)).unwrap().photos.len(), 24);
        s.execute("album.delete", &json!({"id": f})).unwrap();
        assert!(s.catalog.album(lcc_catalog::AlbumId(a)).is_none());
        assert!(s.catalog.album(lcc_catalog::AlbumId(f)).is_none());
        assert_eq!(s.catalog.photos().count(), 24, "collection deletion keeps the photos");
    }
    // Given an album and a folder, when something is created inside each, then only the folder
    // accepts it: the sidebar only lists children of folders, so a child of a plain album would
    // be invisible.
    {
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
        assert_eq!(s.catalog.album(lcc_catalog::AlbumId(smart)).unwrap().parent, Some(lcc_catalog::AlbumId(sub)));
    }
    // Found by fuzzing catalog snapshots: an album filed under one id but naming its own folder's
    // listed itself inside itself, and `albums.list` recursed until the stack overflowed (an abort:
    // no guard catches it). Loading repairs such a catalog (`Catalog::repair`); one that skipped
    // that still lists, the loop cut off.
    {
        let mut s = Session::with_demo();
        let f = s.execute("album.create", &json!({"name": "F", "folder": true})).unwrap()["id"].as_u64().unwrap();
        let a = s.execute("album.create", &json!({"name": "A", "parent": f})).unwrap()["id"].as_u64().unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&s.catalog.to_snapshot()).unwrap();
        v["albums"][a.to_string()]["id"] = f.into();
        // as read, without the repair a load does
        s.catalog = serde_json::from_value(v).unwrap();
        let r = s.execute("albums.list", &json!({})).unwrap();
        assert!(r.to_string().contains("\"F\""), "{r}");
    }
}

#[test]
fn deleting_photos_selects_a_neighbour_in_the_current_view() {
    {
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
    {
        let mut s = demo();
        let visible = s.visible_cloned();
        s.execute("library.select", &json!({"ids": [visible[8], visible[9]], "active": visible[9]})).unwrap();
        let before = s.selection.clone();
        s.execute("photo.delete", &json!({"ids": [visible[3]]})).unwrap();
        assert_eq!(s.selection, before, "deleting an unselected photo doesn't change selection");
        s.execute("photo.delete", &json!({"ids": [visible[9]]})).unwrap();
        assert_eq!(s.selection, Selection::single(visible[8]), "partial deletion retains a selected survivor");
    }
    {
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
}

#[test]
fn permanently_deleting_photos_selects_a_neighbour_in_recently_deleted() {
    {
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
    {
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
}

/// `mask.brushStroke {connect: true}` (a ⇧-click) starts from the last point of the mask's last stroke: one straight
/// stroke when the brush is unchanged, pieces whose size, feather and flow step from the last stroke's to the new
/// values when it changed, and a plain dab when there is nothing to connect to.
#[test]
fn connected_brush_strokes_start_where_the_last_one_ended() {
    {
        use lcc_develop::MaskShape;
        use lcc_geom::Point;
        let mut s = demo();
        let id = s.active().unwrap();
        let r = s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.5]], "size": 0.02, "connect": true})).unwrap();
        assert_eq!(r["connected"], false, "nothing to connect to: a dab");
        let strokes = |s: &Session| {
            let d = s.develop_of(id).unwrap();
            let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
            strokes.clone()
        };
        assert_eq!(strokes(&s)[0].points.len(), 1);
        let r = s.execute("mask.brushStroke", &json!({"points": [[0.6, 0.5]], "size": 0.02, "connect": true})).unwrap();
        assert_eq!(r["connected"], true);
        let line = strokes(&s);
        assert_eq!(line.len(), 2);
        assert_eq!(line[1].points, vec![Point::new(0.2, 0.5), Point::new(0.6, 0.5)]);
        // a bigger, softer brush: the segment tapers from the last stroke's values to the new ones
        let r = s.execute("mask.brushStroke", &json!({"points": [[0.6, 0.9]], "size": 0.04, "feather": 80, "connect": true})).unwrap();
        assert_eq!(r["connected"], true);
        let all = strokes(&s);
        let taper = &all[2..];
        assert!(taper.len() > 4, "{} pieces", taper.len());
        assert_eq!(taper[0].points[0], Point::new(0.6, 0.5), "from the last stroke's end");
        assert_eq!(*taper.last().unwrap().points.last().unwrap(), Point::new(0.6, 0.9), "to the click");
        assert!(taper.windows(2).all(|w| w[1].size >= w[0].size && w[1].feather >= w[0].feather && w[0].points[1] == w[1].points[0]));
        assert!((taper.last().unwrap().size - 0.04).abs() < 1e-12 && (taper.last().unwrap().feather - 80.0).abs() < 1e-9);
        assert!(taper[0].size > 0.02 && taper[0].size < 0.04);
        // all of it is one undo step
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(strokes(&s).len(), 2);
    }
    // A ⇧-click line carries pen pressure: the last dab's at its start and the click's at its end, stepping along a
    // tapered line; with no pressure on either side there is none.
    {
        use lcc_develop::MaskShape;
        let mut s = demo();
        let id = s.active().unwrap();
        let strokes = |s: &Session| {
            let d = s.develop_of(id).unwrap();
            let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
            strokes.clone()
        };
        s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.5]], "size": 0.02, "pressure": [0.4]})).unwrap();
        s.execute("mask.brushStroke", &json!({"points": [[0.6, 0.5]], "size": 0.02, "pressure": [1.0], "connect": true})).unwrap();
        assert_eq!(strokes(&s)[1].pressure, vec![0.4, 1.0]);
        s.execute("mask.brushStroke", &json!({"points": [[0.6, 0.9]], "size": 0.04, "pressure": [0.2], "connect": true})).unwrap();
        let taper = &strokes(&s)[2..];
        assert!(taper.iter().all(|t| t.pressure.len() == 2));
        assert!((taper[0].pressure[0] - 1.0).abs() < 1e-6 && (taper.last().unwrap().pressure[1] - 0.2).abs() < 1e-6);
        assert!(taper.windows(2).all(|w| w[1].pressure[0] <= w[0].pressure[0]));
        // a mouse: no pressure anywhere
        let mut m = demo();
        m.execute("mask.brushStroke", &json!({"points": [[0.2, 0.5]], "size": 0.02})).unwrap();
        m.execute("mask.brushStroke", &json!({"points": [[0.6, 0.5]], "size": 0.02, "connect": true})).unwrap();
        let d = m.develop_of(m.active().unwrap()).unwrap();
        let MaskShape::Brush { strokes } = &d.masks[0].components[0].shape else { panic!("brush") };
        assert!(strokes[1].pressure.is_empty());
    }
}

/// Brush strokes: pen pressure is kept (clamped), bad pressure lists are refused; a stroke goes into the
/// Brush component it names.
#[test]
fn brush_strokes_keep_pen_pressure_and_go_into_the_chosen_component() {
    {
        let mut s = demo();
        s.execute("mask.add", &json!({"kind": "brush"})).unwrap();
        s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2], [0.3, 0.3]], "pressure": [0.25, 7.0]})).unwrap();
        let lcc_develop::MaskShape::Brush { strokes } = &active_dev(&s).masks[0].components[0].shape else { panic!("a brush") };
        assert_eq!(strokes[0].pressure, vec![0.25, 1.0], "kept, clamped to 0..1");
        for bad in [json!([0.5, 0.5, 0.5]), json!("hard"), json!([0.5, "x"])] {
            assert!(s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2], [0.3, 0.3]], "pressure": bad})).is_err(), "{bad}");
        }
        s.execute("mask.brushStroke", &json!({"points": [[0.4, 0.4]]})).unwrap();
        let lcc_develop::MaskShape::Brush { strokes } = &active_dev(&s).masks[0].components[0].shape else { panic!("a brush") };
        assert!(strokes[1].pressure.is_empty(), "a mouse stroke has no pressure data");
        let json = serde_json::to_string(&strokes[1]).unwrap();
        assert!(!json.contains("pressure"), "and stores none: {json}");
    }
    // A stroke goes into the chosen Brush component of a mask (the one armed on the photo), not only
    // the last one; a component that isn't a Brush, or isn't there, is an error and changes nothing.
    {
        use lcc_develop::{MaskOp, MaskShape};
        let mut s = demo();
        s.execute("mask.add", &json!({"kind": "brush"})).unwrap();
        s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2]]})).unwrap();
        s.execute("mask.addComponent", &json!({"kind": "linear", "start": [0.1, 0.5], "end": [0.9, 0.5]})).unwrap();
        s.execute("mask.addComponent", &json!({"kind": "brush", "op": "subtract"})).unwrap();
        let counts = |s: &Session| {
            active_dev(s).masks[0]
                .components
                .iter()
                .map(|c| match &c.shape {
                    MaskShape::Brush { strokes } => strokes.len() as i32,
                    _ => -1,
                })
                .collect::<Vec<_>>()
        };
        let before = counts(&s);
        assert_eq!(before.iter().filter(|n| **n >= 0).count(), 2, "{before:?}");
        let first = before.iter().position(|n| *n >= 0).unwrap();
        let last = before.iter().rposition(|n| *n >= 0).unwrap();
        let linear = before.iter().position(|n| *n < 0).unwrap();
        s.execute("mask.brushStroke", &json!({"points": [[0.3, 0.3]], "component": first})).unwrap();
        assert_eq!(counts(&s)[first], before[first] + 1, "into the first Brush, not the last");
        s.execute("mask.brushStroke", &json!({"points": [[0.4, 0.4]], "component": last, "erase": true})).unwrap();
        assert_eq!(counts(&s)[last], before[last] + 1);
        assert_eq!(active_dev(&s).masks[0].components[last].op, MaskOp::Subtract, "its own operation is kept");
        let now = counts(&s);
        for bad in [json!(linear), json!(9), json!(-1), json!("x")] {
            assert!(s.execute("mask.brushStroke", &json!({"points": [[0.5, 0.5]], "component": bad})).is_err(), "{bad}");
        }
        assert_eq!(counts(&s), now, "a refused stroke changes nothing");
    }
}

#[test]
fn auto_stays_applied_until_a_tone_slider_changes() {
    let mut s = demo();
    let id = s.active().unwrap();
    assert!(!s.auto_is_current(id));
    let r = s.execute("develop.auto", &json!({})).unwrap();
    assert_eq!(r["current"], true, "{r}");
    assert!(s.auto_is_current(id));
    // a change elsewhere keeps it; a tone slider change ends it
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 10})).unwrap();
    assert!(s.auto_is_current(id));
    s.execute("develop.set", &json!({"control": "light.contrast", "value": 3})).unwrap();
    assert!(!s.auto_is_current(id));
    // undoing that change brings the Auto values (and the state) back
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.auto_is_current(id));
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
    let log: String = s.drain_log().iter().map(lcc_catalog::Catalog::op_to_log_line).collect();
    let mut c = lcc_catalog::Catalog::from_snapshot(&snap).unwrap();
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
        // empty params make this one download from download.geonames.org into the real settings folder; its own tests run it against a loopback server
        if c.id == "metadata.downloadPlaceNames" {
            continue;
        }
        // empty params run the whole machine test: full-size exports of the demo photos at up to one
        // lane per core (past 5 GB on a 32-core machine), and it applies its result to every later
        // export of the test process; its own tests run it small (`machine_test.rs`)
        if c.id == "app.testMachine" {
            continue;
        }
        // empty params scan the photos in view, a real scan of the demo library on a thread of
        // its own (its own tests run it, and cancel it)
        if c.id == "photo.qualityScan" {
            continue;
        }
        let _ = s.execute(c.id, &Value::Null);
    }
    // empty params must never start a download from the internet
    assert!(!s.place_names.download_started(), "the sweep started a place names download");
    assert!(s.commands().len() >= 70, "{}", s.commands().len());
}

#[test]
fn upright_and_guides_commands() {
    {
        let mut s = demo();
        let r = s.execute("geometry.upright", &json!({"mode": "level"})).unwrap();
        assert_eq!(r["mode"], "level");
        let d = active_dev(&s);
        assert_eq!(d.geometry.upright, lcc_develop::Upright::Level);
        assert!(d.geometry.upright_transform.is_some(), "analysis result is stored");
        s.execute("geometry.upright", &json!({"mode": "off"})).unwrap();
        assert!(active_dev(&s).geometry.upright_transform.is_none());
        for i in 0..5 {
            let x = 0.1 + 0.15 * i as f64;
            s.execute("geometry.guides", &json!({"guides": [[x, 0.2, x + 0.02, 0.8]], "add": true})).unwrap();
        }
        let d = active_dev(&s);
        assert_eq!(d.geometry.upright, lcc_develop::Upright::Guided);
        assert_eq!(d.geometry.guides.len(), 4, "at most four guides");
        assert!((d.geometry.guides[0].0.x - 0.25).abs() < 1e-9, "oldest dropped");
        let id = s.active().unwrap();
        assert!(s.render_now(id, 160, 160).is_ok());
        assert!(s.execute("geometry.upright", &json!({"mode": "sideways"})).is_err());
    }
    // Upright that finds nothing to correct says why instead of silently doing nothing.
    {
        let mut s = demo();
        let mut noted = 0;
        for id in s.visible().iter().copied().take(4).collect::<Vec<_>>() {
            s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
            let r = s.execute("geometry.upright", &json!({"mode": "vertical"})).unwrap();
            let identity = r["transform"] == serde_json::to_value(lcc_geom::Homography::IDENTITY).unwrap();
            match r["note"].as_str() {
                Some(note) => {
                    noted += 1;
                    assert!(note.starts_with("No vertical correction: "), "{note}");
                }
                None => assert!(!identity, "a vertical correction was found: {r}"),
            }
        }
        assert!(noted > 0, "the demo's natural scenes have no verticals to correct");
        let r = s.execute("geometry.upright", &json!({"mode": "off"})).unwrap();
        assert!(r["note"].is_null());
    }
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

/// A crop of no size (or a hair wide), as a sidecar can set it, made the render's
/// samplers read at infinite coordinates and its blurs ask for sigmas of millions of pixels: with a
/// healing spot and masks on, render workers panicked (an i32 square in the blur, a bilinear
/// neighbour past isize::MAX). Such a photo renders, quickly.
#[test]
fn degenerate_crops_render_with_spots_and_masks() {
    for rect in [[0.5, 0.5, 0.5, 0.5], [0.0, 0.0, 1e-9, 1.0]] {
        let mut s = demo();
        let id = s.active().unwrap();
        s.execute("spot.add", &json!({"points": [[0.5, 0.5]], "size": 0.2, "mode": "heal"})).unwrap();
        s.execute("mask.add", &json!({"kind": "radial", "rx": 0.3, "ry": 0.3})).unwrap();
        s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
        s.execute("develop.set", &json!({"control": "effects.clarity", "value": 80})).unwrap();
        s.execute("develop.set", &json!({"control": "effects.dehaze", "value": 50})).unwrap();
        set_crop_unchecked(&mut s, rect, 30.0);
        let t = std::time::Instant::now();
        let r = s.render_now(id, 700, 450).unwrap();
        assert!(r.image.width >= 1 && r.image.height >= 1, "{rect:?}");
        assert!(t.elapsed().as_secs_f64() < 60.0, "{rect:?}: {:?}", t.elapsed());
    }
}

/// Store a crop as a sidecar can, without `crop.set`'s minimum size, kept inside the turned photo.
fn set_crop_unchecked(s: &mut Session, [x0, y0, x1, y1]: [f64; 4], angle: f64) {
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap();
    let (w, h) = (p.width as f64, p.height as f64);
    let mut d = active_dev(s);
    d.crop.geometry = lcc_geom::CropGeometry { rect: lcc_geom::Rect::new(x0, y0, x1, y1), angle }.constrained(w, h);
    s.set_develop(id, d, "Crop").unwrap();
}

/// Crops of no size, slivers and huge rectangles through `crop.set` and `crop.rotateAspect`.
#[test]
fn degenerate_and_huge_crops_are_refused_or_stay_finite() {
    // `crop.set` took a crop of no size (or a sliver), and the next export wrote a 1×1 image (#1952).
    // It now refuses a crop smaller than a drag of the crop tool can make, and keeps the one it had.
    // A rect far wider than the photo is a sliver too once it is shrunk to fit inside it.
    {
        let mut s = demo();
        s.execute("crop.set", &json!({"rect": [0.1, 0.1, 0.9, 0.9]})).unwrap();
        let before = active_dev(&s).crop.geometry;
        for rect in
            [[0.5, 0.5, 0.5, 0.5], [0.2, 0.2, 0.2, 0.8], [0.2, 0.4, 0.8, 0.4], [0.0, 0.0, 1e-9, 1.0], [0.0, 0.0, 1e6, 1.0], [0.0, 0.0, 1.0, 1e6]]
        {
            let e = s.execute("crop.set", &json!({"rect": rect})).unwrap_err();
            assert!(e.to_string().contains("smaller"), "{rect:?}: {e}");
            assert_eq!(active_dev(&s).crop.geometry, before, "{rect:?}");
        }
        s.execute("crop.set", &json!({"rect": [0.45, 0.45, 0.55, 0.55]})).unwrap();
        assert_eq!(active_dev(&s).crop.geometry.rect, lcc_geom::Rect::new(0.45, 0.45, 0.55, 0.55));
    }
    // `crop.set` with a huge rect shrank it by a scale of 0 (`inf * 0`) and stored a NaN crop. Their
    // maths overflow, so they fall back to a full-size crop and pass the minimum size.
    {
        for rect in [[0.0, 0.0, 1e308, 1e308], [-1e308, -1e308, 1e308, 1e308]] {
            let mut s = demo();
            s.execute("crop.set", &json!({"rect": rect})).unwrap();
            let r = active_dev(&s).crop.geometry.rect;
            assert!([r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite() && (-1e-6..=1.0 + 1e-6).contains(v)), "{rect:?}: {r:?}");
        }
    }
    // `crop.rotateAspect` on a crop of no size took its aspect from 0/0 = NaN and stored a NaN crop
    // rectangle; it now falls back to the image's own aspect, a finite crop inside the image.
    {
        for rect in [[0.5, 0.5, 0.5, 0.5], [0.2, 0.2, 0.2, 0.8], [0.2, 0.2, 0.8, 0.2]] {
            let mut s = demo();
            set_crop_unchecked(&mut s, rect, 0.0);
            s.execute("crop.rotateAspect", &json!({})).unwrap();
            let r = active_dev(&s).crop.geometry.rect;
            assert!([r.x0, r.y0, r.x1, r.y1].iter().all(|v| v.is_finite()), "{rect:?}: {r:?}");
            assert!(r.width() > 0.0 && r.height() > 0.0, "{rect:?}: {r:?}");
            assert!(r.x0 >= 0.0 && r.y0 >= 0.0 && r.x1 <= 1.0 && r.y1 <= 1.0, "{rect:?}: {r:?}");
        }
    }
}

#[test]
fn masks_reorder_and_duplicate_and_invert() {
    {
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
    // `mask.move` with a delta near `i64::MAX` overflowed `index + delta` (a panic wherever overflow
    // checks are on); extreme deltas and indices now move the mask to the bottom or the top.
    {
        let mut s = demo();
        for kind in ["linear", "radial", "brush"] {
            s.execute("mask.add", &json!({"kind": kind})).unwrap();
        }
        let ids = |s: &Session| active_dev(s).masks.iter().map(|m| m.id).collect::<Vec<_>>();
        let [a, b, c] = ids(&s)[..] else { panic!("{:?}", ids(&s)) };
        s.execute("mask.move", &json!({"id": b, "delta": i64::MAX})).unwrap();
        assert_eq!(ids(&s), [a, c, b], "to the bottom");
        s.execute("mask.move", &json!({"id": b, "delta": i64::MIN})).unwrap();
        assert_eq!(ids(&s), [b, a, c], "to the top");
        s.execute("mask.move", &json!({"id": c, "to": i64::MIN})).unwrap();
        assert_eq!(ids(&s), [c, b, a]);
        s.execute("mask.move", &json!({"id": c, "to": i64::MAX})).unwrap();
        assert_eq!(ids(&s), [b, a, c]);
    }
    {
        let mut s = demo();
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        s.execute("mask.feather", &json!({"value": 40.0})).unwrap();
        assert_eq!(active_dev(&s).masks[0].feather, 40.0);
        s.execute("mask.feather", &json!({"value": 250.0})).unwrap();
        assert_eq!(active_dev(&s).masks[0].feather, 100.0);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(active_dev(&s).masks[0].feather, 40.0);
        assert!(s.execute("mask.feather", &json!({"id": 9999, "value": 10.0})).is_err());
    }
    {
        let mut s = demo();
        s.execute("develop.merge", &json!({"settings": {"masks": [{"id": u32::MAX, "name": "Pasted"}]}})).unwrap();
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        s.execute("mask.duplicate", &json!({})).unwrap();
        let ids: Vec<u32> = active_dev(&s).masks.iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id)), "distinct ids: {ids:?}");
    }
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
    {
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
        // built-ins can be renamed (LR-PRE-MANAGE) but never take new settings
        assert!(s.execute("preset.rename", &json!({"id": builtin, "name": "x"})).is_ok());
        assert!(s.execute("preset.update", &json!({"id": builtin})).is_err());
        assert!(s.execute("preset.delete", &json!({"id": builtin})).is_err(), "nor can they be deleted");
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
        let picks = vis.iter().filter(|id| s.catalog.photo(**id).unwrap().flag == lcc_catalog::Flag::Pick).count();
        let r = s.execute("library.selectBy", &json!({"flag": "pick"})).unwrap();
        assert_eq!(r["selected"].as_u64(), Some(picks as u64));
        assert!(picks > 0);
        let four = vis.iter().filter(|id| s.catalog.photo(**id).unwrap().rating >= 4).count();
        assert_eq!(s.execute("library.selectBy", &json!({"rating": 4})).unwrap()["selected"].as_u64(), Some(four as u64));
        let r = s.execute("library.selectBy", &json!({"rating": 0, "ratingOp": "eq", "add": true})).unwrap();
        assert!(r["selected"].as_u64().unwrap() >= four as u64);
        assert!(s.execute("library.selectBy", &json!({})).is_err());
        assert!(s.execute("library.selectBy", &json!({"label": "mauve"})).is_err());
        // a preset applies to the selected photos
        s.execute("library.select", &json!({"ids": [vis[1].0, vis[2].0]})).unwrap();
        s.execute("preset.apply", &json!({"id": "lc.bw-high-contrast", "amount": 100})).unwrap();
        assert_eq!(s.develop_of(vis[1]).unwrap().treatment, lcc_develop::Treatment::Bw);
    }
    {
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
}

#[test]
fn crop_aspect_lock_current_and_toggle() {
    {
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
    {
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
}

/// Dragging crop handles (issues 295 and 614).
#[test]
fn crop_drag_keeps_the_anchor_and_the_ratio_and_refuses_bad_input() {
    // Feature: dragging crop handles (issue 295)
    {
        let mut s = demo();
        s.execute("crop.set", &json!({"rect": [0.2, 0.2, 0.8, 0.8]})).unwrap();
        s.execute("crop.drag", &json!({"handle": "bottomRight", "from": [0.8, 0.8], "to": [1.4, 0.7]})).unwrap();
        let r = active_dev(&s).crop.geometry.rect;
        assert!((r.x0 - 0.2).abs() < 1e-6 && (r.y0 - 0.2).abs() < 1e-6, "anchor moved: {r:?}");
        assert!((r.x1 - 1.0).abs() < 1e-6 && (r.y1 - 0.7).abs() < 1e-6, "{r:?}");
    }
    {
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
    // Feature: Shift while dragging keeps the current proportions (issue 614)
    {
        let mut s = demo();
        s.execute("crop.set", &json!({"rect": [0.1, 0.2, 0.7, 0.6]})).unwrap();
        assert!(active_dev(&s).crop.aspect.is_none(), "lock is off");
        let r = active_dev(&s).crop.geometry.rect;
        let want = r.aspect();
        for (handle, to) in [("left", [r.x0 + 0.1, 0.5]), ("top", [0.5, r.y0 + 0.05]), ("bottomRight", [1.3, 1.3]), ("right", [0.9, 0.5])] {
            s.execute("crop.drag", &json!({"handle": handle, "from": [0.5, 0.5], "to": to, "start": [r.x0, r.y0, r.x1, r.y1], "keepRatio": true}))
                .unwrap();
            let g = active_dev(&s).crop.geometry.rect;
            assert!((g.aspect() - want).abs() < 1e-6, "{handle}: {} vs {want}", g.aspect());
            assert!(active_dev(&s).crop.aspect.is_none(), "the lock toggle is untouched");
        }
    }
    {
        let mut s = demo();
        s.execute("crop.set", &json!({"rect": [0.1, 0.2, 0.7, 0.6]})).unwrap();
        let r = active_dev(&s).crop.geometry.rect;
        let want = r.aspect();
        s.execute("crop.drag", &json!({"handle": "right", "from": [0.7, 0.4], "to": [0.9, 0.4], "start": [r.x0, r.y0, r.x1, r.y1]})).unwrap();
        assert!((active_dev(&s).crop.geometry.rect.aspect() - want).abs() > 1e-3, "ratio changes freely");
        s.execute(
            "crop.drag",
            &json!({"handle": "right", "from": [0.7, 0.4], "to": [0.9, 0.4], "start": [r.x0, r.y0, r.x1, r.y1], "keepRatio": false}),
        )
        .unwrap();
        assert!((active_dev(&s).crop.geometry.rect.aspect() - want).abs() > 1e-3, "keepRatio false is the same");
    }
    {
        let mut s = demo();
        assert!(s.execute("crop.drag", &json!({"handle": "sideways", "from": [0.5, 0.5], "to": [0.6, 0.6]})).is_err());
        assert!(s.execute("crop.drag", &json!({"handle": "move", "from": [0.5], "to": [0.6, 0.6]})).is_err());
        assert!(s.execute("crop.drag", &json!({"handle": "move", "to": [0.6, 0.6]})).is_err());
        // non-numeric points
        assert!(s.execute("crop.drag", &json!({"handle": "move", "from": ["a", 0.5, 0.5], "to": [0.6, 0.6]})).is_err());
        assert!(s.execute("crop.drag", &json!({"handle": "move", "from": [0.5, 0.5], "to": [0.6, 0.6], "start": [0.1, "x", 0.5, 0.5]})).is_err());
    }
}

/// Paste from Previous, and metadata copied, pasted and synced to the selection.
#[test]
fn paste_from_previous_and_copy_paste_and_sync_metadata() {
    {
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
    {
        let mut s = demo();
        let ids: Vec<_> = s.visible().iter().copied().take(3).collect();
        s.execute("library.select", &json!({"ids": [ids[0].0]})).unwrap();
        s.execute("photo.setMeta", &json!({"title": "Dawn", "keywords": ["sky"], "creator": "Me"})).unwrap();
        // alone it has nothing to sync to
        assert!(s.execute("photo.syncMetadata", &json!({})).is_err());
        s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>(), "active": ids[0].0})).unwrap();
        s.execute("photo.syncMetadata", &json!({"fields": ["title", "keywords"]})).unwrap();
        for id in &ids[1..] {
            let m = &s.catalog.photo(*id).unwrap().meta;
            assert_eq!((m.title.as_str(), m.keywords.clone()), ("Dawn", vec!["sky".to_string()]));
            assert_ne!(m.creator, "Me", "only the chosen fields");
        }
        s.execute("photo.syncMetadata", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(ids[2]).unwrap().meta.creator, "Me");
        // one undo step takes the whole sync back
        s.execute("edit.undo", &json!({})).unwrap();
        assert_ne!(s.catalog.photo(ids[2]).unwrap().meta.creator, "Me");
    }
}

#[test]
fn stacks_move_and_split() {
    {
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
    // Issue #160: the stack commands are on exactly when a selected photo is stacked, however many
    // unstacked photos are selected with it.
    {
        let mut s = demo();
        let on = |s: &Session, id: &str| s.commands().into_iter().find(|c| c.id == id).map(|c| (c.enabled, c.disabled_reason.clone()));
        let ids: Vec<_> = s.visible().to_vec();
        assert!(ids.len() >= 4);
        let select = |s: &mut Session, sel: &[lcc_catalog::PhotoId]| {
            s.execute("library.select", &json!({"ids": sel.iter().map(|i| i.0).collect::<Vec<_>>(), "active": sel[0].0})).unwrap();
        };
        select(&mut s, &ids[..2]);
        s.execute("stack.group", &json!({"collapsed": false})).unwrap();
        for id in ["stack.ungroup", "stack.remove", "stack.setTop", "stack.moveUp", "stack.moveDown", "stack.split", "stack.toggle"] {
            select(&mut s, &ids[2..]);
            let (enabled, why) = on(&s, id).unwrap();
            assert!(!enabled && why.as_deref() == Some("no stacked photo selected"), "{id}: {why:?}");
            // one stacked photo among many unstacked ones turns it on
            let mut sel = ids[2..].to_vec();
            sel.push(ids[1]);
            select(&mut s, &sel);
            assert!(on(&s, id).unwrap().0, "{id}");
        }
    }
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
    assert_eq!(d.treatment, lcc_develop::Treatment::Bw);
    let m = d.bw_mix.bands();
    assert!(m.iter().any(|v| *v != 0.0), "some band moved: {m:?}");
    assert!(m.iter().all(|v| v.abs() <= 60.0));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_ne!(active_dev(&s).treatment, lcc_develop::Treatment::Bw, "one undo step");
}

#[test]
fn auto_sync_carries_only_the_changed_settings() {
    use lcc_catalog::PhotoId;
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
    // a slider drag syncs live (LR-AUTOSYNC-LIVE) and commits once, on release
    let shadows: Vec<f64> = ids.iter().map(|id| s.develop_of(PhotoId(*id)).unwrap().light.shadows).collect();
    s.begin_interaction("Shadows").unwrap();
    let mut d = (*s.develop_of(PhotoId(ids[0])).unwrap()).clone();
    d.light.shadows = 40.0;
    s.set_develop(PhotoId(ids[0]), d, "Shadows").unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[2])).unwrap().light.shadows, 40.0, "live while dragging");
    s.end_interaction().unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[2])).unwrap().light.shadows, 40.0);
    // one undo step reverts every photo
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(ids.iter().map(|id| s.develop_of(PhotoId(*id)).unwrap().light.shadows).collect::<Vec<_>>(), shadows);
    // spot removal belongs to one photo
    let mut d = (*s.develop_of(PhotoId(ids[0])).unwrap()).clone();
    d.spots.push(lcc_develop::Spot::default());
    s.set_develop(PhotoId(ids[0]), d, "Remove").unwrap();
    assert!(s.develop_of(PhotoId(ids[1])).unwrap().spots.is_empty());
    // off: only the active photo
    s.execute("develop.autoSync", &json!({"on": false})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": -1})).unwrap();
    assert_eq!(s.develop_of(PhotoId(ids[1])).unwrap().light.exposure, 0.7);
}

/// Auto Sync and Camera Local Tone Mapping: Off is carried like On, and an unrelated edit never copies it.
#[test]
fn auto_sync_carries_camera_local_tone_only_when_it_changes() {
    {
        use lcc_catalog::PhotoId;
        let mut s = demo();
        let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        for (id, (profile, amount, contrast)) in ids.iter().zip([("lc.color", 100, 0), ("lc.vivid", 37, 25), ("lc.mono", 63, -20)]) {
            s.execute("library.select", &json!({"ids": [id]})).unwrap();
            let settings = json!({"profile": {"id": profile, "amount": amount}, "light": {"contrast": contrast}});
            s.execute("develop.merge", &json!({"settings": settings})).unwrap();
        }
        let initial: Vec<_> = ids.iter().map(|id| (*s.develop_of(PhotoId(*id)).unwrap()).clone()).collect();
        assert!(initial.iter().all(|d| !d.profile.camera_local_tone));
        s.execute("library.select", &json!({"ids": [ids[0], ids[1]], "active": ids[0]})).unwrap();
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        let on = json!({"settings": {"profile": {"camera_local_tone": true}}, "label": "Camera Local Tone Mapping"});
        let off = json!({"settings": {"profile": {"camera_local_tone": false}}, "label": "Camera Local Tone Mapping"});

        // Positive control: On reaches both selected photos, changing only this setting.
        s.execute("develop.merge", &on).unwrap();
        let mut synced = initial.clone();
        for d in synced.iter_mut().take(2) {
            d.profile.camera_local_tone = true;
        }
        for (id, expected) in ids.iter().zip(&synced) {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), expected, "On: photo {id}");
        }

        let undo_before = s.undo.len();
        let history_before: Vec<_> = ids.iter().map(|id| s.catalog.photo(PhotoId(*id)).unwrap().history.len()).collect();
        s.execute("develop.merge", &off).unwrap();
        assert!(!s.develop_of(PhotoId(ids[0])).unwrap().profile.camera_local_tone);
        assert!(!s.develop_of(PhotoId(ids[1])).unwrap().profile.camera_local_tone, "Auto Sync must carry camera local tone Off");
        assert_eq!(s.undo.len(), undo_before + 1, "Off is one undoable edit");
        for (i, (id, expected)) in ids.iter().zip(&initial).enumerate() {
            let photo = s.catalog.photo(PhotoId(*id)).unwrap();
            assert_eq!(photo.develop.as_ref(), expected, "Off: photo {id}");
            assert_eq!(photo.history.len(), history_before[i] + usize::from(i < 2), "Off history: photo {id}");
            let stored = photo.develop.to_json();
            assert!(stored["profile"].get("camera_local_tone").is_none(), "false stays omitted: photo {id}");
            assert_eq!(&lcc_develop::DevelopSettings::from_json(&stored).unwrap(), expected);
        }

        // One Undo/Redo restores the complete settings of both selected photos, not the third.
        s.execute("edit.undo", &json!({})).unwrap();
        for (id, expected) in ids.iter().zip(&synced) {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), expected, "Undo: photo {id}");
        }
        s.execute("edit.redo", &json!({})).unwrap();
        for (id, expected) in ids.iter().zip(&initial) {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), expected, "Redo: photo {id}");
        }
    }
    {
        use lcc_catalog::PhotoId;
        let mut s = demo();
        let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
        s.execute("library.select", &json!({"ids": [ids[1]]})).unwrap();
        let settings = json!({"profile": {"id": "lc.vivid", "amount": 37, "camera_local_tone": true}, "light": {"contrast": 25}});
        s.execute("develop.merge", &json!({"settings": settings})).unwrap();
        let mut expected: Vec<_> = ids.iter().map(|id| (*s.develop_of(PhotoId(*id)).unwrap()).clone()).collect();
        assert!(!expected[0].profile.camera_local_tone);
        assert!(expected[1].profile.camera_local_tone);
        s.execute("library.select", &json!({"ids": [ids[0], ids[1]], "active": ids[0]})).unwrap();
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();

        // An unrelated edit must not copy the active photo's unchanged false over the peer's true.
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.7})).unwrap();
        for d in expected.iter_mut().take(2) {
            d.light.exposure = 0.7;
        }
        for (id, settings) in ids.iter().zip(&expected) {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), settings, "Unrelated edit: photo {id}");
        }

        let peer_history = s.catalog.photo(PhotoId(ids[1])).unwrap().history.len();
        s.execute("develop.merge", &json!({"settings": {"profile": {"camera_local_tone": false}}})).unwrap();
        assert_eq!(s.catalog.photo(PhotoId(ids[1])).unwrap().history.len(), peer_history, "unchanged toggle adds no peer history");
        for (id, settings) in ids.iter().zip(&expected) {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), settings, "Unchanged toggle: photo {id}");
        }

        // Direct unchanged edits may still add an active undo step. With Auto Sync off, real
        // Boolean changes affect only the active photo; selected and unselected peers stay exact.
        s.execute("develop.autoSync", &json!({"on": false})).unwrap();
        for on in [true, false] {
            s.execute("develop.merge", &json!({"settings": {"profile": {"camera_local_tone": on}}})).unwrap();
            expected[0].profile.camera_local_tone = on;
            for (id, settings) in ids.iter().zip(&expected) {
                assert_eq!(s.develop_of(PhotoId(*id)).unwrap().as_ref(), settings, "Auto Sync disabled: photo {id}");
            }
        }
    }
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
    use lcc_catalog::PhotoId;
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

/// Issue #345: a background preview build is a row of the activity stack, and `activity.cancel` stops it.
#[test]
fn a_preview_build_is_an_activity_row_that_can_be_cancelled() {
    // Issue #345: a background preview build is a row in the activity stack while it runs.
    {
        let mut s = demo();
        let ids: Vec<u64> = s.catalog.photos().take(4).map(|p| p.id.0).collect();
        let r = s.execute("library.buildPreviews", &json!({"ids": ids, "size": "standard", "edge": 256})).unwrap();
        let tasks = s.activity.list();
        assert_eq!(tasks.first().map(|t| (t.kind, t.total, t.label.as_str())), Some(("previews", 4, "Building previews")), "{tasks:?} {r}");
        assert!(tasks[0].cancellable);
        wait_activity_empty(&s);
        assert_eq!(s.execute("library.previewProgress", &json!({})).unwrap()["running"], false);
    }
    // Issue #345: `activity.cancel` stops a preview build like `library.cancelPreviews`.
    {
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
}

/// Colour range: a click samples the colour under it, so the mask selects that colour (white
/// in the mask view) and not a different one; ⇧ adds samples, up to five.
#[test]
fn color_range_sampling_selects_the_clicked_colour() {
    use lcc_pipeline::{MaskView, Overlay};
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
    let at = |img: &lcc_raster::Rgba8, x: f64, y: f64| img.data[(y * img.height as f64) as usize * img.width + (x * img.width as f64) as usize][0];
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
    use lcc_catalog::PhotoId;
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
    use lcc_catalog::PhotoId;
    let mut s = demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    // reference 1/100 f/4 ISO 100, slider +0.5; other 1/50 f/4 ISO 100 (one stop more light)
    for (i, (sh, ap, iso)) in [("1/100", 4.0f32, 100u32), ("1/50", 4.0, 100), ("", 4.0, 100)].iter().enumerate() {
        let mut m = s.catalog.photo(PhotoId(ids[i])).unwrap().meta.clone();
        m.shutter = sh.to_string();
        m.aperture = Some(*ap);
        m.iso = Some(*iso);
        s.commit("t", lcc_catalog::Op::SetMeta { id: PhotoId(ids[i]), meta: Box::new(m) }).unwrap();
    }
    s.execute("library.select", &json!({"ids": ids})).unwrap();
    s.selection.active = Some(PhotoId(ids[0]));
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5, "ids": [ids[0]]})).unwrap();
    let r = s.execute("develop.matchExposure", &json!({})).unwrap();
    assert_eq!((r["changed"].as_u64(), r["skipped"].clone()), (Some(1), json!([ids[2]])));
    let e = s.develop_of(PhotoId(ids[1])).unwrap().light.exposure;
    assert!((e - (-0.5)).abs() < 1e-9, "one stop more light → one stop less exposure: {e}");
    // the same holds for a catalog string in the camera notation of a whole second (issue #1619): the
    // reference is now 1.3 s, 130× the light of the target 1/100 s, so the slider tops out at +5 EV
    let mut m = s.catalog.photo(PhotoId(ids[0])).unwrap().meta.clone();
    m.shutter = "1.3\u{2033}".into();
    s.commit("t", lcc_catalog::Op::SetMeta { id: PhotoId(ids[0]), meta: Box::new(m) }).unwrap();
    let r = s.execute("develop.matchExposure", &json!({})).unwrap();
    assert_eq!(r["changed"].as_u64(), Some(1), "{r}");
    let e = s.develop_of(PhotoId(ids[1])).unwrap().light.exposure;
    assert_eq!(e, 5.0, "1.3 s against 1/100 s is 7.0 stops, clamped: {e}");
}

/// One test: the first Find Similar makes a signature of every demo photo, which is the cost.
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

    // every value stays within zero and one
    let r = s.execute("library.findSimilar", &json!({"id": id.0, "similarity": -1.0})).unwrap();
    let sims: Vec<f64> = r["photos"].as_array().unwrap().iter().map(|h| h["similarity"].as_f64().unwrap()).collect();
    assert_eq!(sims.len(), s.catalog.len());
    assert!(sims.iter().all(|v| (0.0..=1.0).contains(v)), "{sims:?}");

    // no hits show no photos
    let r = s.execute("library.findSimilar", &json!({"similarity": 1.5})).unwrap();
    assert_eq!(r["photos"].as_array().map(Vec::len), Some(0));
    assert_eq!(s.visible_cloned().len(), 0, "an empty Find Similar result must show no photos, not the whole library");
    s.execute("library.clearFilter", &json!({})).unwrap();
    assert_eq!(s.visible_cloned().len(), s.catalog.len(), "Clear Filters brings everything back");

    // a flat photo's signature is all zeros: it is not similar even to itself
    let photo = s.catalog.photos().next().unwrap().clone();
    s.signatures.insert(crate::media::content_key(&photo), [0.0; 64]);
    let r = s.execute("library.findSimilar", &json!({"id": photo.id.0})).unwrap();
    assert_eq!(r["photos"].as_array().map(Vec::len), Some(0));
    assert_eq!(s.visible_cloned().len(), 0, "no matches show nothing, not the whole library");
    assert_eq!(s.catalog.count(&s.filter), 0);
    let chips = crate::filter_chips(&s.filter, &s.catalog);
    assert_eq!(chips.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Only 0 photos"]);
    s.execute("library.filter", &chips[0].clear).unwrap();
    assert_eq!(s.filter, Default::default(), "the chip clears the empty state");
    assert_eq!(s.visible_cloned().len(), s.catalog.len());
}

/// Upstream issue #3545: the first Find Similar decoded every library photo one after another on the calling thread.
/// The decodes now run side by side, once per file, and the matches stay the same.
#[test]
fn find_similar_decodes_the_library_in_parallel_once_per_file() {
    use lcc_catalog::{Photo, PhotoId, Source};
    use std::sync::{Arc, Mutex};
    let mut s = Session::new();
    for i in 1..=16u64 {
        let name = format!("sim{i}.jpg");
        s.catalog
            .apply(lcc_catalog::Op::AddPhoto {
                photo: Box::new(Photo::new(PhotoId(i), Source::File { path: name.clone() }, &name, "JPEG", 64, 48, "")),
            })
            .unwrap();
    }
    let seen: Arc<Mutex<Vec<(String, std::thread::ThreadId)>>> = Arc::default();
    let log = seen.clone();
    s.media.file_loader = Some(Arc::new(move |path: &str, _max_edge: usize, _options| {
        log.lock().unwrap().push((path.to_string(), std::thread::current().id()));
        std::thread::sleep(std::time::Duration::from_millis(30));
        // odd photos are bright on top, even ones bright at the bottom
        let odd = path.trim_start_matches("sim").trim_end_matches(".jpg").parse::<u64>().unwrap() % 2 == 1;
        let img = lcc_raster::Rgb32f::from_fn(64, 48, |x, y| {
            let v = if odd { 1.0 - y as f32 / 48.0 } else { y as f32 / 48.0 } * 0.8 + x as f32 / 640.0;
            [v, v * 0.9, v * 0.7]
        });
        Ok((img, lcc_pipeline::SourceInfo::default()))
    }));
    let r = s.execute("library.findSimilar", &json!({"id": 1})).unwrap();
    let mut hits: Vec<u64> = r["photos"].as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect();
    hits.sort_unstable();
    assert_eq!(hits, [1, 3, 5, 7, 9, 11, 13, 15], "{r}");
    let loads = seen.lock().unwrap().clone();
    let mut paths: Vec<&str> = loads.iter().map(|l| l.0.as_str()).collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!((loads.len(), paths.len()), (16, 16), "each file decoded once");
    if std::thread::available_parallelism().map_or(1, |n| n.get()) > 1 {
        let threads: std::collections::HashSet<_> = loads.iter().map(|l| l.1).collect();
        assert!(threads.len() > 1, "the library was decoded on {} thread(s)", threads.len());
    }
    s.execute("library.findSimilar", &json!({"id": 2})).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 16, "signatures are cached: a second Find Similar decodes nothing");
}

/// "exactly" or "at most" no stars in the filter bar shows the unrated photos, as in Lightroom,
/// instead of switching the rating rule off; "at least" no stars still shows all.
#[test]
fn rating_exactly_or_at_most_zero_shows_the_unrated_photos() {
    {
        let mut s = demo();
        let ids: Vec<u64> = s.catalog.photos().map(|p| p.id.0).collect();
        assert!(ids.len() >= 3);
        s.execute("photo.rate", &json!({"rating": 0, "ids": ids})).unwrap();
        s.execute("photo.rate", &json!({"rating": 2, "ids": [ids[0]]})).unwrap();
        s.execute("photo.rate", &json!({"rating": 5, "ids": [ids[1]]})).unwrap();
        let unrated = ids.len() - 2;
        for op in ["exactly", "atMost"] {
            s.execute("library.filter", &json!({"rating": 0, "ratingOp": op})).unwrap();
            let shown = s.visible_cloned();
            assert_eq!(shown.len(), unrated, "{op} 0");
            assert!(shown.iter().all(|id| s.catalog.photo(*id).unwrap().rating == 0), "{op} 0 shows only unrated photos");
            assert_eq!(s.catalog.count(&s.filter), unrated);
            let chips = crate::filter_chips(&s.filter, &s.catalog);
            assert_eq!(chips.iter().map(|c| c.label.as_str()).collect::<Vec<_>>(), ["Unrated"], "{op}");
            s.execute("library.filter", &chips[0].clear).unwrap();
            assert_eq!(s.filter, Default::default(), "the chip clears the rule and its operator");
            assert_eq!(s.visible_cloned().len(), ids.len());
        }
        s.execute("library.filter", &json!({"rating": 0, "ratingOp": "atLeast"})).unwrap();
        assert_eq!(s.visible_cloned().len(), ids.len(), "at least no stars is every photo");
        assert!(crate::filter_chips(&s.filter, &s.catalog).is_empty());
    }
    // Upstream #3433: a smart album of rated photos, viewed with "= no stars", saves "= 0" rather than the album's own stars.
    {
        let mut s = demo();
        let r = s.execute("album.createSmart", &json!({"name": "Rated", "rules": {"rating": 3}})).unwrap();
        s.execute("library.source", &json!({"kind": "album", "id": r["id"]})).unwrap();
        s.execute("library.filter", &json!({"rating": 0, "ratingOp": "exactly"})).unwrap();
        let saved = s.view_rules();
        assert_eq!((saved.rating, saved.rating_op), (0, lcc_catalog::RatingOp::Exactly));
    }
}

#[test]
fn quick_develop_adds_to_each_photo() {
    use lcc_catalog::PhotoId;
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
    use lcc_develop::MaskOp;
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
    {
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
        assert_eq!(active_dev(&s).curve, lcc_develop::ToneCurve::default());
        assert!(s.execute("curve.reset", &json!({"channel": "alpha"})).is_err());
    }
    {
        let mut s = demo();
        let s_curve = json!([[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]);
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
        let ids: Vec<u32> = active_dev(&s).masks.iter().map(|m| m.id).collect();
        let curve_of = |s: &crate::Session, k: usize| active_dev(s).masks[k].curve.clone();
        // a channel of one mask's curve: the other mask and the photo's Tone Curve stay linear
        s.execute("develop.curve", &json!({"channel": "red", "points": s_curve, "mask": ids[0]})).unwrap();
        s.execute("develop.curve", &json!({"channel": "master", "points": [[0.0, 0.1], [1.0, 1.0]], "mask": ids[0]})).unwrap();
        assert_eq!(curve_of(&s, 0).red.len(), 4);
        assert_eq!(curve_of(&s, 0).master.len(), 2);
        assert!(curve_of(&s, 1).is_empty());
        assert_eq!(active_dev(&s).curve, lcc_develop::ToneCurve::default());
        // one undo step per change
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(curve_of(&s, 0).master.is_empty() && curve_of(&s, 0).red.len() == 4);
        // what isn't a mask's curve is refused, nothing changed
        let before = active_dev(&s);
        assert!(s.execute("develop.curve", &json!({"channel": "red", "points": s_curve, "mask": 99})).is_err());
        assert!(s.execute("develop.curve", &json!({"channel": "parametric", "points": s_curve, "mask": ids[0]})).is_err());
        assert!(s.execute("develop.curve", &json!({"channel": "red", "points": s_curve, "mask": "first"})).is_err());
        assert!(s.execute("curve.reset", &json!({"channel": "parametric", "mask": ids[0]})).is_err());
        assert_eq!(active_dev(&s), before);
        // presets: applied to, matched on and saved from a mask's curve
        s.execute("curve.applyPreset", &json!({"name": "Strong Contrast", "mask": ids[1]})).unwrap();
        assert_eq!(s.execute("curve.presets", &json!({"mask": ids[1]})).unwrap()["current"], "Strong Contrast");
        assert_eq!(s.execute("curve.presets", &json!({})).unwrap()["current"], "Linear", "the photo's curve is untouched");
        assert!(active_dev(&s).curve.master.is_empty());
        s.execute("curve.savePreset", &json!({"name": "Red Lift", "mask": ids[0]})).unwrap();
        let saved = s.curve_presets.iter().find(|p| p.name == "Red Lift").unwrap().clone();
        assert_eq!((saved.red.len(), saved.master.len()), (4, 0));
        s.execute("curve.applyPreset", &json!({"name": "Red Lift"})).unwrap();
        assert_eq!(active_dev(&s).curve.red.len(), 4, "a mask's curve as a preset for the Tone Curve");
        // reset: one channel, then all of it
        s.execute("curve.reset", &json!({"channel": "red", "mask": ids[0]})).unwrap();
        assert!(curve_of(&s, 0).is_empty());
        s.execute("curve.reset", &json!({"mask": ids[1]})).unwrap();
        assert!(curve_of(&s, 1).is_empty());
        assert_eq!(active_dev(&s).curve.red.len(), 4, "the Tone Curve stays");
    }
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
    let shape = lcc_develop::MaskShape::Object { hint: vec![], exclude: vec![], seg: None, detail: vec![], edge: 0.0 };
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.masks.push(lcc_develop::Mask {
        id: 1,
        components: vec![lcc_develop::MaskComponent { anchor: None, name: None, op: lcc_develop::MaskOp::Add, invert: false, shape }],
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
    {
        use lcc_geom::Rect;
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
        assert!(s.face_job(lcc_catalog::PhotoId(u64::MAX), faces[0], 256).is_none());
    }
    // Regions are on the upright photo; after Rotate Right the close-up still frames the face: the box
    // is carried into the rotated frame the crop lives in, and the crop stays square in rotated pixels.
    {
        use lcc_geom::Rect;
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
}

/// Issue #1147: a face tile with two faces in it outlines the one it is about, and its hover
/// preview shows the whole photo with that face marked; both follow the box through Rotate Right and
/// never panic on a hostile one.
#[test]
fn a_face_tile_says_where_its_face_is_in_the_tile_and_in_the_whole_photo() {
    use lcc_geom::Rect;
    let mut s = demo();
    let id = s.active().unwrap();
    let inside = |r: Rect| (0.0..=1.0).contains(&r.x0) && (0.0..=1.0).contains(&r.y0) && r.x1 <= 1.0 && r.y1 <= 1.0 && r.x1 > r.x0 && r.y1 > r.y0;
    // a face with room on every side sits in the middle of its tile, a 1.7x square around it
    let face = Rect { x0: 0.40, y0: 0.30, x1: 0.50, y1: 0.45 };
    let t = s.face_in_tile(id, face).unwrap();
    assert!(inside(t), "{t:?}");
    assert!((((t.x0 + t.x1) / 2.0) - 0.5).abs() < 1e-6 && (((t.y0 + t.y1) / 2.0) - 0.5).abs() < 1e-6, "{t:?}");
    assert!(t.x1 - t.x0 < 0.9 && t.y1 - t.y0 < 0.9, "the face leaves room around it: {t:?}");
    // a face in a corner: the square is pushed inside the photo, so the face is off centre
    let t = s.face_in_tile(id, Rect { x0: 0.0, y0: 0.0, x1: 0.05, y1: 0.05 }).unwrap();
    assert!(inside(t) && t.x0 < 0.1 && t.y0 < 0.1, "{t:?}");
    // the whole photo: the face box as it is (no crop, no rotation), the job without the crop
    let (job, at) = s.face_photo_job(id, face, 384).unwrap();
    assert!(job.settings.crop.geometry.is_identity());
    assert!((at.x0 - face.x0).abs() < 1e-9 && (at.y1 - face.y1).abs() < 1e-9, "{at:?}");
    // after Rotate Right the left of the photo is at the top: centre (0.45, 0.375) → (0.625, 0.45)
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let (_, at) = s.face_photo_job(id, face, 384).unwrap();
    assert!((((at.x0 + at.x1) / 2.0) - 0.625).abs() < 1e-9 && (((at.y0 + at.y1) / 2.0) - 0.45).abs() < 1e-9, "{at:?}");
    let t = s.face_in_tile(id, face).unwrap();
    assert!((((t.x0 + t.x1) / 2.0) - 0.5).abs() < 1e-6, "{t:?}");
    // hostile boxes: no panic, and whatever comes back lies within the picture
    for face in [
        Rect { x0: -5.0, y0: -5.0, x1: 5.0, y1: 5.0 },
        Rect { x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5 },
        Rect { x0: f64::NAN, y0: 0.5, x1: 0.6, y1: f64::INFINITY },
        Rect { x0: 0.9, y0: 0.9, x1: 0.1, y1: 0.1 },
    ] {
        if let Some(t) = s.face_in_tile(id, face) {
            assert!(inside(t), "{face:?} → {t:?}");
        }
        if let Some((_, at)) = s.face_photo_job(id, face, 384) {
            assert!(inside(at), "{face:?} → {at:?}");
        }
    }
    assert!(s.face_in_tile(lcc_catalog::PhotoId(u64::MAX), face).is_none());
    assert!(s.face_photo_job(lcc_catalog::PhotoId(u64::MAX), face, 384).is_none());
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
        ..Default::default()
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
    {
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
    // Undoing a step that created photos (here a virtual copy) never leaves them in the selection.
    {
        let mut s = demo();
        let r = s.execute("photo.virtualCopy", &json!({})).unwrap();
        let copy = r["ids"][0].as_u64().unwrap();
        s.execute("library.select", &json!({"ids": [copy]})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.selection.ids.iter().all(|&id| s.catalog.photo(id).is_some()), "{:?}", s.selection);
        assert!(s.selection.active.is_none_or(|a| s.selection.ids.contains(&a)));
    }
}

#[test]
fn hdr_edits_render_with_their_hdr_histogram() {
    let mut s = demo();
    let id = s.active().unwrap();
    let plain = s.loupe_job(id, 200, 200, true).unwrap().run().rendered.unwrap();
    assert!(plain.histogram.hdr.is_none(), "SDR edits: the image's histogram");
    s.execute("develop.hdr", &json!({"enabled": true, "maxEv": 3})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let r = s.loupe_job(id, 200, 200, true).unwrap().run().rendered.unwrap();
    let bins = r.histogram.hdr.expect("an HDR edit's histogram");
    assert_eq!(bins.stops, lcc_develop::Hdr::MAX_EV_LIMIT as f32, "a fixed scale");
    assert!(r.histogram.above_sdr() > 0.0);
    assert!(r.deep.is_none(), "the preview itself stays the 8-bit SDR rendition");
}

/// The `groups` of Copy, Paste, Paste from Previous and Sync: an unknown name, or a value that is not a list of
/// names, is refused and changes nothing (no edit, no history step); a valid list limits what is carried, and none
/// (or null) means everything copied, or for Sync the groups its dialog remembers.
#[test]
fn develop_group_lists_are_validated_and_limit_what_is_carried() {
    let mut s = demo();
    let ids: Vec<_> = s.visible().iter().copied().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.0, "color.vibrance": 30, "detail.sharpenAmount": 90}})).unwrap();
    let err = s.execute("develop.copy", &json!({"groups": ["notAGroup"]})).unwrap_err();
    assert!(matches!(err, crate::EngineError::BadParams { .. }), "develop.copy: {err}");
    s.execute("develop.copy", &json!({"groups": ["light", "color"]})).unwrap();
    // `b` is the active photo, `a` the previous one
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    let before = (*s.develop_of(b).unwrap()).clone();
    let history = s.catalog.photo(b).unwrap().history.len();
    let refused = |s: &mut Session, cmd: &str, params: Value, bad_groups: &[Value]| {
        for bad in bad_groups {
            let mut p = params.clone();
            p["groups"] = bad.clone();
            let err = s.execute(cmd, &p).unwrap_err();
            assert!(matches!(err, crate::EngineError::BadParams { .. }), "{cmd} {bad}: {err}");
            assert_eq!(*s.develop_of(b).unwrap(), before, "{cmd} {bad} changed the photo");
            assert_eq!(s.catalog.photo(b).unwrap().history.len(), history, "{cmd} {bad} added a history step");
        }
    };
    let both = json!({"ids": [a.0, b.0], "active": a.0});
    refused(&mut s, "develop.pastePrevious", json!({}), &[json!(["badGroup"])]);
    let paste_bad = [
        json!(["bogusGroup"]),
        json!(["LIGHT"]),
        json!(["Exposure"]),
        json!("light"),
        json!(42),
        json!(true),
        json!({"name": "light"}),
        json!([123]),
        json!(["light", 456]),
        json!([true]),
    ];
    refused(&mut s, "develop.paste", json!({"ids": [b.0]}), &paste_bad);
    s.execute("library.select", &both).unwrap();
    let sync_bad = [
        json!(["bogusGroup"]),
        json!(["LIGHT"]),
        json!(["WhiteBalance"]),
        json!("light"),
        json!(100),
        json!(false),
        json!({"groups": "light"}),
        json!([999]),
        json!(["color", false]),
        json!([null]),
    ];
    refused(&mut s, "develop.sync", json!({}), &sync_bad);

    // Paste: only the chosen groups, also one the copy left out (`detail`; upstream #3148)
    s.execute("develop.paste", &json!({"ids": [b.0], "groups": ["light", "detail"]})).unwrap();
    let d = (*s.develop_of(b).unwrap()).clone();
    assert_eq!(d.light.exposure, 1.0);
    assert_eq!(d.color.vibrance, before.color.vibrance, "`color` was not asked for");
    assert_eq!(d.detail.sharpen_amount, 90.0, "`detail` was asked for");
    // `b` back to its defaults, selected alone; returns its vibrance there
    let reset_b = |s: &mut Session| {
        s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
        s.execute("develop.reset", &json!({})).unwrap();
        let d = s.develop_of(b).unwrap();
        assert_eq!(d.light.exposure, 0.0);
        d.color.vibrance
    };
    // no groups, or null: what was copied, and nothing the copy left out
    for params in [json!({"ids": [b.0]}), json!({"ids": [b.0], "groups": null})] {
        reset_b(&mut s);
        s.execute("develop.paste", &params).unwrap();
        let d = s.develop_of(b).unwrap();
        assert_eq!((d.light.exposure, d.color.vibrance), (1.0, 30.0), "paste {params}");
        assert_eq!(d.detail.sharpen_amount, before.detail.sharpen_amount, "paste {params}: `detail` was not copied");
    }
    // Paste from Previous (`a`, left for `b`) with a valid group
    reset_b(&mut s);
    s.execute("develop.pastePrevious", &json!({"groups": ["light"]})).unwrap();
    assert_eq!(s.develop_of(b).unwrap().light.exposure, 1.0);

    // Sync: only the chosen group
    let base_vibrance = reset_b(&mut s);
    s.execute("library.select", &both).unwrap();
    s.execute("develop.sync", &json!({"groups": ["light"]})).unwrap();
    let d = (*s.develop_of(b).unwrap()).clone();
    assert_eq!((d.light.exposure, d.color.vibrance), (1.0, base_vibrance), "syncing `light` leaves `color`");
    // no groups: what Sync ticked last (remembered); with fresh memory, the default copy groups. Null is the same.
    for params in [json!({}), json!({"groups": null})] {
        reset_b(&mut s);
        s.execute("library.select", &both).unwrap();
        s.dialogs = Default::default();
        s.execute("develop.sync", &params).unwrap();
        let d = s.develop_of(b).unwrap();
        assert_eq!((d.light.exposure, d.color.vibrance), (1.0, 30.0), "sync {params}");
    }
    // without `ids`, Paste goes to the selection
    reset_b(&mut s);
    s.execute("develop.paste", &json!({})).unwrap();
    assert_eq!(s.develop_of(b).unwrap().light.exposure, 1.0, "pasted to the selected photo");
}

#[test]
fn presets_validate_groups_on_create_and_update() {
    let mut s = demo();
    s.execute("develop.set", &json!({"values": {"light.exposure": 1.0, "color.vibrance": 25}})).unwrap();
    let initial_preset_count = s.presets.len();

    // 1. preset.create rejects invalid group names and incorrect JSON types without creating a preset
    for invalid_groups in [json!(["bogusGroup"]), json!("light"), json!(123), json!([true]), json!(["light", 99])] {
        let err = s.execute("preset.create", &json!({"name": "Test", "groups": invalid_groups})).unwrap_err();
        assert!(matches!(err, crate::EngineError::BadParams { .. }));
        assert_eq!(s.presets.len(), initial_preset_count);
    }

    // 2. preset.create with valid groups creates a preset with only those groups
    let res = s.execute("preset.create", &json!({"name": "ValidPreset", "groups": ["light"]})).unwrap();
    let pid = res["id"].as_str().unwrap().to_string();
    assert_eq!(s.presets.len(), initial_preset_count + 1);
    let p = s.presets.iter().find(|p| p.id == pid).unwrap();
    assert!(p.settings.get("light").is_some());
    assert!(p.settings.get("color").is_none());

    // 3. preset.create with omitted groups: what New Preset ticked last; fresh memory = the defaults
    s.dialogs = Default::default();
    let res_omitted = s.execute("preset.create", &json!({"name": "DefaultPreset"})).unwrap();
    let pid_omitted = res_omitted["id"].as_str().unwrap().to_string();
    let p_omitted = s.presets.iter().find(|p| p.id == pid_omitted).unwrap();
    assert!(p_omitted.settings.get("light").is_some());
    assert!(p_omitted.settings.get("color").is_some());

    // Change develop settings
    s.execute("develop.set", &json!({"values": {"light.exposure": 2.0, "color.vibrance": 50}})).unwrap();

    let preset_before_update = s.presets.iter().find(|p| p.id == pid).unwrap().clone();

    // 4. preset.update rejects invalid group names and incorrect JSON types without modifying the preset
    for invalid_groups in [json!(["bogusGroup"]), json!("color"), json!(999), json!([false]), json!(["color", "bad"])] {
        let err = s.execute("preset.update", &json!({"id": pid, "groups": invalid_groups})).unwrap_err();
        assert!(matches!(err, crate::EngineError::BadParams { .. }));
        let current = s.presets.iter().find(|p| p.id == pid).unwrap();
        assert_eq!(*current, preset_before_update);
    }

    // 5. preset.update with valid groups updates specified groups
    s.execute("preset.update", &json!({"id": pid, "groups": ["light", "color"]})).unwrap();
    let p_after_valid = s.presets.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p_after_valid.settings["light"]["exposure"], 2.0);
    assert_eq!(p_after_valid.settings["color"]["vibrance"], 50.0);

    // 6. preset.update with omitted groups updates existing preset keys
    s.execute("develop.set", &json!({"values": {"light.exposure": -1.0, "color.vibrance": -20}})).unwrap();
    s.execute("preset.update", &json!({"id": pid})).unwrap();
    let p_after_omitted = s.presets.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p_after_omitted.settings["light"]["exposure"], -1.0);
    assert_eq!(p_after_omitted.settings["color"]["vibrance"], -20.0);
}

/// A Heal or Remove spot whose size (from a sidecar, which isn't held to the slider's range) is far past the photo
/// made the render's radius infinite: its heal blur and the source it reads went with it, overflowing the blur's box
/// radii and the bilinear sampler (issue #1519). Such spots render, quickly, as a spot over the whole frame.
#[test]
fn oversized_spots_render() {
    use lcc_develop::{Spot, SpotMode};
    let mut s = demo();
    let id = s.active().unwrap();
    let base = (*s.develop_of(id).unwrap_or_default()).clone();
    for size in [10.0, 1e30, f64::MAX, f64::INFINITY, f64::NAN] {
        for mode in [SpotMode::Heal, SpotMode::Remove, SpotMode::Clone] {
            let mut dd = base.clone();
            dd.spots.push(Spot { mode, points: vec![lcc_geom::Point::new(0.5, 0.5)], size, ..Default::default() });
            s.set_develop(id, dd, "Remove").unwrap();
            let t = std::time::Instant::now();
            let r = s.render_now(id, 300, 200).unwrap();
            assert!(r.image.width >= 1 && r.image.height >= 1, "{size} {mode:?}");
            assert!(t.elapsed().as_secs_f64() < 60.0, "{size} {mode:?}: {:?}", t.elapsed());
        }
    }
}

/// Rotate turns the crop with the image: a crop of the left half becomes the top half, and turning back restores it.
#[test]
fn rotate_carries_the_crop_with_the_image() {
    use lcc_geom::Rect;
    let mut s = demo();
    let id = s.active().unwrap();
    let mut d = active_dev(&s);
    let left_half = Rect { x0: 0.0, y0: 0.0, x1: 0.5, y1: 1.0 };
    d.crop.geometry.rect = left_half;
    d.crop.aspect = Some((2, 3));
    s.set_develop(id, d, "Crop").unwrap();
    s.execute("photo.rotateRight", &json!({})).unwrap();
    let c = active_dev(&s).crop;
    assert_eq!(c.geometry.rect, Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 0.5 });
    assert_eq!(c.aspect, Some((3, 2)), "a locked aspect turns too");
    s.execute("photo.rotateLeft", &json!({})).unwrap();
    let c = active_dev(&s).crop;
    assert_eq!(c.geometry.rect, left_half);
    assert_eq!(c.aspect, Some((2, 3)));
}

/// A call that names its photos with a malformed `id` or `ids` (a string, a mixed list) is refused and changes
/// nothing; it used to act on the selection, or on the valid part of the list (issue #1506). Leaving the targets
/// out still means the selection.
#[test]
fn malformed_photo_targets_are_refused_not_widened() {
    use lcc_catalog::PhotoId;
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": [ids[0], ids[1]]})).unwrap();
    let rating = |s: &Session, i: u64| s.catalog.photo(PhotoId(i)).unwrap().rating;
    let before: Vec<u8> = ids.iter().map(|i| rating(&s, *i)).collect();
    for bad in [
        json!({"rating": 5, "id": "5"}),
        json!({"rating": 5, "ids": [ids[2], "x"]}),
        json!({"rating": 5, "ids": ids[2].to_string()}),
        json!({"rating": 5, "id": -1}),
    ] {
        let err = s.execute("photo.rate", &bad).unwrap_err();
        assert!(matches!(&err, crate::EngineError::BadParams { cmd, .. } if cmd == "photo.rate"), "{bad}: {err}");
        assert_eq!(ids.iter().map(|i| rating(&s, *i)).collect::<Vec<_>>(), before, "{bad} changed a rating");
    }
    let snapshot = s.catalog.to_snapshot();
    assert!(s.execute("photo.deletePermanently", &json!({"id": "5"})).is_err());
    assert_eq!(s.catalog.to_snapshot(), snapshot, "nothing deleted");
    // no targets, or a null one: the selection
    s.execute("photo.rate", &json!({"rating": 4, "id": null})).unwrap();
    assert_eq!((rating(&s, ids[0]), rating(&s, ids[1]), rating(&s, ids[2])), (4, 4, before[2]));
    // a preset's id is not a photo target
    let preset = s.presets.first().unwrap().id.clone();
    s.execute("preset.apply", &json!({"id": preset})).unwrap();
}

/// Issue #175 (AC-2 of #160): evaluating the stack commands' `enabled` does work proportional to
/// the selected and stacked photos, not their product. Counts the photos and stacks the checks
/// look at (not time): a library four times larger costs less than eight times the work.
#[test]
fn stack_command_enabled_work_grows_linearly_with_the_library() {
    use crate::cmd::STACK_WORK;
    use lcc_catalog::{Op, Photo, PhotoId, Source};
    const STACK_CMDS: [&str; 7] = ["stack.ungroup", "stack.remove", "stack.setTop", "stack.moveUp", "stack.moveDown", "stack.split", "stack.toggle"];
    // `n` photos in stacks of two, all selected; `last_only` stacks just the last pair instead.
    let work_for = |n: u64, last_only: bool| -> u64 {
        let mut s = demo();
        let mut ids = Vec::new();
        for i in 0..n {
            let id = s.catalog.alloc_photo_id();
            let p = Photo::new(id, Source::Demo { scene: 1 }, &format!("s{i}.jpg"), "JPEG", 6, 4, "2026-01-01T00:00:00");
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            ids.push(id);
        }
        let pairs: Vec<&[PhotoId]> = if last_only { vec![&ids[ids.len() - 2..]] } else { ids.chunks(2).collect() };
        for pair in pairs {
            let op = s.catalog.group_ops(pair[0], &pair[1..], false).unwrap();
            s.catalog.apply(op).unwrap();
        }
        s.selection = Selection { ids: ids.clone(), active: ids.first().copied() };
        STACK_WORK.with(|w| w.set(0));
        let specs = command_specs();
        for id in STACK_CMDS {
            let spec = specs.iter().find(|c| c.id == id).unwrap();
            assert!((spec.enabled)(&s).is_ok(), "{id} is on with stacked photos selected");
        }
        STACK_WORK.with(std::cell::Cell::get)
    };
    for last_only in [false, true] {
        let (small, large) = (work_for(1000, last_only), work_for(4000, last_only));
        assert!(large < small.max(1) * 8, "last_only={last_only}: work {small} -> {large} for 4x the photos");
    }
}

/// Rate a few photos 1 star, leave the rest unrated: a rating filter shows only what it asks for, in the view
/// and in Select All (what an export of the selection takes), with the view held or not, set before or after rating.
#[test]
fn rating_filter_shows_only_rated_photos() {
    use lcc_catalog::PhotoId;
    for hold in [false, true] {
        for filter_first in [false, true] {
            let mut s = demo();
            if hold {
                s.hold_view = true;
            }
            let all = s.visible_cloned();
            s.execute("photo.rate", &json!({"rating": 0, "ids": all})).unwrap();
            let rated: Vec<PhotoId> = all.iter().copied().step_by(5).collect();
            if filter_first {
                s.execute("library.filter", &json!({"rating": 1})).unwrap();
                assert!(s.visible().is_empty(), "nothing rated yet");
            }
            s.execute("photo.rate", &json!({"rating": 1, "ids": rated})).unwrap();
            s.execute("photo.rate", &json!({"rating": 3, "ids": [all[1]]})).unwrap();
            let ctx = format!("hold {hold}, filter first {filter_first}");
            let shown = |s: &mut Session, f: Value| {
                s.execute("library.clearFilter", &json!({})).unwrap();
                s.execute("library.filter", &f).unwrap();
                let mut v = s.visible_cloned();
                v.sort();
                v
            };
            let mut at_least_1: Vec<PhotoId> = rated.iter().copied().chain([all[1]]).collect();
            at_least_1.sort();
            assert_eq!(shown(&mut s, json!({"rating": 1})), at_least_1, "≥1 ({ctx})");
            s.execute("library.selectAll", &json!({})).unwrap();
            let mut sel = s.selection.ids.clone();
            sel.sort();
            assert_eq!(sel, at_least_1, "Select All under ≥1 ({ctx})");
            let mut exactly_1 = rated.clone();
            exactly_1.sort();
            assert_eq!(shown(&mut s, json!({"rating": 1, "ratingOp": "exactly"})), exactly_1, "=1 ({ctx})");
            assert_eq!(shown(&mut s, json!({"rating": 3, "ratingOp": "exactly"})), vec![all[1]], "=3 ({ctx})");
            assert_eq!(shown(&mut s, json!({"rating": 2})), vec![all[1]], "≥2 ({ctx})");
            // Lightroom's "≤ 1": unrated photos are at most one star too
            let at_most_1 = shown(&mut s, json!({"rating": 1, "ratingOp": "atMost"}));
            assert_eq!(at_most_1.len(), all.len() - 1, "≤1 ({ctx})");
            assert!(!at_most_1.contains(&all[1]));
        }
    }
}

#[test]
fn auto_sync_hdr_enabled_preserves_per_photo_tuning() {
    {
        use lcc_develop::{DevelopSettings, Hdr, ProcessVersion};
        let mut s = demo();
        let ids: Vec<_> = s.catalog.photos().take(2).map(|p| p.id).collect();
        assert_eq!(ids.len(), 2);
        s.execute("develop.autoSync", &json!({"on": false})).unwrap();
        // Keep distinct stored processes explicit: the default is now V2; no rendering claim is made.
        let mut expected = [DevelopSettings::default(), DevelopSettings { process: ProcessVersion(2), ..DevelopSettings::default() }];
        expected[0].process = ProcessVersion(1);
        expected[0].light.exposure = 0.5;
        expected[0].light.contrast = 12.0;
        expected[1].light.exposure = -0.25;
        expected[1].light.contrast = -22.0;
        expected[1].hdr = Hdr {
            enabled: false,
            max_ev: 4.0,
            sdr_brightness: 11.0,
            sdr_contrast: -12.0,
            sdr_highlights: 13.0,
            sdr_shadows: -14.0,
            sdr_whites: 15.0,
            sdr_clarity: -16.0,
            ..Hdr::default()
        };
        for (id, settings) in ids.iter().zip(&expected) {
            s.set_develop(*id, settings.clone(), "HDR tuning fixture").unwrap();
        }
        s.execute("library.select", &json!({"ids": ids.iter().map(|id| id.0).collect::<Vec<_>>(), "active": ids[0].0})).unwrap();
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        let selection = s.selection.clone();
        for enabled in [true, false] {
            s.execute("develop.hdr", &json!({"enabled": enabled})).unwrap();
            for (id, settings) in ids.iter().zip(&mut expected) {
                settings.hdr.enabled = enabled;
                let actual = s.develop_of(*id).unwrap();
                assert_eq!(actual.hdr, settings.hdr, "HDR tuning must survive enabled={enabled} for photo {id:?}");
                assert_eq!(actual.as_ref(), &*settings, "other per-photo edits and process must survive enabled={enabled}");
            }
            assert_eq!(s.selection, selection, "selection and active photo must survive enabled={enabled}");
            assert!(s.auto_sync, "Auto Sync must remain enabled");
        }
    }
    {
        use lcc_catalog::PhotoId;
        let mut s = demo();
        let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
        assert_eq!(ids.len(), 2);
        for id in &ids {
            assert_eq!(s.develop_of(PhotoId(*id)).unwrap().hdr, lcc_develop::Hdr::default(), "default-HDR fixture");
        }
        s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
        s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        let enabled = |s: &Session| ids.iter().map(|id| s.develop_of(PhotoId(*id)).unwrap().hdr.enabled).collect::<Vec<_>>();
        s.execute("develop.hdr", &json!({"enabled": true})).unwrap();
        assert_eq!(enabled(&s), vec![true, true], "HDR On must reach both selected photos");
        s.execute("develop.hdr", &json!({"enabled": false})).unwrap();
        assert_eq!(enabled(&s), vec![false, false], "HDR Off must reach both selected photos");
    }
}

/// The Remove tools behave differently: a clicked Remove spot gets a source it chose and chooses
/// again when moved (Heal keeps its own); a brushed Remove stroke has none (it is rebuilt from its
/// surroundings); an explicit `repick` is a no-op for Heal and adds no undo step.
#[test]
fn remove_picks_its_own_source_and_heal_keeps_its_own() {
    let mut s = demo();
    let id = s.active().unwrap();
    let r = s.execute("spot.add", &json!({"mode": "remove", "points": [[0.3, 0.4]], "size": 0.02})).unwrap();
    let i = r["index"].as_u64().unwrap() as usize;
    assert!(active_dev(&s).spots[i].source_offset.is_some(), "a click: a source of its own");
    let brushed = s.execute("spot.add", &json!({"mode": "remove", "points": [[0.6, 0.6], [0.65, 0.62], [0.7, 0.6]], "size": 0.02})).unwrap();
    let b = brushed["index"].as_u64().unwrap() as usize;
    assert_eq!(active_dev(&s).spots[b].source_offset, None, "a brushed stroke: rebuilt, no source");
    let heal = s.execute("spot.add", &json!({"mode": "heal", "points": [[0.2, 0.7]], "size": 0.02, "source": [0.1, 0.0]})).unwrap();
    let hi = heal["index"].as_u64().unwrap() as usize;
    // moving: Heal keeps its offset (so does Remove: it chooses again only when asked)
    s.execute("spot.update", &json!({"index": hi, "move": [0.3, -0.2]})).unwrap();
    assert_eq!(active_dev(&s).spots[hi].source_offset, Some(lcc_geom::Point::new(0.1, 0.0)));
    let steps = |s: &mut Session| s.execute("history.list", &json!({})).unwrap()["history"].as_array().unwrap().len();
    s.execute("spot.update", &json!({"index": i, "move": [0.35, 0.3]})).unwrap();
    assert!(active_dev(&s).spots[i].source_offset.is_some());
    // asked, Remove chooses again where it now is; then it has that source: asking again changes
    // nothing (no undo step), nor does asking Heal
    s.execute("spot.update", &json!({"index": i, "repick": true})).unwrap();
    let n = steps(&mut s);
    s.execute("spot.update", &json!({"index": i, "repick": true})).unwrap();
    s.execute("spot.update", &json!({"index": hi, "repick": true})).unwrap();
    assert_eq!(steps(&mut s), n);
    let _ = id;
}

/// Delete Rejected Photos takes every rejected photo of the view in one undo step, to Recently
/// Deleted (files stay); a bad `files` is refused. Ported from OneSlips' LR-DELETE-REJECTED.
#[test]
fn delete_rejected_photos_takes_the_views_rejects_in_one_step() {
    let mut s = demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|p| p.0).collect();
    let before = s.visible_cloned().len();
    s.execute("photo.flag", &json!({"ids": [ids[0], ids[2]], "flag": "reject"})).unwrap();
    s.execute("photo.flag", &json!({"ids": [ids[1]], "flag": "none"})).unwrap();
    let view = s.visible_cloned();
    let rejects = view.iter().filter(|id| s.catalog.photo(**id).is_some_and(|p| p.flag == lcc_catalog::Flag::Reject)).count();
    assert!(rejects >= 2);
    assert!(s.execute("library.deleteRejected", &json!({"files": "shred"})).is_err());
    let r = s.execute("library.deleteRejected", &json!({})).unwrap();
    assert_eq!(r["deleted"], rejects, "{r}");
    assert_eq!(s.visible_cloned().len(), before - rejects);
    assert!(s.catalog.photo(lcc_catalog::PhotoId(ids[0])).is_some_and(|p| p.deleted), "to Recently Deleted");
    assert!(s.catalog.photo(lcc_catalog::PhotoId(ids[1])).is_some_and(|p| !p.deleted), "a photo not rejected stays");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.visible_cloned().len(), before, "one undo step brings both back");
    let r = s.execute("library.deleteRejected", &json!({"files": "recycle"}));
    assert!(r.is_err(), "sending files to the trash needs confirm: {r:?}");
    let all: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    s.execute("photo.flag", &json!({"ids": all, "flag": "none"})).unwrap();
    assert_eq!(s.execute("library.deleteRejected", &json!({})).unwrap()["deleted"], 0);
}

/// The filter bar's live counts: the view by mark, counted without the marks' own filter, so the
/// pick count stays while only picks are shown. Ported from OneSlips' LR-FILTER-BAR (engine).
#[test]
fn tally_counts_the_view_without_its_mark_filter() {
    let mut s = demo();
    let ids: Vec<u64> = s.visible_cloned().iter().map(|p| p.0).collect();
    s.execute("photo.flag", &json!({"ids": ids, "flag": "none"})).unwrap();
    s.execute("photo.flag", &json!({"ids": [ids[0], ids[1]], "flag": "pick"})).unwrap();
    s.execute("photo.flag", &json!({"ids": [ids[2]], "flag": "reject"})).unwrap();
    s.execute("photo.label", &json!({"ids": [ids[0]], "label": "red"})).unwrap();
    let t = s.execute("library.tally", &json!({})).unwrap();
    assert_eq!((t["picks"].as_u64(), t["rejects"].as_u64(), t["total"].as_u64()), (Some(2), Some(1), Some(ids.len() as u64)), "{t}");
    assert!(t["labels"][0].as_u64().is_some_and(|n| n >= 1), "{t}");
    s.execute("library.filter", &json!({"flag": "pick"})).unwrap();
    assert_eq!(s.visible_cloned().len(), 2);
    let t = s.tally();
    assert_eq!((t.picks, t.rejects, t.total), (2, 1, ids.len()), "the flag filter itself isn't counted in");
}

/// A gesture painted in parts (⌥ pressed and let go during it) arrives as `runs`, one stroke per
/// mode; a live update (`continue`) replaces what the gesture sent last time (`replace` strokes),
/// so the gesture stays as many strokes as it has runs. Hostile `runs` are errors.
#[test]
fn brush_stroke_runs_and_live_updates_replace_what_the_gesture_sent() {
    use lcc_develop::MaskShape;
    let mut s = demo();
    s.execute("mask.add", &json!({"kind": "brush"})).unwrap();
    s.execute("mask.brushStroke", &json!({"points": [[0.1, 0.1]]})).unwrap();
    let strokes = |s: &Session| match &active_dev(s).masks[0].components[0].shape {
        MaskShape::Brush { strokes } => strokes.clone(),
        _ => panic!("brush"),
    };
    // the live stroke: first sent plain, then again as it grows
    s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2]], "size": 0.05})).unwrap();
    s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2], [0.3, 0.3]], "size": 0.05, "continue": true})).unwrap();
    assert_eq!(strokes(&s).len(), 2, "the live update replaced the stroke it grew from");
    assert_eq!(strokes(&s)[1].points.len(), 2);
    // ⌥ pressed part-way: two runs replace the one stroke sent so far
    let runs = json!({"runs": [{"points": [[0.2, 0.2], [0.3, 0.3]]}, {"points": [[0.3, 0.3], [0.4, 0.4]], "erase": true, "size": 0.08}],
        "size": 0.05, "continue": true});
    s.execute("mask.brushStroke", &runs).unwrap();
    let now = strokes(&s);
    assert_eq!(now.len(), 3, "{now:?}");
    assert!(!now[1].erase && now[2].erase, "each run keeps its mode");
    assert_eq!((now[1].size, now[2].size), (0.05, 0.08), "a run's own value, else the top level's");
    // the next update says how many it sent last time
    let mut more = runs.clone();
    more["replace"] = json!(2);
    s.execute("mask.brushStroke", &more).unwrap();
    assert_eq!(strokes(&s).len(), 3, "two replaced by two");
    assert!(
        s.execute("mask.brushStroke", &json!({"runs": [{"points": [[0.1, 0.1]]}], "replace": -4, "continue": true})).is_ok(),
        "a bad count is one"
    );
    for bad in [json!({"runs": []}), json!({"runs": 3}), json!({"runs": [{"size": 0.1}]})] {
        assert!(s.execute("mask.brushStroke", &bad).is_err(), "{bad}");
    }
}

/// Masks are named after what they select ("Radial Gradient − Linear Gradient") and follow their
/// components while the name is automatic; a name the photographer gives is kept.
#[test]
fn masks_get_automatic_names_until_renamed() {
    {
        let mut s = demo();
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        assert_eq!(active_dev(&s).masks[0].name, "Radial Gradient");
        s.execute("mask.addComponent", &json!({"kind": "linear", "op": "subtract"})).unwrap();
        assert_eq!(active_dev(&s).masks[0].name, "Radial Gradient − Linear Gradient");
        s.execute("mask.rename", &json!({"name": "Face light"})).unwrap();
        s.execute("mask.addComponent", &json!({"kind": "brush", "op": "add"})).unwrap();
        assert_eq!(active_dev(&s).masks[0].name, "Face light", "a given name is kept");
        // a brush stroke into no mask makes one named after it
        s.execute("mask.select", &json!({"id": null})).unwrap();
        s.execute("mask.brushStroke", &json!({"points": [[0.5, 0.5]]})).unwrap();
        assert_eq!(active_dev(&s).masks[1].name, "Brush");
    }
    {
        let mut s = demo();
        s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
        s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
        s.execute("mask.rename", &json!({"name": "Warmth"})).unwrap();
        let r = s.execute("preset.create", &json!({"name": "Glow", "groups": ["masks"]})).unwrap();
        let pid = r["id"].as_str().unwrap().to_string();
        s.execute("library.next", &json!({})).unwrap();
        s.execute("preset.apply", &json!({"id": pid})).unwrap();
        let names: Vec<String> = active_dev(&s).masks.iter().map(|m| m.name.clone()).collect();
        assert!(names.iter().any(|n| n == "Warmth"), "the given name is kept: {names:?}");
        assert!(names.iter().any(|n| n.starts_with("Glow")), "the automatic one is named after the preset: {names:?}");
    }
}

// ---- upstream #3148 (issue #3056): Paste Selected Settings pastes groups the copy left out; a pasted crop is
// refitted to the target's shape as Sync and Auto Sync refit it (`sync_same::synced_crop`)

/// Two demo photos of the same shape and one of the other (landscape / portrait).
fn photos_by_shape(s: &mut Session) -> (lcc_catalog::PhotoId, lcc_catalog::PhotoId, lcc_catalog::PhotoId) {
    let vis = s.visible_cloned();
    let portrait = |id: &lcc_catalog::PhotoId| s.catalog.photo(*id).is_some_and(|p| p.height > p.width);
    let land: Vec<_> = vis.iter().copied().filter(|id| !portrait(id)).take(2).collect();
    let tall = vis.iter().copied().find(portrait).expect("the demo library has a portrait photo");
    (land[0], land[1], tall)
}

/// The crop of `id` in pixels of its oriented frame.
fn crop_px(s: &Session, id: lcc_catalog::PhotoId) -> lcc_geom::Rect {
    let p = s.catalog.photo(id).unwrap();
    let (w, h) = if p.develop.orientation.swaps_axes() { (p.height, p.width) } else { (p.width, p.height) };
    p.develop.crop.geometry.rect_px(w as f64, h as f64)
}

/// Copy leaves crop and Remove out by default, as Lightroom does, so a plain paste doesn't carry
/// them; ticking them in Paste Selected Settings pastes them onto every target, in one undo step.
#[test]
fn paste_selected_settings_pastes_crop_and_remove_left_out_of_the_copy() {
    let mut s = demo();
    let (a, b, c) = photos_by_shape(&mut s);
    let c2 = s.visible_cloned().into_iter().find(|id| ![a, b, c].contains(id) && s.catalog.photo(*id).is_some_and(|p| p.width > p.height)).unwrap();
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.1, 0.2, 0.7, 0.8]})).unwrap();
    s.execute("spot.add", &json!({"points": [[0.4, 0.5]], "size": 0.05, "mode": "heal"})).unwrap();
    s.execute("develop.set", &json!({"values": {"light.exposure": 0.5}})).unwrap();
    let src = (*s.develop_of(a).unwrap()).clone();
    assert!(!src.spots.is_empty());
    s.execute("develop.copy", &json!({})).unwrap();

    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    let d = s.develop_of(b).unwrap();
    assert_eq!(d.light.exposure, 0.5);
    assert!(d.crop.geometry.is_identity() && d.spots.is_empty(), "a plain paste carries no crop or Remove: {:?}", d.crop);

    let before: Vec<_> = [b, c2].iter().map(|id| (*s.develop_of(*id).unwrap()).clone()).collect();
    let r = s.execute("develop.paste", &json!({"ids": [b.0, c2.0], "groups": ["crop", "spots"]})).unwrap();
    assert_eq!(r["changed"], 2);
    for id in [b, c2] {
        let d = s.develop_of(id).unwrap();
        assert_eq!(d.crop.geometry, src.crop.geometry, "same shape: the same crop");
        assert_eq!(d.spots.len(), src.spots.len());
    }
    s.execute("edit.undo", &json!({})).unwrap();
    for (id, was) in [b, c2].iter().zip(&before) {
        assert_eq!(*s.develop_of(*id).unwrap(), *was, "one undo step takes back the whole paste");
    }
}

/// A crop pasted (or synced) onto a photo of another shape keeps its aspect ratio instead of its
/// normalized rectangle (which would stretch it).
#[test]
fn pasted_crop_keeps_its_aspect_on_a_photo_of_another_shape() {
    let mut s = demo();
    let (a, _, tall) = photos_by_shape(&mut s);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.3, 0.2, 0.3 + 0.6 * 4.0 / 6.0, 0.8]})).unwrap();
    let src = crop_px(&s, a);
    assert!((src.aspect() - 1.0).abs() < 1e-3, "{src:?}");
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("develop.paste", &json!({"ids": [tall.0], "groups": ["crop"]})).unwrap();
    let r = crop_px(&s, tall);
    assert!((r.aspect() - 1.0).abs() < 1e-3, "the square stays square on the portrait photo: {r:?}");
    let d = s.develop_of(tall).unwrap();
    let (w, h) = (s.catalog.photo(tall).unwrap().width as f64, s.catalog.photo(tall).unwrap().height as f64);
    assert!(d.crop.geometry.is_within_image(w, h));

    // Sync Settings with the crop does the same
    s.execute("develop.reset", &json!({"ids": [tall.0]})).unwrap();
    s.execute("library.select", &json!({"ids": [a.0, tall.0], "active": a.0})).unwrap();
    s.execute("develop.sync", &json!({"groups": ["crop"]})).unwrap();
    let synced = crop_px(&s, tall);
    assert!((synced.aspect() - 1.0).abs() < 1e-3, "sync: {synced:?}");
    assert_eq!(synced, r, "Paste and Sync refit a crop the same way");
}

/// An "Original" lock pasted onto a photo of another shape takes that photo's own shape (and becomes its
/// Original lock).
#[test]
fn pasted_original_lock_takes_the_targets_shape() {
    let mut s = demo();
    let (a, _, tall) = photos_by_shape(&mut s);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("crop.aspect", &json!({"aspect": "original"})).unwrap();
    let p = s.catalog.photo(a).unwrap();
    assert_eq!(s.develop_of(a).unwrap().crop.aspect, Some(lcc_develop::Crop::original_aspect(p.width as f64, p.height as f64)));
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("develop.paste", &json!({"ids": [tall.0], "groups": ["crop"]})).unwrap();
    let d = s.develop_of(tall).unwrap();
    let p = s.catalog.photo(tall).unwrap();
    assert_eq!(d.crop.aspect, Some(lcc_develop::Crop::original_aspect(p.width as f64, p.height as f64)));
    assert!(d.crop.geometry.is_identity(), "Original: the whole portrait: {:?}", d.crop);
}

/// The Crop group carries the orientation: the target's shape is the one it has after the paste.
#[test]
fn pasted_crop_refits_to_the_orientation_it_carries() {
    let mut s = demo();
    let (a, b, _) = photos_by_shape(&mut s);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("photo.rotateRight", &json!({"ids": [a.0]})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.2, 0.1, 0.8, 0.5]})).unwrap();
    let src = s.develop_of(a).unwrap();
    assert!(src.orientation.swaps_axes());
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0], "groups": ["crop"]})).unwrap();
    let d = s.develop_of(b).unwrap();
    assert_eq!(d.orientation, src.orientation);
    assert_eq!(d.crop, src.crop, "same shape once turned: the crop as it is");
}

/// Paste Settings from Previous refits a crop as Paste does.
#[test]
fn paste_from_previous_refits_the_crop() {
    let mut s = demo();
    let (a, _, tall) = photos_by_shape(&mut s);
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("crop.set", &json!({"rect": [0.3, 0.2, 0.3 + 0.6 * 4.0 / 6.0, 0.8]})).unwrap();
    s.execute("library.select", &json!({"ids": [tall.0]})).unwrap();
    s.execute("develop.pastePrevious", &json!({"groups": ["crop"]})).unwrap();
    let r = crop_px(&s, tall);
    assert!((r.aspect() - 1.0).abs() < 1e-3, "the square stays square: {r:?}");
}

/// Auto Sync carries a crop as Sync Settings (and Paste) does: one refit.
#[test]
fn auto_sync_refits_a_crop_like_sync() {
    let crop_of_tall = |auto: bool| {
        let mut s = demo();
        let (a, _, tall) = photos_by_shape(&mut s);
        s.execute("library.select", &json!({"ids": [a.0, tall.0], "active": a.0})).unwrap();
        if auto {
            s.execute("develop.autoSync", &json!({"on": true})).unwrap();
        }
        s.execute("crop.set", &json!({"rect": [0.3, 0.2, 0.7, 0.8]})).unwrap();
        if !auto {
            s.execute("develop.sync", &json!({"groups": ["crop"]})).unwrap();
        }
        s.develop_of(tall).unwrap().crop
    };
    assert_eq!(crop_of_tall(true), crop_of_tall(false));
}
