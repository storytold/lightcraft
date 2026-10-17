//! Pixel-filter plug-ins in the pipeline: Source and Display stages, missing plug-ins, caching
//! and windows (WAT fixtures, no binary modules).

use std::sync::Arc;

use lcc_develop::{DevelopSettings, FilterStage, PluginFilter};
use lcc_raster::Rgb32f;

use crate::plugin_filters::{FilterHost, FilterHostRef};
use crate::{PixelWindow, Quality, RenderRequest, SourceInfo, StageCache, render, render_cached};

/// A filter module with manifest id `id` whose `pc_filter` sets every sample to `1 - v`.
pub(crate) fn invert_module(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"id":"{id}","name":"Invert","version":"1","kind":"filter","params":{{"amount":{{"type":"number","min":0,"max":1,"default":1}}}}}}"#
    );
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func (export "pc_filter") (param $buf i32) (param $len i32) (param i32 i32 i32 i32 i32 i32) (result i32) (local $i i32) (local $p i32)
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (local.set $p (i32.add (local.get $buf) (local.get $i)))
      (f32.store (local.get $p) (f32.sub (f32.const 1) (f32.load (local.get $p))))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l)))
    (i32.const 0)))"#,
        m = manifest.replace('"', "\\\""),
        len = manifest.len()
    );
    wat::parse_str(&src).unwrap()
}

struct Host(Vec<Arc<lcc_filters::Filter>>);

impl FilterHost for Host {
    fn resolve(&self, plugin: &str, _hash: &str) -> Result<Arc<lcc_filters::Filter>, String> {
        self.0.iter().find(|f| f.id() == plugin).cloned().ok_or_else(|| "not installed".into())
    }
}

fn info() -> SourceInfo {
    let f = lcc_filters::Filter::load(&invert_module("t.invert"), lcc_filters::Limits::default()).unwrap();
    SourceInfo { filters: Some(FilterHostRef(Arc::new(Host(vec![Arc::new(f)])))), ..Default::default() }
}

fn source() -> Rgb32f {
    let (w, h) = (96, 64);
    let mut img = Rgb32f::new(w, h);
    for y in 0..h {
        for x in 0..w {
            img.data[y * w + x] = [0.05 + 0.6 * x as f32 / w as f32, 0.2 + 0.3 * y as f32 / h as f32, 0.3];
        }
    }
    img
}

fn with(stages: &[FilterStage], plugin: &str) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    for (i, st) in stages.iter().enumerate() {
        s.plugin_filters.push(PluginFilter { uid: i as u32 + 1, plugin: plugin.into(), name: "Invert".into(), stage: *st, ..Default::default() });
    }
    s
}

fn max_diff(a: &lcc_raster::Rgba8, b: &lcc_raster::Rgba8) -> i32 {
    a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |k| (p[k] as i32 - q[k] as i32).abs())).max().unwrap()
}

#[test]
fn display_invert_inverts_the_output() {
    let (src, info) = (source(), info());
    let req = RenderRequest::fit(96, 64);
    let plain = render(&src, &info, &DevelopSettings::default(), &req);
    let inv = render(&src, &info, &with(&[FilterStage::Display], "t.invert"), &req);
    assert!(inv.warnings.is_empty(), "{:?}", inv.warnings);
    for (p, q) in plain.image.data.iter().zip(&inv.image.data) {
        for k in 0..3 {
            assert!((255 - p[k] as i32 - q[k] as i32).abs() <= 1, "{p:?} {q:?}");
        }
    }
    let twice = render(&src, &info, &with(&[FilterStage::Display, FilterStage::Display], "t.invert"), &req);
    assert!(max_diff(&plain.image, &twice.image) <= 1);
}

#[test]
fn source_filter_changes_the_render_and_twice_cancels() {
    let (src, info) = (source(), info());
    let req = RenderRequest::fit(96, 64);
    let plain = render(&src, &info, &DevelopSettings::default(), &req);
    let once = render(&src, &info, &with(&[FilterStage::Source], "t.invert"), &req);
    assert!(once.warnings.is_empty());
    assert!(max_diff(&plain.image, &once.image) > 40);
    let twice = render(&src, &info, &with(&[FilterStage::Source, FilterStage::Source], "t.invert"), &req);
    assert!(max_diff(&plain.image, &twice.image) <= 2);
}

#[test]
fn missing_plugin_warns_and_renders_unfiltered() {
    let (src, info) = (source(), info());
    let req = RenderRequest::fit(96, 64);
    let plain = render(&src, &info, &DevelopSettings::default(), &req);
    let r = render(&src, &info, &with(&[FilterStage::Source, FilterStage::Display], "t.absent"), &req);
    assert_eq!(r.warnings.len(), 2);
    assert_eq!(max_diff(&plain.image, &r.image), 0);
    // a host-less render is the same
    let r = render(&src, &SourceInfo::default(), &with(&[FilterStage::Display], "t.invert"), &req);
    assert!(!r.warnings.is_empty());
}

#[test]
fn cached_renders_match_and_drafts_reuse_the_source_result() {
    let (src, info) = (Arc::new(source()), info());
    let s = with(&[FilterStage::Source], "t.invert");
    let req = RenderRequest::fit(96, 64);
    let cache = StageCache::default();
    let a = render_cached(&src, &info, &s, &req, &cache);
    let mut s2 = s.clone();
    s2.light.exposure = 0.3;
    let draft = render_cached(&src, &info, &s2, &RenderRequest { quality: Quality::Draft, ..req }, &cache);
    assert_eq!(draft.image.data, render(&src, &info, &s2, &RenderRequest { quality: Quality::Draft, ..req }).image.data);
    assert_eq!(a.image.data, render(&src, &info, &s, &req).image.data);
}

#[test]
fn window_matches_the_whole_render() {
    let (src, info) = (source(), info());
    let s = with(&[FilterStage::Source, FilterStage::Display], "t.invert");
    let full = render(&src, &info, &s, &RenderRequest::fit(96, 64));
    let win = PixelWindow { x: 20, y: 10, w: 30, h: 20 };
    let part = render(&src, &info, &s, &RenderRequest { window: Some(win), ..RenderRequest::fit(96, 64) });
    assert!(max_diff(&full.image.crop(20, 10, 30, 20), &part.image) <= 1);
}

#[test]
fn per_pixel_filter_preview_matches_export_scale() {
    let (src, info) = (source(), info());
    let s = with(&[FilterStage::Source, FilterStage::Display], "t.invert");
    let big = render(&src, &info, &s, &RenderRequest::fit(96, 64));
    let small = render(&src, &info, &s, &RenderRequest::fit(48, 32));
    let mean = |i: &lcc_raster::Rgba8| {
        let n = i.data.len() as f64;
        [0, 1, 2].map(|k| i.data.iter().map(|p| p[k] as f64).sum::<f64>() / n)
    };
    let (a, b) = (mean(&big.image), mean(&small.image));
    assert!((0..3).all(|k| (a[k] - b[k]).abs() < 2.0), "{a:?} {b:?}");
}
