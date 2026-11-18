//! Remembered objects (`concepts.*`): the store's commands, hostile input, and with SAM 2.1
//! installed (`<PREFIX>_SAM2_DIR` = its folder) teaching on one photo and finding on others.

use serde_json::json;

use crate::Session;

#[test]
fn concept_commands_and_masks_refuse_bad_input_and_a_concept_mask_is_a_recipe() {
    let mut s = Session::with_demo();
    assert_eq!(s.execute("concepts.list", &json!({})).unwrap()["concepts"], json!([]));
    assert!(s.execute("concepts.teach", &json!({})).is_err(), "no name");
    assert!(s.execute("concepts.teach", &json!({"name": "Dress"})).is_err(), "no mask");
    assert!(s.execute("concepts.teach", &json!({"name": "Dress", "mask": 4_000_000_000_u64})).is_err());
    assert!(s.execute("concepts.teach", &json!({"name": "x".repeat(500), "mask": 1})).is_err());
    assert!(s.execute("concepts.find", &json!({"name": "Nothing"})).is_err(), "unknown object");
    assert!(s.execute("concepts.find", &json!({"name": "Nothing", "op": "explode"})).is_err());
    assert!(s.execute("concepts.rename", &json!({"from": "a", "to": "b"})).is_err());
    assert_eq!(s.execute("concepts.forget", &json!({"name": "a"})).unwrap()["forgotten"], false);
    s.with_concepts(|st| st.teach("Dress", 1, json!(null))).unwrap().unwrap();
    s.execute("concepts.rename", &json!({"from": "dress", "to": "Wedding dress"})).unwrap();
    let l = s.execute("concepts.list", &json!({})).unwrap();
    assert_eq!(l["concepts"][0]["name"], "Wedding dress");
    // an example that isn't one is no example
    let e = s.execute("concepts.find", &json!({"name": "Wedding dress"})).unwrap_err().to_string();
    assert!(!e.is_empty());
    assert_eq!(s.execute("concepts.forget", &json!({"name": "WEDDING DRESS"})).unwrap()["forgotten"], true);

    // a concept mask is a recipe, and refuses bad names
    s.segmenter.models_root = Some(std::env::temp_dir().join("no-such-models-folder"));
    assert!(s.execute("mask.add", &json!({"kind": "concept"})).is_err(), "no name");
    assert!(s.execute("mask.add", &json!({"kind": "concept", "name": "x".repeat(500)})).is_err());
    assert!(s.execute("mask.add", &json!({"kind": "concept", "name": "Dress"})).is_err(), "no SAM 2.1, no such object");
    // a stored selection needs no model; its recipe drops it, so a preset finds it again
    let seg = lcc_develop::SegMask::from_logits(8, &[4.0; 64]);
    s.execute("mask.add", &json!({"kind": "concept", "name": " Dress ", "seg": seg})).unwrap();
    let d = s.develop_of(s.active().unwrap()).unwrap();
    let shape = &d.masks.last().unwrap().components[0].shape;
    assert!(matches!(shape, lcc_develop::MaskShape::Concept { name, seg: Some(_) } if name == "Dress"));
    assert!(matches!(shape.recipe(), lcc_develop::MaskShape::Concept { seg: None, .. }));
    assert!(shape.is_ai() && shape.stores_seg());
}

#[cfg(feature = "sam")]
#[test]
fn a_taught_object_and_its_concept_recipe_are_found_again_with_sam2() {
    let Some(dir) = lcc_brand::env_os("SAM2_DIR").map(std::path::PathBuf::from) else {
        lcc_testkit::skip!(Model, "set {} to SAM 2.1's folder", lcc_brand::env_var("SAM2_DIR"));
        return;
    };
    let mut s = Session::with_demo();
    s.segmenter.models_root = dir.parent().map(std::path::PathBuf::from);
    s.segmenter.dir = s.segmenter.models_root.as_ref().map(|r| r.join("sam3"));
    s.execute("ai.models.setObjectModel", &json!({"model": "sam2"})).unwrap();
    // what is in the demo photo's middle, picked with a click
    s.execute("mask.add", &json!({"kind": "object", "points": [[0.5, 0.5]]})).unwrap();
    let r = s.execute("concepts.teach", &json!({"name": "Middle"})).unwrap();
    assert_eq!(r["examples"], 1, "{r}");
    // found or not on each photo, never a crash; where found, an Object selection named after it
    // grown from the clicks the object's look picked
    let ids: Vec<u64> = s.visible_cloned().iter().take(4).map(|p| p.0).collect();
    let mut found = 0;
    for id in ids {
        match s.execute("concepts.find", &json!({"name": "middle", "id": id})) {
            Ok(_) => {
                found += 1;
                let d = s.develop_of(s.active().unwrap()).unwrap();
                let m = d.masks.last().unwrap();
                assert_eq!(m.name, "Middle");
                assert!(matches!(&m.components[0].shape, lcc_develop::MaskShape::Object { seg: Some(_), hint, .. } if !hint.is_empty()));
            }
            Err(e) => assert!(e.to_string().contains("wasn't found"), "{e}"),
        }
    }
    assert!(found > 0, "the object is found on some of the demo photos");

    // A `concept` mask (the recipe) is found again on each photo by Recompute AI Masks.
    // found on some of the demo photos (not found elsewhere: an error, never a crash)
    let ids: Vec<u64> = s.visible_cloned().iter().take(4).map(|p| p.0).collect();
    let mut on = None;
    for id in ids {
        s.execute("library.select", &json!({"ids": [id]})).unwrap();
        match s.execute("mask.add", &json!({"kind": "concept", "name": "Middle"})) {
            Ok(_) => on = s.active(),
            Err(e) => assert!(e.to_string().contains("wasn't found"), "{e}"),
        }
        if on.is_some() {
            break;
        }
    }
    let Some(id) = on else { panic!("the object is found on some of the demo photos") };
    let d = s.develop_of(id).unwrap();
    assert!(matches!(&d.masks.last().unwrap().components[0].shape, lcc_develop::MaskShape::Concept { seg: Some(_), .. }));
    // dropped and found again by Recompute AI Masks
    let done = s.compute_masks(id, true).unwrap();
    assert!(done.notes.is_empty(), "{:?}", done.notes);
    let d = s.develop_of(id).unwrap();
    assert!(matches!(&d.masks.last().unwrap().components[0].shape, lcc_develop::MaskShape::Concept { seg: Some(_), .. }));
}
