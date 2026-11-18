//! Generative fill end to end against the local mock provider (never the internet): consent and
//! keys, Generate, the patch file, renders that show it under later white balance and exposure
//! edits, variations, a missing patch file, the activity row, and `spot.fromMask`.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::Session;
use lcc_genfill::cloud::mock::{Filler, MockServer, mean_filler};

const KEY: &str = "test-genfill-key-0123456789";

fn temp(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("lc-genfill-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A demo session wired to a mock provider with `filler`, keys in `dir`.
fn session(dir: &std::path::Path, filler: Filler) -> (Session, MockServer) {
    let mock = MockServer::start(0, Some(KEY), filler).unwrap();
    let mut s = Session::with_demo();
    s.genfill.keys = lcc_genfill::cloud::keys::KeyStore::new(Some(dir.join("genfill-keys.json")));
    s.genfill.mock = Some(mock.base.clone());
    (s, mock)
}

/// The rendered sRGB pixel at normalized `(x, y)` of the active photo (uncropped, 400 px).
fn pixel(s: &mut Session, x: f64, y: f64) -> [u8; 4] {
    let id = s.active().unwrap();
    let img = s.render_job(id, 400, 400, false, false).unwrap().run().rendered.unwrap().image;
    let (px, py) = ((x * img.width as f64) as usize, (y * img.height as f64) as usize);
    img.data[py * img.width + px]
}

/// A filler that paints the hole a strong blue, so the patch is easy to find in a render.
fn blue() -> Filler {
    Arc::new(|w, h, rgb, mask, var| {
        let mut out = rgb.to_vec();
        for i in 0..w * h {
            if mask[i] >= 128 {
                out[i * 3..i * 3 + 3].copy_from_slice(&[60, 80 + var as u8 * 25, 120]);
            }
        }
        out
    })
}

#[test]
fn generate_needs_consent_and_a_key_and_never_shows_the_key() {
    let dir = temp("consent");
    let (mut s, mock) = session(&dir, mean_filler());
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.5, 0.5]], "size": 0.03})).unwrap();
    let e = s.execute("spot.generate", &json!({"provider": "openai"})).unwrap_err().to_string();
    assert!(e.contains("terms"), "{e}");
    s.execute("genfill.consent", &json!({"provider": "openai", "accept": true})).unwrap();
    let e = s.execute("spot.generate", &json!({"provider": "openai"})).unwrap_err().to_string();
    assert!(e.contains("API key"), "{e}");
    let r = s.execute("genfill.setKey", &json!({"provider": "openai", "key": KEY})).unwrap();
    assert_eq!(r["configured"], true);
    let listing = s.execute("genfill.providers", &json!({})).unwrap();
    for v in [&r, &listing] {
        assert!(!v.to_string().contains(KEY), "the key is never returned: {v}");
    }
    let openai = listing["providers"].as_array().unwrap().iter().find(|p| p["id"] == "openai").unwrap().clone();
    assert_eq!(
        (openai["consented"].clone(), openai["keySource"].clone(), openai["keyHint"].clone()),
        (json!(true), json!("settings"), json!("6789"))
    );
    // setKey is never journaled (the key stays out of every log)
    assert!(!s.journal.iter().any(|(_, p)| p.to_string().contains(KEY)));
    // nothing went to the provider before Generate
    assert!(mock.seen().is_empty());
    // a key the provider refuses: an error without the key in it
    s.execute("genfill.setKey", &json!({"provider": "openai", "key": "wrong-key-0000000000"})).unwrap();
    let e = s.execute("spot.generate", &json!({"provider": "openai"})).unwrap_err().to_string();
    assert!(e.contains("refused") && !e.contains("wrong-key-0000000000"), "{e}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_generated_patch_renders_under_later_white_balance_and_exposure() {
    let dir = temp("render");
    let (mut s, mock) = session(&dir, blue());
    s.execute("genfill.setKey", &json!({"provider": "stability", "key": KEY})).unwrap();
    s.execute("genfill.consent", &json!({"provider": "stability", "accept": true})).unwrap();
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.5, 0.5]], "size": 0.04})).unwrap();
    let before = pixel(&mut s, 0.5, 0.5);
    let plan = s.execute("genfill.plan", &json!({"provider": "stability", "variations": 2})).unwrap();
    assert_eq!((plan["requests"].as_u64(), plan["soft"].as_bool()), (Some(2), Some(false)), "{plan}");
    let r = s.execute("spot.generate", &json!({"provider": "stability", "variations": 2})).unwrap();
    assert_eq!(r["generated"]["variants"], 2, "{r}");
    let id = s.active().unwrap();
    let sp = s.develop_of(id).unwrap().spots[0].clone();
    let g = sp.shown_gen().expect("the spot shows its patch").clone();
    assert!(s.genfill_dir(id).join(g.file_name(0)).is_file() && s.genfill_dir(id).join(g.file_name(1)).is_file());
    // the crop sent was the native-size context crop, not the whole photo
    let sent = mock.seen().into_iter().find(|x| x.path.starts_with("/v2beta/")).unwrap();
    let (pw, ph) = (s.catalog.photo(id).unwrap().width as usize, s.catalog.photo(id).unwrap().height as usize);
    let (cw, chh) = sent.image.unwrap();
    assert!(cw < pw || chh < ph, "a crop, not the photo ({cw}×{chh} of {pw}×{ph})");
    // the patch shows: blue in the middle of the spot
    let after = pixel(&mut s, 0.5, 0.5);
    assert!(after[2] > after[0] + 25 && after != before, "{before:?} → {after:?}");
    // white balance and exposure after generating still apply to the patch
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 1.0})).unwrap();
    let brighter = pixel(&mut s, 0.5, 0.5);
    assert!(brighter[2] > after[2] || brighter[2] == 255, "{after:?} → {brighter:?}");
    assert!(brighter[1] > after[1], "{after:?} → {brighter:?}");
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 10000.0})).unwrap();
    let warmer = pixel(&mut s, 0.5, 0.5);
    assert!(warmer[0] > brighter[0] || warmer[2] < brighter[2], "{brighter:?} → {warmer:?}");
    // the second variation differs (the mock paints it greener)
    s.execute("spot.variation", &json!({"delta": 1})).unwrap();
    let other = pixel(&mut s, 0.5, 0.5);
    assert_ne!(other, warmer);
    // undo goes back to the first variation; moving the spot shows a content-aware fill instead
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(pixel(&mut s, 0.5, 0.5), warmer);
    // a missing patch file: no crash, the photo's own pixels in its place, and the session knows
    std::fs::remove_file(s.genfill_dir(id).join(g.file_name(0))).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(3100));
    let missing = pixel(&mut s, 0.5, 0.5);
    assert_ne!(missing, warmer, "the blue patch is gone");
    assert!(s.genfill_missing(id, 0));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_mask_becomes_a_spot_that_covers_it() {
    let mut s = Session::with_demo();
    let r = s.execute("mask.add", &json!({"kind": "radial", "center": [0.5, 0.5], "rx": 0.1, "ry": 0.1, "feather": 0})).unwrap();
    let mid = r["id"].as_u64().or(s.active_mask.map(u64::from)).unwrap();
    let r = s.execute("spot.fromMask", &json!({"mask": mid, "mode": "generate"})).unwrap();
    let id = s.active().unwrap();
    let d = s.develop_of(id).unwrap();
    assert!(d.masks.is_empty(), "the mask was used up");
    let sp = &d.spots[r["index"].as_u64().unwrap() as usize];
    assert_eq!(sp.mode, lcc_develop::SpotMode::Generate);
    assert!(sp.points.len() > 20 && sp.points.len() <= 900, "{}", sp.points.len());
    // the dabs lie inside the ellipse (normalized: rx is relative to the long edge, so check loosely)
    let (xs, ys): (Vec<f64>, Vec<f64>) = sp.points.iter().map(|p| (p.x, p.y)).unzip();
    let ext = |v: &[f64]| (v.iter().cloned().fold(f64::MAX, f64::min), v.iter().cloned().fold(f64::MIN, f64::max));
    let (ex, ey) = (ext(&xs), ext(&ys));
    assert!(ex.0 > 0.3 && ex.1 < 0.7 && ey.0 > 0.2 && ey.1 < 0.8 && (ex.0 + ex.1 - 1.0).abs() < 0.03, "{ex:?} {ey:?}");
    assert!(s.execute("spot.fromMask", &json!({"mask": 999})).is_err());
    let _: Value = s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().masks.len(), 1);
}

/// How many generation requests reached the mock (not counting anything else it was asked).
fn generations(mock: &MockServer) -> usize {
    mock.seen().iter().filter(|x| x.image.is_some()).count()
}

/// Also: a background generation holds a cancellable activity row until it is applied.
///
/// Never several pieces (decided): a normal area goes in exactly one request, with its cost known
/// before; an area too large for one request sends nothing, starts no activity and says so in the
/// decided words, from the estimate and from Generate alike.
#[test]
fn a_fill_is_one_request_or_none() {
    let dir = temp("onerequest");
    // slow enough that the job is still running when its activity row is looked at
    let slow: Filler = Arc::new(|w, h, rgb, mask, v| {
        std::thread::sleep(std::time::Duration::from_millis(300));
        (mean_filler())(w, h, rgb, mask, v)
    });
    let (mut s, mock) = session(&dir, slow);
    s.genfill.background = true;
    s.execute("genfill.setKey", &json!({"provider": "openai", "key": KEY})).unwrap();
    s.execute("genfill.consent", &json!({"provider": "openai", "accept": true})).unwrap();
    let id = s.active().unwrap();
    let (pw, ph) = (s.catalog.photo(id).unwrap().width, s.catalog.photo(id).unwrap().height);
    assert!(pw.max(ph) >= 3000, "the demo photo is large enough to test this ({pw}×{ph})");
    // too large: a broad stroke across most of the photo
    let pts: Vec<[f64; 2]> = (0..40).map(|i| [0.1 + i as f64 * 0.02, 0.2 + (i % 5) as f64 * 0.15]).collect();
    s.execute("spot.add", &json!({"mode": "generate", "points": pts, "size": 0.2})).unwrap();
    let e = s.execute("genfill.plan", &json!({"provider": "openai"})).unwrap_err().to_string();
    assert!(e.ends_with(crate::genfill::TOO_LARGE), "{e}");
    let e = s.execute("spot.generate", &json!({"provider": "openai"})).unwrap_err().to_string();
    assert!(e.ends_with("The model can't take an area this large. Please select a smaller area."), "{e}");
    assert_eq!(generations(&mock), 0, "nothing was sent");
    assert!(s.genfill.job.is_none() && !s.activity.list().iter().any(|t| t.kind == "genfill"), "no job, no activity row");
    // a normal area: one request, its cost known first
    s.execute("spot.delete", &json!({})).unwrap();
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.4, 0.5], [0.45, 0.5]], "size": 0.03})).unwrap();
    let plan = s.execute("genfill.plan", &json!({"provider": "openai"})).unwrap();
    assert_eq!(plan["requests"], 1, "{plan}");
    assert!(plan["usd"].as_f64().unwrap() > 0.0 && plan["crop"]["w"].as_u64().unwrap() > 0, "{plan}");
    assert_eq!(s.execute("spot.generate", &json!({"provider": "openai", "prompt": "a lawn"})).unwrap()["started"], true);
    // a background generation holds a cancellable activity row until it is applied, one at a time
    let rows = s.activity.list();
    assert!(rows.iter().any(|t| t.kind == "genfill" && t.cancellable), "{:?}", rows.iter().map(|t| t.kind).collect::<Vec<_>>());
    assert!(s.execute("spot.generate", &json!({"provider": "openai"})).is_err(), "one at a time");
    let t0 = std::time::Instant::now();
    let done = loop {
        if let Some(r) = s.genfill_poll() {
            break r;
        }
        assert!(t0.elapsed().as_secs() < 60, "the generation never finished");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(done.is_ok(), "{done:?}");
    assert_eq!(generations(&mock), 1, "exactly one request");
    assert!(!s.activity.list().iter().any(|t| t.kind == "genfill"), "the row goes with the job");
    assert!(s.develop_of(id).unwrap().spots[0].shown_gen().is_some());
    let st = s.execute("genfill.status", &json!({})).unwrap();
    assert_eq!(st["last"]["ok"], true, "{st}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A session set up for `provider` with a 1-variation generation on a spot at (0.5, 0.5) done.
fn generated(tag: &str, provider: &str) -> (Session, MockServer, std::path::PathBuf) {
    let dir = temp(tag);
    let (mut s, mock) = session(&dir, blue());
    s.execute("genfill.setKey", &json!({"provider": provider, "key": KEY})).unwrap();
    s.execute("genfill.consent", &json!({"provider": provider, "accept": true})).unwrap();
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.5, 0.5]], "size": 0.04})).unwrap();
    s.execute("spot.generate", &json!({"provider": provider})).unwrap();
    (s, mock, dir)
}

fn shown(s: &Session, i: usize) -> Option<String> {
    let id = s.active().unwrap();
    s.develop_of(id).unwrap().spots.get(i).and_then(|sp| sp.shown_gen().map(|g| g.id.clone()))
}

fn posts(mock: &MockServer) -> usize {
    mock.seen().iter().filter(|x| x.method == "POST").count()
}

/// One paid generation, then everything that must never lose it (one test: each generation renders the full-size
/// demo photo, so the fixture is the cost).
///
/// - Test note 5: the brush size changed with the generated spot still selected resized it, and a nudge moved it,
///   and either hid the paid fill. The spot keeps its stroke; the fill stays.
/// - A spot an older build let be resized (its fill hidden) is put back where the fill was made, from the photo's
///   history when the fill has no record of its stroke.
/// - Every generation stays in the library and can be put on the photo again, also once no spot refers to it: from
///   its record, or (fills made before records were kept) from the photo's history.
/// - Generate Again keeps the earlier paid result: both are offered as variations, either can be shown.
#[test]
fn a_paid_generation_is_never_lost() {
    let (mut s, _mock, dir) = generated("keep", "stability");
    let id = s.active().unwrap();
    let g = shown(&s, 0).expect("generated");
    let blue_px = pixel(&mut s, 0.5, 0.5);
    let made = (*s.develop_of(id).unwrap()).clone();

    // a brush size or a nudge never hides it
    for p in [json!({"index": 0, "size": 0.004}), json!({"index": 0, "move": [0.003, -0.002]}), json!({"size": 0.01, "feather": 20})] {
        let e = s.execute("spot.update", &p).unwrap_err().to_string();
        assert!(e.contains("stays where it was made"), "{e}");
    }
    assert_eq!(shown(&s, 0), Some(g.clone()));
    assert_eq!(pixel(&mut s, 0.5, 0.5), blue_px, "refused updates leave the fill");
    // opacity and feather alone still apply
    s.execute("spot.update", &json!({"index": 0, "opacity": 80})).unwrap();

    // as an older build left it: resized, and no stroke kept in the fill
    let mut d = made.clone();
    d.spots[0].size = 0.004;
    d.spots[0].generated.as_mut().unwrap().made_for = None;
    s.set_develop(id, d, "Edit Spot").unwrap();
    assert!(shown(&s, 0).is_none());
    let r = s.execute("spot.putBack", &json!({"index": 0})).unwrap();
    assert_eq!(r["shown"], true, "{r}");
    assert_eq!(pixel(&mut s, 0.5, 0.5), blue_px, "put back");

    // restored from the library once no spot refers to it
    s.set_develop(id, made.clone(), "Edit Spot").unwrap();
    assert!(s.genfill_dir(id).join(format!("{g}.json")).is_file(), "the record is written with the patch");
    s.execute("spot.delete", &json!({"index": 0})).unwrap();
    assert_ne!(pixel(&mut s, 0.5, 0.5), blue_px);
    let list = s.execute("genfill.results", &json!({})).unwrap();
    // (the demo library is not on disk: its fills share a temporary folder with other tests)
    let one = list["results"].as_array().unwrap().iter().find(|r| r["id"] == g.as_str()).expect("listed");
    assert_eq!((one["spot"].is_null(), one["provider"].as_str()), (true, Some("stability")), "{one}");
    let r = s.execute("spot.restoreGenerated", &json!({"id": g})).unwrap();
    assert_eq!(shown(&s, r["index"].as_u64().unwrap() as usize), Some(g.clone()));
    assert_eq!(pixel(&mut s, 0.5, 0.5), blue_px, "restored from its record");
    // without its record: found in the photo's history
    std::fs::remove_file(s.genfill_dir(id).join(format!("{g}.json"))).unwrap();
    s.execute("spot.delete", &json!({})).unwrap();
    s.execute("spot.restoreGenerated", &json!({"id": g})).unwrap();
    assert_eq!(pixel(&mut s, 0.5, 0.5), blue_px, "restored from the history");
    assert!(s.execute("spot.restoreGenerated", &json!({"id": "../../etc/passwd"})).is_err());
    assert!(s.execute("spot.restoreGenerated", &json!({"id": "00000000000000ff"})).is_err());

    // Generate Again keeps the earlier result
    s.set_develop(id, made, "Edit Spot").unwrap();
    s.execute("spot.generate", &json!({"provider": "stability"})).unwrap();
    let second = shown(&s, 0).unwrap();
    assert_ne!(g, second);
    let r = s.execute("spot.variation", &json!({"delta": -1})).unwrap();
    assert_eq!((r["results"].as_u64(), r["result"].as_u64(), r["id"].as_str()), (Some(2), Some(0), Some(g.as_str())), "{r}");
    assert_eq!(shown(&s, 0), Some(g));
    s.execute("spot.variation", &json!({"result": 1})).unwrap();
    assert_eq!(shown(&s, 0), Some(second));
    let _ = std::fs::remove_dir_all(dir);
}

/// A generation that finishes after its spot was moved or deleted still lands (it is paid for): back on its stroke,
/// or on a spot of its own again.
#[test]
fn a_finished_generation_lands_whatever_happened_to_its_spot() {
    let dir = temp("lands");
    let slow: Filler = Arc::new(|w, h, rgb, mask, v| {
        std::thread::sleep(std::time::Duration::from_millis(150));
        (blue())(w, h, rgb, mask, v)
    });
    let (mut s, _mock) = session(&dir, slow);
    s.genfill.background = true;
    s.execute("genfill.setKey", &json!({"provider": "stability", "key": KEY})).unwrap();
    s.execute("genfill.consent", &json!({"provider": "stability", "accept": true})).unwrap();
    let wait = |s: &mut Session| {
        let t0 = std::time::Instant::now();
        loop {
            if let Some(r) = s.genfill_poll() {
                break r;
            }
            assert!(t0.elapsed().as_secs() < 60, "never finished");
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    };
    // moved while generating
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.5, 0.5]], "size": 0.04})).unwrap();
    s.execute("spot.generate", &json!({"provider": "stability"})).unwrap();
    s.execute("spot.update", &json!({"index": 0, "move": [0.05, 0.0]})).unwrap();
    let r = wait(&mut s).unwrap();
    assert!(r.warnings.iter().any(|w| w.contains("back where the fill was made")), "{:?}", r.warnings);
    assert_eq!(shown(&s, 0).as_deref(), Some(r.fill.id.as_str()));
    // deleted while generating
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.2, 0.3]], "size": 0.03})).unwrap();
    s.execute("spot.generate", &json!({"provider": "stability", "index": 1})).unwrap();
    s.execute("spot.delete", &json!({"index": 1})).unwrap();
    let r = wait(&mut s).unwrap();
    assert_eq!(shown(&s, 1).as_deref(), Some(r.fill.id.as_str()), "the spot came back with its fill");
    let _ = std::fs::remove_dir_all(dir);
}

/// Test note 5: a request failed after the service accepted it. Retry fetches that generation instead of paying for
/// a new one.
#[test]
fn retry_fetches_an_accepted_generation_instead_of_paying_again() {
    let dir = temp("resume");
    let (mut s, mock) = session(&dir, blue());
    s.execute("genfill.setKey", &json!({"provider": "bfl", "key": KEY})).unwrap();
    s.execute("genfill.consent", &json!({"provider": "bfl", "accept": true})).unwrap();
    s.execute("spot.add", &json!({"mode": "generate", "points": [[0.5, 0.5]], "size": 0.04})).unwrap();
    mock.drop_next("/v1/get_result", 3);
    let e = s.execute("spot.generate", &json!({"provider": "bfl"})).unwrap_err().to_string();
    assert!(e.contains("Retry") && e.contains("without paying again"), "{e}");
    assert_eq!(posts(&mock), 1);
    s.execute("spot.generate", &json!({"provider": "bfl"})).unwrap();
    assert_eq!(posts(&mock), 1, "fetched, not bought again");
    assert!(shown(&s, 0).is_some());
    assert!(s.genfill.resume.is_none());
    let _ = std::fs::remove_dir_all(dir);
}
