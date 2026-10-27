//! Mask effect presets end to end: managing them (rename, update, duplicate, delete, stars,
//! groups), applying (several masks, other photos, new masks remembering the effect), saving
//! some sliders, and importing Lightroom / Camera Raw local presets from folders and zips.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-effects-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A Lightroom local preset (synthetic values) in the Lua shape Lightroom writes.
fn lr_local(title: &str, value: &str) -> String {
    format!(
        "s = {{\n\tid = \"0\",\n\tinternalName = \"{title}\",\n\ttitle = \"{title}\",\n\ttype = \"LocalizedAdjustmentPreset\",\n\tvalue = {{\n{value}\n\t}},\n\tversion = 0,\n}}"
    )
}

const ZERO: &str = "\t\texposure2012 = 0,\n\t\ttoningHue = 240,\n\t\ttoningSaturation = 0,";

fn names(v: &Value) -> Vec<String> {
    v["effects"].as_array().unwrap().iter().map(|e| e["name"].as_str().unwrap().to_string()).collect()
}

fn mask_adjust(s: &Session, id: u32) -> lightcraft_develop::LocalAdjustments {
    s.develop_of(s.active().unwrap()).unwrap().masks.iter().find(|m| m.id == id).unwrap().adjust
}

fn with_effect(s: &mut Session, name: &str, values: Value) {
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    s.execute("mask.adjust", &json!({"values": values})).unwrap();
    s.execute("mask.saveEffect", &json!({"name": name})).unwrap();
    s.execute("mask.delete", &json!({})).unwrap();
}

#[test]
fn managing_effects() {
    let mut s = Session::with_demo();
    with_effect(&mut s, "Lift", json!({"shadows": 30}));
    with_effect(&mut s, "Dark", json!({"exposure": -1}));
    // rename (not onto another one's name)
    assert!(s.execute("mask.renameEffect", &json!({"name": "lift", "to": "dark"})).is_err());
    assert!(s.execute("mask.renameEffect", &json!({"name": "nope", "to": "x"})).is_err());
    let v = s.execute("mask.renameEffect", &json!({"name": "lift", "to": "Lift Shadows"})).unwrap();
    assert_eq!(names(&v), ["Lift Shadows", "Dark"]);
    // duplicate: "<name> copy", then "<name> copy 2"
    let v = s.execute("mask.duplicateEffect", &json!({"name": "Dark"})).unwrap();
    assert_eq!(v["name"], "Dark copy");
    let v = s.execute("mask.duplicateEffect", &json!({"name": "Dark"})).unwrap();
    assert_eq!(v["name"], "Dark copy 2");
    assert!(s.execute("mask.duplicateEffect", &json!({"name": "Dark", "to": "dark copy"})).is_err());
    // stars and groups
    s.execute("mask.favoriteEffect", &json!({"name": "Dark copy"})).unwrap();
    assert!(s.mask_effects[2].favorite);
    s.execute("mask.favoriteEffect", &json!({"name": "Dark copy"})).unwrap();
    assert!(!s.mask_effects[2].favorite, "toggles");
    s.execute("mask.setEffectGroup", &json!({"names": ["Dark", "Dark copy"], "group": "Burn"})).unwrap();
    s.execute("mask.setEffectGroup", &json!({"name": "Dark copy 2", "group": "burn"})).unwrap();
    assert_eq!(s.mask_effects[3].group, "Burn", "an existing group's spelling wins");
    let v = s.execute("mask.effects", &json!({})).unwrap();
    assert_eq!(v["groups"], json!([{"name": "", "effects": ["Lift Shadows"]}, {"name": "Burn", "effects": ["Dark", "Dark copy", "Dark copy 2"]}]));
    s.execute("mask.renameEffectGroup", &json!({"group": "BURN", "to": "Darken"})).unwrap();
    assert!(s.mask_effects.iter().skip(1).all(|e| e.group == "Darken"));
    assert!(s.execute("mask.renameEffectGroup", &json!({"group": "Burn", "to": "x"})).is_err());
    // update with the selected mask's sliders
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    s.execute("mask.adjust", &json!({"values": {"exposure": -2, "clarity": 10}})).unwrap();
    s.execute("mask.updateEffect", &json!({"name": "Dark"})).unwrap();
    let dark = &s.mask_effects[1];
    assert_eq!((dark.adjust.exposure, dark.adjust.clarity, dark.group.as_str()), (-2.0, 10.0, "Darken"));
    // delete several (the menu asks first)
    s.execute("mask.deleteEffect", &json!({"names": ["Dark copy", "Dark copy 2"]})).unwrap();
    assert!(s.execute("mask.deleteEffect", &json!({"names": ["Dark", "gone"]})).is_err(), "nothing deleted when one is missing");
    assert_eq!(s.mask_effects.len(), 2);
    for bad in [json!({}), json!({"name": ""}), json!({"name": 3}), json!({"names": [1, 2]})] {
        assert!(s.execute("mask.deleteEffect", &bad).is_err(), "{bad}");
        assert!(s.execute("mask.favoriteEffect", &bad).is_err());
    }
}

#[test]
fn saving_some_sliders_and_amount() {
    let mut s = Session::with_demo();
    s.execute("mask.add", &json!({"kind": "brush"})).unwrap();
    s.execute("mask.adjust", &json!({"values": {"shadows": 25, "texture": -10, "amount": 60}})).unwrap();
    s.execute("mask.saveEffect", &json!({"name": "Only", "fields": "changed", "amount": true, "brush": {"size": 0.1, "flow": 40}})).unwrap();
    let e = &s.mask_effects[0];
    assert_eq!(e.fields, ["shadows", "texture"]);
    assert!(e.with_amount);
    assert_eq!(e.brush.map(|b| (b.size, b.flow)), Some((0.1, 40.0)));
    s.execute("mask.saveEffect", &json!({"name": "Pick", "fields": ["exposure", "bogus"]})).unwrap();
    assert_eq!(s.mask_effects[1].fields, ["exposure"]);
    for bad in [
        json!({"name": "x", "fields": []}),
        json!({"name": "x", "fields": 3}),
        json!({"name": "x", "fields": "some"}),
        json!({"name": "x", "brush": 7}),
    ] {
        assert!(s.execute("mask.saveEffect", &bad).is_err(), "{bad}");
    }
    // applying a partial effect leaves the other sliders and sets Amount; it brings its brush
    s.execute("mask.adjust", &json!({"values": {"shadows": 0, "texture": 0, "clarity": 50, "amount": 100}})).unwrap();
    let mid = s.active_mask.unwrap();
    let r = s.execute("mask.applyEffect", &json!({"name": "only"})).unwrap();
    assert_eq!(r["brush"]["flow"], 40.0);
    let a = mask_adjust(&s, mid);
    assert_eq!((a.shadows, a.texture, a.clarity, a.amount), (25.0, -10.0, 50.0, 60.0));
    // a mask that changes nothing has nothing to save as "changed"
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    assert!(s.execute("mask.saveEffect", &json!({"name": "Empty", "fields": "changed"})).is_err());
}

#[test]
fn applying_to_several_masks_and_photos_is_one_undo_step() {
    let mut s = Session::with_demo();
    with_effect(&mut s, "Bright", json!({"exposure": 1}));
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
    for _ in 0..2 {
        s.execute("mask.add", &json!({"kind": "radial", "name": "Face"})).unwrap();
    }
    s.execute("mask.add", &json!({"kind": "linear", "name": "Sky"})).unwrap();
    let all: Vec<u32> = s.develop_of(s.active().unwrap()).unwrap().masks.iter().map(|m| m.id).collect();
    let face = all[all.len() - 3];
    s.execute("mask.applyEffect", &json!({"name": "Bright", "ids": [face, all[all.len() - 2]], "remember": false})).unwrap();
    assert_eq!(mask_adjust(&s, face).exposure, 1.0);
    assert_eq!(mask_adjust(&s, *all.last().unwrap()).exposure, 0.0, "only the masks named");
    assert!(s.execute("mask.applyEffect", &json!({"name": "Bright", "ids": [999]})).is_err());
    assert!(s.execute("mask.applyEffect", &json!({"name": "Bright", "ids": []})).is_err());
    s.execute("mask.applyEffect", &json!({"name": "Bright", "all": true, "remember": false})).unwrap();
    assert_eq!(mask_adjust(&s, *all.last().unwrap()).exposure, 1.0);
    // the other selected photo's "Face" mask gets the effect too, in the same undo step
    s.execute("library.select", &json!({"ids": ids, "active": ids[1]})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial", "name": "face"})).unwrap();
    s.execute("mask.add", &json!({"kind": "radial", "name": "Other"})).unwrap();
    s.execute("library.select", &json!({"ids": ids, "active": ids[0]})).unwrap();
    with_effect(&mut s, "Warm", json!({"temp": 20}));
    let r = s.execute("mask.applyEffect", &json!({"name": "Warm", "id": face, "photos": "selected", "remember": false})).unwrap();
    assert_eq!(r["photos"], 2);
    let other = s.develop_of(lightcraft_catalog::PhotoId(ids[1])).unwrap();
    assert_eq!(other.masks.iter().map(|m| m.adjust.temp).collect::<Vec<_>>(), [20.0, 0.0]);
    s.execute("edit.undo", &json!({})).unwrap();
    let other = s.develop_of(lightcraft_catalog::PhotoId(ids[1])).unwrap();
    assert_eq!(other.masks[0].adjust.temp, 0.0, "undone on every photo");
    assert_eq!(mask_adjust(&s, face).temp, 0.0);
}

#[test]
fn new_masks_start_with_the_last_effect_of_their_tool() {
    let mut s = Session::with_demo();
    with_effect(&mut s, "Dodge", json!({"exposure": 0.5}));
    with_effect(&mut s, "Grad", json!({"highlights": -40}));
    s.execute("mask.add", &json!({"kind": "brush"})).unwrap();
    s.execute("mask.applyEffect", &json!({"name": "Dodge"})).unwrap();
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    s.execute("mask.applyEffect", &json!({"name": "Grad"})).unwrap();
    assert_eq!(s.execute("mask.effects", &json!({})).unwrap()["toolEffects"], json!({"brush": "Dodge", "linear": "Grad"}));
    // a new brush mask (painted with no mask selected) starts as Dodge, a new gradient as Grad
    s.execute("mask.select", &json!({"id": null})).unwrap();
    s.execute("mask.brushStroke", &json!({"points": [[0.2, 0.2], [0.3, 0.3]]})).unwrap();
    assert_eq!(mask_adjust(&s, s.active_mask.unwrap()).exposure, 0.5);
    s.execute("mask.add", &json!({"kind": "linear"})).unwrap();
    assert_eq!(mask_adjust(&s, s.active_mask.unwrap()).highlights, -40.0);
    s.execute("mask.add", &json!({"kind": "radial"})).unwrap();
    assert_eq!(mask_adjust(&s, s.active_mask.unwrap()).exposure, 0.0, "radial has none");
    // set / clear explicitly; renaming and deleting the effect follow
    s.execute("mask.toolEffect", &json!({"tool": "radial", "name": "dodge"})).unwrap();
    assert!(s.execute("mask.toolEffect", &json!({"tool": "radial", "name": "nope"})).is_err());
    s.execute("mask.renameEffect", &json!({"name": "Dodge", "to": "Dodge 2"})).unwrap();
    assert_eq!(s.mask_tool_effects.get("brush").map(String::as_str), Some("Dodge 2"));
    s.execute("mask.deleteEffect", &json!({"name": "Dodge 2"})).unwrap();
    assert!(!s.mask_tool_effects.contains_key("brush") && !s.mask_tool_effects.contains_key("radial"));
    s.execute("mask.toolEffect", &json!({"tool": "linear"})).unwrap();
    assert!(s.mask_tool_effects.is_empty());
}

#[test]
fn lightroom_folder_import_groups_by_dividers_and_round_trips() {
    let lib = temp_dir("folder");
    let src = lib.join("Local Adjustment Presets");
    std::fs::create_dir_all(src.join("Kit")).unwrap();
    let files: [(&str, String); 7] = [
        ("01 Burn.lrtemplate", lr_local("01 Burn", "\t\texposure2012 = -0.25,")),
        ("10 ----- DETAIL ----- 10.lrtemplate", lr_local("10 ----- DETAIL ----- 10", ZERO)),
        ("12 Sky.lrtemplate", lr_local("12 Sky | Cloud", "\t\thighlights2012 = -1,\n\t\tgrain = 0.06,")),
        ("20 ----- RETOUCHING ----- 20.lrtemplate", lr_local("20 ----- RETOUCHING ----- 20", ZERO)),
        ("21 Skin.lrtemplate", lr_local("21 Skin", "\t\tclarity2012 = -1,\n\t\tsharpness = -0.25,")),
        ("Kit/Glow.lrtemplate", lr_local("Glow", "\t\tdehaze = -0.2,")),
        ("Look.lrtemplate", "s = { title = \"Look\", type = \"Develop\", value = { settings = { Exposure2012 = 0.5 } } }".to_string()),
    ];
    for (f, text) in &files {
        std::fs::write(src.join(f), text).unwrap();
    }
    std::fs::write(src.join("broken.lrtemplate"), "s = { type = \"LocalizedAdjustmentPreset\", value = 5 }").unwrap();
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, true).unwrap();
        // an earlier import that kept the dividers as effects
        s.mask_effects.push(crate::cmd::mask_effects::MaskEffect { name: "10 ----- DETAIL ----- 10".into(), ..Default::default() });
        let r = s.execute("preset.import", &json!({"paths": [src.to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 1, "the develop template stays a develop preset");
        assert_eq!(r["effectDividers"].as_array().unwrap().len(), 2);
        assert_eq!(r["failed"].as_array().unwrap().len(), 1, "{r}");
        let groups: Vec<(String, String)> = s.mask_effects.iter().map(|e| (e.name.clone(), e.group.clone())).collect();
        assert_eq!(
            groups,
            [
                ("01 Burn".to_string(), "".to_string()),
                ("12 Sky | Cloud".into(), "Detail".into()),
                ("21 Skin".into(), "Retouching".into()),
                ("Glow".into(), "Kit".into()),
            ],
            "the old divider effect is gone; folders and dividers give groups"
        );
        let sky = &r["effects"].as_array().unwrap().iter().find(|e| e["name"] == "12 Sky | Cloud").unwrap()["unmapped"];
        assert_eq!(sky, &json!(["Grain"]));
        // the effects' own import: a dry run changes nothing
        let r = s.execute("mask.importEffects", &json!({"path": src.to_string_lossy(), "dryRun": true, "group": "X"})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 4);
        assert!(s.mask_effects.iter().all(|e| e.group != "X"));
        // export one group, everything; re-import keeps groups, stars and the kept grain
        s.execute("mask.favoriteEffect", &json!({"name": "21 Skin"})).unwrap();
        let out = lib.join("backup.lceffect");
        let r = s.execute("mask.exportEffects", &json!({"path": out.to_string_lossy(), "group": "retouching"})).unwrap();
        assert_eq!(r["count"], 1);
        assert!(s.execute("mask.exportEffects", &json!({"path": out.to_string_lossy(), "group": "none"})).is_err());
        s.execute("mask.exportEffects", &json!({"path": out.to_string_lossy()})).unwrap();
        let before = s.mask_effects.clone();
        s.execute("mask.deleteEffect", &json!({"names": before.iter().map(|e| e.name.clone()).collect::<Vec<_>>()})).unwrap();
        // through the general import too (File ▸ Import Profiles & Presets…, drag & drop)
        let r = s.execute("preset.import", &json!({"paths": [out.to_string_lossy()]})).unwrap();
        assert_eq!(r["effects"].as_array().unwrap().len(), 4);
        assert_eq!(s.mask_effects, before, "round trip");
        assert_eq!(s.mask_effects[1].extra.get("grain"), Some(&json!(0.06)));
        s.close_library().unwrap();
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.mask_effects.len(), 4, "persisted");
    assert!(s.mask_effects[2].favorite);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn zips_and_camera_raw_xmp_presets_import() {
    let lib = temp_dir("zip");
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:PresetType="Local" crs:LocalSaturation="-0.5">
 <crs:Name><rdf:Alt><rdf:li xml:lang="x-default">Mute</rdf:li></rdf:Alt></crs:Name>
</rdf:Description></rdf:RDF></x:xmpmeta>"#;
    let develop_xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="0.5" crs:Name="Bright"/></rdf:RDF></x:xmpmeta>"#;
    let brush = lr_local("Brush A", "\t\tshadows2012 = 0.5,");
    let zip = crate::preset_import::tests::zip(
        &[("Skin Kit/Brush A.lrtemplate", brush.as_bytes()), ("Skin Kit/Mute.xmp", xmp.as_bytes()), ("Looks/Bright.xmp", develop_xmp.as_bytes())],
        true,
    );
    let path = lib.join("bundle.zip");
    std::fs::write(&path, zip).unwrap();
    let mut s = Session::with_demo();
    let r = s.execute("preset.import", &json!({"paths": [path.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 1, "the develop XMP: {r}");
    assert_eq!(r["effects"].as_array().unwrap().len(), 2);
    let got: Vec<(String, String)> = s.mask_effects.iter().map(|e| (e.name.clone(), e.group.clone())).collect();
    assert_eq!(got, [("Brush A".to_string(), "Skin Kit".to_string()), ("Mute".into(), "Skin Kit".into())]);
    assert_eq!(s.mask_effects[1].adjust.saturation, -50.0);
    // the effects' own import reads the zip and a lone XMP; a develop XMP named on its own fails
    let mut s = Session::with_demo();
    let lone = lib.join("Mute.xmp");
    std::fs::write(&lone, xmp).unwrap();
    let dev = lib.join("Bright.xmp");
    std::fs::write(&dev, develop_xmp).unwrap();
    let r = s.execute("mask.importEffects", &json!({"paths": [path.to_string_lossy(), lone.to_string_lossy(), dev.to_string_lossy()]})).unwrap();
    assert_eq!(r["imported"].as_array().unwrap().len(), 3);
    assert_eq!(r["failed"].as_array().unwrap().len(), 1);
    assert_eq!(s.mask_effects.len(), 2, "same names replaced");
    // unreadable things are errors, never panics
    std::fs::write(lib.join("junk.zip"), b"PK\x03\x04\xff\xff").unwrap();
    std::fs::write(lib.join("junk.lceffect"), b"{\"effects\": [{\"name\": \"x\", \"adjust\": {\"exposure\": \"high\"}}]}").unwrap();
    for f in ["junk.zip", "junk.lceffect", "missing.lrtemplate"] {
        let r = s.execute("mask.importEffects", &json!({"path": lib.join(f).to_string_lossy()})).unwrap();
        assert_eq!(r["failed"].as_array().unwrap().len(), 1, "{f}: {r}");
        let _ = s.execute("preset.import", &json!({"paths": [lib.join(f).to_string_lossy()]}));
    }
    assert!(s.execute("mask.importEffects", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(&lib);
}
