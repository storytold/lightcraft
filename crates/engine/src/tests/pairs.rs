//! RAW+JPEG pairing: synced culling, delete stays single-file, the grid can show one member.

use std::path::PathBuf;

use lightcraft_catalog::{MediaKind, Op, Photo, Source};
use serde_json::json;

use crate::Session;
use crate::media::SourceRef;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-pairs-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn add(s: &mut Session, path: &str, kind: MediaKind) -> lightcraft_catalog::PhotoId {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let id = s.catalog.alloc_photo_id();
    let mut p =
        Photo::new(id, Source::File { path: path.to_string() }, name, if kind == MediaKind::Raw { "RAF" } else { "JPEG" }, 32, 24, "2026-01-01");
    p.kind = kind;
    p.captured = Some("2026-04-01T10:00:00".into());
    p.meta.camera = "Fujifilm X-S10".into();
    p.develop = std::sync::Arc::new(p.camera_defaults());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn rate_of(s: &Session, id: lightcraft_catalog::PhotoId) -> u8 {
    s.catalog.photo(id).unwrap().rating
}

#[test]
fn sync_off_rates_one_file_and_sync_on_is_one_undo_step() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    assert_eq!(s.execute("photo.joinPairs", &json!({})).unwrap()["joined"], 1);
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4})).unwrap();
    assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (4, 0));

    s.execute("library.pairPreferences", &json!({"syncPairMetadata": true, "syncPairLabels": false})).unwrap();
    s.execute("photo.rate", &json!({"rating": 2})).unwrap();
    assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (2, 2));
    s.execute("photo.label", &json!({"label": "red"})).unwrap();
    assert!(s.catalog.photo(jpeg).unwrap().label.is_none(), "labels stay put until syncPairLabels");
    s.execute("edit.undo", &json!({})).unwrap();
    s.execute("photo.pick", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(jpeg).unwrap().flag, lightcraft_catalog::Flag::Pick);
    s.execute("library.pairPreferences", &json!({"syncPairLabels": true})).unwrap();
    s.execute("photo.label", &json!({"label": "green"})).unwrap();
    assert_eq!(s.catalog.photo(jpeg).unwrap().label, s.catalog.photo(raw).unwrap().label);

    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(jpeg).unwrap().label.is_none());
    assert!(s.catalog.photo(raw).unwrap().label.is_none());
    assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (2, 2));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.photo(jpeg).unwrap().flag, lightcraft_catalog::Flag::None);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (4, 0));
}

#[test]
fn delete_removes_one_catalog_photo_and_leaves_both_files() {
    let dir = temp_dir("delete");
    let raw_path = dir.join("DSCF0001.RAF");
    let jpeg_path = dir.join("DSCF0001.JPG");
    std::fs::write(&raw_path, b"raw").unwrap();
    std::fs::write(&jpeg_path, b"jpeg").unwrap();
    let mut s = Session::new();
    let raw = add(&mut s, &raw_path.to_string_lossy(), MediaKind::Raw);
    let jpeg = add(&mut s, &jpeg_path.to_string_lossy(), MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    s.execute("photo.delete", &json!({})).unwrap();
    assert!(s.catalog.photo(raw).unwrap().deleted);
    assert!(!s.catalog.photo(jpeg).unwrap().deleted);
    assert!(s.catalog.pair_of(raw).is_some());
    s.paired_culling = true;
    let vis: Vec<_> = s.visible().to_vec();
    assert!(vis.contains(&jpeg));
    assert!(!vis.contains(&raw));
    s.execute("photo.deletePermanently", &json!({"ids": [raw.0]})).unwrap();
    assert!(s.catalog.photo(raw).is_none());
    assert!(s.catalog.photo(jpeg).is_some());
    assert!(s.catalog.pair_of(jpeg).is_none());
    assert!(raw_path.is_file() && jpeg_path.is_file());
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(s.catalog.photo(raw).unwrap().deleted);
    assert_eq!(s.catalog.pair_partner(raw), Some(jpeg));
    s.execute("edit.undo", &json!({})).unwrap();
    assert!(!s.catalog.photo(raw).unwrap().deleted);
    assert_eq!(s.catalog.pair_partner(raw), Some(jpeg));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn auto_write_uses_two_sidecar_names() {
    let dir = temp_dir("xmp");
    let lib = temp_dir("xmp-lib");
    let raw_path = dir.join("DSCF0001.RAF");
    let jpeg_path = dir.join("DSCF0001.JPG");
    std::fs::write(&raw_path, b"raw").unwrap();
    std::fs::write(&jpeg_path, b"jpeg").unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let raw = add(&mut s, &raw_path.to_string_lossy(), MediaKind::Raw);
    let jpeg = add(&mut s, &jpeg_path.to_string_lossy(), MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.xmpPreferences", &json!({"autoWrite": true})).unwrap();
    s.execute("library.pairPreferences", &json!({"syncPairMetadata": true})).unwrap();
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 5})).unwrap();
    let stem = dir.join("DSCF0001.xmp");
    let full = dir.join("DSCF0001.JPG.xmp");
    let stem_txt = std::fs::read_to_string(&stem).unwrap();
    let full_txt = std::fs::read_to_string(&full).unwrap();
    assert_eq!(lightcraft_meta::parse_xmp(&stem_txt).unwrap().metadata.rating, Some(5), "{stem_txt}");
    assert_eq!(lightcraft_meta::parse_xmp(&full_txt).unwrap().metadata.rating, Some(5), "{full_txt}");
    assert_ne!(stem, full);
    let _ = (jpeg, std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&lib));
}

#[test]
fn reopening_paired_culling_preserves_the_active_member() {
    for case in ["raw", "both-jpeg", "both-raw", "active-only-raw"] {
        let lib = temp_dir(&format!("focused-view-{case}"));
        let (raw, jpeg, selection) = {
            let mut s = Session::new().with_fs();
            s.open_library(&lib, false).unwrap();
            let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
            let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
            s.execute("photo.joinPairs", &json!({})).unwrap();
            s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
            s.execute("library.select", &json!({"ids": [jpeg.0]})).unwrap();
            s.execute("photo.pairFocus", &json!({"which": "raw"})).unwrap();
            match case {
                "both-jpeg" => {
                    s.execute("photo.pairFocus", &json!({"which": "jpeg"})).unwrap();
                    s.execute("library.select", &json!({"ids": [raw.0, jpeg.0], "active": jpeg.0})).unwrap();
                }
                "both-raw" => {
                    s.execute("library.select", &json!({"ids": [raw.0, jpeg.0], "active": raw.0})).unwrap();
                }
                "active-only-raw" => {
                    s.execute("library.select", &json!({"ids": [], "active": raw.0})).unwrap();
                }
                _ => {}
            }
            let selection = s.selection.clone();
            s.save_view();
            s.close_library().unwrap();
            (raw, jpeg, selection)
        };
        let mut reopened = Session::new().with_fs();
        reopened.open_library(&lib, false).unwrap();
        assert!(reopened.paired_culling);
        assert_eq!(reopened.selection, selection, "{case}");
        let (shown, hidden) = if case == "both-jpeg" { (jpeg, raw) } else { (raw, jpeg) };
        assert!(reopened.visible().contains(&shown), "{case}: the restored active photo must be visible");
        assert!(!reopened.visible().contains(&hidden), "{case}");
        reopened.close_library().unwrap();
        let _ = std::fs::remove_dir_all(&lib);
    }
}

#[test]
fn single_file_import_and_ambiguous_stems_do_not_pair() {
    let dir = temp_dir("import");
    let lib = temp_dir("import-lib");
    let png = dir.join("only.png");
    let (w, h) = (8usize, 8usize);
    let data: Vec<[u8; 4]> = vec![[10, 20, 30, 255]; w * h];
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(&png, bytes).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    s.execute("library.import", &json!({"paths": [png.to_string_lossy()]})).unwrap();
    let found = s.execute("photo.pairCandidates", &json!({})).unwrap();
    assert!(found["pairs"].as_array().unwrap().is_empty());
    assert_eq!(s.catalog.pairs().count(), 0);

    let raw = add(&mut s, &dir.join("DSCF0006.RAF").to_string_lossy(), MediaKind::Raw);
    let _j1 = add(&mut s, &dir.join("DSCF0006.JPG").to_string_lossy(), MediaKind::Image);
    let _j2 = add(&mut s, &dir.join("DSCF0006.JPEG").to_string_lossy(), MediaKind::Image);
    let again = s.execute("photo.pairCandidates", &json!({})).unwrap();
    assert!(again["pairs"].as_array().unwrap().is_empty());
    assert!(again["ambiguous"].as_array().unwrap().iter().any(|a| a["reason"] == "more than one JPEG"));
    assert_eq!(s.execute("photo.joinPairs", &json!({"ids": [raw.0]})).unwrap()["joined"], 0);
    let _ = (std::fs::remove_dir_all(&dir), std::fs::remove_dir_all(&lib));
}

#[test]
fn paired_culling_shows_the_jpeg_unless_stacked_or_focused_on_the_raw() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    let both = s.visible().to_vec();
    assert!(both.contains(&raw) && both.contains(&jpeg));
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    let one = s.visible().to_vec();
    assert!(one.contains(&jpeg));
    assert!(!one.contains(&raw));

    s.execute("photo.pairFocus", &json!({"ids": [jpeg.0], "which": "raw"})).unwrap();
    let swapped = s.visible().to_vec();
    assert!(swapped.contains(&raw) && !swapped.contains(&jpeg));
    assert_eq!(s.selection.active, Some(raw));
    let jpeg_look = s.catalog.photo(jpeg).unwrap().develop.clone();
    let mut raw_look = (*s.catalog.photo(raw).unwrap().develop).clone();
    raw_look.light.exposure = 1.0;
    s.catalog
        .apply(Op::SetDevelop {
            id: raw,
            settings: std::sync::Arc::new(raw_look),
            label: "Exposure".into(),
            edited: Some("2026-04-01T10:00:01".into()),
        })
        .unwrap();
    assert_eq!(s.catalog.photo(jpeg).unwrap().develop, jpeg_look);

    let op = s.catalog.group_ops(raw, &[jpeg], false).unwrap();
    s.catalog.apply(op).unwrap();
    s.execute("photo.pairFocus", &json!({"ids": [jpeg.0], "which": "raw"})).unwrap();
    let stacked = s.visible().to_vec();
    assert!(stacked.contains(&raw) && stacked.contains(&jpeg));
}

#[test]
fn grid_thumb_of_an_unedited_raw_uses_the_jpeg_file() {
    let dir = temp_dir("thumb");
    let raw_path = dir.join("DSCF0001.RAF");
    let jpeg_path = dir.join("DSCF0001.JPG");
    std::fs::write(&raw_path, b"raw").unwrap();
    std::fs::write(&jpeg_path, b"jpeg").unwrap();
    let mut s = Session::new().with_fs();
    let raw = add(&mut s, &raw_path.to_string_lossy(), MediaKind::Raw);
    let _jpeg = add(&mut s, &jpeg_path.to_string_lossy(), MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.paired_culling = true;
    s.execute("photo.pairFocus", &json!({"ids": [raw.0], "which": "raw"})).unwrap();
    let job = s.thumb_job(raw, 256).unwrap();
    match &job.source {
        SourceRef::File { path, .. } => assert!(path.ends_with("DSCF0001.JPG"), "{path}"),
        _ => panic!("expected the JPEG file"),
    }
    assert_eq!(job.photo, raw);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn relative_rating_copies_the_selected_result_to_the_unselected_partner() {
    for (command, selected, partner, expected) in
        [("photo.increaseRating", 0, 4, 1), ("photo.decreaseRating", 4, 0, 3), ("photo.increaseRating", 5, 0, 5), ("photo.decreaseRating", 0, 5, 0)]
    {
        let mut s = Session::new();
        let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
        let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
        s.catalog.apply(Op::SetRating { id: raw, rating: partner }).unwrap();
        s.catalog.apply(Op::SetRating { id: jpeg, rating: selected }).unwrap();
        s.execute("photo.joinPairs", &json!({})).unwrap();
        s.execute("library.pairPreferences", &json!({"pairedCulling": true, "syncPairMetadata": true})).unwrap();
        s.execute("library.select", &json!({"ids": [jpeg.0]})).unwrap();
        s.execute(command, &json!({})).unwrap();
        assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (expected, expected), "{command}");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (partner, selected));
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (expected, expected));
    }
}

#[test]
fn relative_flags_copy_the_selected_result_to_the_unselected_partner() {
    use lightcraft_catalog::Flag;
    for (command, selected, partner, expected) in [
        ("photo.cycleFlag", Flag::None, Flag::Reject, Flag::Pick),
        ("photo.increaseFlag", Flag::Pick, Flag::Reject, Flag::Pick),
        ("photo.decreaseFlag", Flag::Reject, Flag::Pick, Flag::Reject),
    ] {
        let mut s = Session::new();
        let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
        let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
        s.catalog.apply(Op::SetFlag { id: raw, flag: partner }).unwrap();
        s.catalog.apply(Op::SetFlag { id: jpeg, flag: selected }).unwrap();
        s.execute("photo.joinPairs", &json!({})).unwrap();
        s.execute("library.pairPreferences", &json!({"syncPairMetadata": true})).unwrap();
        s.execute("library.select", &json!({"ids": [jpeg.0]})).unwrap();
        s.execute(command, &json!({})).unwrap();
        assert_eq!(s.catalog.photo(raw).unwrap().flag, expected, "{command}");
        assert_eq!(s.catalog.photo(jpeg).unwrap().flag, expected, "{command}");
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.catalog.photo(raw).unwrap().flag, partner);
        assert_eq!(s.catalog.photo(jpeg).unwrap().flag, selected);
    }
}

#[test]
fn relative_rating_with_both_selected_adjusts_each_once() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.catalog.apply(Op::SetRating { id: raw, rating: 4 }).unwrap();
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.pairPreferences", &json!({"syncPairMetadata": true})).unwrap();
    s.execute("library.select", &json!({"ids": [raw.0, jpeg.0]})).unwrap();
    s.execute("photo.increaseRating", &json!({"ids": [raw.0, jpeg.0]})).unwrap();
    assert_eq!((rate_of(&s, raw), rate_of(&s, jpeg)), (5, 1));
}

#[test]
fn enabling_paired_culling_keeps_a_raw_selected_when_its_jpeg_is_filtered_out() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let _jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.filter", &json!({"kind": "raw"})).unwrap();
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    assert_eq!(s.visible(), &[raw]);
    assert_eq!(s.selection.active, Some(raw));
    assert_eq!(s.selection.ids, vec![raw]);
}

#[test]
fn pair_focus_refuses_a_counterpart_excluded_by_the_view() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    s.execute("library.filter", &json!({"kind": "image"})).unwrap();
    s.execute("library.select", &json!({"ids": [jpeg.0]})).unwrap();
    let before = s.selection.clone();
    let error = s.execute("photo.pairFocus", &json!({"which": "raw"})).unwrap_err();
    assert!(error.to_string().contains("filter"), "{error}");
    assert_eq!(s.selection, before);
    assert_eq!(s.visible(), &[jpeg]);
    assert!(!s.pair_show_raw.contains(&s.catalog.pair_of(raw).unwrap().id));
}

#[test]
fn joining_in_paired_culling_selects_the_visible_jpeg() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    s.execute("photo.joinPairs", &json!({})).unwrap();
    assert_eq!(s.visible(), &[jpeg]);
    assert_eq!(s.selection.active, Some(jpeg));
}

#[test]
fn joining_a_new_pair_preserves_existing_raw_focus() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    s.execute("photo.pairFocus", &json!({"ids": [raw.0], "which": "raw"})).unwrap();
    let new_raw = add(&mut s, "/pics/DSCF0002.RAF", MediaKind::Raw);
    let _new_jpeg = add(&mut s, "/pics/DSCF0002.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({"ids": [new_raw.0]})).unwrap();
    assert_eq!(s.selection.active, Some(raw));
    assert!(s.visible().contains(&raw));
    assert!(!s.visible().contains(&jpeg));
}

#[test]
fn joining_a_new_pair_preserves_a_selected_stacked_raw() {
    let mut s = Session::new();
    let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
    let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({})).unwrap();
    let stack = s.catalog.group_ops(raw, &[jpeg], false).unwrap();
    s.catalog.apply(stack).unwrap();
    s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
    s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
    let new_raw = add(&mut s, "/pics/DSCF0002.RAF", MediaKind::Raw);
    let _new_jpeg = add(&mut s, "/pics/DSCF0002.JPG", MediaKind::Image);
    s.execute("photo.joinPairs", &json!({"ids": [new_raw.0]})).unwrap();
    assert_eq!(s.selection.active, Some(raw));
    assert!(s.visible().contains(&raw) && s.visible().contains(&jpeg));
}

#[test]
fn pair_navigation_finishes_the_previous_edit_and_clears_its_mask_and_spot() {
    for action in ["focus", "preferences", "join"] {
        let mut s = Session::new();
        let raw = add(&mut s, "/pics/DSCF0001.RAF", MediaKind::Raw);
        let jpeg = add(&mut s, "/pics/DSCF0001.JPG", MediaKind::Image);
        if action != "join" {
            s.execute("photo.joinPairs", &json!({})).unwrap();
        }
        if action != "preferences" {
            s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
        }
        if action == "focus" {
            s.execute("library.select", &json!({"ids": [jpeg.0]})).unwrap();
            s.execute("photo.pairFocus", &json!({"which": "raw"})).unwrap();
        } else {
            s.execute("library.select", &json!({"ids": [raw.0]})).unwrap();
        }
        s.begin_interaction("Exposure").unwrap();
        let mut settings = (*s.develop_of(raw).unwrap()).clone();
        settings.light.exposure = 1.0;
        s.set_develop(raw, settings, "Exposure").unwrap();
        s.active_mask = Some(7);
        s.active_spot = Some(3);
        match action {
            "focus" => {
                s.execute("photo.pairFocus", &json!({"which": "jpeg"})).unwrap();
            }
            "preferences" => {
                s.execute("library.pairPreferences", &json!({"pairedCulling": true})).unwrap();
            }
            _ => {
                s.execute("photo.joinPairs", &json!({})).unwrap();
            }
        }
        assert_eq!(s.selection.active, Some(jpeg), "{action}");
        assert!(s.interaction.is_none(), "{action}: edit must have an undo step");
        assert_eq!(s.active_mask, None, "{action}");
        assert_eq!(s.active_spot, None, "{action}");
        assert_eq!(s.develop_of(raw).unwrap().light.exposure, 1.0);
        assert_eq!(s.develop_of(jpeg).unwrap().light.exposure, 0.0);
        if action == "join" {
            s.execute("edit.undo", &json!({})).unwrap();
            assert!(s.catalog.pair_of(raw).is_none());
            assert_eq!(s.develop_of(raw).unwrap().light.exposure, 1.0);
        }
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.develop_of(raw).unwrap().light.exposure, 0.0, "{action}");
    }
}
