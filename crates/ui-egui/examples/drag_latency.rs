//! Slider-drag latency through the whole app: the real session, renderer, job pool and GPU (when
//! there is one), driven by a headless host at 60 frames a second, as a window with vsync would be.
//!
//! `cargo run --release -p lightcraft-ui-egui --example drag_latency -- LIBRARY_DIR PHOTO [CONTROL…]`
//! Env: `SIZE=1920x1200` (points), `SCALE=1`, `TICKS=30`, `GPU=0` for the CPU pipeline.
//!
//! Per control: a drag of `TICKS` value changes, one per frame; for each tick the frames (and ms)
//! until its draft became the loupe texture, how many ticks were never shown, the UI thread's
//! frame time, and after release the time until the full-quality render is the texture. (A window
//! presents the texture one frame later: add ~16.7 ms for photons.)
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lightcraft_engine::Session;
use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::render::Slot;
use lightcraft_ui_egui::{LightcraftApp, Services};
use serde_json::json;

const FRAME: Duration = Duration::from_micros(16_667);

const CONTROLS: &[(&str, f64, f64)] = &[
    ("light.exposure", 0.0, 1.0),
    ("light.contrast", 0.0, 50.0),
    ("light.highlights", 0.0, -60.0),
    ("light.shadows", 0.0, 60.0),
    ("light.whites", 0.0, 40.0),
    ("light.blacks", 0.0, -40.0),
    ("wb.temp", 5000.0, 6500.0),
    ("wb.tint", 0.0, 20.0),
    ("color.vibrance", 0.0, 40.0),
    ("color.saturation", 0.0, 30.0),
    ("curve.shadows", 0.0, 30.0),
    ("mixer.orange.hue", 0.0, 30.0),
    ("mixer.blue.sat", 0.0, -40.0),
    ("grading.shadows.sat", 0.0, 30.0),
    ("calibration.redHue", 0.0, 30.0),
    ("effects.texture", 0.0, 40.0),
    ("effects.clarity", 0.0, 40.0),
    ("effects.dehaze", 0.0, 30.0),
    ("vignette.amount", 0.0, -40.0),
    ("grain.amount", 0.0, 30.0),
    ("detail.sharpenAmount", 40.0, 90.0),
    ("detail.nrLuminance", 0.0, 40.0),
    ("detail.nrColor", 25.0, 60.0),
    ("optics.distortion", 0.0, 30.0),
    ("optics.vignetting", 0.0, 50.0),
    ("optics.defringePurple", 0.0, 5.0),
    ("geometry.vertical", 0.0, 20.0),
];

/// Step one frame at the 60 Hz cadence that started at `t0`.
fn frame(h: &mut Headless, t0: Instant, n: &mut u32) -> f64 {
    *n += 1;
    let due = t0 + FRAME * *n;
    if let Some(d) = due.checked_duration_since(Instant::now()) {
        std::thread::sleep(d);
    }
    let t = Instant::now();
    h.step();
    t.elapsed().as_secs_f64() * 1e3
}

/// The view slot measured: the zoom window with `ZOOM=<percent>` (set after opening), else the loupe.
fn watch() -> Slot {
    if ZOOMED.load(std::sync::atomic::Ordering::Relaxed) { Slot::Region } else { Slot::Main }
}

static ZOOMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn settle(h: &mut Headless, id: lightcraft_catalog::PhotoId) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(120) {
        h.step();
        let r = &h.app.renderer;
        let done = r.textures.get(&watch()).is_some_and(|t| t.photo == id && Some(t.key) == r.wanted(watch())) && !r.is_pending(watch());
        if done && !h.busy() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    eprintln!("settle: timed out");
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (lib, file) = (args.first().expect("LIBRARY_DIR"), args.get(1).expect("PHOTO"));
    let only: Vec<&str> = args[2..].iter().map(String::as_str).collect();
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (w, hgt) = env("SIZE", "1920x1200").split_once('x').map(|(a, b)| (a.parse::<f32>().unwrap(), b.parse::<f32>().unwrap())).unwrap();
    let scale: f32 = env("SCALE", "1").parse().unwrap();
    let ticks: u32 = env("TICKS", "30").parse().unwrap();
    let gpu = env("GPU", "1") != "0";

    let mut s = Session::new().with_fs().with_system_clock();
    s.open_library(lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [file]})).unwrap();
    let id = r["imported"].as_array().and_then(|a| a.first()).and_then(|v| v.as_u64()).map(lightcraft_catalog::PhotoId).unwrap_or_else(|| {
        // already in the library
        let name = std::path::Path::new(file).file_name().unwrap().to_string_lossy().to_string();
        s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).expect("photo in library")
    });
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let mut app = LightcraftApp::new(s, Services::default());
    app.ui.settings.gpu = gpu;
    let mut h = Headless::new(app, [w, hgt], scale);
    let t = Duration::from_secs(60);
    h.request("ui.set", json!({"view": "detail", "right": "edit", "filmstrip": true}), t);
    let t_open = Instant::now();
    settle(&mut h, id);
    if let Ok(z) = std::env::var("ZOOM") {
        let pct: f64 = z.parse().unwrap();
        h.app.run("view.navigate", json!({"zoom": {"percent": pct}, "pan": [0.5, 0.5]})).unwrap();
        for _ in 0..5 {
            h.step();
        }
        ZOOMED.store(true, std::sync::atomic::Ordering::Relaxed);
        let t = Instant::now();
        settle(&mut h, id);
        eprintln!("zoomed to {pct} % in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
    }
    let tex = h.app.renderer.textures.get(&watch()).map(|t| t.size);
    eprintln!("opened in {:.0} ms, loupe texture {tex:?}, gpu {gpu}", t_open.elapsed().as_secs_f64() * 1e3);

    println!("control,shown/ticks,lat_frames_med,lat_ms_med,lat_ms_p90,lat_ms_max,frame_ms_med,frame_ms_max,final_ms,draft_px,final_px");
    // the controls named (in that order, repeats allowed), else all of them
    let list: Vec<(&str, f64, f64)> =
        if only.is_empty() { CONTROLS.to_vec() } else { only.iter().filter_map(|n| CONTROLS.iter().find(|c| c.0 == *n).copied()).collect() };
    for (control, a, b) in list {
        let mut asked: HashMap<u64, (u32, Instant)> = HashMap::new();
        let mut lat_frames = Vec::new();
        let mut lat_ms = Vec::new();
        let mut frame_ms = Vec::new();
        let mut shown_key = h.app.renderer.textures.get(&watch()).map(|t| t.key);
        let mut draft_px = None;
        let t0 = Instant::now();
        let mut n = 0u32;
        h.app.run("develop.beginInteraction", json!({"label": control})).unwrap();
        #[allow(clippy::too_many_arguments)]
        fn observe(
            h: &Headless,
            n: u32,
            asked: &HashMap<u64, (u32, Instant)>,
            shown_key: &mut Option<u64>,
            lat_frames: &mut Vec<f64>,
            lat_ms: &mut Vec<f64>,
            draft_px: &mut Option<[usize; 2]>,
        ) {
            let r = &h.app.renderer;
            if let Some(tex) = r.textures.get(&watch())
                && Some(tex.key) != *shown_key
            {
                *shown_key = Some(tex.key);
                if let Some((k, at)) = asked.get(&tex.key) {
                    lat_frames.push((n - k) as f64);
                    lat_ms.push(at.elapsed().as_secs_f64() * 1e3);
                    *draft_px = Some(tex.size);
                }
            }
        }
        for i in 1..=ticks {
            let v = a + (b - a) * i as f64 / ticks as f64;
            // the event arrives at the start of the frame (as input does)
            let due = t0 + FRAME * (n + 1);
            if let Some(d) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(d);
            }
            let start = Instant::now();
            h.app.run("develop.set", json!({"control": control, "value": v})).unwrap();
            n += 1;
            h.step();
            frame_ms.push(start.elapsed().as_secs_f64() * 1e3);
            if let Some(k) = h.app.renderer.wanted(watch()) {
                asked.entry(k).or_insert((n, start));
            }
            observe(&h, n, &asked, &mut shown_key, &mut lat_frames, &mut lat_ms, &mut draft_px);
        }
        // release
        let due = t0 + FRAME * (n + 1);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        let release = Instant::now();
        h.app.run("develop.endInteraction", json!({})).unwrap();
        let mut final_ms = f64::NAN;
        let mut final_px = None;
        while release.elapsed() < Duration::from_secs(60) {
            let fm = frame(&mut h, t0, &mut n);
            frame_ms.push(fm);
            observe(&h, n, &asked, &mut shown_key, &mut lat_frames, &mut lat_ms, &mut draft_px);
            let r = &h.app.renderer;
            if h.app.session.interaction.is_none()
                && let Some(tex) = r.textures.get(&watch())
                && Some(tex.key) == r.wanted(watch())
                && !r.is_pending(watch())
            {
                final_ms = release.elapsed().as_secs_f64() * 1e3;
                final_px = Some(tex.size);
                break;
            }
        }
        let shown = lat_ms.len();
        let fmax = frame_ms.iter().cloned().fold(0.0, f64::max);
        let lmax = lat_ms.iter().cloned().fold(0.0, f64::max);
        println!(
            "{control},{shown}/{ticks},{:.0},{:.1},{:.1},{lmax:.1},{:.1},{fmax:.1},{final_ms:.1},{:?},{:?}",
            pct(&mut lat_frames, 0.5),
            pct(&mut lat_ms, 0.5),
            pct(&mut lat_ms, 0.9),
            pct(&mut frame_ms, 0.5),
            draft_px.unwrap_or([0, 0]),
            final_px.unwrap_or([0, 0]),
        );
        // back to where it was (one undo step), settled
        h.app.run("develop.set", json!({"control": control, "value": a})).unwrap();
        settle(&mut h, id);
        std::thread::sleep(Duration::from_millis(200));
    }
}
