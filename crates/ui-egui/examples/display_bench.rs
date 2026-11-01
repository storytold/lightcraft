//! What an edited gallery looks like while stepping, headless and in real time (notes 3, 10, 11, 30
//! and 31 of 2026-10-24): browse FOLDER (read only: nothing is imported or written there), edit N
//! photos (exposure and a crop, kept in LIBRARY_DIR only), then step through them in the loupe:
//! a first pass, a revisit, a pass after the settings of all of them changed off screen (a sync),
//! and the same zoomed to 100 %. Reports per pass the time to the first picture of each step and
//! where it came from, frames that showed the unedited camera JPEG, render jobs per pass, and the
//! grid's empty cells.
//! `cargo run --release -p lightcraft-ui-egui --example display_bench -- FOLDER LIBRARY_DIR [N] [START]`
use lightcraft_catalog::PhotoId;
use lightcraft_engine::Session;
use lightcraft_ui_egui::render::Slot;
use lightcraft_ui_egui::{LightcraftApp, Services, headless::Headless};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn pct(v: &[f64], p: f64) -> f64 {
    let mut v = v.to_vec();
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

/// Frames at about 60 a second for `ms`, each frame's loupe picture handed to `seen`.
fn run(h: &mut Headless, ms: f64, seen: &mut dyn FnMut(&Headless, f64)) {
    let begin = Instant::now();
    while begin.elapsed().as_secs_f64() * 1e3 < ms {
        let f = Instant::now();
        h.step();
        seen(h, begin.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(Duration::from_millis(16).saturating_sub(f.elapsed()));
    }
}

/// Render jobs finished so far (every slot: the loupe, its stand-ins and the 1:1 window).
fn full_renders(h: &Headless, _slot: &str) -> usize {
    usize::try_from(h.app.renderer.completed).unwrap_or(usize::MAX)
}

#[derive(Default)]
struct Pass {
    /// ms to the first picture of the photo, and its source
    first: Vec<(f64, &'static str)>,
    /// ms to the edit's picture (render, cached, last)
    edit: Vec<f64>,
    camera_frames: usize,
    blank_frames: usize,
    steps: usize,
}

/// Step through `ids` (from the one on screen) dwelling `dwell` ms each; zoomed in, with Next /
/// Previous Photo (`step`), which keep the zoom, else by selecting each.
fn pass(h: &mut Headless, ids: &[PhotoId], dwell: f64, zoomed: Option<&str>) -> Pass {
    let mut out = Pass::default();
    for (k, id) in ids.iter().enumerate() {
        let r = match zoomed {
            Some(step) if k > 0 => h.request("engine.execute", json!({"command": step}), Duration::from_secs(60)),
            _ => h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), Duration::from_secs(60)),
        };
        if zoomed.is_some() && k == 0 && h.app.ui.zoom == lightcraft_ui_egui::state::Zoom::Fit {
            h.request("engine.execute", json!({"command": "view.zoom100"}), Duration::from_secs(60));
        }
        let zoomed = zoomed.is_some();
        if r["ok"] != true {
            eprintln!("select {}: {r}", id.0);
        }
        let (mut first, mut edit) = (None, None);
        let (mut camera, mut blank) = (0, 0);
        run(h, dwell, &mut |h, t| {
            let shown = h.app.loupe_shown.filter(|(p, _)| p == id).map(|(_, s)| s);
            let window =
                h.app.region_view.is_some_and(|v| v.photo == *id && h.app.renderer.textures.get(&Slot::Region).is_some_and(|x| x.key == v.key));
            match shown {
                Some("none") | None => blank += 1,
                Some("embedded") => camera += 1,
                Some(s) => {
                    first.get_or_insert((t, s));
                    if (!zoomed && matches!(s, "render" | "cached" | "last")) || (zoomed && window) {
                        edit.get_or_insert(t);
                    }
                }
            }
            if let (Some("embedded"), None) = (shown, first) {
                first = Some((t, "embedded"));
            }
        });
        if k > 0 {
            out.steps += 1;
            out.first.push(first.unwrap_or((f64::INFINITY, "never")));
            out.edit.push(edit.unwrap_or(f64::INFINITY));
            out.camera_frames += camera;
            out.blank_frames += blank;
        }
    }
    out
}

fn report(name: &str, p: &Pass, renders: usize) {
    let firsts: Vec<f64> = p.first.iter().map(|f| f.0).collect();
    let mut srcs: std::collections::BTreeMap<&str, usize> = Default::default();
    for f in &p.first {
        *srcs.entry(f.1).or_default() += 1;
    }
    let never = p.edit.iter().filter(|e| !e.is_finite()).count();
    eprintln!(
        "[{name}] {} steps · first picture p50 {:.0} p90 {:.0} ms {srcs:?} · edit shown p50 {:.0} p90 {:.0} ms (never {never}) · camera-JPEG frames {} · blank frames {} · render jobs {renders}",
        p.steps,
        pct(&firsts, 0.5),
        pct(&firsts, 0.9),
        pct(&p.edit, 0.5),
        pct(&p.edit, 0.9),
        p.camera_frames,
        p.blank_frames,
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(folder), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: display_bench FOLDER LIBRARY_DIR [N] [START]");
        return;
    };
    let n: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let start: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);
    let mut s = Session::new().with_fs();
    // nothing is written next to the photos
    s.xmp.auto_write = false;
    if let Err(e) = s.open_library(&dir, false) {
        eprintln!("{dir}: {e}");
        return;
    }
    let r = s.execute("library.browse", &json!({"path": folder}));
    eprintln!("browse: {}", r.map(|v| v["count"].to_string()).unwrap_or_else(|e| e.to_string()));
    let all = s.visible_cloned();
    let ids: Vec<PhotoId> = all.iter().skip(start).take(n).copied().collect();
    // an edited gallery: exposure and a crop on each
    for id in &ids {
        let Some(p) = s.catalog.photo(*id) else { continue };
        let mut d = (*p.develop).clone();
        d.light.exposure = 0.6;
        d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.1, y0: 0.08, x1: 0.9, y1: 0.92 };
        let _ = s.set_develop(*id, d, "Bench edit");
    }
    let mut h = Headless::new(LightcraftApp::new(s, Services::default()), [1600.0, 1000.0], 1.0);
    let req = |h: &mut Headless, m: &str, p: Value| h.request(m, p, Duration::from_secs(60));
    req(&mut h, "ui.set", json!({"view": "detail"}));
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0].0]}}));
    run(&mut h, 3000.0, &mut |_, _| {});

    let r0 = full_renders(&h, "main");
    let p = pass(&mut h, &ids, 2500.0, None);
    report("first pass, 2.5 s a photo", &p, full_renders(&h, "main") - r0);

    let back: Vec<PhotoId> = ids.iter().rev().copied().collect();
    let r0 = full_renders(&h, "main");
    let p = pass(&mut h, &back, 600.0, None);
    report("revisit, 0.6 s a photo", &p, full_renders(&h, "main") - r0);

    // a sync while the user is on the first photo: every other photo changes off screen
    let on = ids[0];
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [on.0]}}));
    for id in ids.iter().skip(1) {
        let Some(p) = h.app.session.catalog.photo(*id) else { continue };
        let mut d = (*p.develop).clone();
        d.light.exposure = -0.4;
        let _ = h.app.session.set_develop(*id, d, "Bench sync");
    }
    let wait: f64 = std::env::var("BENCH_SYNC_WAIT_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(30_000.0);
    run(&mut h, wait, &mut |_, _| {});
    eprintln!(
        "after the sync, prep: {}",
        h.app.session.execute("prefs.backgroundPrep", &json!({})).map(|v| v["building"].to_string()).unwrap_or_default()
    );
    let r0 = full_renders(&h, "main");
    let p = pass(&mut h, &ids, 600.0, None);
    report(&format!("after a sync, waited {:.0} s, 0.6 s a photo", wait / 1000.0), &p, full_renders(&h, "main") - r0);

    // 100 %: forward (windows made ahead), then back (kept windows)
    let env = |k: &str, d: f64| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
    let zoom: Vec<PhotoId> = ids.iter().take(env("BENCH_ZOOM_N", 11.0) as usize).copied().collect();
    let zoom_dwell = env("BENCH_ZOOM_DWELL_MS", 3000.0);
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [zoom[0].0]}}));
    req(&mut h, "engine.execute", json!({"command": "view.zoom100"}));
    run(&mut h, 4000.0, &mut |_, _| {});
    eprintln!("zoomed: {:?} window {:?}", h.app.ui.zoom, h.app.region_view.map(|v| (v.photo, v.full, v.window)));
    let r0 = full_renders(&h, "region");
    let p = pass(&mut h, &zoom, zoom_dwell, Some("library.next"));
    report(&format!("100 %, forward, {:.1} s a photo", zoom_dwell / 1000.0), &p, full_renders(&h, "region") - r0);
    let back: Vec<PhotoId> = zoom.iter().rev().copied().collect();
    let r0 = full_renders(&h, "region");
    let p = pass(&mut h, &back, 800.0, Some("library.previous"));
    report("100 %, back, 0.8 s a photo", &p, full_renders(&h, "region") - r0);

    // the grid over the edited photos
    req(&mut h, "engine.execute", json!({"command": "view.photoGrid"}));
    h.app.ui.thumb_size = 220.0;
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0].0]}}));
    h.app.thumb_first.reset();
    run(&mut h, 4000.0, &mut |_, _| {});
    let g = h.app.thumb_first.summary();
    eprintln!(
        "[grid] cells {} · first pixels p50 {:.0} p90 {:.0} max {:.0} · empty frame-cells {} · still empty {}",
        g.seen, g.p50, g.p90, g.max, g.empty_cell_frames, g.empty_now
    );
    let pc = h.app.session.execute("library.previewProgress", &json!({}));
    eprintln!("preview build: {}", pc.map(|v| v.to_string()).unwrap_or_default());
    let _ = h.app.session.execute("library.cancelPreviews", &json!({}));
    std::process::exit(0);
}
