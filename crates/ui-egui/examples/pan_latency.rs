//! Panning a zoomed photo through the whole app: the real session, renderer, job pool and GPU
//! (when there is one), driven by a headless host at 60 frames a second.
//!
//! `cargo run --release -p lightcraft-ui-egui --example pan_latency -- LIBRARY_DIR PHOTO`
//! Env: `SIZE=1920x1200` (points), `SCALE=1`, `ZOOM=100`, `FRAMES=180` (drag length),
//! `SPEED=12` (points per frame), `START=now` (drag at once, not after the view is still), `GPU=0`
//! for the CPU pipeline.
//!
//! The photo is opened at `ZOOM` %, left to settle, then dragged with the primary button: left for
//! a third of the frames, down for a third, then right. Per frame: the UI thread's time and how much
//! of the photo on screen is drawn sharp (from window renders) rather than from the whole-frame
//! render magnified. After the release, the time until all of it is sharp.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use egui::{Event, PointerButton, vec2};
use lightcraft_engine::Session;
use lightcraft_ui_egui::headless::Headless;
use lightcraft_ui_egui::{LightcraftApp, Services};
use serde_json::json;

const FRAME: Duration = Duration::from_micros(16_667);

fn pct(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (lib, file) = (args.first().expect("LIBRARY_DIR"), args.get(1).expect("PHOTO"));
    let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
    let (w, hgt) = env("SIZE", "1920x1200").split_once('x').map(|(a, b)| (a.parse::<f32>().unwrap(), b.parse::<f32>().unwrap())).unwrap();
    let scale: f32 = env("SCALE", "1").parse().unwrap();
    let zoom: f64 = env("ZOOM", "100").parse().unwrap();
    let frames: usize = env("FRAMES", "180").parse().unwrap();
    let speed: f32 = env("SPEED", "12").parse().unwrap();
    let gpu = env("GPU", "1") != "0";

    let mut s = Session::new().with_fs().with_system_clock();
    s.open_library(lib, false).unwrap();
    let r = s.execute("library.import", &json!({"paths": [file]})).unwrap();
    let id = r["imported"].as_array().and_then(|a| a.first()).and_then(|v| v.as_u64()).map(lightcraft_catalog::PhotoId).unwrap_or_else(|| {
        let name = std::path::Path::new(file).file_name().unwrap().to_string_lossy().to_string();
        s.catalog.photos().find(|p| p.file_name == name).map(|p| p.id).expect("photo in library")
    });
    s.execute("library.select", &json!({"ids": [id.0]})).unwrap();
    let mut app = LightcraftApp::new(s, Services::default());
    app.ui.settings.gpu = gpu;
    let mut h = Headless::new(app, [w, hgt], scale);
    let t = Duration::from_secs(120);
    h.request("ui.set", json!({"view": "detail", "right": "edit", "filmstrip": true}), t);
    h.settle(t);
    h.app.run("view.navigate", json!({"zoom": {"percent": zoom}, "pan": [0.5, 0.5]})).unwrap();
    let t_zoom = Instant::now();
    let sharp_ok = |h: &Headless| h.app.loupe_sharp.is_some_and(|f| f >= 0.999);
    // `START=now`: the drag starts as soon as the view is sharp, before the windows around it are made
    let wait_ahead = env("START", "still") != "now";
    h.step_until(t, |h| sharp_ok(h) && (!wait_ahead || !h.busy()));
    eprintln!("zoomed to {zoom} % and sharp in {:.0} ms (gpu {gpu}, {:?})", t_zoom.elapsed().as_secs_f64() * 1e3, h.app.loupe_sharp);
    let renders_before = h.app.renderer.window_renders();
    let pan0 = h.app.ui.pan;

    let at = h.app.image_rect.expect("photo").intersect(h.app.canvas_rect.expect("canvas")).center();
    let mut pos = at;
    let mut frame_ms = Vec::new();
    let mut sharp = Vec::new();
    let mut not_drawn = 0usize;
    let t0 = Instant::now();
    h.app.synthetic.push(Event::PointerMoved(pos));
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    for i in 0..frames {
        let dir = match i * 3 / frames.max(1) {
            0 => vec2(-1.0, 0.0),
            1 => vec2(0.0, -1.0),
            _ => vec2(1.0, 0.0),
        };
        pos += dir * speed;
        let due = t0 + FRAME * (i as u32 + 1);
        if let Some(d) = due.checked_duration_since(Instant::now()) {
            std::thread::sleep(d);
        }
        h.app.synthetic.push(Event::PointerMoved(pos));
        let s = Instant::now();
        h.step();
        frame_ms.push(s.elapsed().as_secs_f64() * 1e3);
        sharp.push(h.app.loupe_sharp.unwrap_or(0.0) as f64);
        if h.app.loupe_shown.is_none_or(|(_, w)| w == "none") {
            not_drawn += 1;
        }
    }
    let windows = h.app.renderer.window_renders() - renders_before;
    h.app.synthetic.push(Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    let release = Instant::now();
    h.step();
    let settled = h.step_until(t, sharp_ok);
    eprintln!("pan {pan0:?} -> {:?}", h.app.ui.pan);
    let to_sharp = release.elapsed().as_secs_f64() * 1e3;
    let over16 = frame_ms.iter().filter(|m| **m > 16.0).count();
    let over50 = frame_ms.iter().filter(|m| **m > 50.0).count();
    println!(
        "frames {frames}: ui median {:.1} / p95 {:.1} / max {:.1} ms, >16 ms {over16}, >50 ms {over50}; \
         sharp share min {:.2} / median {:.2} / mean {:.2}; frames with nothing drawn {not_drawn}; \
         window renders asked during the drag {windows}; sharp {to_sharp:.0} ms after release (settled {settled})",
        pct(&frame_ms, 0.5),
        pct(&frame_ms, 0.95),
        pct(&frame_ms, 1.0),
        pct(&sharp, 0.0),
        pct(&sharp, 0.5),
        sharp.iter().sum::<f64>() / sharp.len().max(1) as f64,
    );
}
