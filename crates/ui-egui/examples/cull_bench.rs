//! Culling a folder as the Import tab does, headless and in real time: browse FOLDER, open the
//! loupe (with the filmstrip), step through photos with a dwell each, then go back to the grid a
//! few screens down. Reports, per phase, the loupe's time to a picture (any / full size / sharp),
//! the filmstrip's and the grid's empty cells and time to first pixels, and what the background
//! preparation did meanwhile.
//! `cargo run --release -p lightcraft-ui-egui --example cull_bench -- FOLDER LIBRARY_DIR [STEPS] [DWELL_MS]`
//! `CULL_PREP='{"previews":false,"ahead":0}'` changes the Culling mode's background preparation.
use lightcraft_engine::Session;
use lightcraft_ui_egui::{LightcraftApp, Services, headless::Headless};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(folder), Some(dir)) = (args.next(), args.next()) else {
        eprintln!("usage: cull_bench FOLDER LIBRARY_DIR [STEPS] [DWELL_MS]");
        return;
    };
    let steps: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let dwell: f64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(700.0);
    let mut s = Session::new().with_fs();
    if let Err(e) = s.open_library(&dir, false) {
        eprintln!("{dir}: {e}");
        std::process::exit(1);
    }
    let t0 = Instant::now();
    let ms = move || t0.elapsed().as_secs_f64() * 1e3;
    if let Ok(patch) = std::env::var("CULL_PREP") {
        // (a run with other settings than the ones asked for would time the wrong thing)
        let set = serde_json::from_str::<Value>(&patch)
            .map_err(|e| e.to_string())
            .and_then(|v| s.execute("prefs.setBackgroundPrep", &json!({"settings": {"culling": v}})).map_err(|e| e.to_string()));
        match set {
            // (the library's own settings would win over the app-wide ones set here: CodeRabbit on #3080)
            Ok(v) if v["own"].as_bool() == Some(true) => {
                eprintln!("CULL_PREP={patch}: this library has its own preparation settings, which take precedence");
                std::process::exit(1);
            }
            Ok(v) => eprintln!("prep {patch}: {}", v["settings"]["culling"]),
            Err(e) => {
                eprintln!("CULL_PREP={patch}: {e}");
                std::process::exit(1);
            }
        }
    }
    match s.execute("library.browse", &json!({"path": folder})) {
        Ok(v) => eprintln!("{:9.1} browse: {v}", ms()),
        Err(e) => {
            // (the phases below would measure an empty view, not this folder)
            eprintln!("{folder}: browse failed: {e}");
            std::process::exit(1);
        }
    }
    let mut h = Headless::new(LightcraftApp::new(s, Services::default()), [1600.0, 1000.0], 1.0);
    // the first pass copies the app's saved settings into the session: those asked for are the
    // app's too, so the run times them (CodeRabbit on #3080)
    h.app.ui.settings.background_prep = h.app.session.prep.app;
    let req = |h: &mut Headless, m: &str, p: Value| h.request(m, p, Duration::from_secs(60));
    let n = h.app.session.visible_cloned().len();
    if n == 0 {
        // (nothing to step through: the numbers would be empty, not this folder's; CodeRabbit on #3577)
        eprintln!("{folder}: no photos to cull");
        std::process::exit(1);
    }
    let start_at = n / 10;
    let first = h.app.session.visible_cloned().get(start_at).map(|p| p.0).unwrap_or(1);
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [first]}}));
    req(&mut h, "ui.set", json!({"view": "detail"}));
    h.app.thumb_first.reset();
    let frame_loop = |h: &mut Headless, until: f64| {
        let begin = Instant::now();
        while begin.elapsed().as_secs_f64() * 1e3 < until {
            let f = Instant::now();
            h.step();
            std::thread::sleep(Duration::from_millis(16).saturating_sub(f.elapsed()));
        }
    };
    frame_loop(&mut h, dwell);
    for _ in 0..steps {
        req(&mut h, "engine.execute", json!({"command": "library.next"}));
        frame_loop(&mut h, dwell);
    }
    if let Ok(shot) = std::env::var("CULL_SHOT") {
        let r = req(&mut h, "ui.screenshot", json!({"path": shot, "headless": true}));
        eprintln!("screenshot: {r}");
        if r["ok"] != true {
            eprintln!("CULL_SHOT={shot}: the screenshot was not written");
            std::process::exit(1);
        }
    }
    let lt = h.app.loupe_timing.to_json();
    let st = lt["steps"].as_array().cloned().unwrap_or_default();
    // (the loupe keeps only its latest steps: the numbers below cover those)
    let total = lt["total"].as_u64().unwrap_or(st.len() as u64);
    if (st.len() as u64) < total {
        eprintln!("          [loupe] {total} steps timed, the latest {} kept: the numbers cover those", st.len());
    }
    let col = |k: &str| -> (Vec<f64>, usize) {
        let v: Vec<f64> = st.iter().filter_map(|x| x[k].as_f64()).collect();
        let missing = st.len() - v.len();
        (v, missing)
    };
    let (mut any, any_m) = col("anyMs");
    let (mut full, full_m) = col("fullMs");
    let (mut sharp, sharp_m) = col("sharpMs");
    let srcs: Vec<String> =
        st.iter().map(|x| format!("{}/{}", x["anySource"].as_str().unwrap_or("?"), x["fullSource"].as_str().unwrap_or("?"))).collect();
    let f = h.app.thumb_first.summary();
    eprintln!(
        "{:9.1} [loupe, {} of {total} steps, dwell {dwell} ms] any p50 {:.0} p90 {:.0} (never {any_m}) · full p50 {:.0} p90 {:.0} (never {full_m}) · sharp p50 {:.0} p90 {:.0} (never {sharp_m})",
        ms(),
        st.len(),
        pct(&mut any, 0.5),
        pct(&mut any, 0.9),
        pct(&mut full, 0.5),
        pct(&mut full, 0.9),
        pct(&mut sharp, 0.5),
        pct(&mut sharp, 0.9),
    );
    eprintln!(
        "          [filmstrip] cells {} · first pixels p50 {:.0} p90 {:.0} max {:.0} · never filled {} · empty on screen now {} · empty frame-cells {}",
        f.seen, f.p50, f.p90, f.max, f.waiting, f.empty_now, f.empty_cell_frames
    );
    eprintln!(
        "          renderer: queued {} running {} pending {:?}",
        h.app.renderer.queued(),
        h.app.renderer.in_flight(),
        h.app.renderer.pending_slots()
    );
    eprintln!("          sources (any/full): {}", srcs.join(" "));
    eprintln!("          prep: {}", h.app.session.execute("prefs.backgroundPrep", &json!({})).map(|v| v["building"].to_string()).unwrap_or_default());
    // the grid, a few screens on
    req(&mut h, "engine.execute", json!({"command": "view.photoGrid"}));
    h.app.ui.thumb_size = 220.0;
    let at = (start_at + steps + 200).min(n.saturating_sub(1));
    let id = h.app.session.visible_cloned().get(at).map(|p| p.0).unwrap_or(1);
    req(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}));
    h.app.thumb_first.reset();
    let begin = ms();
    let mut full_at = None;
    while ms() - begin < 10_000.0 {
        let fr = Instant::now();
        h.step();
        let s = h.app.thumb_first.summary();
        if full_at.is_none() && s.seen > 0 && s.empty_now == 0 {
            full_at = Some(ms() - begin);
            break;
        }
        std::thread::sleep(Duration::from_millis(16).saturating_sub(fr.elapsed()));
    }
    let g = h.app.thumb_first.summary();
    eprintln!(
        "{:9.1} [grid] cells {} · first pixels p50 {:.0} p90 {:.0} max {:.0} · every cell filled after {} · empty frame-cells {}",
        ms(),
        g.seen,
        g.p50,
        g.p90,
        g.max,
        full_at.map_or("never (10 s)".to_string(), |f| format!("{f:.0} ms")),
        g.empty_cell_frames
    );
    eprintln!("          prep: {}", h.app.session.execute("prefs.backgroundPrep", &json!({})).map(|v| v["building"].to_string()).unwrap_or_default());
    let _ = h.app.session.execute("library.cancelPreviews", &json!({}));
    std::process::exit(0);
}
