//! CPU window cost and memory probe for #1146. Run in release mode with one of
//! `matte`, `hidden`, `dehaze24`, `dehaze`, `all`, or `plain` as the argument.
//! Each invocation reports one cold render, five warm Exposure edits and cache bytes.
use std::{sync::Arc, time::Instant};

use lightcraft_develop::{DevelopSettings, LocalAdjustments, Mask, MaskComponent, MaskOp, MaskShape};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PixelWindow, RenderRequest, SourceInfo, StageCache};
use lightcraft_raster::Rgb32f;

fn source(w: usize, h: usize) -> Arc<Rgb32f> {
    Arc::new(Rgb32f::from_fn(w, h, |x, y| {
        let cell = |size: usize, k: usize| ((x / size * 7 + y / size * 13 + k) % 5) as f32 / 4.;
        let l = 0.02 + 0.5 * cell(97, 1) + 0.25 * cell(31, 2) + 0.1 * cell(11, 3);
        [l * 1.1, l, l * 0.8]
    }))
}
fn mask(shape: MaskShape, refine: f64) -> Mask {
    Mask {
        components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }],
        adjust: LocalAdjustments { exposure: 1.5, clarity: 60., texture: 60., noise: 70., moire: 50., defringe: 80., ..Default::default() },
        refine,
        ..Default::default()
    }
}
fn combined() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.effects.dehaze = 80.;
    s.effects.clarity = 80.;
    s.effects.texture = 60.;
    s.light.highlights = -80.;
    s.light.shadows = 80.;
    s.detail.nr_luminance = 70.;
    s.detail.nr_color = 60.;
    s.detail.sharpen_amount = 60.;
    s.masks.push(mask(MaskShape::Radial { center: Point::new(0.45, 0.5), rx: 0.24, ry: 0.2, angle: 15., feather: 65., invert: false }, 35.));
    s
}
fn bench(mode: &str) {
    let (w, h) = if mode == "dehaze24" { (6000, 4000) } else { (9504, 6336) };
    let src = source(w, h);
    let mut info = SourceInfo::default();
    let win = if mode == "dehaze24" { PixelWindow { x: 1024, y: 512, w: 4096, h: 3072 } } else { PixelWindow { x: 2560, y: 1792, w: 4096, h: 2816 } };
    let req = RenderRequest { window: Some(win), ..RenderRequest::fit(w, h) };
    let mut s = match mode {
        "all" => combined(),
        _ => DevelopSettings::default(),
    };
    if mode == "dehaze" || mode == "dehaze24" {
        s.effects.dehaze = 100.;
    }
    if mode == "all" {
        s.masks[0].refine = 100.;
        s.masks.push(mask(MaskShape::Subject, 100.));
        s.masks.push(mask(MaskShape::Sky, 100.));
        s.optics.defringe_purple_amount = 10.;
    }
    if mode == "matte" {
        let mut mattes = lightcraft_pipeline::masks::Mattes::default();
        mattes.push(
            lightcraft_pipeline::masks::MatteKind::Person,
            lightcraft_raster::Image::from_fn(600, 400, |x, y| if x > 200 && x < 400 && y > 100 && y < 350 { 255 } else { 0 }),
        );
        info.mattes = Some(Arc::new(mattes));
        let mut m = mask(MaskShape::Subject, 0.);
        m.adjust = LocalAdjustments { exposure: 1., ..Default::default() };
        s.masks.push(m);
    }
    if mode == "hidden" {
        let mut m = mask(MaskShape::Linear { start: Point::new(0.1, 0.1), end: Point::new(0.9, 0.9) }, 0.);
        m.visible = false;
        m.adjust.highlights = -80.;
        s.masks.push(m);
    }
    let p = lightcraft_pipeline::plan(&src, &info, &s, &req);
    println!("BENCH mode={mode} work={}x{} pixels={} keep={:?}", p.w, p.h, p.w * p.h, p.keep);
    let cache = StageCache::default();
    let mut times = Vec::new();
    for i in 0..6 {
        s.light.exposure = i as f64 * 0.01;
        let now = Instant::now();
        let r = lightcraft_pipeline::render_cached(&src, &info, &s, &req, &cache).unwrap();
        std::hint::black_box(&r);
        let ms = now.elapsed().as_secs_f64() * 1000.;
        println!("TIME i={i} ms={ms:.3}");
        if i > 0 {
            times.push(ms);
        }
    }
    times.sort_by(f64::total_cmp);
    println!("MEDIAN {mode} {:.3} cache_bytes={}", times[times.len() / 2], cache.bytes());
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "plain".into());
    if !matches!(mode.as_str(), "matte" | "hidden" | "dehaze24" | "dehaze" | "all" | "plain") {
        return Err("choose matte, hidden, dehaze24, dehaze, all, or plain".into());
    }
    bench(&mode);
    Ok(())
}
