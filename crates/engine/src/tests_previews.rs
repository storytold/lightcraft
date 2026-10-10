//! True 1:1 (P1.6): a window of the loupe at 1:1 has the pixels of the full-size export, and the
//! preview store's 1:1 previews, settings and discards.

use dac_pipeline::{OutputDepth, OutputSpace, PixelWindow};
use serde_json::json;

use crate::Session;
use crate::cmd::previews::{DiscardAfter, discard_full};

/// The largest per-channel difference between the `visible` pixels of `part` (a render of
/// window `win`) and the same pixels of `full`.
fn max_diff(full: &dac_raster::Rgba8, part: &dac_raster::Rgba8, win: PixelWindow, visible: PixelWindow) -> u8 {
    let mut worst = 0u8;
    for y in visible.y..visible.y + visible.h {
        for x in visible.x..visible.x + visible.w {
            let a = part.data[(y - win.y) * part.width + (x - win.x)];
            let b = full.data[y * full.width + x];
            for c in 0..3 {
                worst = worst.max(a[c].abs_diff(b[c]));
            }
        }
    }
    worst
}

/// Render photo `id` at full size for export and windows of its 1:1 view, and compare them.
fn check_one_to_one(s: &mut Session, id: dac_catalog::PhotoId) {
    let p = s.catalog.photo(id).unwrap().clone();
    let export = s.render_export(id, p.width as usize, p.height as usize, OutputSpace::Srgb, OutputDepth::U8).unwrap().image;
    let (w, h) = (export.width, export.height);
    assert!(w.max(h) >= (p.width.max(p.height) as usize) - 1, "export is full size: {w}×{h} of {}×{}", p.width, p.height);
    // the 1:1 frame is the export's frame: the loupe asks for windows of (w, h), with the margin
    // of context it renders around what is on screen (as `region::margin_for` in the UI)
    let margin = ((w.max(h) as f64 * 0.045).min(768.0) as usize).div_ceil(256).max(1) * 256;
    for (fx, fy) in [(0.0, 0.0), (0.37, 0.52), (0.8, 0.7)] {
        let x = ((w as f64 * fx) as usize).min(w.saturating_sub(512));
        let y = ((h as f64 * fy) as usize).min(h.saturating_sub(384));
        let visible = PixelWindow { x, y, w: 512.min(w), h: 384.min(h) };
        let (wx, wy) = (x.saturating_sub(margin), y.saturating_sub(margin));
        let win = PixelWindow { x: wx, y: wy, w: (x + visible.w + margin).min(w) - wx, h: (y + visible.h + margin).min(h) - wy };
        let job = s.region_job(id, w, h, win, true).expect("a window of the 1:1 view");
        let part = job.run().rendered.unwrap().image;
        assert_eq!((part.width, part.height), (win.w, win.h));
        let d = max_diff(&export, &part, win, visible);
        assert!(d <= 2, "1:1 window {win:?} differs from the export by {d} levels");
    }
}

// Given a procedural photo with edits, every 1:1 window equals the full-resolution export
#[test]
fn one_to_one_pixels_equal_full_resolution_export_pixels() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
    s.execute("develop.set", &json!({"control": "effects.clarity", "value": 20})).unwrap();
    s.execute("develop.set", &json!({"control": "detail.sharpenAmount", "value": 60})).ok();
    check_one_to_one(&mut s, id);
}

// Given corpus raws (`cargo xtask corpus --download`), the same holds for real files; skipped
// cleanly without them
#[test]
fn one_to_one_pixels_equal_export_pixels_on_corpus_raws() {
    let dir = std::env::var_os("DAC_CORPUS_RAW")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/raw"));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skipped: no raw corpus at {}", dir.display());
        return;
    };
    let files: Vec<_> = rd.flatten().map(|e| e.path()).filter(|p| p.is_file()).take(3).collect();
    if files.is_empty() {
        eprintln!("skipped: empty raw corpus");
        return;
    }
    let mut s = Session::new();
    let paths: Vec<String> = files.iter().map(|p| p.to_string_lossy().to_string()).collect();
    s.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<_> = s.catalog.photos().map(|p| p.id).collect();
    for id in ids {
        check_one_to_one(&mut s, id);
    }
}

// Given 1:1 previews built, they are kept apart from the standard ones and discarded on their own
#[test]
fn one_to_one_previews_are_discarded_on_their_own() {
    let mut s = Session::with_demo();
    let id = s.active().unwrap();
    let p = s.catalog.photo(id).unwrap().clone();
    s.execute("library.buildPreviews", &json!({"size": "full", "ids": [id.0], "wait": true})).unwrap();
    let full = Session::full_view_key(&p);
    // (the view preview is written once the render hands it over)
    assert!(s.media.rendered.contains(full), "1:1 preview built");
    let img = s.media.rendered.get(full).unwrap();
    assert_eq!(img.width.max(img.height), p.width.max(p.height) as usize, "at the photo's own size");
    // a month's policy keeps a fresh one
    assert_eq!(discard_full(&s, &[id], DiscardAfter::Month.max_age()).removed, 0);
    let r = s.execute("library.discardPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["removed"], 1);
    assert!(!s.media.rendered.contains(full));
    let r = s.execute("library.discardPreviews", &json!({"ids": [id.0]})).unwrap();
    assert_eq!(r["removed"], 0, "nothing left to discard");
}

// Given the preview settings, they are validated, applied and reported
#[test]
fn preview_settings_are_validated_and_applied() {
    let mut s = Session::with_demo();
    let r = s.execute("library.previewSettings", &json!({})).unwrap();
    assert_eq!(r, json!({"standardEdge": 2048, "discardFull": "month", "atImport": "minimal"}));
    let r = s.execute("library.previewSettings", &json!({"standardEdge": 1440, "discardFull": "never", "atImport": "full"})).unwrap();
    assert_eq!(r, json!({"standardEdge": 1440, "discardFull": "never", "atImport": "full"}));
    assert!(s.execute("library.previewSettings", &json!({"standardEdge": 10})).is_err());
    assert!(s.execute("library.previewSettings", &json!({"discardFull": "fortnight"})).is_err());
    assert!(s.execute("library.previewSettings", &json!({"atImport": 3})).is_err());
    assert!(s.execute("library.discardPreviews", &json!({"size": "standard"})).is_err());
    assert_eq!(s.preview_prefs.standard_edge(), 1440, "a refused change leaves the settings");
}

/// Panning at 1:1 on a 45 MP photo (8256 × 5504) in a 2560 × 1440 canvas: the loupe draws the
/// window it has every frame (the GPU moves it) and renders a new window, snapped to a 256 px
/// grid with a margin, only when the view leaves the one it has. Measures each window render,
/// CPU and GPU. Opt in (release build recommended):
/// `cargo test --release -p dac-engine --lib pan_at_one_to_one -- --ignored --nocapture`
#[test]
#[ignore]
fn pan_at_one_to_one_on_45_megapixels() {
    use std::sync::Arc;
    let scene = dac_scenes::demo_library().into_iter().next().unwrap();
    let (fw, fh) = (8256usize, 5504usize);
    let src = Arc::new(scene.render(fw, fh));
    let info = dac_pipeline::SourceInfo::default();
    let mut d = dac_develop::DevelopSettings::default();
    d.light.exposure = 0.3;
    d.effects.clarity = 20.0;
    let (cw, ch) = (2560usize, 1440usize);
    let margin = ((fw.max(fh) as f64 * 0.045).min(768.0) as usize).div_ceil(256).max(1) * 256;
    let window_for = |cx: usize, cy: usize| {
        let snap = |lo: usize, hi: usize, full: usize| {
            let a = lo.saturating_sub(margin) / 256 * 256;
            let b = (hi + margin).div_ceil(256) * 256;
            (a, b.min(full))
        };
        let (x0, x1) = snap(cx, cx + cw, fw);
        let (y0, y1) = snap(cy, cy + ch, fh);
        PixelWindow { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
    };
    for gpu in [false, true] {
        if gpu && !dac_gpu::available() {
            eprintln!("gpu: not available");
            continue;
        }
        let stages = dac_pipeline::StageCache::default();
        // a brisk pan: 40 px a frame for 3 s at 60 fps
        let (mut cur, mut renders, mut total, mut worst) = (None::<PixelWindow>, 0usize, 0.0f64, 0.0f64);
        let frames = 180;
        for f in 0..frames {
            let cx = (1000 + f * 40).min(fw - cw);
            // the window there is still covers the view while the visible pixels are inside it
            if cur.is_some_and(|w| cx >= w.x && cx + cw <= w.x + w.w) {
                continue;
            }
            let want = window_for(cx, 2000);
            let req = dac_pipeline::RenderRequest { window: Some(want), ..dac_pipeline::RenderRequest::fit(fw, fh) };
            let t = std::time::Instant::now();
            let r = crate::media::develop(&src, &info, &d, &req, Some(&stages), gpu);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            assert_eq!((r.image.width, r.image.height), (want.w, want.h));
            total += ms;
            worst = worst.max(ms);
            renders += 1;
            cur = Some(want);
        }
        let mean = total / renders.max(1) as f64;
        // windows render in the background while the one there is drawn moved: the view stays
        // sharp as long as a render finishes within the frames the margin buys
        let frames_per_render = frames as f64 / renders.max(1) as f64;
        let sharp_fps = 1000.0 / mean * frames_per_render;
        eprintln!(
            "{}: {renders} window renders in {frames} frames (margin {margin} px), mean {mean:.1} ms, worst {worst:.1} ms → sharp up to {sharp_fps:.0} fps at 40 px/frame",
            if gpu { "gpu" } else { "cpu" }
        );
    }
}
