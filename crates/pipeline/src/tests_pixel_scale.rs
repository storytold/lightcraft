//! Pixel-scale Source filters (`overlap` > 0) run at the frame's native resolution, so a preview,
//! a zoomed window and an export agree (WAT fixture: a 3 × 3 box blur).

use std::sync::Arc;

use lcc_develop::{DevelopSettings, FilterStage, PluginFilter};
use lcc_raster::{Rgb32f, Rgba8};

use crate::plugin_filters::{FilterHost, FilterHostRef};
use crate::{PixelWindow, Quality, RenderRequest, SourceInfo, StageCache, render, render_cached};

/// A filter (`overlap` 1) whose `pc_filter` replaces each inner pixel by the mean of its 3 × 3
/// neighbourhood (read from a copy of the buffer).
fn blur_module() -> Vec<u8> {
    let manifest = r#"{"id":"t.blur","name":"Blur","version":"1","kind":"filter","overlap":1,"params":{}}"#;
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func $grow (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.add (i32.shr_u (local.get $n) (i32.const 16)) (i32.const 1))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "pc_alloc") (param $n i32) (result i32) (call $grow (local.get $n)))
  (func (export "pc_filter") (param $buf i32) (param $len i32) (param $w i32) (param $h i32) (param i32 i32 i32 i32) (result i32)
    (local $tmp i32) (local $i i32) (local $x i32) (local $y i32) (local $c i32) (local $rs i32) (local $p i32) (local $q i32) (local $acc f32)
    (local.set $tmp (call $grow (local.get $len)))
    (if (i32.eqz (local.get $tmp)) (then (return (i32.const 1))))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (f32.store (i32.add (local.get $tmp) (local.get $i)) (f32.load (i32.add (local.get $buf) (local.get $i))))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l)))
    (local.set $rs (i32.mul (local.get $w) (i32.const 12)))
    (local.set $y (i32.const 1))
    (block $yd (loop $yl
      (br_if $yd (i32.ge_s (local.get $y) (i32.sub (local.get $h) (i32.const 1))))
      (local.set $x (i32.const 1))
      (block $xd (loop $xl
        (br_if $xd (i32.ge_s (local.get $x) (i32.sub (local.get $w) (i32.const 1))))
        (local.set $c (i32.const 0))
        (block $cd (loop $cl
          (br_if $cd (i32.ge_s (local.get $c) (i32.const 3)))
          (local.set $p (i32.add (i32.add (i32.mul (local.get $y) (local.get $rs)) (i32.mul (local.get $x) (i32.const 12))) (i32.mul (local.get $c) (i32.const 4))))
          (local.set $q (i32.add (local.get $tmp) (i32.sub (local.get $p) (local.get $rs))))
          (local.set $acc (f32.add (f32.add (f32.load (i32.sub (local.get $q) (i32.const 12))) (f32.load (local.get $q))) (f32.load (i32.add (local.get $q) (i32.const 12)))))
          (local.set $q (i32.add (local.get $q) (local.get $rs)))
          (local.set $acc (f32.add (local.get $acc) (f32.add (f32.add (f32.load (i32.sub (local.get $q) (i32.const 12))) (f32.load (local.get $q))) (f32.load (i32.add (local.get $q) (i32.const 12))))))
          (local.set $q (i32.add (local.get $q) (local.get $rs)))
          (local.set $acc (f32.add (local.get $acc) (f32.add (f32.add (f32.load (i32.sub (local.get $q) (i32.const 12))) (f32.load (local.get $q))) (f32.load (i32.add (local.get $q) (i32.const 12))))))
          (f32.store (i32.add (local.get $buf) (local.get $p)) (f32.div (local.get $acc) (f32.const 9)))
          (local.set $c (i32.add (local.get $c) (i32.const 1)))
          (br $cl)))
        (local.set $x (i32.add (local.get $x) (i32.const 1)))
        (br $xl)))
      (local.set $y (i32.add (local.get $y) (i32.const 1)))
      (br $yl)))
    (i32.const 0)))"#,
        m = manifest.replace('"', "\\\""),
        len = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

struct Host(Arc<lcc_filters::Filter>);

impl FilterHost for Host {
    fn resolve(&self, plugin: &str, _hash: &str) -> Result<Arc<lcc_filters::Filter>, String> {
        (self.0.id() == plugin).then(|| self.0.clone()).ok_or_else(|| "not installed".into())
    }
}

fn info() -> SourceInfo {
    let f = lcc_filters::Filter::load(&blur_module(), lcc_filters::Limits::default()).unwrap();
    SourceInfo { filters: Some(FilterHostRef(Arc::new(Host(Arc::new(f))))), ..Default::default() }
}

/// 192 × 128 with stripes 16 px wide (a 3 × 3 blur softens their edges at a quarter of the
/// scale, barely at the source's).
fn source(seed: f32) -> Rgb32f {
    let (w, h) = (192, 128);
    let mut img = Rgb32f::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let stripe = if (x / 16) % 2 == 0 { 0.6 } else { 0.08 };
            img.data[y * w + x] = [stripe, 0.1 + 0.4 * y as f32 / h as f32, seed];
        }
    }
    img
}

fn settings() -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.detail.sharpen_amount = 0.0;
    s.plugin_filters.push(PluginFilter { uid: 1, plugin: "t.blur".into(), name: "Blur".into(), stage: FilterStage::Source, ..Default::default() });
    s
}

fn box_down(i: &Rgba8, f: usize) -> Vec<[f64; 3]> {
    let (w, h) = (i.width / f, i.height / f);
    let mut out = vec![[0.0; 3]; w * h];
    for y in 0..h * f {
        for x in 0..w * f {
            let p = i.data[y * i.width + x];
            for k in 0..3 {
                out[(y / f) * w + x / f][k] += p[k] as f64 / (f * f) as f64;
            }
        }
    }
    out
}

fn mean_abs(a: &[[f64; 3]], b: &Rgba8) -> f64 {
    let n = a.len() as f64 * 3.0;
    a.iter().zip(&b.data).map(|(p, q)| (0..3).map(|k| (p[k] - q[k] as f64).abs()).sum::<f64>()).sum::<f64>() / n
}

#[test]
fn pixel_scale_preview_matches_downsampled_export() {
    let (src, info, s) = (source(0.31), info(), settings());
    let export = render(&src, &info, &s, &RenderRequest::fit(192, 128));
    let preview = render(&src, &info, &s, &RenderRequest::fit(48, 32));
    assert!(export.warnings.is_empty() && preview.warnings.is_empty(), "{:?}", preview.warnings);
    let down = box_down(&export.image, 4);
    let err = mean_abs(&down, &preview.image);
    // what resampling alone accounts for: the same comparison without the filter
    let plain = DevelopSettings { plugin_filters: vec![], ..s.clone() };
    let base = mean_abs(
        &box_down(&render(&src, &info, &plain, &RenderRequest::fit(192, 128)).image, 4),
        &render(&src, &info, &plain, &RenderRequest::fit(48, 32)).image,
    );
    assert!(err < base + 0.5, "preview differs from the downsampled export by {err} (resampling alone: {base})");
}

#[test]
fn pixel_scale_zoomed_window_matches_the_whole_render() {
    let (src, info, s) = (source(0.32), info(), settings());
    let req = RenderRequest::fit(384, 256);
    let full = render(&src, &info, &s, &req);
    let win = PixelWindow { x: 100, y: 60, w: 80, h: 50 };
    let part = render(&src, &info, &s, &RenderRequest { window: Some(win), ..req });
    let crop = full.image.crop(100, 60, 80, 50);
    let d = crop.data.iter().zip(&part.image.data).flat_map(|(p, q)| (0..3).map(move |k| (p[k] as i32 - q[k] as i32).abs())).max().unwrap();
    assert!(d <= 2, "{d}");
}

#[test]
fn pixel_scale_draft_reuses_the_native_result() {
    let (src, info, s) = (Arc::new(source(0.33)), info(), settings());
    let req = RenderRequest::fit(48, 32);
    let cache = StageCache::default();
    let full = render_cached(&src, &info, &s, &req, &cache);
    let mut s2 = s.clone();
    s2.light.exposure = 0.0001;
    let draft = render_cached(&src, &info, &s2, &RenderRequest { quality: Quality::Draft, ..req }, &cache);
    let d = full.image.data.iter().zip(&draft.image.data).flat_map(|(p, q)| (0..3).map(move |k| (p[k] as i32 - q[k] as i32).abs())).max().unwrap();
    assert!(d <= 2, "{d}");
}

#[test]
fn aux_source_is_the_filtered_photo_and_follows_the_stack() {
    let f = lcc_filters::Filter::load(&crate::tests_plugin_filters::invert_module("t.inv"), lcc_filters::Limits::default()).unwrap();
    let info = SourceInfo { filters: Some(FilterHostRef(Arc::new(Host(Arc::new(f))))), ..Default::default() };
    let src = source(0.34);
    assert!(crate::source_stage::aux_source(&src, &info, &DevelopSettings::default()).is_none());
    let mut s = DevelopSettings::default();
    s.plugin_filters.push(PluginFilter { uid: 1, plugin: "t.inv".into(), name: "Invert".into(), stage: FilterStage::Source, ..Default::default() });
    let a = crate::source_stage::aux_source(&src, &info, &s).unwrap();
    // an inverted scan: dark where the source is bright
    let (p, q) = (src.data[0], a.data[0]);
    assert!(q[1] > p[1] + 0.5, "{p:?} {q:?}");
    // the helpers that key their caches on the pixels they read see a different photo
    assert_ne!(crate::optics::fingerprint(&a), crate::optics::fingerprint(&src));
    // switching the filter off brings back the plain source
    s.plugin_filters[0].enabled = false;
    assert!(crate::source_stage::aux_source(&src, &info, &s).is_none());
}

#[test]
fn pixel_scale_preview_from_a_reduced_decode_matches_the_export() {
    let (src, info, s) = (source(0.35), info(), settings());
    let export = render(&src, &info, &s, &RenderRequest::fit(192, 128));
    // a preview level: the photo decoded at half size, its full size known
    let half = lcc_raster::resample::resize(&src, 96, 64, lcc_raster::resample::Filter::Box);
    let preview = render(&half, &SourceInfo { native_long: Some(192), ..info.clone() }, &s, &RenderRequest::fit(48, 32));
    let err = mean_abs(&box_down(&export.image, 4), &preview.image);
    let plain = DevelopSettings { plugin_filters: vec![], ..s.clone() };
    let base = mean_abs(
        &box_down(&render(&src, &info, &plain, &RenderRequest::fit(192, 128)).image, 4),
        &render(&half, &info, &plain, &RenderRequest::fit(48, 32)).image,
    );
    assert!(err < base + 0.5, "preview differs from the downsampled export by {err} (resampling alone: {base})");
}
