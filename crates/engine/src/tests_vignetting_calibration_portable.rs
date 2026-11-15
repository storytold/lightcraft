//! Flat-field maps across the catalog's edge (`vignetting_calibration_io`): sidecars, the clipboard, presets, the control channel
//! and peer sync carry the map in the portable form; the catalog that reads them stores it, without ever trusting
//! the other catalog's photo ids.

use std::sync::Arc;

use lcc_catalog::{Op, Photo, PhotoId, Source, VignettingMapRecord, reference_photo};
use lcc_develop::{EmbeddedVignettingMap, VignettingMapId, VignettingMapOrigin};
use serde_json::json;

const REF_SHA: &str = "a9993e364706816aba3e25717850c26c9cd0d89d";

fn the_map() -> EmbeddedVignettingMap {
    let source = VignettingMapOrigin { sha1: Some(REF_SHA.into()), file_name: Some("REF_0001.NEF".into()), ..Default::default() };
    EmbeddedVignettingMap::from_ev([40, 30], [4, 3], None, source, &(0..12).map(|i| f64::from(i) / 11.0).collect::<Vec<_>>()).unwrap()
}

fn add(s: &mut crate::Session, name: &str, sha: &str) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/pics/{name}") }, name, "NEF", 40, 30, "2026-01-01");
    p.sha1 = Some(sha.into());
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

/// Catalog A: the reference photo, the map linked to it, and a photo using it.
fn catalog_a() -> (crate::Session, PhotoId, PhotoId, VignettingMapId) {
    let mut s = crate::Session::new();
    let reference = add(&mut s, "REF_0001.NEF", REF_SHA);
    let photo = add(&mut s, "SHOT_0002.NEF", "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    let map = the_map();
    let id = VignettingMapId::of(&map);
    s.catalog
        .apply(Op::AddVignettingMap {
            id: id.clone(),
            record: Box::new(VignettingMapRecord { map, photo: Some(reference), added: String::new(), withdrawn: false }),
        })
        .unwrap();
    let mut d = (*s.catalog.photo(photo).unwrap().develop).clone();
    d.optics.vignetting_calibration = Some(id.clone());
    d.optics.vignetting_calibration_amount = 80.0;
    s.catalog.apply(Op::SetDevelop { id: photo, settings: Arc::new(d), label: "Flat Field".into(), edited: None }).unwrap();
    (s, reference, photo, id)
}

fn select(s: &mut crate::Session, id: PhotoId) {
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
}

fn uses(s: &crate::Session, id: PhotoId) -> Option<VignettingMapId> {
    s.catalog.photo(id).and_then(|p| p.develop.optics.vignetting_calibration.clone())
}

/// The approved cross-catalog rule: a map read from a sidecar into another catalog, where the old photo id belongs to
/// a different photo, never links to that photo; it finds its reference by the raw's SHA-1.
#[test]
fn a_sidecar_carries_the_map_and_never_the_other_catalogs_photo_id() {
    let (a, reference_a, photo_a, id) = catalog_a();
    let packet = crate::sidecar::sidecar_packet(a.catalog.photo(photo_a).unwrap(), &a.catalog);
    let sc = crate::sidecar::parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap();
    assert_eq!(sc.vignetting_maps.len(), 1, "the sidecar carries the map");
    assert!(!packet.contains(&format!("\"photo\":{}", reference_a.0)), "no photo id travels");

    // catalog B: photo 1 is an unrelated photo with another file, photo 2 the one the sidecar is for
    let mut b = crate::Session::new();
    let squatter = add(&mut b, "UNRELATED.NEF", "dddddddddddddddddddddddddddddddddddddddd");
    assert_eq!(squatter, reference_a, "the old reference id is taken by another photo here");
    let target = add(&mut b, "SHOT_0002.NEF", "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
    let op = b.sidecar_op(target, sc).unwrap();
    b.commit("Read Metadata from File", op).unwrap();
    assert_eq!(uses(&b, target).as_ref(), Some(&id));
    let r = b.catalog.vignetting_map(&id).expect("the map is stored");
    assert_eq!(r.photo, None, "no link from the portable form");
    assert_eq!(reference_photo(&b.catalog, &id), None, "not the unrelated photo with the old id");
    // once the reference raw is in this catalog too, the SHA-1 finds it
    let real = add(&mut b, "REF_0001.NEF", REF_SHA);
    assert_eq!(reference_photo(&b.catalog, &id), Some(real));
    // and the photo renders with it
    assert!(b.job_info(b.catalog.photo(target).unwrap()).vignetting_maps.is_some());
}

#[test]
fn copy_in_one_catalog_and_paste_in_another_takes_the_map() {
    let (mut a, _, photo_a, id) = catalog_a();
    select(&mut a, photo_a);
    a.execute("develop.copy", &json!({"settings": ["optics.vignettingCalibration"]})).unwrap();
    let clip = a.clipboard.clone().unwrap();
    assert!(clip["optics"].get("vignetting_calibration_map").is_some(), "the clipboard is portable: {clip}");
    // pasting in the same catalog stores nothing new
    let other_a = add(&mut a, "SHOT_0003.NEF", "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    select(&mut a, other_a);
    a.execute("develop.paste", &json!({"ids": [other_a.0]})).unwrap();
    assert_eq!((uses(&a, other_a), a.catalog.vignetting_maps().count()), (Some(id.clone()), 1));
    // another catalog (the clipboard outlives a library switch)
    let mut b = crate::Session::new();
    let photo_b = add(&mut b, "X.NEF", "ffffffffffffffffffffffffffffffffffffffff");
    b.clipboard = Some(clip);
    select(&mut b, photo_b);
    b.execute("develop.paste", &json!({"ids": [photo_b.0]})).unwrap();
    assert_eq!(uses(&b, photo_b), Some(id.clone()));
    assert!(b.catalog.vignetting_map(&id).is_some_and(|r| r.photo.is_none()));
    assert_eq!(b.catalog.photo(photo_b).unwrap().develop.optics.vignetting_calibration_amount, 80.0);
    // undo takes the settings back, never the map
    b.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(uses(&b, photo_b), None);
    assert!(b.catalog.vignetting_map(&id).is_some());
}

#[test]
fn a_preset_carries_the_map_to_every_catalog() {
    let (mut a, _, photo_a, id) = catalog_a();
    select(&mut a, photo_a);
    let made = a.execute("preset.create", &json!({"name": "Z 7 85mm", "settings": ["optics.vignettingCalibration"]})).unwrap();
    let pid = made["id"].as_str().unwrap().to_string();
    let preset = a.presets.iter().find(|p| p.id == pid).unwrap().clone();
    assert!(preset.settings["optics"].get("vignetting_calibration_map").is_some(), "{}", preset.settings);
    let mut b = crate::Session::new();
    let photo_b = add(&mut b, "Y.NEF", "1111111111111111111111111111111111111111");
    b.presets.push(preset);
    select(&mut b, photo_b);
    b.execute("preset.apply", &json!({"id": pid, "ids": [photo_b.0]})).unwrap();
    assert_eq!(uses(&b, photo_b), Some(id.clone()));
    assert!(b.catalog.vignetting_map(&id).is_some());
}

#[test]
fn the_control_channel_reads_and_writes_the_portable_form() {
    let (mut a, _, photo_a, id) = catalog_a();
    let got = a.execute("develop.get", &json!({"id": photo_a.0})).unwrap();
    assert_eq!(got["optics"]["vignetting_calibration"], json!(id.as_str()));
    assert!(got["optics"].get("vignetting_calibration_map").is_some());
    let mut b = crate::Session::new();
    let photo_b = add(&mut b, "Z.NEF", "2222222222222222222222222222222222222222");
    select(&mut b, photo_b);
    b.execute("develop.merge", &json!({"settings": {"optics": got["optics"].clone()}})).unwrap();
    assert_eq!(uses(&b, photo_b), Some(id.clone()));
    assert!(b.catalog.vignetting_map(&id).is_some());
    // a map edited by hand gets its own id; a broken one stores nothing and leaves the id unresolved
    let mut tampered = got["optics"].clone();
    tampered["vignetting_calibration_map"]["grid"] = json!([2, 6]);
    tampered["vignetting_calibration_map"]["source"]["file_name"] = json!("EDITED.NEF");
    b.execute("develop.merge", &json!({"settings": {"optics": tampered.clone()}})).unwrap();
    let edited = uses(&b, photo_b).unwrap();
    assert_ne!(edited, id);
    assert!(b.catalog.vignetting_map(&edited).is_some_and(|r| r.map.source.file_name.as_deref() == Some("EDITED.NEF")));
    let mut broken = tampered;
    broken["vignetting_calibration_map"]["ev"] = json!("not base64 !");
    broken["vignetting_calibration"] = json!("0".repeat(64));
    let before = b.catalog.vignetting_maps().count();
    b.execute("develop.merge", &json!({"settings": {"optics": broken}})).unwrap();
    assert_eq!(b.catalog.vignetting_maps().count(), before);
    let unknown = VignettingMapId::parse(&"0".repeat(64)).unwrap();
    assert_eq!(uses(&b, photo_b), Some(unknown.clone()));
    let store = b.job_info(b.catalog.photo(photo_b).unwrap()).vignetting_maps.expect("its History uses the earlier maps");
    assert!(store.0.map(&unknown).is_none(), "an unknown map renders as none");
}

#[test]
fn synced_records_carry_the_map() {
    let (a, _, photo_a, id) = catalog_a();
    let (v, _) = crate::device_sync::record::value(a.catalog.photo(photo_a).unwrap(), &a.catalog, "develop").unwrap();
    assert_eq!(v["maps"].as_array().map(Vec::len), Some(1));
    assert!(!v.to_string().contains("\"photo\""), "{v}");
    let (plain, _) = crate::device_sync::record::value(a.catalog.photo(PhotoId(1)).unwrap(), &a.catalog, "develop").unwrap();
    assert!(plain.get("maps").is_none());
    assert!(a.catalog.vignetting_map(&id).is_some());
}
