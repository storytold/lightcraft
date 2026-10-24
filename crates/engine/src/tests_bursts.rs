//! Bursts arranged by hand (`photo.moveToBurst`): a photo moved within its burst, into another,
//! into a new one or out of every burst; the pick follows the first place, and with it the
//! Best of Similar Shots rule, the Pick flag culling set and the burst's stack. Culling run again
//! keeps what was arranged by hand.

use lightcraft_catalog::{Analysis, Flag, Op, Photo, PhotoId, Source, Stack};
use serde_json::{Value, json};

use crate::Session;

/// A photo culled into `group` (`None`: culled, in no burst; `best`: its pick), or never culled.
fn add(s: &mut Session, name: &str, when: &str, culled: Option<(Option<u32>, f32, bool)>) -> PhotoId {
    let id = s.catalog.alloc_photo_id();
    let mut p = Photo::new(id, Source::File { path: format!("/nowhere/{name}") }, name, "JPEG", 40, 30, "2026-05-01T12:00:00");
    p.captured = Some(when.into());
    p.analysis = culled.map(|(group, sharpness, best)| Analysis { sharpness, group, best, ..Default::default() });
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    id
}

fn an(s: &Session, id: PhotoId) -> Analysis {
    s.catalog.photo(id).unwrap().analysis.unwrap()
}

fn flag(s: &Session, id: PhotoId) -> Flag {
    s.catalog.photo(id).unwrap().flag
}

fn mv(s: &mut Session, p: Value) -> Value {
    s.execute("photo.moveToBurst", &p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// Burst 1 (a best and picked, b, c), burst 2 (d best, e), x culled in no burst, y never culled.
fn library() -> (Session, [PhotoId; 7]) {
    let mut s = Session::new();
    let a = add(&mut s, "a.jpg", "2026-05-01T10:00:00", Some((Some(1), 80.0, true)));
    let b = add(&mut s, "b.jpg", "2026-05-01T10:00:01", Some((Some(1), 60.0, false)));
    let c = add(&mut s, "c.jpg", "2026-05-01T10:00:02", Some((Some(1), 40.0, false)));
    let d = add(&mut s, "d.jpg", "2026-05-01T11:00:00", Some((Some(2), 70.0, true)));
    let e = add(&mut s, "e.jpg", "2026-05-01T11:00:01", Some((Some(2), 30.0, false)));
    let x = add(&mut s, "x.jpg", "2026-05-01T12:00:00", Some((None, 50.0, false)));
    let y = add(&mut s, "y.jpg", "2026-05-01T13:00:00", None);
    s.catalog.apply(Op::SetFlag { id: a, flag: Flag::Pick }).unwrap();
    (s, [a, b, c, d, e, x, y])
}

#[test]
fn a_photo_dragged_to_the_front_of_its_burst_becomes_the_pick() {
    let (mut s, [a, b, c, ..]) = library();
    assert_eq!(s.catalog.burst(1), vec![a, b, c]);
    let commits = s.commits();
    // the softest shot is the one the person likes best
    let r = mv(&mut s, json!({"ids": [c.0], "index": 0}));
    assert_eq!((r["burst"].clone(), r["photos"].clone(), r["pick"].clone()), (json!(1), json!([c.0, a.0, b.0]), json!(c.0)), "{r}");
    assert_eq!(s.catalog.burst(1), vec![c, a, b]);
    assert!(an(&s, c).best && !an(&s, a).best);
    assert_eq!([an(&s, c).rank, an(&s, a).rank, an(&s, b).rank], [Some(0), Some(1), Some(2)], "the whole burst is now arranged by hand");
    // the Pick culling gave the old best goes with it
    assert_eq!((flag(&s, c), flag(&s, a)), (Flag::Pick, Flag::None));
    // and so does Best of Similar Shots
    s.execute("library.filter", &json!({"ruleSet": {"rules": [{"field": "bestOfGroup", "op": "is", "value": true}]}})).unwrap();
    let shown = s.visible_cloned();
    assert!(shown.contains(&c) && !shown.contains(&a), "{shown:?}");
    // one undo step
    assert_eq!(s.commits(), commits + 1);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.burst(1), vec![a, b, c]);
    assert!(an(&s, a).best && an(&s, a).rank.is_none());
    assert_eq!((flag(&s, a), flag(&s, c)), (Flag::Pick, Flag::None));
    // reordering further back: the pick stays, the order is kept
    mv(&mut s, json!({"ids": [b.0], "index": 2}));
    assert_eq!(s.catalog.burst(1), vec![a, c, b]);
    assert!(an(&s, a).best);
    assert_eq!(flag(&s, a), Flag::Pick);
    // a rejected shot dragged to the front was chosen by hand: it takes the Pick
    s.catalog.apply(Op::SetFlag { id: b, flag: Flag::Reject }).unwrap();
    mv(&mut s, json!({"ids": [b.0], "index": 0}));
    assert_eq!((flag(&s, b), flag(&s, a)), (Flag::Pick, Flag::None));
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!((flag(&s, b), flag(&s, a)), (Flag::Reject, Flag::Pick));
    // placing a photo where it already is changes nothing and makes no undo step
    let commits = s.commits();
    let r = mv(&mut s, json!({"ids": [a.0], "index": 0}));
    assert_eq!(r["changed"], false, "{r}");
    assert_eq!(s.commits(), commits);
}

#[test]
fn photos_move_between_bursts_into_new_ones_and_out() {
    let (mut s, [a, b, c, d, e, x, y]) = library();
    // e joins burst 1 in second place; burst 2 keeps only d, which is then in no burst
    let r = mv(&mut s, json!({"ids": [e.0], "burst": 1, "index": 1}));
    assert_eq!(r["photos"], json!([a.0, e.0, b.0, c.0]), "{r}");
    assert_eq!(an(&s, e).group, Some(1));
    assert!(s.catalog.burst(2).is_empty());
    assert_eq!((an(&s, d).group, an(&s, d).best, an(&s, d).by_hand()), (None, false, true));
    // without an index it goes last; a culled loose photo can join too
    mv(&mut s, json!({"ids": [x.0], "burst": 1}));
    assert_eq!(s.catalog.burst(1), vec![a, e, b, c, x]);
    // two photos make a new burst, in the order named
    let r = mv(&mut s, json!({"ids": [c.0, b.0], "burst": "new"}));
    let new = r["burst"].as_u64().unwrap() as u32;
    assert_ne!(new, 1, "a number no photo uses: {r}");
    assert_eq!(s.catalog.burst(new), vec![c, b]);
    assert!(an(&s, c).best);
    assert_eq!(s.catalog.burst(1), vec![a, e, x]);
    // out of every burst: the photo left alone in its burst is out too
    let r = mv(&mut s, json!({"ids": [b.0], "burst": null}));
    assert_eq!(r["burst"], Value::Null, "{r}");
    assert!(s.catalog.burst(new).is_empty());
    assert!([b, c].iter().all(|p| an(&s, *p).group.is_none() && !an(&s, *p).best && an(&s, *p).by_hand()));
    // a whole burst dropped on another merges them, the target's pick first
    mv(&mut s, json!({"ids": [b.0, c.0], "burst": "new"}));
    let merged = s.catalog.burst(1);
    let theirs = s.catalog.bursts_among(&[b], None)[0].clone();
    mv(&mut s, json!({"ids": theirs.1.iter().map(|p| p.0).collect::<Vec<_>>(), "burst": 1}));
    assert_eq!(s.catalog.burst(1), [merged, theirs.1].concat());
    assert!(s.catalog.burst(theirs.0).is_empty());
    // refused, with nothing changed: a photo never culled, a burst that isn't there, a bad index
    let commits = s.commits();
    for (p, why) in [
        (json!({"ids": [y.0], "burst": 1}), "Assisted Culling"),
        (json!({"ids": [a.0], "burst": 99}), "no burst 99"),
        (json!({"ids": [a.0], "index": -1}), "index"),
        (json!({"ids": [a.0], "burst": "first"}), "burst"),
        (json!({"ids": [d.0]}), "isn't in a burst"),
        (json!({"ids": []}), "no photos"),
    ] {
        let err = s.execute("photo.moveToBurst", &p).expect_err(&p.to_string()).to_string();
        assert!(err.contains(why), "{p}: {err}");
    }
    assert_eq!(s.commits(), commits);
    // a new burst may start with one photo, for more to join
    let r = mv(&mut s, json!({"ids": [d.0], "burst": "new"}));
    let solo = r["burst"].as_u64().unwrap() as u32;
    assert_eq!(s.catalog.burst(solo), vec![d]);
    assert!(an(&s, d).best);
    mv(&mut s, json!({"ids": [x.0], "burst": solo, "index": 0}));
    assert_eq!(s.catalog.burst(solo), vec![x, d]);
}

#[test]
fn a_stacked_burst_follows_its_pick_and_its_members() {
    let (mut s, [a, b, c, d, e, ..]) = library();
    // as Assisted Culling with "Stack each burst" leaves them: collapsed, the best on top
    s.catalog.apply(Op::AddStack { stack: Stack { id: lightcraft_catalog::StackId(1), photos: vec![a, b, c], collapsed: true } }).unwrap();
    s.catalog.apply(Op::AddStack { stack: Stack { id: lightcraft_catalog::StackId(2), photos: vec![d, e], collapsed: true } }).unwrap();
    mv(&mut s, json!({"ids": [c.0], "index": 0}));
    let st = s.catalog.stack_of(c).unwrap();
    assert_eq!((st.photos.clone(), st.collapsed), (vec![c, a, b], true), "the new pick tops the stack");
    // d moves into burst 1: its stack (now just e) goes, burst 1's stack takes it in
    s.selection = crate::Selection::single(d);
    mv(&mut s, json!({"ids": [d.0], "burst": 1, "index": 1}));
    assert_eq!(s.catalog.stack_of(c).unwrap().photos, vec![c, d, a, b]);
    assert!(s.catalog.stack_of(e).is_none());
    // d is hidden in the collapsed stack now: the stack's top is the active photo
    assert_eq!(s.selection.active, Some(c));
    // one undo brings both stacks back as they were
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.catalog.stack_of(e).unwrap().photos, vec![d, e]);
    assert_eq!(s.catalog.stack_of(c).unwrap().photos, vec![c, a, b]);
    // a photo stacked with something else than its burst keeps that stack when it goes to an unstacked burst
    let (mut s, [a, b, c, _, e, x, _]) = library();
    s.catalog.apply(Op::AddStack { stack: Stack { id: lightcraft_catalog::StackId(5), photos: vec![x, e], collapsed: false } }).unwrap();
    mv(&mut s, json!({"ids": [e.0], "burst": 1}));
    assert_eq!(s.catalog.stack_of(e).unwrap().photos, vec![x, e]);
    assert_eq!(s.catalog.burst(1), vec![a, b, c, e]);
}

/// Look-alike PNGs (a scene and two softer copies) for the culling runs below.
fn burst_files(tag: &str, scene: usize) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-bursts-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_scenes::demo_library()[scene].render(480, 320);
    let soften = |img: &lightcraft_raster::Rgb32f, r: usize| {
        let (w, h) = (img.width, img.height);
        let mut out = img.clone();
        for y in 0..h {
            for x in 0..w {
                let (mut acc, mut n) = ([0f32; 3], 0.0);
                for yy in y.saturating_sub(r)..(y + r + 1).min(h) {
                    for xx in x.saturating_sub(r)..(x + r + 1).min(w) {
                        let c = img.data[yy * w + xx];
                        acc = [acc[0] + c[0], acc[1] + c[1], acc[2] + c[2]];
                        n += 1.0;
                    }
                }
                out.data[y * w + x] = acc.map(|v| v / n);
            }
        }
        out
    };
    for (name, im) in [("1_sharp.png", img.clone()), ("2_soft.png", soften(&img, 3)), ("3_softer.png", soften(&img, 6))] {
        let data: Vec<[u8; 4]> = im
            .data
            .iter()
            .map(|c| {
                let e = |v: f32| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8;
                [e(c[0]), e(c[1]), e(c[2]), 255]
            })
            .collect();
        let png = lightcraft_codecs::encode_png(
            &lightcraft_codecs::EncodeImage::rgba8(&lightcraft_raster::Rgba8 { width: im.width, height: im.height, data }),
            &Default::default(),
        )
        .unwrap();
        std::fs::write(dir.join(format!("s{scene}_{name}")), png).unwrap();
    }
    dir
}

#[test]
fn culling_again_keeps_bursts_arranged_by_hand() {
    let (one, two) = (burst_files("keep-1", 0), burst_files("keep-2", 7));
    let mut s = Session::new().with_fs();
    let import = |s: &mut Session, dir: &std::path::Path, hour: u32| -> Vec<PhotoId> {
        let r = s.execute("library.import", &json!({"paths": [dir.to_string_lossy()]})).unwrap();
        let mut ids: Vec<PhotoId> = r["imported"].as_array().unwrap().iter().filter_map(Value::as_u64).map(PhotoId).collect();
        ids.sort_by_key(|id| s.catalog.photo(*id).unwrap().file_name.clone());
        for (i, id) in ids.iter().enumerate() {
            s.commit("t", Op::SetCaptured { id: *id, captured: Some(format!("2026-05-01T{hour:02}:00:0{i}")) }).unwrap();
        }
        ids
    };
    let first = import(&mut s, &one, 10);
    let second = import(&mut s, &two, 14);
    let ids = |v: &[PhotoId]| v.iter().map(|p| p.0).collect::<Vec<_>>();
    // two runs on different photos number their bursts apart
    s.execute("photo.analyze", &json!({"ids": ids(&first), "pickBest": true})).unwrap();
    s.execute("photo.analyze", &json!({"ids": ids(&second)})).unwrap();
    let (g1, g2) = (an(&s, first[0]).group.unwrap(), an(&s, second[0]).group.unwrap());
    assert_ne!(g1, g2, "each run's bursts get numbers of their own");
    assert_eq!(s.catalog.burst(g1), first, "sharpest first");
    // the softest is the person's pick
    let softest = first[2];
    mv(&mut s, json!({"ids": [softest.0], "index": 0}));
    assert_eq!(flag(&s, softest), Flag::Pick);
    // culling the whole view again keeps that burst as arranged, with fresh scores
    let r = s.execute("photo.analyze", &json!({"ids": ids(&[first.clone(), second.clone()].concat()), "pickBest": true})).unwrap();
    assert_eq!(r["kept"], 3, "{r}");
    assert_eq!(s.catalog.burst(g1), vec![first[2], first[0], first[1]]);
    assert!(an(&s, softest).best && !an(&s, first[0]).best);
    assert_eq!(flag(&s, first[0]), Flag::None, "pickBest picks the pick chosen by hand, not the sharpest");
    let g2_again = an(&s, second[0]).group.unwrap();
    assert_ne!(g2_again, g1, "an automatic burst never takes a hand-made burst's number");
    assert_eq!(s.catalog.burst(g2_again), second);
    // stacking each burst stacks the one arranged by hand too, in its order, the pick on top
    let r = s.execute("photo.analyze", &json!({"ids": ids(&[first.clone(), second.clone()].concat()), "stackGroups": true})).unwrap();
    assert_eq!(r["stacks"], 2, "{r}");
    let st = s.catalog.stack_of(softest).expect("the hand-arranged burst is stacked");
    assert_eq!(st.photos, vec![first[2], first[0], first[1]], "in the order set by hand, its pick on top");
    assert_eq!(s.catalog.stack_of(second[0]).map(|st| st.top()), Some(second[0]), "an automatic burst: the sharpest on top");
    // asked to regroup, culling forgets the order set by hand
    s.execute("photo.analyze", &json!({"ids": ids(&first), "resetBursts": true})).unwrap();
    let g = an(&s, first[0]).group.unwrap();
    assert_eq!(s.catalog.burst(g), first);
    assert!(first.iter().all(|p| !an(&s, *p).by_hand()));
    assert!(s.execute("photo.analyze", &json!({"ids": ids(&first), "resetBursts": "yes"})).is_err());
    let _ = std::fs::remove_dir_all(&one);
    let _ = std::fs::remove_dir_all(&two);
}
