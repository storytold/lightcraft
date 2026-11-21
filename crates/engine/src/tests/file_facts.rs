//! Issue #4367: what a file carries is recorded for photos imported before LightCraft read it
//! (`cmd::file_facts`), and nothing else about them changes.

use std::sync::Arc;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::Session;
use crate::media::ProbeInfo;

/// A library with four DNGs imported "before": never looked at. One carries teeth and skin mattes, one carries
/// none, one is offline, and one is a virtual copy of the first. Plus a JPEG, which is never looked into, and a DNG
/// already looked at.
fn older_library() -> (Session, [PhotoId; 6]) {
    let mut s = Session::with_demo();
    let template = Photo::clone(s.catalog.photos().next().unwrap());
    let mut add = |name: &str, format: &str, mattes: Option<Vec<&str>>, copy_of: Option<PhotoId>| {
        let mut p = template.clone();
        p.id = s.catalog.alloc_photo_id();
        p.source = Source::File { path: format!("/old/{name}") };
        p.file_name = name.into();
        p.format = format.into();
        p.copy_of = copy_of;
        p.meta.mattes = mattes.map(|m| m.iter().map(|k| k.to_string()).collect());
        p.meta.title = "Mine".into();
        let id = p.id;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let with = add("with.dng", "DNG", None, None);
    let without = add("without.dng", "DNG", None, None);
    let offline = add("offline.dng", "DNG", None, None);
    let copy = add("with.dng", "DNG", None, Some(with));
    let jpeg = add("photo.jpg", "JPEG", None, None);
    let known = add("known.dng", "DNG", Some(vec!["sky"]), None);
    s.media.file_probe = Some(Arc::new(|path: &str| {
        let mattes = match path {
            "/old/with.dng" => vec!["skin".to_string(), "teeth".to_string()],
            "/old/without.dng" => Vec::new(),
            _ => return Err(format!("{path}: not found")),
        };
        let mut info = ProbeInfo { width: 1, height: 1, ..Default::default() };
        info.meta.mattes = Some(mattes);
        info.meta.title = "From the file".into();
        Ok(info)
    }));
    (s, [with, without, offline, copy, jpeg, known])
}

fn mattes(s: &Session, id: PhotoId) -> Option<Vec<String>> {
    s.catalog.photo(id).unwrap().meta.mattes.clone()
}

/// The three originals never looked at are pending (the copy lists its original; the JPEG and the DNG already
/// looked at don't count). The pass records the two it can read, the copy with its original, and leaves the offline
/// one pending. It is not an undo step, and it changes nothing else: titles, sizes and hashes stay.
#[test]
fn the_pass_records_only_what_was_never_looked_at() {
    let (mut s, [with, without, offline, copy, jpeg, known]) = older_library();
    assert_eq!(s.execute("library.fileFactsPending", &json!({})).unwrap()["pending"], 3);
    let before = Photo::clone(s.catalog.photo(with).unwrap());
    let (undo, commits) = (s.undo.len(), s.commits());

    let r = s.execute("library.recordFileFacts", &json!({})).unwrap();
    assert_eq!((r["checked"].clone(), r["pending"].clone()), (json!(3), json!(1)), "{r}");
    let mut recorded: Vec<u64> = r["recorded"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    recorded.sort_unstable();
    let mut want = vec![with.0, without.0, copy.0];
    want.sort_unstable();
    assert_eq!(recorded, want);

    assert_eq!(mattes(&s, with), Some(vec!["skin".to_string(), "teeth".to_string()]));
    assert_eq!(mattes(&s, copy), mattes(&s, with), "a virtual copy shares the file");
    assert_eq!(mattes(&s, without), Some(Vec::new()), "looked at, none");
    assert_eq!(mattes(&s, offline), None, "couldn't be read: still not looked at");
    assert_eq!(mattes(&s, jpeg), None, "a format that isn't looked into is left alone");
    assert_eq!(mattes(&s, known), Some(vec!["sky".to_string()]), "what was recorded stays");

    // nothing else about the photo changed, and it is not an undo step
    let after = Photo::clone(s.catalog.photo(with).unwrap());
    assert_eq!(Photo { meta: lightcraft_catalog::Meta { mattes: None, facts: 0, ..after.meta.clone() }, ..after }, before);
    assert_eq!((s.undo.len(), s.commits()), (undo, commits));

    // a second pass reads only what is left
    let again = s.execute("library.recordFileFacts", &json!({})).unwrap();
    assert_eq!((again["checked"].clone(), again["recorded"].clone(), again["pending"].clone()), (json!(1), json!([]), json!(1)));
}

/// `ids` narrows the pass (a copy's id reads its original's file), and a photo relinked while its file was read
/// keeps what it had.
#[test]
fn ids_narrow_the_pass_and_a_stale_result_changes_nothing() {
    let (mut s, [with, without, _, copy, ..]) = older_library();
    let r = s.execute("library.recordFileFacts", &json!({"ids": [copy.0]})).unwrap();
    assert_eq!(r["checked"], 1, "{r}");
    assert!(mattes(&s, with).is_some() && mattes(&s, copy).is_some());
    assert_eq!(mattes(&s, without), None, "not asked for");

    let listed = crate::cmd::file_facts::missing(&s.catalog, Some(&[without]));
    let probe = s.media.file_probe.clone().unwrap();
    let results = crate::cmd::file_facts::probe(&probe, listed);
    let relinked =
        Op::Relink { id: without, file_name: "without.dng".into(), source: Source::File { path: "/elsewhere/without.dng".into() }, format: None };
    s.catalog.apply(relinked).unwrap();
    assert!(crate::cmd::file_facts::apply(&mut s, results).unwrap().is_empty());
    assert_eq!(mattes(&s, without), None);
}

/// Without file access (the web build, a session without a library folder) the pass says so; the count still
/// answers.
#[test]
fn without_file_access_the_pass_is_refused() {
    let (mut s, _) = older_library();
    s.media.file_probe = None;
    assert!(s.execute("library.recordFileFacts", &json!({})).unwrap_err().to_string().contains("no file access"));
    assert_eq!(s.execute("library.fileFactsPending", &json!({})).unwrap()["pending"], 3);
}

fn facts(s: &Session, id: PhotoId) -> u16 {
    s.catalog.photo(id).unwrap().meta.facts
}

/// Issue #4371: the pass stamps the probe's version on every file it reads, whatever the file had, and a file it
/// couldn't read keeps its stamp. A photo that has the first version's fact from before versions were recorded
/// counts as stamped and isn't read.
#[test]
fn the_pass_stamps_the_probe_version_on_what_it_read() {
    let (mut s, [with, without, offline, copy, jpeg, known]) = older_library();
    s.execute("library.recordFileFacts", &json!({})).unwrap();
    let v = crate::files::FACTS_VERSION;
    assert_eq!([facts(&s, with), facts(&s, without), facts(&s, copy)], [v, v, v]);
    assert_eq!([facts(&s, offline), facts(&s, jpeg), facts(&s, known)], [0, 0, 0], "not read: not stamped");
    assert_eq!(crate::cmd::file_facts::looked(&s.catalog.photo(known).unwrap().meta), 1);
}

/// Issue #4371: a fact the probe learns later makes every photo below its version pending, without a "not looked at"
/// state of its own: here a version 2 for every format, whose fact the files don't even have. The pass reads each
/// such file once and is then done, the offline file aside.
#[test]
fn a_later_probe_version_lists_older_photos_once() {
    use crate::cmd::file_facts::{apply_as, behind, missing_by, probe};
    let (mut s, [with, without, offline, _copy, jpeg, known]) = older_library();
    s.execute("library.recordFileFacts", &json!({})).unwrap();
    assert_eq!(s.execute("library.fileFactsPending", &json!({})).unwrap()["pending"], 1, "today's probe: only the offline one");

    let later: [(u16, &[&str]); 2] = [(1, &["DNG"]), (2, &[])];
    let ours = [with, without, offline, jpeg, known];
    let listed: Vec<PhotoId> = missing_by(&later, &s.catalog, Some(&ours)).iter().map(|m| m.id).collect();
    assert_eq!(listed.len(), 5, "stamped 1, counted as 1 and never stamped alike: {listed:?}");

    // the files say nothing new (the JPEG's and the known DNG's can't even be read by this probe: give them one)
    s.media.file_probe = Some(Arc::new(|path: &str| {
        if path.contains("offline") { Err(format!("{path}: not found")) } else { Ok(ProbeInfo { width: 1, height: 1, ..Default::default() }) }
    }));
    let fp = s.media.file_probe.clone().unwrap();
    let (undo, kept) = (s.undo.len(), mattes(&s, with));
    let results = probe(&fp, missing_by(&later, &s.catalog, Some(&ours)));
    let recorded = apply_as(2, &mut s, results).unwrap();
    assert_eq!(recorded.len(), 5, "four files and the copy: {recorded:?}");
    assert_eq!([facts(&s, with), facts(&s, without), facts(&s, jpeg), facts(&s, known)], [2, 2, 2, 2]);
    assert_eq!((mattes(&s, with), mattes(&s, jpeg), s.undo.len()), (kept, None, undo), "nothing but the stamp changed");
    let left: Vec<PhotoId> = missing_by(&later, &s.catalog, Some(&ours)).iter().map(|m| m.id).collect();
    assert_eq!(left, [offline], "read once, done");

    // a row for another format doesn't list this one; a stamp at or above the row is not behind
    assert!(!behind(&[(3, &["HEIC"])], 2, "DNG") && behind(&[(3, &["HEIC"])], 2, "heic") && !behind(&[(3, &["HEIC"])], 3, "HEIC"));
}

/// The probe's version is the newest row of its history, and the rows are in order, so "below the current version"
/// and "behind a row" agree.
#[test]
fn the_probe_history_ends_at_its_version() {
    let rows = crate::files::FACTS_ADDED;
    assert!(rows.windows(2).all(|w| w[0].0 < w[1].0) && rows.first().is_some_and(|r| r.0 >= 1));
    assert_eq!(rows.last().map(|r| r.0), Some(crate::files::FACTS_VERSION));
}
