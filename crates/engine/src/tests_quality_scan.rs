//! The Image Quality Scanner (issues #1313 and #1362): `photo.qualityScan` measures a real decoded
//! source per photo, stores the score on the photo (catalog format 21) and maps it to a Pick /
//! Reject flag by two cutoffs — and to a star rating by configurable breakpoints;
//! `photo.qualityScanPump` writes a background run's scores as one undo step.

use serde_json::{Value, json};

use lcc_catalog::{Flag, QualityVerdict};

use crate::Session;

/// The engine-driven part of the pump, without the app: run it to the end, as the app's frames
/// would. Answers the summary (the frame the run ends).
fn pump_to_end(s: &mut Session) -> Value {
    for _ in 0..2000 {
        let r = s.execute("photo.qualityScanPump", &json!({})).unwrap();
        if r["done"] == true || r["active"] != true {
            return r;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the scan did not finish");
}

/// One test: measuring the demo photos is the cost, so every check that needs a library shares it.
#[test]
fn scan_stores_scores_verdicts_and_optional_flags() {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    assert!(vis.len() >= 4);
    let ids: Vec<u64> = vis.iter().take(4).map(|i| i.0).collect();
    // the demo library flags some photos itself: start from photos nobody has flagged
    s.execute("photo.flag", &json!({"ids": ids, "flag": "none"})).unwrap();

    // 1. measures, but with `applyFlags` off it only says what it would do
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true})).unwrap();
    let measured = r["measured"].as_u64().unwrap_or(0);
    assert!(measured >= 4, "measured {measured} of 4: {r}");
    assert_eq!(r["failed"].as_u64().unwrap_or(1), 0, "{r}");
    assert_eq!(r["flagged"].as_u64().unwrap_or(1), 0, "no flag is written unless asked: {r}");
    for id in vis.iter().take(4) {
        let p = s.catalog.photo(*id).unwrap();
        let q = p.analysis.and_then(|a| a.quality).expect("a score is stored on the photo");
        assert!((0.0..=100.0).contains(&q.score), "{} scored {}", id.0, q.score);
        for factor in [q.sharpness, q.noise, q.exposure] {
            assert!((0.0..=100.0).contains(&factor), "{}: {factor} is not 0..100", id.0);
        }
        if let Some(c) = q.compression {
            assert!((0.0..=100.0).contains(&c), "{}: compression {c} is not 0..100", id.0);
        }
        assert_eq!(p.flag, Flag::None, "{} was flagged without applyFlags", id.0);
    }

    // 2. unscanned only: a second run has nothing to do
    let again = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true})).unwrap();
    assert_eq!(again["measured"].as_u64().unwrap_or(9), 0, "{again}");
    assert_eq!(again["skipped"].as_u64().unwrap_or(0), 4, "{again}");

    // 3. rescan measures them again (and can move a score)
    let rescanned = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true})).unwrap();
    assert!(rescanned["measured"].as_u64().unwrap_or(0) >= 4, "{rescanned}");

    // 4. flags: cutoffs at the ends of the scale put every photo on one side of them
    let all_pick =
        s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "accepted": 0, "rejected": 0, "applyFlags": true})).unwrap();
    assert!(all_pick["flagged"].as_u64().unwrap_or(0) >= 4, "{all_pick}");
    // (a score of exactly 0 is a reject at both cutoffs, the rejected one winning a tie, so the
    // flag each photo carries is its own verdict's)
    for row in all_pick["photos"].as_array().cloned().unwrap_or_default() {
        let id = lcc_catalog::PhotoId(row["id"].as_u64().unwrap_or(0));
        let want = match row["verdict"].as_str().unwrap_or("") {
            "accepted" => Flag::Pick,
            "rejected" => Flag::Reject,
            other => panic!("{other} is not a verdict: {row}"),
        };
        assert_eq!(s.catalog.photo(id).unwrap().flag, want, "{row}");
    }
    // the other way round: nothing is a pick at 100, everything below 100 is a reject
    let all_reject = s
        .execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "accepted": 100, "rejected": 100, "applyFlags": true}))
        .unwrap();
    assert!(all_reject["rejected"].as_u64().unwrap_or(0) >= 4, "{all_reject}");
    for id in vis.iter().take(4) {
        // the scan set the Pick itself, so it may take it back (only what the user set is the user's)
        assert_eq!(s.catalog.photo(*id).unwrap().flag, Flag::Reject, "{}", id.0);
    }

    // 5. the undo step brings the flags and the scores back together
    s.execute("edit.undo", &json!({})).unwrap();
    for id in vis.iter().take(4) {
        assert_eq!(s.catalog.photo(*id).unwrap().flag, Flag::Pick, "{}", id.0);
    }

    // 6. a photo the user flagged is never re-flagged by a scan, whatever the score says: the
    // user's Reject stays where the scan's verdict asks for a Pick
    let id = vis[0];
    s.execute("photo.flag", &json!({"ids": [id.0], "flag": "reject"})).unwrap();
    // (a cutoff of 0 makes every score a Pick, so this run's verdict asks for the other flag)
    let just_this = json!({"ids": [id.0], "wait": true, "rescan": true, "accepted": 0, "rejected": 0, "applyFlags": true});
    let r = s.execute("photo.qualityScan", &just_this).unwrap();
    assert_eq!(r["measured"].as_u64().unwrap_or(0), 1, "{r}");
    assert_eq!(r["flagged"].as_u64().unwrap_or(1), 0, "{r}");
    assert_eq!(s.catalog.photo(id).unwrap().flag, Flag::Reject, "the user's flag stays");
    let claims = s.catalog.photo(id).unwrap().analysis.and_then(|a| a.quality).and_then(|q| q.flag_written);
    assert_eq!(claims, None, "the scan wrote no flag, so it claims none");
}

/// What the scan refuses, and what it skips.
#[test]
fn scan_refuses_what_it_cannot_use_and_skips_clips() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|i| i.0).collect();
    // cutoffs that cross, a number out of range, every factor off
    assert!(s.execute("photo.qualityScan", &json!({"ids": ids, "accepted": 20, "rejected": 40})).is_err());
    assert!(s.execute("photo.qualityScan", &json!({"ids": ids, "accepted": 101})).is_err());
    assert!(s.execute("photo.qualityScan", &json!({"ids": ids, "weights": {"blur": 0, "noise": 0, "exposure": 0, "compression": 0}})).is_err());
    // a folder scope with no folder shown (the Folders panel's or Browse's) says so
    s.library_folder = None;
    s.browse = None;
    let e = s.execute("photo.qualityScan", &json!({"scope": "folder"})).unwrap_err();
    assert!(format!("{e}").contains("folder shown"), "{e}");
    // an unknown scope
    assert!(s.execute("photo.qualityScan", &json!({"scope": "somewhere", "ids": ids})).is_err());
    // a clip: its poster frame is not the clip's quality
    let clip = lcc_catalog::Photo {
        kind: lcc_catalog::MediaKind::Video,
        ..lcc_catalog::Photo::new(lcc_catalog::PhotoId(9999), lcc_catalog::Source::Demo { scene: 0 }, "clip.mp4", "MP4", 4, 4, "2026-01-01")
    };
    let id = clip.id;
    s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(clip) }).unwrap();
    let r = s.execute("photo.qualityScan", &json!({"ids": [id.0], "wait": true})).unwrap();
    assert_eq!(r["measured"].as_u64().unwrap_or(9), 0, "{r}");
    assert_eq!(r["clips"].as_u64().unwrap_or(0), 1, "{r}");

    // empty params are a plain answer, never a panic (they scan the photos in view, as the
    // dialog's default says; the run is stopped here so the test leaves nothing going — the
    // command sweep skips this command for the same reason)
    let r = s.execute("photo.qualityScan", &Value::Null).unwrap();
    assert!(r.is_object(), "{r}");
    s.execute("photo.qualityScan", &json!({"cancel": true})).unwrap();
    let _ = pump_to_end(&mut s);
    let r = s.execute("photo.qualityScanPump", &Value::Null).unwrap();
    assert_eq!(r["active"], false, "{r}");
    assert_eq!(r["done"], false, "{r}");
}

/// `scope: "folder"` looks where the Library is: `path` when given, else the Folders panel's folder,
/// else the one Browse shows — and only that folder (and its subfolders, when it says so). Every
/// photo carries a score already, so a run measures nothing and answers with what it left alone
/// (`skipped`, the photos in scope): no decode, and the scope alone is what the test reads.
#[test]
fn the_folder_scope_reads_the_folder_shown_and_its_depth() {
    let mut s = Session::new();
    for (id, path) in [(1u64, "/lib/trip/a.jpg"), (2, "/lib/trip/day2/b.jpg"), (3, "/lib/other/c.jpg")] {
        let mut photo = lcc_catalog::Photo::new(
            lcc_catalog::PhotoId(id),
            lcc_catalog::Source::File { path: path.to_string() },
            &format!("p{id}.jpg"),
            "JPEG",
            100,
            100,
            "2026-01-01",
        );
        // scored already: an unscanned-only run leaves each photo alone and counts it
        photo.analysis = Some(lcc_catalog::Analysis { quality: Some(lcc_catalog::QualityScan::default()), ..Default::default() });
        s.catalog.apply(lcc_catalog::Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    let in_scope = |s: &mut Session, extra: Value| -> u64 {
        let mut p = json!({"scope": "folder", "wait": true});
        if let (Some(p), Some(extra)) = (p.as_object_mut(), extra.as_object()) {
            p.extend(extra.clone());
        }
        let r = s.execute("photo.qualityScan", &p).unwrap();
        assert_eq!(r["measured"].as_u64().unwrap_or(9), 0, "every photo was scored already: {r}");
        r["skipped"].as_u64().unwrap_or(0)
    };
    // `path`: the folder and what is below it (the default), or just its own files
    assert_eq!(in_scope(&mut s, json!({"path": "/lib/trip"})), 2);
    assert_eq!(in_scope(&mut s, json!({"path": "/lib/trip", "subfolders": false})), 1);
    // the Folders panel's folder, at the depth the view shows
    s.library_folder = Some(lcc_catalog::query::folder_key("/lib/trip"));
    s.folder_subfolders = false;
    assert_eq!(in_scope(&mut s, json!({})), 1);
    s.folder_subfolders = true;
    assert_eq!(in_scope(&mut s, json!({})), 2);
    // else the folder Browse shows, and whether Browse shows its subfolders
    s.library_folder = None;
    s.browse = Some(crate::Browse { path: "/lib".into(), subfolders: false, not_imported: false });
    assert_eq!(in_scope(&mut s, json!({})), 0, "no photo sits directly in /lib");
    s.browse = Some(crate::Browse { path: "/lib".into(), subfolders: true, not_imported: false });
    assert_eq!(in_scope(&mut s, json!({})), 3);
}

/// A background run: the command answers at once, the pump moves it along, and the frame it ends
/// carries the summary once (with the scores already on the photos).
#[test]
fn background_scan_is_pumped_and_written_once() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|i| i.0).collect();
    let started = s.execute("photo.qualityScan", &json!({"ids": ids})).unwrap();
    assert_eq!(started["started"], true, "{started}");
    assert_eq!(started["photos"].as_u64().unwrap_or(0), 3, "{started}");
    // a status call before the pump has taken anything says it is running
    let status = s.execute("photo.qualityScan", &json!({"status": true})).unwrap();
    assert_eq!(status["active"], true, "{status}");
    // the app's frames
    let summary = pump_to_end(&mut s);
    assert_eq!(summary["done"], true, "{summary}");
    assert!(summary["measured"].as_u64().unwrap_or(0) >= 3, "{summary}");
    for row in summary["photos"].as_array().cloned().unwrap_or_default() {
        assert!(matches!(row["verdict"].as_str(), Some("accepted" | "pending" | "rejected")), "{row}");
    }
    for id in &ids {
        let p = s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap();
        assert!(p.analysis.and_then(|a| a.quality).is_some(), "{id} has no score");
    }
    // taken once: the next frame is the plain "nothing running" (the summary came with the frame
    // the run ended on, never again)
    let next = s.execute("photo.qualityScanPump", &json!({})).unwrap();
    assert_eq!(next["active"], false, "{next}");
    assert_eq!(next["done"], false, "{next}");

    // cancel: the command answers at once and the run writes what it measured
    let cancelled = s.execute("photo.qualityScan", &json!({"ids": ids, "rescan": true})).unwrap();
    assert_eq!(cancelled["started"], true, "{cancelled}");
    let r = s.execute("photo.qualityScan", &json!({"cancel": true})).unwrap();
    assert_eq!(r["cancelled"], true, "{r}");
    let summary = pump_to_end(&mut s);
    assert_eq!(summary["cancelled"], true, "{summary}");

    // a second scan while one runs is refused with a way out
    let ids: Vec<u64> = s.visible_cloned().iter().take(4).map(|i| i.0).collect();
    s.execute("photo.qualityScan", &json!({"ids": ids, "rescan": true})).unwrap();
    let err = s.execute("photo.qualityScan", &json!({"ids": ids, "rescan": true})).unwrap_err();
    assert!(format!("{err}").contains("in progress"), "{err}");
    let _ = pump_to_end(&mut s);
}

/// The catalog's own verdict rules, through a scan: a score at a cutoff is accepted or rejected at
/// the cutoff, and a score between them leaves the photo unflagged.
#[test]
fn verdicts_read_the_cutoffs_inclusively() {
    for (score, accepted, rejected, want) in [
        (70.0, 70.0, 25.0, QualityVerdict::Accepted),
        (69.9, 70.0, 25.0, QualityVerdict::Pending),
        (25.0, 70.0, 25.0, QualityVerdict::Rejected),
        (25.1, 70.0, 25.0, QualityVerdict::Pending),
    ] {
        assert_eq!(QualityVerdict::of(score, accepted, rejected), want, "{score} of {accepted}/{rejected}");
    }
    // and the thresholds run through the command: everything measured lands on the side the two
    // cutoffs put it
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|i| i.0).collect();
    let accepted = 70.0f64;
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "accepted": accepted, "rejected": 0.0})).unwrap();
    let rows = r["photos"].as_array().cloned().unwrap_or_default();
    for row in rows {
        let score = row["score"].as_f64().unwrap_or(0.0);
        let verdict = row["verdict"].as_str().unwrap_or("");
        // (the answer's score is rounded to a tenth, the verdict read the number itself: a score
        // within a twentieth of a cutoff could sit on either side of it, so neither is asserted)
        let near = |c: f64| (score - c).abs() <= 0.05;
        let want = if score <= 0.0 {
            Some("rejected")
        } else if near(0.0) || near(accepted) {
            None
        } else if score >= accepted {
            Some("accepted")
        } else {
            Some("pending")
        };
        if let Some(want) = want {
            assert_eq!(verdict, want, "{row}");
        }
    }
}

/// Star ratings (issue #1362): with `autoRate` the scan writes the rating its score asks for, only
/// onto photos the user never rated (or whose rating the scan itself wrote); a re-scan may change
/// its own, never the user's — unless `overwriteMetadata` says otherwise.
#[test]
fn scan_assigns_star_ratings_and_preserves_the_users() {
    use crate::quality_scan::{DEFAULT_STAR_BREAKS, star_of};

    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    let ids: Vec<u64> = vis.iter().take(4).map(|i| i.0).collect();
    // start from photos nobody has rated
    s.execute("photo.rate", &json!({"ids": ids, "rating": 0})).unwrap();

    // 1. autoRate maps the score to stars by the default breakpoints and stores them
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "autoRate": true})).unwrap();
    assert_eq!(r["failed"].as_u64().unwrap_or(1), 0, "{r}");
    let rows = r["photos"].as_array().cloned().unwrap_or_default();
    assert_eq!(rows.len(), 4, "{r}");
    // (a photo whose score asks for no star keeps the 0 it had, so `rated` counts the rows above it:
    // the demo's scores are what they are, and the count must follow them rather than an assumption)
    let wanted: u64 = rows.iter().filter(|row| star_of(row["score"].as_f64().unwrap_or(-1.0) as f32, &DEFAULT_STAR_BREAKS) > 0).count() as u64;
    assert_eq!(r["rated"].as_u64().unwrap_or(u64::MAX), wanted, "{r}");
    for row in &rows {
        let score = row["score"].as_f64().unwrap_or(-1.0) as f32;
        let want = star_of(score, &DEFAULT_STAR_BREAKS) as u64;
        assert_eq!(row["rating"].as_u64().unwrap_or(9), want, "the answer carries the rating it wrote: {row}");
        let id = lcc_catalog::PhotoId(row["id"].as_u64().unwrap_or(0));
        assert_eq!(s.catalog.photo(id).unwrap().rating as u64, want, "the photo carries it: {row}");
    }
    // the record the scan left on each photo: its own rating is the scan's to move (a photo the scan
    // left at 0 stars, from 0, has nothing to claim — `rating_written` is `Some` only for what changed)
    for id in &ids {
        let q = s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().analysis.and_then(|a| a.quality).expect("a score is stored");
        let stored = s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating;
        assert_eq!(q.rating_written, (stored > 0).then_some(stored), "the scan claims the rating it wrote for {id}");
    }

    // 2. the scan's own ratings may move on a rescan; a rating the user set does not
    let first = lcc_catalog::PhotoId(ids[0]);
    s.execute("photo.rate", &json!({"ids": [ids[0]], "rating": 5})).unwrap();
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "autoRate": true})).unwrap();
    assert_eq!(s.catalog.photo(first).unwrap().rating, 5, "the user's rating survives a rescan: {r}");

    // 3. "Overwrite existing" replaces the user's rating too
    let r =
        s.execute("photo.qualityScan", &json!({"ids": [ids[0]], "wait": true, "rescan": true, "autoRate": true, "overwriteMetadata": true})).unwrap();
    let row = r["photos"].as_array().cloned().unwrap_or_default().pop().expect("one photo was measured");
    assert_eq!(s.catalog.photo(first).unwrap().rating as u64, row["rating"].as_u64().unwrap_or(9), "overwriteMetadata replaced the rating: {r}");

    // 4. custom breakpoints are read as given
    s.execute("photo.rate", &json!({"ids": ids, "rating": 0})).unwrap();
    let r = s
        .execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "autoRate": true, "starBreaks": [10, 30, 50, 70, 90]}))
        .unwrap();
    for row in r["photos"].as_array().cloned().unwrap_or_default() {
        let score = row["score"].as_f64().unwrap_or(-1.0) as f32;
        let want = star_of(score, &[10.0, 30.0, 50.0, 70.0, 90.0]) as u64;
        let id = lcc_catalog::PhotoId(row["id"].as_u64().unwrap_or(0));
        assert_eq!(s.catalog.photo(id).unwrap().rating as u64, want, "{row}");
    }

    // 5. autoRate off: the scan writes no rating at all
    s.execute("photo.rate", &json!({"ids": ids, "rating": 0})).unwrap();
    let before: Vec<u8> = ids.iter().map(|id| s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating).collect();
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true})).unwrap();
    assert_eq!(r["rated"].as_u64().unwrap_or(1), 0, "not asked to rate: {r}");
    let after: Vec<u8> = ids.iter().map(|id| s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating).collect();
    assert_eq!(before, after, "no rating is written when autoRate is off");

    // 6. one undo step brings the scores and the ratings back together
    s.execute("photo.rate", &json!({"ids": ids, "rating": 0})).unwrap();
    let r = s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "autoRate": true})).unwrap();
    s.execute("edit.undo", &json!({})).unwrap();
    for id in &ids {
        assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating, 0, "undo brings the rating back: {id}");
    }
    let changed = r["photos"].as_array().cloned().unwrap_or_default().iter().filter(|row| row["rating"].as_u64().unwrap_or(0) > 0).count() as u64;
    assert_eq!(r["rated"].as_u64().unwrap_or(u64::MAX), changed, "{r}");
}

/// The ratings the scan writes go through the same `Op::SetRating` the Library Filter Bar reads:
/// filtering by a scan-written star shows exactly the photos at that rating, whether the scan wrote
/// it or the demo library did.
#[test]
fn scan_ratings_are_filterable() {
    let mut s = Session::with_demo();
    let vis = s.visible_cloned();
    let ids: Vec<u64> = vis.iter().take(4).map(|i| i.0).collect();
    s.execute("photo.rate", &json!({"ids": ids, "rating": 0})).unwrap();
    // the demo library rates photos of its own, so the ratings of the photos this scan is not given
    // are noted to check it leaves them alone
    let others: Vec<(u64, u8)> = vis.iter().map(|i| (i.0, s.catalog.photo(*i).unwrap().rating)).filter(|(id, _)| !ids.contains(id)).collect();
    // (breakpoints low enough that every score earns a star: the filter is what is under test here,
    // not the demo's scores)
    let r =
        s.execute("photo.qualityScan", &json!({"ids": ids, "wait": true, "rescan": true, "autoRate": true, "starBreaks": [0, 1, 2, 3, 4]})).unwrap();
    assert_eq!(r["failed"].as_u64().unwrap_or(1), 0, "{r}");
    let ratings: Vec<u8> = ids.iter().map(|id| s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating).collect();
    let top = ratings.iter().copied().max().unwrap_or(0);
    assert!(top > 0, "the scan rated at least one photo star: {ratings:?}");
    // the filter bar: at least `top` stars shows exactly the photos at that rating — the demo's own
    // rated photos answering too, since the op reads the photo and not who wrote its stars
    for (id, rating) in &others {
        assert_eq!(s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().rating, *rating, "the scan left {id} alone");
    }
    s.execute("library.filter", &json!({"rating": top, "ratingOp": "atLeast"})).unwrap();
    let mut shown: Vec<u64> = s.visible_cloned().iter().map(|i| i.0).collect();
    let mut want: Vec<u64> = ids.iter().zip(ratings.iter()).filter(|(_, rating)| **rating >= top).map(|(id, _)| *id).collect();
    want.extend(others.iter().filter(|(_, rating)| *rating >= top).map(|(id, _)| *id));
    shown.sort_unstable();
    want.sort_unstable();
    assert_eq!(shown, want, "the filter's answer is the scan's rating, through the same op");
}
