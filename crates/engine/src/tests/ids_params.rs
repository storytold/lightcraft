//! `ids` that aren't photo ids are an error, never "the selection": a command that fell back to the
//! selection on `{"ids": 3}` acted on photos the call didn't name.

use lightcraft_catalog::{AlbumId, PhotoId};
use serde_json::{Value, json};

use crate::Session;

/// A demo session with the first two photos selected, the third active elsewhere in the grid, and
/// an album holding the selection.
fn session() -> (Session, Vec<PhotoId>, AlbumId) {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [vis[0].0, vis[1].0], "active": vis[0].0})).unwrap();
    let r = s.execute("album.create", &json!({"name": "Trip", "addSelected": true})).unwrap();
    let album = AlbumId(r["id"].as_u64().unwrap());
    (s, vis.to_vec(), album)
}

const BAD_IDS: [fn() -> Value; 5] = [|| json!(3), || json!("3"), || json!(["3"]), || json!([3, -1]), || json!({"id": 3})];

#[test]
fn album_commands_refuse_ids_that_are_not_photo_ids() {
    let (mut s, vis, album) = session();
    let members = |s: &Session| s.catalog.album(album).unwrap().photos.clone();
    let before = members(&s);
    assert_eq!(before.len(), 2);
    for bad in BAD_IDS.map(|f| f()) {
        for cmd in ["album.removePhotos", "album.addPhotos"] {
            let e = s.execute(cmd, &json!({"id": album.0, "ids": bad})).unwrap_err();
            assert!(e.to_string().contains("ids"), "{cmd} {bad}: {e}");
            assert_eq!(members(&s), before, "{cmd} {bad} changed the album");
        }
        assert!(s.execute("album.toggleTarget", &json!({"ids": bad})).is_err(), "album.toggleTarget {bad}");
    }
    // well-formed ids still work, and no ids still means the selection
    s.execute("album.removePhotos", &json!({"id": album.0, "ids": [vis[1].0]})).unwrap();
    assert_eq!(members(&s), vec![vis[0]]);
    s.execute("album.addPhotos", &json!({"id": album.0, "ids": null})).unwrap();
    assert_eq!(members(&s).len(), 2);
}

#[test]
fn develop_set_refuses_ids_that_are_not_photo_ids() {
    let (mut s, vis, _) = session();
    let exposure = |s: &Session, id: PhotoId| s.develop_of(id).unwrap().light.exposure;
    let before: Vec<f64> = vis.iter().map(|id| exposure(&s, *id)).collect();
    for bad in BAD_IDS.map(|f| f()) {
        let e = s.execute("develop.set", &json!({"control": "light.exposure", "value": 2.5, "ids": bad})).unwrap_err();
        assert!(e.to_string().contains("ids"), "{bad}: {e}");
        assert_eq!(vis.iter().map(|id| exposure(&s, *id)).collect::<Vec<_>>(), before, "{bad} edited a photo");
    }
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 2.5, "ids": [vis[2].0]})).unwrap();
    assert_eq!((exposure(&s, vis[0]), exposure(&s, vis[2])), (before[0], 2.5));
}

#[test]
fn library_select_refuses_ids_that_are_not_photo_ids() {
    let (mut s, vis, _) = session();
    let before = s.selection.ids.clone();
    for bad in BAD_IDS.map(|f| f()) {
        assert!(s.execute("library.select", &json!({"ids": bad})).is_err(), "{bad}");
        assert_eq!(s.selection.ids, before, "{bad} changed the selection");
    }
    s.execute("library.select", &json!({"ids": [vis[2].0]})).unwrap();
    assert_eq!(s.selection.ids, vec![vis[2]]);
}

/// The same for a single `id`: a value that isn't a photo id used to mean the active photo
/// (`photo.relink` then relinked the active photo to the file, `photo.removeRegion` removed its face
/// box), or, for `denoise.cancel`, every photo. (The stack commands share the helper; `in_stack`
/// keeps them disabled on a photo outside a stack, so they aren't called here.)
#[test]
fn single_photo_commands_refuse_an_id_that_is_not_a_photo_id() {
    let (mut s, vis, _) = session();
    let active = s.active().unwrap();
    let mut meta = s.catalog.photo(active).unwrap().meta.clone();
    meta.regions = vec![lightcraft_meta::Region {
        rect: lightcraft_geom::Rect { x0: 0.1, y0: 0.1, x1: 0.3, y1: 0.3 },
        kind: lightcraft_meta::RegionKind::Face,
        name: None,
        description: None,
    }];
    s.commit("setup", lightcraft_catalog::Op::SetMeta { id: active, meta: Box::new(meta) }).unwrap();
    let source = |s: &Session| format!("{:?}", s.catalog.photo(active).unwrap().source);
    let before = source(&s);
    let commits = s.commits();
    for bad in [json!("5"), json!(-1), json!(1.5), json!([vis[2].0]), json!({"id": 1})] {
        for (cmd, mut params) in [
            ("photo.relink", json!({"path": "/elsewhere/other.jpg"})),
            ("photo.removeRegion", json!({"index": 0})),
            ("photo.setRegion", json!({"index": 0, "rect": {"x0": 0.2, "y0": 0.2, "x1": 0.4, "y1": 0.4}})),
            ("faces.setName", json!({"index": 0, "name": "Ann"})),
            ("library.findSimilar", json!({})),
            ("denoise.cancel", json!({})),
            ("photo.inspect", json!({})),
        ] {
            params["id"] = bad.clone();
            let e = s.execute(cmd, &params).unwrap_err();
            assert!(e.to_string().contains("`id`"), "{cmd} {bad}: {e}");
        }
    }
    assert_eq!(s.commits(), commits, "nothing was changed");
    assert_eq!(source(&s), before);
    assert_eq!(s.catalog.photo(active).unwrap().meta.regions.len(), 1);
}

/// Mask ids and spot indexes the same way: `mask.delete {"id": "1"}` deleted the selected mask, and an
/// id past `u32` wrapped (`4294967297 as u32` is 1) onto another mask.
#[test]
fn mask_and_spot_commands_refuse_ids_that_are_not_ids() {
    let (mut s, _, _) = session();
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("spot.add", &json!({"mode": "heal", "points": [[0.4, 0.5]], "size": 0.02, "source": [0.1, 0.0]})).unwrap();
    let state = |s: &Session| {
        let d = s.develop_of(s.active().unwrap()).unwrap();
        (d.masks.iter().map(|m| (m.id, m.name.clone(), m.visible)).collect::<Vec<_>>(), d.spots.len())
    };
    let before = state(&s);
    assert_eq!((before.0.len(), before.1), (2, 1));
    let first = before.0[0].0;
    let commits = s.commits();
    for bad in [json!(u64::from(u32::MAX) + 1 + u64::from(first)), json!("1"), json!(-1), json!(1.5), json!([first])] {
        for (cmd, params) in [
            ("mask.delete", json!({})),
            ("mask.rename", json!({"name": "renamed"})),
            ("mask.visible", json!({"visible": false})),
            ("mask.select", json!({})),
        ] {
            let mut params = params;
            params["id"] = bad.clone();
            assert!(s.execute(cmd, &params).is_err(), "{cmd} {bad}");
        }
        for cmd in ["spot.delete", "spot.select"] {
            assert!(s.execute(cmd, &json!({"index": bad})).is_err(), "{cmd} {bad}");
        }
    }
    assert_eq!(s.commits(), commits, "nothing was changed");
    assert_eq!(state(&s), before);
    // well-formed ids and a null still work
    s.execute("mask.delete", &json!({"id": first})).unwrap();
    s.execute("spot.select", &json!({"index": null})).unwrap();
    assert_eq!(state(&s).0.len(), 1);
}

/// `photo.label` without a label, or with one that isn't text, cleared the colour label of every
/// selected photo (a missing `label` read as `none`); now it is an error and only `none` or null clear.
#[test]
fn photo_label_refuses_a_missing_or_non_text_label() {
    let (mut s, vis, _) = session();
    s.execute("photo.label", &json!({"label": "red"})).unwrap();
    let labels = |s: &Session| vis.iter().take(2).map(|id| s.catalog.photo(*id).unwrap().label).collect::<Vec<_>>();
    let before = labels(&s);
    assert!(before.iter().all(Option::is_some));
    for params in [json!({}), json!({"label": 3}), json!({"label": true}), json!({"label": ["red"]}), json!({"label": "crimson"})] {
        assert!(s.execute("photo.label", &params).is_err(), "{params}");
        assert_eq!(labels(&s), before, "{params} changed the labels");
    }
    s.execute("photo.label", &json!({"label": null})).unwrap();
    assert!(labels(&s).iter().all(Option::is_none));
    s.execute("photo.label", &json!({"label": "green"})).unwrap();
    s.execute("photo.label", &json!({"label": "none"})).unwrap();
    assert!(labels(&s).iter().all(Option::is_none));
}

/// The mask-effect commands and a mask's `component` the same way: `mask.saveEffect {"id": "2"}`
/// saved the selected mask's sliders, and `mask.update {"component": "1"}` reshaped component 0.
#[test]
fn mask_effects_and_components_refuse_values_that_are_not_ids() {
    let (mut s, _, _) = session();
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("mask.saveEffect", &json!({"name": "Kept"})).unwrap();
    let state = |s: &Session| (format!("{:?}", s.develop_of(s.active().unwrap()).unwrap().masks), s.mask_effects.len());
    let before = state(&s);
    let commits = s.commits();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    let shape = serde_json::to_value(&d.masks.last().unwrap().components[0].shape).unwrap();
    for bad in [json!("1"), json!(-1), json!(1.5), json!([1]), json!(u64::from(u32::MAX) + 2)] {
        for (cmd, params) in [
            ("mask.applyEffect", json!({"name": "Kept"})),
            ("mask.saveEffect", json!({"name": "New"})),
            ("mask.updateEffect", json!({"name": "Kept"})),
        ] {
            let mut params = params;
            params["id"] = bad.clone();
            let e = s.execute(cmd, &params).unwrap_err();
            assert!(e.to_string().contains("`id`"), "{cmd} {bad}: {e}");
        }
        if bad.is_u64() {
            continue; // a well-formed index past the end is "no such component", not a type error
        }
        for (cmd, params) in
            [("mask.update", json!({"shape": shape})), ("mask.component", json!({"action": "invert"})), ("mask.refineDetail", json!({}))]
        {
            let mut params = params;
            params["component"] = bad.clone();
            let e = s.execute(cmd, &params).unwrap_err();
            assert!(e.to_string().contains("`component`"), "{cmd} {bad}: {e}");
        }
    }
    assert_eq!(s.commits(), commits, "nothing was changed");
    assert_eq!(state(&s), before);
}
