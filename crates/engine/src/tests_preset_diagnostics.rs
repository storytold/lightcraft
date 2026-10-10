//! Procedural preset diagnostics survive native persistence and reach the photo in one undo step.
use crate::Session;
use lightcraft_catalog::{Op, PhotoId};
use lightcraft_develop::Preset;
use serde_json::json;
use std::path::{Path, PathBuf};

const FOREIGN: &str = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Name="Own probe" crs:UUID="own-probe" crs:Exposure2012="1" crs:CameraProfile="OwnSyntheticProfile" crs:OwnPresetMarker="True"><crs:FilterList rdf:parseType="Resource" crs:OwnFilterMarker="True"/></rdf:Description></rdf:RDF></x:xmpmeta>"#;
const FIELDS: [&str; 3] = ["CameraProfile", "FilterList", "OwnPresetMarker"];

fn dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-preset-diagnostics-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
fn imported(s: &mut Session, dir: &Path) -> String {
    let path = dir.join("own.xmp");
    std::fs::write(&path, FOREIGN).unwrap();
    let r = s.execute("preset.import", &json!({"paths":[path]})).unwrap();
    assert_eq!(r["imported"][0]["unmapped"], json!(FIELDS));
    r["imported"][0]["id"].as_str().unwrap().to_string()
}
fn png(path: &Path) {
    let image =
        lightcraft_raster::Rgba8 { width: 2, height: 2, data: vec![[20, 80, 130, 255], [180, 30, 60, 255], [50, 190, 40, 255], [200, 190, 10, 255]] };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}
fn fields(s: &Session, id: PhotoId) -> Vec<String> {
    s.catalog.photo(id).unwrap().unsupported_edits.clone()
}

#[test]
fn imported_preset_round_trip_apply_union_zero_and_undo_redo() {
    let dir = dir("roundtrip");
    let mut source = Session::new();
    let pid = imported(&mut source, &dir);
    assert_eq!(source.presets.iter().find(|p| p.id == pid).unwrap().unsupported_edits, FIELDS);
    let native = dir.join("own.lcpreset");
    source.execute("preset.export", &json!({"ids":[pid],"path":native})).unwrap();
    let mut target = Session::with_demo();
    let id = target.active().unwrap();
    target.commit("Prior diagnostics", Op::SetUnsupportedEdits { id, fields: vec!["OwnPhotoMarker".into()] }).unwrap();
    let before = target.develop_of(id).unwrap();
    let r = target.execute("preset.import", &json!({"paths":[native]})).unwrap();
    assert_eq!(r["imported"][0]["unmapped"], json!(FIELDS));
    let undo_before = target.undo.len();
    target.execute("preset.apply", &json!({"id":pid,"amount":0})).unwrap();
    assert_eq!(target.develop_of(id).unwrap(), before);
    assert_eq!(fields(&target, id), ["OwnPhotoMarker"]);
    assert_eq!(target.undo.len(), undo_before);
    target.execute("preset.apply", &json!({"id":pid,"amount":100})).unwrap();
    let merged = vec!["CameraProfile".to_string(), "FilterList".into(), "OwnPhotoMarker".into(), "OwnPresetMarker".into()];
    assert_eq!(fields(&target, id), merged);
    assert_eq!(target.develop_of(id).unwrap().light.exposure, 1.0);
    assert_eq!(target.undo.len(), undo_before + 1);
    target.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(target.develop_of(id).unwrap(), before);
    assert_eq!(fields(&target, id), ["OwnPhotoMarker"]);
    target.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(fields(&target, id), merged);
    assert_eq!(target.develop_of(id).unwrap().light.exposure, 1.0);
    let undo_before = target.undo.len();
    target.execute("preset.apply", &json!({"id":pid,"amount":100})).unwrap();
    assert_eq!(target.undo.len(), undo_before, "reapplying unchanged preset is a no-op");
    target.execute("preset.apply", &json!({"id":"lc.warm-glow","amount":100})).unwrap();
    assert_eq!(fields(&target, id), merged, "supported partial presets cannot erase prior limitations");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn import_preset_and_import_defaults_report_final_merged_names() {
    for default in [false, true] {
        let dir = dir(if default { "default" } else { "import" });
        let image = dir.join("own.png");
        png(&image);
        std::fs::write(dir.join("own.xmp"), r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.4" crs:OwnPhotoMarker="True"/>"#).unwrap();
        let mut s = Session::new().with_fs();
        // Use a distinct preset filename so the photo sidecar remains intact.
        let preset_dir = dir.join("preset");
        std::fs::create_dir_all(&preset_dir).unwrap();
        let pid = imported(&mut s, &preset_dir);
        if default {
            s.execute("library.preferences", &json!({"import":{"otherPreset":pid}})).unwrap();
        }
        let params = if default { json!({"paths":[image]}) } else { json!({"paths":[image],"preset":pid}) };
        let result = s.execute("library.import", &params).unwrap();
        let id = PhotoId(result["imported"][0].as_u64().unwrap());
        let merged = vec!["CameraProfile".to_string(), "FilterList".into(), "OwnPhotoMarker".into(), "OwnPresetMarker".into()];
        assert_eq!(fields(&s, id), merged);
        assert_eq!(result["unsupportedEdits"][0]["fields"], json!(merged));
        assert_eq!(s.develop_of(id).unwrap().light.exposure, if default { 0.4 } else { 1.0 });
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(s.catalog.photo(id).is_none());
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(fields(&s, id), merged);
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn direct_xmp_parser_and_duplicate_detection_keep_diagnostics() {
    let p = crate::presets::parse_preset_file("own.xmp", FOREIGN.as_bytes()).unwrap().remove(0);
    assert_eq!(p.unsupported_edits, FIELDS);
    let mut s = Session::new();
    let mut old = p.clone();
    old.unsupported_edits.clear();
    assert_eq!(s.add_presets(vec![old]).len(), 1);
    assert_eq!(s.add_presets(vec![p.clone()]).len(), 1, "an earlier lossy import must not suppress repaired diagnostics");
    assert!(s.add_presets(vec![p]).is_empty());
    let old: Preset = serde_json::from_value(json!({"id":"old","name":"Old","group":"User","settings":{"light":{"exposure":1}}})).unwrap();
    assert!(old.unsupported_edits.is_empty());
}

#[test]
fn diagnostics_survive_library_preset_storage_and_invalid_metadata_is_visible() {
    let dir = dir("library");
    let mut s = Session::new().with_fs();
    s.open_library(&dir, false).unwrap();
    let pid = imported(&mut s, &dir);
    s.close_library().unwrap();
    drop(s);
    let mut reopened = Session::new().with_fs();
    reopened.open_library(&dir, false).unwrap();
    assert_eq!(reopened.presets.iter().find(|p| p.id == pid).unwrap().unsupported_edits, FIELDS);
    reopened.close_library().unwrap();
    drop(reopened);
    let path = dir.join("presets.json");
    let mut file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    file["user"][0]["unsupported_edits"] = json!(["FilterList",{"OwnPayload":"discard"}]);
    std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
    let mut reopened = Session::new().with_fs();
    reopened.open_library(&dir, false).unwrap();
    let p = reopened.presets.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(p.unsupported_edits, ["FilterList", lightcraft_develop::unsupported_edits::INVALID_DIAGNOSTICS]);
    reopened.close_library().unwrap();
    drop(reopened);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn preset_update_retains_origin_diagnostics_but_new_native_snapshot_starts_clean() {
    let dir = dir("update-policy");
    let mut s = Session::with_demo();
    let pid = imported(&mut s, &dir);
    s.execute("develop.set", &json!({"control":"light.exposure","value":0.25})).unwrap();
    s.execute("preset.update", &json!({"id":pid,"groups":["light"]})).unwrap();
    let updated = s.presets.iter().find(|p| p.id == pid).unwrap();
    assert_eq!(updated.settings["light"]["exposure"], json!(0.25));
    assert_eq!(updated.unsupported_edits, FIELDS, "editing native controls does not establish support for the foreign preset's missing settings");
    let native = crate::presets::to_lcpreset(std::slice::from_ref(updated));
    let imported_again = crate::presets::parse_preset_file("own.lcpreset", native.as_bytes()).unwrap();
    assert_eq!(imported_again[0].unsupported_edits, FIELDS);

    let id = s.active().unwrap();
    s.commit("Prior photo diagnostics", Op::SetUnsupportedEdits { id, fields: vec!["OwnPhotoMarker".into()] }).unwrap();
    let result = s.execute("preset.create", &json!({"name":"Own native snapshot","groups":["light"]})).unwrap();
    let created = s.presets.iter().find(|p| p.id == result["id"].as_str().unwrap()).unwrap();
    assert!(created.unsupported_edits.is_empty(), "a new native snapshot contains only the supported settings it explicitly captures");
    assert_eq!(fields(&s, id), ["OwnPhotoMarker"], "creating a preset does not resolve the source photo's diagnostics");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn full_native_sidecar_replaces_default_warnings_only_when_own_diagnostics_are_present() {
    for explicit_empty in [false, true] {
        let dir = dir(if explicit_empty { "full-native-empty" } else { "full-native-legacy" });
        let image = dir.join("own.png");
        png(&image);
        let mut native = lightcraft_develop::DevelopSettings::default();
        native.light.exposure = 0.4;
        let settings = serde_json::to_string(&native).unwrap().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let owned = if explicit_empty { "<lc:unsupportedEdits>[]</lc:unsupportedEdits>" } else { "" };
        let packet = format!(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:lc="http://ns.lightcraft.app/lc/1.0/"><lc:settings>{settings}</lc:settings>{owned}</rdf:Description>"#
        );
        std::fs::write(dir.join("own.xmp"), packet).unwrap();
        let preset_dir = dir.join("preset");
        std::fs::create_dir_all(&preset_dir).unwrap();
        let mut s = Session::new().with_fs();
        let pid = imported(&mut s, &preset_dir);
        s.execute("library.preferences", &json!({"import":{"otherPreset":pid}})).unwrap();
        let result = s.execute("library.import", &json!({"paths":[image]})).unwrap();
        let id = PhotoId(result["imported"][0].as_u64().unwrap());
        assert_eq!(s.develop_of(id).unwrap().light.exposure, 0.4, "the full native snapshot replaces the default preset's numeric settings");
        let expected: Vec<String> = if explicit_empty { Vec::new() } else { FIELDS.iter().map(|name| name.to_string()).collect() };
        assert_eq!(fields(&s, id), expected);
        if explicit_empty {
            assert!(result.get("unsupportedEdits").is_none(), "an explicit empty owned list resolves the import-default diagnostic provenance");
        } else {
            assert_eq!(
                result["unsupportedEdits"][0]["fields"],
                json!(expected),
                "legacy absence retains prior diagnostics under the existing conservative policy"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn warned_preset_undo_redo_returns_to_its_photo_even_without_a_numeric_change() {
    for change_settings in [false, true] {
        let mut s = Session::with_demo();
        let a = s.active().unwrap();
        let b = s.visible_cloned().into_iter().find(|id| *id != a && !s.catalog.photo(*id).unwrap().is_edited()).unwrap();
        s.execute("library.select", &json!({"ids": [a.0], "active": a.0})).unwrap();
        let original = s.develop_of(a).unwrap();
        let other = s.catalog.photo(b).unwrap().clone();
        let exposure = original.light.exposure + if change_settings { 0.5 } else { 0.0 };
        let preset: Preset = serde_json::from_value(json!({
            "id": "user.own-warning-navigation", "name": "Own warning navigation", "group": "User",
            "settings": {"light": {"exposure": exposure}}, "unsupported_edits": ["OwnUnmappedSetting"]
        }))
        .unwrap();
        s.presets.push(preset);
        let undo_before = s.undo.len();
        s.execute("preset.apply", &json!({"id": "user.own-warning-navigation", "amount": 100})).unwrap();
        assert_eq!(s.undo.len(), undo_before + 1, "numeric settings and diagnostics belong to one undo step");
        assert_eq!(s.develop_of(a).unwrap().light.exposure, exposure);
        assert_eq!(fields(&s, a), ["OwnUnmappedSetting"]);

        s.execute("library.select", &json!({"ids": [b.0], "active": b.0})).unwrap();
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.active(), Some(a), "the diagnostics operation must still identify the one changed photo");
        assert_eq!(s.develop_of(a).unwrap(), original);
        assert!(fields(&s, a).is_empty());

        s.execute("library.select", &json!({"ids": [b.0], "active": b.0})).unwrap();
        s.execute("edit.redo", &json!({})).unwrap();
        assert_eq!(s.active(), Some(a), "redo must also show the affected photo");
        assert_eq!(s.develop_of(a).unwrap().light.exposure, exposure);
        assert_eq!(fields(&s, a), ["OwnUnmappedSetting"]);
        assert_eq!(s.catalog.photo(b).unwrap().as_ref(), other.as_ref(), "navigation must not edit the other photo");
    }
}
