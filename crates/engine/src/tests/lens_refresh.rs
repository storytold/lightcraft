//! Lens profiles stay per lens. Presets, Copy / Paste and Sync carry "Enable Lens Corrections" and
//! its amounts, never a profile: each photo keeps the one found for its own lens. And photos imported before the lens
//! database was there get their profile when it arrives (`cmd::lens_refresh`).

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::{EmbeddedLens, EmbeddedWarp, LensSource};
use serde_json::json;

use crate::Session;

fn lens(k: f64) -> EmbeddedLens {
    EmbeddedLens {
        warp: Some(EmbeddedWarp { planes: [[1.0, k, 0.0, 0.0, 0.0, 0.0]; 3], radius: 0.6, ..Default::default() }),
        source: LensSource::Database,
        ..Default::default()
    }
}

fn lens_of(s: &Session, id: PhotoId) -> Option<EmbeddedLens> {
    s.catalog.photo(id).and_then(|p| p.embedded_lens)
}

/// Two demo photos "taken with different lenses".
fn two_lenses() -> (Session, PhotoId, PhotoId) {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible().iter().copied().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    s.catalog.apply(Op::SetEmbeddedLens { id: a, lens: Some(Box::new(lens(-0.05))) }).unwrap();
    s.catalog.apply(Op::SetEmbeddedLens { id: b, lens: Some(Box::new(lens(0.03))) }).unwrap();
    s.execute("library.select", &json!({"ids": [a.0]})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"optics": {"lens_profile": true}}, "label": "Lens Corrections"})).unwrap();
    s.execute("develop.set", &json!({"values": {"optics.profileDistortion": 60}})).unwrap();
    (s, a, b)
}

fn carried(s: &Session, id: PhotoId) {
    let d = s.develop_of(id).unwrap();
    assert!(d.optics.lens_profile, "the switch travels");
    assert_eq!(d.optics.profile_distortion, 60.0, "the amount travels");
}

#[test]
fn copy_paste_carries_the_switch_and_amounts_never_the_profile() {
    let (mut s, a, b) = two_lenses();
    s.execute("develop.copy", &json!({})).unwrap();
    s.execute("develop.paste", &json!({"ids": [b.0]})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)), "b keeps its own lens's profile");
    assert_eq!(lens_of(&s, a), Some(lens(-0.05)));
}

#[test]
fn sync_and_presets_keep_each_photos_own_profile() {
    let (mut s, a, b) = two_lenses();
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.sync", &json!({})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)));
    // a preset made on a's settings, applied to b
    let pid = s.execute("preset.create", &json!({"name": "Optics", "groups": ["optics"]})).unwrap()["id"].as_str().unwrap().to_string();
    s.execute("library.select", &json!({"ids": [b.0]})).unwrap();
    s.execute("develop.merge", &json!({"settings": {"optics": {"lens_profile": false}}, "label": "Lens Corrections"})).unwrap();
    s.execute("preset.apply", &json!({"id": pid})).unwrap();
    carried(&s, b);
    assert_eq!(lens_of(&s, b), Some(lens(0.03)), "a preset never brings another lens's profile");
    assert_eq!(lens_of(&s, a), Some(lens(-0.05)));
}

/// A batch of results gives each probed photo's profile to that photo and to its own virtual copies, never to the
/// copies of a photo that wasn't probed.
#[test]
fn each_result_reaches_its_photos_virtual_copies_and_no_others() {
    let mut s = Session::new();
    for p in [raw(1, "50mm F1.8", false), raw(2, "50mm F1.8", false), raw(3, "50mm F1.8", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for (id, of) in [(11, 1), (12, 1), (21, 2), (31, 3)] {
        let mut copy = raw(id, "50mm F1.8", false);
        copy.copy_of = Some(PhotoId(of));
        s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    }
    let probed = [1, 2].map(|id| {
        let m = crate::cmd::lens_refresh::Missing { id: PhotoId(id), path: format!("/photos/{id}.cr3"), checked: String::new() };
        (m, Ok(crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() }))
    });
    let mut found = crate::cmd::lens_refresh::apply(&mut s, probed.into()).unwrap();
    found.sort();
    assert_eq!(found, [1, 2, 11, 12, 21].map(PhotoId), "each probed photo and its own copies");
    for id in [1, 2, 11, 12, 21] {
        assert!(s.catalog.photo(PhotoId(id)).unwrap().embedded_lens.is_some(), "photo {id} gets the profile");
    }
    for id in [3, 31] {
        assert!(s.catalog.photo(PhotoId(id)).unwrap().embedded_lens.is_none(), "photo {id} wasn't probed: unchanged");
    }
}

/// A raw in a library folder, as an older import left it: no lens data.
fn raw(id: u64, lens_name: &str, edited: bool) -> Photo {
    let mut p =
        Photo::new(PhotoId(id), Source::File { path: format!("/photos/{id}.cr3") }, &format!("{id}.cr3"), "CR3", 600, 400, "2026-10-01T00:00:00");
    p.kind = MediaKind::Raw;
    p.meta.camera = "Testcam TC-1".into();
    p.meta.lens = lens_name.into();
    if edited {
        let mut d = (*p.develop).clone();
        d.light.exposure = 0.5;
        p.develop = Arc::new(d);
        p.edited = Some("2026-10-02T00:00:00".into());
    }
    p
}

const DB: &str = r#"<lensdatabase>
  <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1</cropfactor></camera>
  <lens><maker>Testcam</maker><model>Testcam 50mm f/1.8</model><mount>T</mount><cropfactor>1</cropfactor>
    <calibration><distortion model="poly3" focal="50" k1="-0.01" /></calibration></lens></lensdatabase>"#;

/// When the database arrives, the library's raws whose lens it knows get their profile; untouched photos get the
/// correction switched on as import would, edited ones keep their settings; lenses it doesn't know are not probed.
#[test]
fn photos_get_their_profile_when_the_database_arrives() {
    let mut s = Session::new();
    for p in [raw(1, "50mm F1.8", false), raw(2, "50mm F1.8", true), raw(3, "300mm F4", false), raw(4, "", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    let listed = crate::cmd::lens_refresh::missing(&s.catalog, &db, None, false);
    assert_eq!(listed.iter().map(|m| m.id.0).collect::<Vec<_>>(), [1, 2], "only lenses the database knows");
    assert_eq!(crate::cmd::lens_refresh::missing(&s.catalog, &db, Some(&[PhotoId(2)]), false).len(), 1, "or only the photos asked for");
    // what the files give when read again
    let results = listed.into_iter().map(|m| (m, Ok(crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() }))).collect();
    let found = crate::cmd::lens_refresh::apply(&mut s, results).unwrap();
    assert_eq!(found, [PhotoId(1), PhotoId(2)]);
    let p = |id: u64| s.catalog.photo(PhotoId(id)).unwrap().clone();
    assert!(p(1).embedded_lens.is_some() && p(1).develop.optics.lens_profile, "untouched: on, as import would");
    assert!(p(2).embedded_lens.is_some() && !p(2).develop.optics.lens_profile, "edited: its settings stay");
    assert!(p(3).embedded_lens.is_none());
    assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
    // the Optics panel's words
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(3)).as_deref(), Some("No lens profile found for 300mm F4."));
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(4)).as_deref(), Some("This file names no lens."));
    assert_eq!(crate::cmd::lens_refresh::no_profile_reason(&p(1)), None);
    // a result read from a file the photo no longer has (moved meanwhile) is dropped, for its virtual copies too
    let mut copy = raw(5, "300mm F4", false);
    copy.copy_of = Some(PhotoId(3));
    s.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
    let stale = crate::cmd::lens_refresh::Missing { id: PhotoId(3), path: "/elsewhere/3.cr3".into(), checked: "stale".into() };
    let ok = crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() };
    assert!(crate::cmd::lens_refresh::apply(&mut s, vec![(stale, Ok(ok))]).unwrap().is_empty());
    assert!(s.catalog.photo(PhotoId(3)).unwrap().embedded_lens.is_none(), "a stale probe changes nothing");
    assert!(s.catalog.photo(PhotoId(5)).unwrap().embedded_lens.is_none(), "nor the copies");
    assert!(s.catalog.photo(PhotoId(3)).unwrap().lens_checked.is_empty(), "nor is it remembered as looked up");
    // without file access the command says so instead of clearing anything
    assert!(s.execute("library.refreshLensProfiles", &json!({})).is_err());
    assert!(s.execute("library.refreshLensProfiles", &json!({"ids": "x"})).is_err());
}

/// Issue #3767: a Nikon DSLR raw names no lens, only its specification, which the catalog keeps as "50.0 mm f/1.8".
/// The database knows that by name (any maker's 50 mm f/1.8 scores), so the library-wide lookup lists the photo; but
/// the file's own lookup takes only a profile named exactly by the specification and finds none. That outcome is
/// remembered: the next start (same database) doesn't read the file again, while a changed database, Enable Lens
/// Corrections (the photo asked for by id) and an agent's `all` still do.
#[test]
fn a_lookup_that_found_nothing_is_not_repeated_until_the_database_changes() {
    use lightcraft_geom::Orientation;
    let spec = [50.0, 50.0, 1.8, 1.8];
    let name = lightcraft_meta::lens_spec_name(spec).unwrap();
    let mut s = Session::new();
    let mut copy = raw(2, &name, false);
    copy.copy_of = Some(PhotoId(1));
    for p in [raw(1, &name, false), copy] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    // the two sides disagree about this photo
    assert!(crate::lens_profiles::profile_name(&db, "Testcam TC-1", &name).is_some(), "known by name: listed");
    let file = lightcraft_meta::Metadata {
        make: Some("Testcam".into()),
        model: Some("TC-1".into()),
        lens_spec: Some(spec),
        focal_length: Some(50.0),
        f_number: Some(1.8),
        ..Default::default()
    };
    let found = crate::lens_profiles::lookup(&db, &file, 600.0, 400.0, Orientation::Normal);
    assert!(found.is_none(), "the file's lookup finds no profile named \"50mm f/1.8\"");
    let ids = |v: &[crate::cmd::lens_refresh::Missing]| v.iter().map(|m| m.id.0).collect::<Vec<_>>();
    let missing = |s: &Session, db: &lightcraft_lensdb::LensDb, only: Option<&[PhotoId]>, again: bool| {
        crate::cmd::lens_refresh::missing(&s.catalog, db, only, again)
    };
    // the first lookup reads it, and finds nothing
    let listed = missing(&s, &db, None, false);
    assert_eq!(ids(&listed), [1]);
    let results = listed.into_iter().map(|m| (m, Ok(crate::media::ProbeInfo { embedded_lens: found, ..Default::default() }))).collect();
    assert!(crate::cmd::lens_refresh::apply(&mut s, results).unwrap().is_empty());
    assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
    // the next start, same database (read again from the same files): not read again
    let same = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    assert!(missing(&s, &same, None, false).is_empty(), "a completed lookup is not repeated");
    // asked for by id (Enable Lens Corrections ticked, on it or its virtual copy), or every photo (`all`): read again
    assert_eq!(ids(&missing(&s, &same, Some(&[PhotoId(1)]), false)), [1]);
    assert_eq!(ids(&missing(&s, &same, Some(&[PhotoId(2)]), false)), [1]);
    assert_eq!(ids(&missing(&s, &same, None, true)), [1]);
    // a replaced database (a recalibrated profile, or the alias that names the specification): listed once more
    let aliased = DB.replace("<model>Testcam 50mm f/1.8</model>", "<model>Testcam 50mm f/1.8</model><model>50mm f/1.8</model>");
    let replaced = lightcraft_lensdb::LensDb::parse(&aliased).unwrap();
    let listed = missing(&s, &replaced, None, false);
    assert_eq!(ids(&listed), [1], "the new database may give a profile");
    let found = crate::lens_profiles::lookup(&replaced, &file, 600.0, 400.0, Orientation::Normal);
    assert!(found.is_some(), "the alias names the specification");
    let results = listed.into_iter().map(|m| (m, Ok(crate::media::ProbeInfo { embedded_lens: found, ..Default::default() }))).collect();
    assert_eq!(crate::cmd::lens_refresh::apply(&mut s, results).unwrap(), [PhotoId(1), PhotoId(2)]);
    assert!(s.catalog.photo(PhotoId(1)).unwrap().lens_checked.is_empty(), "found: nothing to remember");
    // a file that couldn't be read is tried again next time
    s.catalog.apply(Op::AddPhoto { photo: Box::new(raw(3, &name, false)) }).unwrap();
    let listed = missing(&s, &db, None, false);
    assert_eq!(ids(&listed), [3]);
    let failed = listed.into_iter().map(|m| (m, Err("the share went away".to_string()))).collect();
    assert!(crate::cmd::lens_refresh::apply(&mut s, failed).unwrap().is_empty());
    assert_eq!(ids(&missing(&s, &db, None, false)), [3], "an unread file is not remembered as looked up");
}

/// eliets2 on #3867: a library-wide listing that leaves out a photo for its current stamp says so (`skipped`), so
/// "nothing to check" is told apart from "already checked"; a photo asked for by id is read, not skipped. A photo
/// stamped by another database is listed and counted as read again because the database changed (`recheck`); an
/// unstamped one is not.
#[test]
fn a_listing_counts_photos_skipped_for_their_stamp_and_rechecked_for_a_new_database() {
    use crate::cmd::lens_refresh::listing;
    let mut s = Session::new();
    let mut copy = raw(11, "50mm F1.8", false);
    copy.copy_of = Some(PhotoId(1));
    for p in [raw(1, "50mm F1.8", false), copy, raw(2, "50mm F1.8", false), raw(3, "50mm F1.8", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    let current = crate::lens_profiles::stamp(&db);
    s.catalog.apply(Op::SetLensChecked { id: PhotoId(1), checked: current.clone() }).unwrap();
    s.catalog.apply(Op::SetLensChecked { id: PhotoId(2), checked: "1:another-database".into() }).unwrap();
    let ids = |v: &[crate::cmd::lens_refresh::Missing]| v.iter().map(|m| m.id.0).collect::<Vec<_>>();

    let sweep = listing(&s.catalog, &db, None, false);
    assert_eq!(ids(&sweep.list), [2, 3], "the photo with a current stamp is left out");
    assert_eq!(sweep.skipped, 1, "and counted once, though its virtual copy names it too");
    assert_eq!(sweep.recheck, 1, "photo 2's stamp is another database's: read again for the new one");

    let asked = listing(&s.catalog, &db, Some(&[PhotoId(1)]), false);
    assert_eq!(ids(&asked.list), [1], "asked for by id: read regardless of its stamp");
    assert_eq!((asked.skipped, asked.recheck), (0, 0), "not a skip, and its stamp is current");

    // only the stamped photo, swept: nothing listed, but not "nothing to look up"
    let mut only = Session::new();
    only.catalog.apply(Op::AddPhoto { photo: Box::new(raw(1, "50mm F1.8", false)) }).unwrap();
    only.catalog.apply(Op::SetLensChecked { id: PhotoId(1), checked: current }).unwrap();
    let swept = listing(&only.catalog, &db, None, false);
    assert!(swept.list.is_empty());
    assert_eq!(swept.skipped, 1);
    assert!(crate::cmd::lens_refresh::missing(&only.catalog, &db, None, false).is_empty(), "missing() is the listing's list");
}

/// CodeRabbit round 1 on #3867: a probe that fell back to the file's embedded preview ran no lens
/// lookup (the raw couldn't be decoded, so nothing was looked up in the database), so its result
/// must not be remembered as a lookup: no stamp is set, and a stamp the photo already carries is
/// dropped, so a later sweep reads the file again once it can be decoded. A read that did run the
/// lookup and found nothing stamps as before.
#[test]
fn a_preview_only_result_is_not_remembered_as_a_lookup() {
    let mut s = Session::new();
    for p in [raw(1, "50mm F1.8", false), raw(2, "50mm F1.8", false), raw(3, "50mm F1.8", false)] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    s.catalog.apply(Op::SetLensChecked { id: PhotoId(2), checked: "1:old".into() }).unwrap();
    let miss = |id: u64| crate::cmd::lens_refresh::Missing { id: PhotoId(id), path: format!("/photos/{id}.cr3"), checked: "1:new".into() };
    let preview = crate::media::ProbeInfo { preview_only: Some("a raw variant we can't decode yet".into()), ..Default::default() };
    crate::cmd::lens_refresh::apply(&mut s, vec![(miss(1), Ok(preview.clone())), (miss(2), Ok(preview))]).unwrap();
    assert!(s.catalog.photo(PhotoId(1)).unwrap().lens_checked.is_empty(), "preview-only: not stamped");
    assert!(s.catalog.photo(PhotoId(2)).unwrap().lens_checked.is_empty(), "preview-only: an old stamp is dropped");
    // control: a whole-file read that found nothing stamps as before
    crate::cmd::lens_refresh::apply(&mut s, vec![(miss(3), Ok(crate::media::ProbeInfo::default()))]).unwrap();
    assert_eq!(s.catalog.photo(PhotoId(3)).unwrap().lens_checked, "1:new", "a real lookup that found nothing is remembered");
}

/// CodeRabbit round 1 on #3867: Reload from Disk (and the preview-only recheck, which shares
/// `reload_ops`) invalidates the lens miss stamp when the file's content changed — the remembered
/// lookup read the old file, and this read may have run while no lens database was there. An
/// unchanged file keeps its stamp.
#[test]
fn reload_clears_the_lens_miss_stamp_only_when_the_file_changed() {
    let mut s = Session::new();
    let mut copy = raw(2, "50mm F1.8", false);
    copy.copy_of = Some(PhotoId(1));
    for p in [raw(1, "50mm F1.8", false), copy] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for id in [PhotoId(1), PhotoId(2)] {
        s.catalog.apply(Op::SetLensChecked { id, checked: "1:old".into() }).unwrap();
    }
    let ph = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let same = crate::media::ProbeInfo {
        width: ph.width,
        height: ph.height,
        file_size: ph.file_size,
        content_hash: ph.content_hash.clone(),
        ..Default::default()
    };
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(1), &same, &mut ops, &mut changed);
    assert!(ops.is_empty() && changed.is_empty(), "unchanged: nothing to bring up to date");
    let different = crate::media::ProbeInfo { content_hash: Some("the editor saved it again".into()), ..same };
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(1), &different, &mut ops, &mut changed);
    assert_eq!(changed, [PhotoId(1), PhotoId(2)], "the photo and its virtual copies");
    for op in ops {
        s.catalog.apply(op).unwrap();
    }
    for id in [PhotoId(1), PhotoId(2)] {
        assert!(s.catalog.photo(id).unwrap().lens_checked.is_empty(), "photo {}: the stamp read the old file", id.0);
    }
}

/// CodeRabbit's Linked Issues check on #3867 (round 3): the stamp also goes when a reload
/// changes what a lookup keys on without the file's content changing — here the catalog had
/// no camera/lens fields for the photo (it predates them being read) and this read fills them
/// in, so the remembered miss was made without the metadata a lookup needs.
#[test]
fn reload_clears_the_lens_miss_stamp_when_lookup_metadata_arrives_without_a_content_change() {
    let mut s = Session::new();
    let mut ph0 = raw(1, "", false);
    ph0.meta.camera = String::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(ph0) }).unwrap();
    s.catalog.apply(Op::SetLensChecked { id: PhotoId(1), checked: "1:old".into() }).unwrap();
    let ph = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let mut info = crate::media::ProbeInfo {
        width: ph.width,
        height: ph.height,
        file_size: ph.file_size,
        content_hash: ph.content_hash.clone(),
        ..Default::default()
    };
    info.meta.camera = "Testcam TC-1".into();
    info.meta.lens = "50mm F1.8".into();
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(1), &info, &mut ops, &mut changed);
    assert_eq!(changed, [PhotoId(1)]);
    for op in ops {
        s.catalog.apply(op).unwrap();
    }
    let after = s.catalog.photo(PhotoId(1)).unwrap();
    assert_eq!(after.meta.lens, "50mm F1.8", "the metadata fill landed");
    assert!(after.lens_checked.is_empty(), "the stamp read the photo without its lens metadata");
}

/// CodeRabbit round 4 on #3867: a virtual copy's reload that changes the shared file's
/// lookup inputs clears the ORIGINAL's miss stamp too — the lookup keys on the file, not
/// the record. Here the original's own read changes nothing (it already carries the
/// file's embedded lens data) while the copy, which predates that data being read,
/// receives it.
#[test]
fn a_copy_reload_that_changes_lookup_inputs_clears_the_originals_stamp_too() {
    let mut s = Session::new();
    let mut master = raw(1, "50mm F1.8", false);
    master.embedded_lens = Some(lens(-0.01));
    let mut copy = raw(2, "50mm F1.8", false);
    copy.copy_of = Some(PhotoId(1));
    for p in [master, copy] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for id in [PhotoId(1), PhotoId(2)] {
        s.catalog.apply(Op::SetLensChecked { id, checked: "1:old".into() }).unwrap();
    }
    let ph = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let info = crate::media::ProbeInfo {
        width: ph.width,
        height: ph.height,
        file_size: ph.file_size,
        content_hash: ph.content_hash.clone(),
        embedded_lens: Some(lens(-0.01)),
        ..Default::default()
    };
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(1), &info, &mut ops, &mut changed);
    for op in ops {
        s.catalog.apply(op).unwrap();
    }
    assert!(s.catalog.photo(PhotoId(2)).unwrap().embedded_lens.is_some(), "the copy received the file's lens data");
    assert!(changed.contains(&PhotoId(1)) && changed.contains(&PhotoId(2)), "both records changed: {changed:?}");
    for id in [PhotoId(1), PhotoId(2)] {
        assert!(s.catalog.photo(id).unwrap().lens_checked.is_empty(), "photo {}: the stamp read the file without its lens data", id.0);
    }
}

/// CodeRabbit round 5 on #3867: the metadata-only case reaches virtual copies too. The
/// original's catalog predates the camera/lens fields being read and this read fills them
/// in with the content unchanged; the copy's own content and lens ops are empty (its record
/// already matches the probe on those), yet its stamp keyed on the same shared file, so it
/// goes as well \u2014 a copy left behind by a deleted original is looked up by its own stamp.
#[test]
fn a_metadata_only_reload_clears_a_virtual_copys_lens_miss_stamp_too() {
    let mut s = Session::new();
    let mut master = raw(1, "", false);
    master.meta.camera = String::new();
    let mut copy = raw(2, "50mm F1.8", false);
    copy.copy_of = Some(PhotoId(1));
    for p in [master, copy] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    for id in [PhotoId(1), PhotoId(2)] {
        s.catalog.apply(Op::SetLensChecked { id, checked: "1:old".into() }).unwrap();
    }
    let ph = s.catalog.photo(PhotoId(1)).unwrap().clone();
    let mut info = crate::media::ProbeInfo {
        width: ph.width,
        height: ph.height,
        file_size: ph.file_size,
        content_hash: ph.content_hash.clone(),
        ..Default::default()
    };
    info.meta.camera = "Testcam TC-1".into();
    info.meta.lens = "50mm F1.8".into();
    let (mut ops, mut changed) = (Vec::new(), Vec::new());
    crate::cmd::convert::reload_ops(&s.catalog, PhotoId(1), &info, &mut ops, &mut changed);
    assert!(changed.contains(&PhotoId(1)) && changed.contains(&PhotoId(2)), "both records changed: {changed:?}");
    for op in ops {
        s.catalog.apply(op).unwrap();
    }
    assert_eq!(s.catalog.photo(PhotoId(1)).unwrap().meta.lens, "50mm F1.8", "the metadata fill landed");
    for id in [PhotoId(1), PhotoId(2)] {
        assert!(s.catalog.photo(id).unwrap().lens_checked.is_empty(), "photo {}: the stamp read the file without its lens metadata", id.0);
    }
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-lens-refresh-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A small procedural PNG (distinct per seed), as the import tests write.
fn write_png(path: &std::path::Path, seed: u8) {
    let (w, h) = (48usize, 32usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 5) as u8, (i / w * 7) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// CodeRabbit round 1 on #3867: relinking a photo to a different file brings the replacement's
/// probed lens data with it and invalidates the miss stamp remembered against the old file, so a
/// later sweep reads the replacement instead of skipping it. Relinking to the same content (the
/// usual moved file) keeps the stamp.
#[test]
fn relinking_to_a_changed_file_applies_its_lens_and_drops_the_miss_stamp() {
    let dir = temp_dir("relink");
    let (a, b) = (dir.join("a.png"), dir.join("b.png"));
    write_png(&a, 1);
    write_png(&b, 2);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [a.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    s.catalog.apply(Op::SetLensChecked { id, checked: "1:old".into() }).unwrap();
    // the replacement's file, as its probe reads it: a lens profile the old file never gave
    s.media.file_probe = Some(Arc::new(|_| Ok(crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() })));
    s.execute("photo.relink", &json!({"id": id.0, "path": b.to_string_lossy()})).unwrap();
    let p = s.catalog.photo(id).unwrap();
    assert_eq!(p.embedded_lens, Some(lens(-0.01)), "the replacement's lens data follows it");
    assert!(p.lens_checked.is_empty(), "the old file's miss stamp is invalidated");
    let _ = std::fs::remove_dir_all(&dir);

    // unchanged content: the stamp (and the skip it buys) stays
    let dir = temp_dir("relink-same");
    let (a, b) = (dir.join("a.png"), dir.join("b.png"));
    write_png(&a, 1);
    write_png(&b, 1);
    let mut s = Session::new().with_fs();
    s.execute("library.import", &json!({"paths": [a.to_string_lossy()]})).unwrap();
    let id = s.active().unwrap();
    s.catalog.apply(Op::SetLensChecked { id, checked: "1:old".into() }).unwrap();
    let ph = s.catalog.photo(id).unwrap().clone();
    s.media.file_probe = Some(Arc::new(move |_| {
        Ok(crate::media::ProbeInfo {
            width: ph.width,
            height: ph.height,
            file_size: ph.file_size,
            content_hash: ph.content_hash.clone(),
            embedded_lens: ph.embedded_lens,
            ..Default::default()
        })
    }));
    s.execute("photo.relink", &json!({"id": id.0, "path": b.to_string_lossy()})).unwrap();
    assert_eq!(s.catalog.photo(id).unwrap().lens_checked, "1:old", "same content: still remembered as looked up");
    let _ = std::fs::remove_dir_all(&dir);
}

fn with_lens(mut p: Photo, l: Option<EmbeddedLens>, on: bool) -> Photo {
    p.embedded_lens = l;
    let mut d = (*p.develop).clone();
    d.optics.lens_profile = on;
    p.develop = Arc::new(d);
    p
}

/// When the database's files were replaced, the photos whose profile came from the old database are read again
/// (they get the new profile, or lose one the database no longer gives), and so are the raws that named no lens when
/// they were imported. A profile from the file itself is left alone, and none of these are read at an ordinary start.
#[test]
fn a_replaced_database_looks_up_database_profiles_and_unnamed_photos_again() {
    let mut s = Session::new();
    let file_lens = EmbeddedLens { source: LensSource::File, ..lens(-0.04) };
    for p in [
        with_lens(raw(1, "50mm F1.8", false), Some(lens(-0.01)), true),
        with_lens(raw(2, "50mm F1.8", false), Some(file_lens), true),
        raw(3, "", false),
        with_lens(raw(4, "300mm F4", false), Some(lens(-0.02)), true),
    ] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let db = lightcraft_lensdb::LensDb::parse(DB).unwrap();
    let ids = |v: Vec<crate::cmd::lens_refresh::Missing>| v.iter().map(|m| m.id.0).collect::<Vec<_>>();
    assert!(crate::cmd::lens_refresh::missing(&s.catalog, &db, None, false).is_empty(), "an ordinary start reads none of them");
    let listed = crate::cmd::lens_refresh::missing(&s.catalog, &db, None, true);
    assert_eq!(ids(listed.clone()), [1, 3, 4], "database profiles and unnamed raws, never the file's own correction");
    // what the files give with the new database: 1 another profile, 3 its lens (now read) and a profile, 4 nothing
    let gives = |id: u64| match id {
        1 => crate::media::ProbeInfo { embedded_lens: Some(lens(-0.03)), ..Default::default() },
        3 => {
            let mut info = crate::media::ProbeInfo { embedded_lens: Some(lens(-0.01)), ..Default::default() };
            info.meta.lens = "50mm F1.8".into();
            info
        }
        _ => crate::media::ProbeInfo::default(),
    };
    let results = listed.into_iter().map(|m| (m.clone(), Ok(gives(m.id.0)))).collect();
    let found = crate::cmd::lens_refresh::apply(&mut s, results).unwrap();
    assert_eq!(found, [PhotoId(1), PhotoId(3)]);
    assert_eq!(lens_of(&s, PhotoId(1)), Some(lens(-0.03)), "the new database's profile");
    assert_eq!(lens_of(&s, PhotoId(2)), Some(file_lens), "the file's own correction stays");
    assert_eq!(lens_of(&s, PhotoId(3)), Some(lens(-0.01)));
    assert_eq!(s.catalog.photo(PhotoId(3)).unwrap().meta.lens, "50mm F1.8", "the lens's name, read now");
    assert_eq!(lens_of(&s, PhotoId(4)), None, "a profile the database no longer gives is gone");
    assert!(s.undo.is_empty(), "bookkeeping, not an undo step");
}

/// Every render of a photo uses the one database profile the catalog holds for it: a fresh decode that looked the lens
/// up in a newer database doesn't override it (a cached preview and a fresh render agree), while the file's own
/// correction, part of the file, still comes from the decode.
#[test]
fn a_decode_never_overrides_the_photos_database_profile() {
    use crate::media::DecodedSource;
    let img = Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
    let decoded = |l: EmbeddedLens| DecodedSource::new(img.clone(), Some(lightcraft_pipeline::SourceInfo { lens: Some(l), ..Default::default() }));
    let header = |l: Option<EmbeddedLens>| lightcraft_pipeline::SourceInfo { lens: l, ..Default::default() };
    assert_eq!(decoded(lens(-0.05)).info_or(header(Some(lens(-0.01)))).lens, Some(lens(-0.01)), "the catalog's profile");
    assert_eq!(decoded(lens(-0.05)).info_or(header(None)).lens, None, "none until the photo is given one");
    let file_lens = EmbeddedLens { source: LensSource::File, ..lens(-0.04) };
    assert_eq!(decoded(file_lens).info_or(header(None)).lens, Some(file_lens), "the file's own correction");
    // the original was replaced by one without a correction: the decode finds none, and the catalog's old file
    // correction isn't put back
    let none = DecodedSource::new(img.clone(), Some(lightcraft_pipeline::SourceInfo { lens: None, ..Default::default() }));
    assert_eq!(none.info_or(header(Some(file_lens))).lens, None, "no file correction the file no longer has");
    assert_eq!(none.info_or(header(Some(lens(-0.01)))).lens, Some(lens(-0.01)), "the database profile stays");
}

/// When a photo's database profile changes (a replaced database looked up again), its render key and its thumbnail
/// and variant cache keys change, so no cached render made with the old profile is shown. With the correction off
/// they change too: the decode fits the raw's starting colour through the profile's vignetting either way.
#[test]
fn a_new_database_profile_changes_the_render_and_cache_keys() {
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(with_lens(raw(1, "50mm F1.8", false), Some(lens(-0.01)), true)) }).unwrap();
    let id = PhotoId(1);
    let keys = |s: &mut Session| {
        let render = s.render_job(id, 800, 600, false, true).unwrap().key;
        let thumb = s.thumb_job(id, 256).unwrap().cache.unwrap().1;
        let p = s.catalog.photo(id).unwrap().clone();
        (render, thumb, Session::variant_key(&p, &p.develop, 128, 0), s.view_key(&p, true))
    };
    let before = keys(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(lens(-0.03))) }).unwrap();
    let after = keys(&mut s);
    assert_ne!(before.0, after.0, "render key");
    assert_ne!(before.1, after.1, "thumbnail cache key");
    assert_ne!(before.2, after.2, "variant cache key");
    assert_ne!(before.3, after.3, "loupe view cache key");
    // correction off: the starting colour's fit still uses the profile, so the keys change as well
    let off = with_lens((**s.catalog.photo(id).unwrap()).clone(), Some(lens(-0.03)), false);
    s.catalog.apply(Op::SetDevelop { id, settings: off.develop.clone(), label: "Lens Corrections".into(), edited: None }).unwrap();
    let off_before = keys(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(lens(-0.05))) }).unwrap();
    let off_after = keys(&mut s);
    assert_ne!(off_before.0, off_after.0, "render key, correction off");
    assert_ne!(off_before.1, off_after.1, "thumbnail cache key, correction off");
    assert_ne!(off_before.2, off_after.2, "variant cache key, correction off");
    assert_ne!(off_before.3, off_after.3, "loupe view cache key, correction off");
}

/// `lens.downloadDatabase` selects the folder it downloads into, so the session reads the database once it is in
/// place (a headless session would otherwise download it and go on without it); and the app's own folder is read
/// only when it is whole (its marker, written last, is there).
#[test]
fn the_downloaded_database_is_selected_and_read_only_when_whole() {
    use crate::{lens_profiles, lensfun_download};
    if std::env::var_os(lens_profiles::ENV).is_some_and(|v| !v.is_empty()) {
        lightcraft_testkit::skip!(Input, "LIGHTCRAFT_LENSFUN_DIR names the folder in use");
        return;
    }
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let root = std::env::temp_dir().join(format!("lc-lens-select-{}-{unique}", std::process::id()));
    // selecting the download folder switches the process-wide lens folder: take the
    // turn, and let the guard put the folder back and remove the scratch folder,
    // also on panic
    let _dir = lens_profiles::DirGuard::hold(root.clone());
    let dir = root.join("lensfun");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("lenses.xml"), DB).unwrap();
    let mut s = Session::new();
    // nothing listens on port 1: the download fails at once, without the network
    crate::cmd::lens_refresh::start_download(&mut s, &dir, vec!["http://127.0.0.1:1/db".into()], lensfun_download::FILES).unwrap();
    assert_eq!(lens_profiles::dir().as_deref(), Some(dir.as_path()), "the session reads the folder it downloads into");
    assert!(lens_profiles::database().is_none(), "no marker: a download that never finished is not read");
    std::fs::write(dir.join(lensfun_download::MARKER), "whole").unwrap();
    lens_profiles::reload();
    assert_eq!(lens_profiles::database().map(|db| db.lenses.len()), Some(1), "whole: read");
    let t0 = std::time::Instant::now();
    while s.lensfun.running() && t0.elapsed() < std::time::Duration::from_secs(60) {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // the guard puts the folder back and removes `root` when it drops
}
