//! Process versions through the commands (`docs/process-versions.md`): new photos and Reset get
//! the latest process, Update to Current Process is one undo step, copies inherit, copy / paste /
//! sync / presets / Auto Sync keep each photo's own, and numbers this build doesn't know survive a
//! save and a sidecar round trip.

use std::sync::Arc;

use lightcraft_catalog::{Photo, PhotoId, Source};
use lightcraft_develop::{DevelopSettings, ProcessVersion};
use serde_json::json;

use crate::{EngineError, Session};

/// No process is older than V1 yet: 0 (which no LightCraft writes) stands in for one.
const OLDER: ProcessVersion = ProcessVersion(0);
/// A number from a newer LightCraft.
const NEWER: ProcessVersion = ProcessVersion(ProcessVersion::LATEST.0 + 6);

fn process(s: &Session, id: PhotoId) -> ProcessVersion {
    s.develop_of(id).unwrap().process
}

/// Put photo `id` on process `p` (with exposure `ev`) without going through a command.
fn put(s: &mut Session, id: PhotoId, p: ProcessVersion, ev: f64) {
    let mut d = (*s.develop_of(id).unwrap()).clone();
    d.process = p;
    d.light.exposure = ev;
    s.set_develop(id, d, "test").unwrap();
}

fn select(s: &mut Session, ids: &[PhotoId]) {
    s.execute("library.select", &json!({"ids": ids.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
}

fn update_info(s: &Session) -> crate::CommandInfo {
    s.commands().into_iter().find(|c| c.id == "develop.updateProcess").unwrap()
}

#[test]
fn new_photos_and_reset_get_the_latest_process() {
    let mut s = Session::with_demo();
    assert!(s.catalog.photos().all(|p| p.develop.process == ProcessVersion::LATEST));
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    put(&mut s, ids[0], OLDER, 0.7);
    put(&mut s, ids[1], NEWER, -0.3);
    // a section or a slider reset leaves the process alone
    select(&mut s, &ids[1..]);
    s.execute("develop.resetSection", &json!({"section": "light"})).unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.2})).unwrap();
    s.execute("develop.resetControl", &json!({"control": "light.exposure"})).unwrap();
    let d = s.develop_of(ids[1]).unwrap();
    assert_eq!((d.process, d.light.exposure), (NEWER, 0.0));
    // a full reset is a fresh start on the latest process, also from a newer one
    select(&mut s, &ids);
    s.execute("develop.reset", &json!({})).unwrap();
    for id in &ids {
        assert_eq!(process(&s, *id), ProcessVersion::LATEST);
        assert!(!s.catalog.photo(*id).unwrap().is_edited());
    }
    // a photo whose default preset's look was recorded on an older process
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(process(&s, ids[0]), OLDER);
    let mut p = (**s.catalog.photo(ids[0]).unwrap()).clone();
    let mut look = p.camera_defaults();
    look.light.contrast = 20.0;
    look.process = OLDER;
    p.import_look = Some(Arc::new(look));
    s.catalog.apply(lightcraft_catalog::Op::RemovePhoto { id: p.id }).unwrap();
    s.catalog.apply(lightcraft_catalog::Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("develop.reset", &json!({"ids": [ids[0].0]})).unwrap();
    let d = s.develop_of(ids[0]).unwrap();
    assert_eq!((d.process, d.light.contrast), (ProcessVersion::LATEST, 20.0));
}

#[test]
fn update_to_current_process_is_one_undo_step_and_only_when_needed() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    select(&mut s, &ids);
    // everything is current: disabled, with the reason
    let info = update_info(&s);
    assert!(!info.enabled && info.disabled_reason.as_deref().is_some_and(|r| r.contains("already on the current process")), "{info:?}");
    assert!(matches!(s.execute("develop.updateProcess", &json!({})), Err(EngineError::Disabled(..))));
    // in the Photo menu
    assert_eq!(info.menu, vec!["Photo"]);

    put(&mut s, ids[0], OLDER, 0.5);
    put(&mut s, ids[1], OLDER, -0.5);
    put(&mut s, ids[2], NEWER, 0.25);
    select(&mut s, &ids);
    assert!(update_info(&s).enabled);
    let undo = s.undo.len();
    let r = s.execute("develop.updateProcess", &json!({})).unwrap();
    assert_eq!(r, json!({"changed": 2, "process": ProcessVersion::LATEST.0}));
    assert_eq!(s.undo.len(), undo + 1, "one undo step");
    assert_eq!(s.undo.last().unwrap().label, "Update to Current Process");
    assert_eq!([process(&s, ids[0]), process(&s, ids[1])], [ProcessVersion::LATEST; 2]);
    assert_eq!(process(&s, ids[2]), NEWER, "a newer process is never downgraded");
    assert_eq!(s.develop_of(ids[0]).unwrap().light.exposure, 0.5, "the sliders keep their values");
    assert_eq!(s.catalog.photo(ids[1]).unwrap().history.last().unwrap().label, "Update to Current Process");
    assert!(!update_info(&s).enabled, "nothing left to update");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!([process(&s, ids[0]), process(&s, ids[1])], [OLDER; 2]);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!([process(&s, ids[0]), process(&s, ids[1])], [ProcessVersion::LATEST; 2]);
    // a photo on a newer process alone: nothing to update
    select(&mut s, &ids[2..]);
    assert!(!update_info(&s).enabled);
    // the active photo when nothing else is selected
    put(&mut s, ids[0], OLDER, 0.5);
    s.selection = crate::Selection { ids: Vec::new(), active: Some(ids[0]) };
    assert!(update_info(&s).enabled);
    assert_eq!(s.execute("develop.updateProcess", &json!({})).unwrap()["changed"], 1);
}

#[test]
fn virtual_copies_inherit_the_process() {
    let mut s = Session::with_demo();
    let src = s.visible_cloned()[0];
    put(&mut s, src, NEWER, 0.4);
    select(&mut s, &[src]);
    let r = s.execute("photo.virtualCopy", &json!({})).unwrap();
    let copy = PhotoId(r["ids"][0].as_u64().unwrap());
    assert_eq!(process(&s, copy), NEWER);
    assert!(s.develop_of(copy).unwrap() == s.develop_of(src).unwrap());
}

#[test]
fn copy_paste_sync_presets_and_auto_sync_keep_each_photos_process() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    let (src, a, b) = (ids[0], ids[1], ids[2]);
    put(&mut s, src, NEWER, 1.0);
    put(&mut s, b, OLDER, 0.0);
    select(&mut s, &[src]);
    s.execute("develop.copy", &json!({"groups": lightcraft_develop::SettingsGroup::ALL})).unwrap();
    select(&mut s, &[a, b]);
    s.execute("develop.paste", &json!({})).unwrap();
    assert_eq!([process(&s, a), process(&s, b)], [ProcessVersion::LATEST, OLDER]);
    assert_eq!(s.develop_of(b).unwrap().light.exposure, 1.0, "the values are pasted");
    // sync from the active photo
    put(&mut s, src, NEWER, -1.0);
    s.selection = crate::Selection { ids: vec![src, a, b], active: Some(src) };
    s.execute("develop.sync", &json!({})).unwrap();
    assert_eq!([process(&s, a), process(&s, b)], [ProcessVersion::LATEST, OLDER]);
    assert_eq!(s.develop_of(a).unwrap().light.exposure, -1.0);
    // a preset made from the photo
    select(&mut s, &[src]);
    // (not Grain: its integer seed doesn't survive scaling, a separate issue)
    let groups: Vec<_> = lightcraft_develop::SettingsGroup::ALL.into_iter().filter(|g| *g != lightcraft_develop::SettingsGroup::Grain).collect();
    let pid = s.execute("preset.create", &json!({"name": "Everything", "groups": groups})).unwrap()["id"].clone();
    put(&mut s, b, OLDER, 0.0);
    select(&mut s, &[b]);
    s.execute("preset.apply", &json!({"id": pid, "amount": 50})).unwrap();
    assert_eq!(process(&s, b), OLDER);
    assert_eq!(s.develop_of(b).unwrap().light.exposure, -0.5, "the preset applied (at 50 %)");
    // Auto Sync carries an edit of the active photo, not its process
    s.selection = crate::Selection { ids: vec![src, a], active: Some(src) };
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"process": OLDER.0, "light": {"contrast": 15}}})).unwrap();
    assert_eq!(process(&s, src), OLDER, "Apply Settings JSON sets what it names");
    let d = s.develop_of(a).unwrap();
    assert_eq!((d.process, d.light.contrast), (ProcessVersion::LATEST, 15.0));
}

#[test]
fn process_numbers_survive_a_reopen_and_develop_get_reports_them() {
    let dir = std::env::temp_dir().join(format!("lc-process-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = Session::new();
    s.open_library(&dir, true).unwrap();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    select(&mut s, &ids[..1]);
    s.execute("develop.merge", &json!({"settings": {"process": NEWER.0, "light": {"exposure": 0.5}}})).unwrap();
    assert_eq!(s.execute("develop.get", &json!({"id": ids[0].0})).unwrap()["process"], json!(NEWER.0));
    assert_eq!(s.execute("develop.get", &json!({"id": ids[1].0})).unwrap()["process"], json!(ProcessVersion::LATEST.0));
    // it renders (with the newest process this build has)
    assert!(s.render_now(ids[0], 48, 48).is_ok());
    let expect = s.catalog.to_snapshot();
    drop(s);
    let mut s = Session::new();
    s.open_library(&dir, false).unwrap();
    assert_eq!(s.catalog.to_snapshot(), expect);
    assert_eq!(process(&s, ids[0]), NEWER);
    assert_eq!(process(&s, ids[1]), ProcessVersion::LATEST);
    drop(s);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sidecars_keep_the_process_and_interchange_edits_get_the_latest() {
    use crate::sidecar::{DevelopPatch, merge_into, parse_sidecar, sidecar_packet};
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0];
    // our own settings round-trip with their process; V1 settings say nothing (as before process
    // versions, so sidecars written then read as V1)
    put(&mut s, id, NEWER, 0.3);
    let p = s.catalog.photo(id).unwrap().clone();
    let back = parse_sidecar(&sidecar_packet(&p, &s.catalog), crate::crs::Target::Rendered).unwrap();
    let Some(DevelopPatch::Full(d)) = &back.develop else { panic!("no lc:settings") };
    assert_eq!((d.process, d.light.exposure), (NEWER, 0.3));
    put(&mut s, id, ProcessVersion::V1, 0.3);
    let packet = sidecar_packet(s.catalog.photo(id).unwrap(), &s.catalog);
    assert!(!packet.contains("process"), "{packet}");
    let Some(DevelopPatch::Full(d)) = parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap().develop else { panic!("no lc:settings") };
    assert_eq!(d.process, ProcessVersion::V1);
    // a damaged process value reads as V1 and costs nothing else: the sidecar's edits still come back
    for bad in ["-1", "1.5", "\"two\"", "null", "4294967296"] {
        let json = format!(r#"{{"process": {bad}, "light": {{"exposure": 0.5}}}}"#);
        let packet = lightcraft_meta::write_xmp(&lightcraft_meta::Metadata::default(), Some(&json));
        let Some(DevelopPatch::Full(d)) = parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap().develop else {
            panic!("{bad}: the edit was dropped")
        };
        assert_eq!((d.process, d.light.exposure), (ProcessVersion::V1, 0.5), "{bad}");
    }
    // `crs:` edits (other raw developers, Lightroom): their ProcessVersion numbers another
    // renderer and is ignored; mapped like a preset, they leave the photo's process as it is,
    // which for a newly imported photo is the latest
    let xmp = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    crs:ProcessVersion="11.0" crs:Exposure2012="+1.10" crs:Contrast2012="+20"/></rdf:RDF></x:xmpmeta>"#;
    let sc = parse_sidecar(xmp, crate::crs::Target::RawAbsolute).unwrap();
    let Some(DevelopPatch::Partial(partial)) = &sc.develop else { panic!("no crs: edit") };
    assert!(partial.get("process").is_none(), "{partial}");
    let mut fresh = Photo::new(PhotoId(999), Source::Demo { scene: 1 }, "a.jpg", "JPEG", 10, 10, "2026-10-09T00:00:00");
    assert!(merge_into(&mut fresh, &sc, "2026-10-09T00:00:00"));
    assert_eq!((fresh.develop.process, fresh.develop.light.exposure), (ProcessVersion::LATEST, 1.1));
    let mut older = fresh.clone();
    older.develop = Arc::new(DevelopSettings { process: OLDER, ..Default::default() });
    assert!(merge_into(&mut older, &sc, "2026-10-09T00:00:00"));
    assert_eq!(older.develop.process, OLDER);
}

#[test]
fn update_to_current_process_checks_named_photos_not_the_selection() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    let (old, current, other) = (ids[0], ids[1], ids[2]);
    put(&mut s, old, OLDER, 0.5);
    // a different, current photo is selected: the menu item is off, but naming the photo works
    select(&mut s, &[current]);
    assert!(!update_info(&s).enabled);
    let undo = s.undo.len();
    assert_eq!(s.execute("develop.updateProcess", &json!({"ids": [old.0]})).unwrap()["changed"], 1);
    assert_eq!(s.undo.len(), undo + 1);
    assert_eq!(process(&s, old), ProcessVersion::LATEST);
    assert_eq!(s.develop_of(old).unwrap().light.exposure, 0.5);
    // with nothing selected at all, by `ids` or `id`
    put(&mut s, old, OLDER, 0.5);
    put(&mut s, other, OLDER, 0.0);
    s.selection = crate::Selection { ids: Vec::new(), active: None };
    assert!(!update_info(&s).enabled, "no selection");
    assert_eq!(s.execute("develop.updateProcess", &json!({"ids": [old.0, current.0]})).unwrap()["changed"], 1);
    assert_eq!(s.execute("develop.updateProcess", &json!({"id": other.0})).unwrap()["changed"], 1);
    assert_eq!([process(&s, old), process(&s, other)], [ProcessVersion::LATEST; 2]);
    // named photos are validated: all current, unknown, malformed or none
    let err = s.execute("develop.updateProcess", &json!({"ids": [current.0]})).unwrap_err();
    assert!(matches!(&err, EngineError::Disabled(_, why) if why.contains("already on the current process")), "{err}");
    for bad in [json!({"ids": [99_999]}), json!({"ids": ["x"]}), json!({"ids": "x"}), json!({"ids": []}), json!({"id": -1})] {
        assert!(matches!(s.execute("develop.updateProcess", &bad), Err(EngineError::BadParams { .. })), "{bad}");
    }
    // `ids: null` names nothing: the selection decides (and there is none)
    assert!(matches!(s.execute("develop.updateProcess", &json!({"ids": null})), Err(EngineError::Disabled(..))));
}
