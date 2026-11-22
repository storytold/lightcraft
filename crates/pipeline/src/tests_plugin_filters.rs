//! Pixel-filter plug-ins in the pipeline: Source and Display stages, missing plug-ins, caching
//! and windows (WAT fixtures, no binary modules).

use std::sync::Arc;

use lcc_develop::{DevelopSettings, FilterStage, PluginFilter};
use lcc_raster::Rgb32f;

use crate::plugin_filters::{FilterHost, FilterHostRef};
use crate::{PixelWindow, Quality, RenderRequest, SourceInfo, StageCache, render, render_cached};

/// A filter module with manifest id `id` whose `pc_filter` sets every sample to `1 - v`.
pub(crate) fn invert_module(id: &str) -> Vec<u8> {
    invert_module_with(id, "")
}

/// [`invert_module`] with more manifest fields (`,"key":value…`).
fn invert_module_with(id: &str, more: &str) -> Vec<u8> {
    let manifest = format!(
        r#"{{"id":"{id}","name":"Invert","version":"1","kind":"filter","params":{{"amount":{{"type":"number","min":0,"max":1,"default":1}}}}{more}}}"#
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
    let load = |m: Vec<u8>| Arc::new(lcc_filters::Filter::load(&m, lcc_filters::Limits::default()).unwrap());
    let filters = vec![load(invert_module("t.invert")), load(invert_module_with("t.rendered", r#","baseTone":"neutral""#))];
    SourceInfo { filters: Some(FilterHostRef(Arc::new(Host(filters)))), ..Default::default() }
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

/// A raw is toned by its base curve after a Source filter, unless the filter says it renders a
/// finished picture (`"baseTone": "neutral"`): then it is shown as a non-raw source would be.
#[test]
fn a_rendering_source_filter_turns_off_the_raw_base_tone() {
    let src = source();
    let req = RenderRequest::fit(96, 64);
    // a camera's own curve, as a raw file can carry
    let curve = crate::tone::CameraTone::new(std::array::from_fn(|i| {
        let x = 0.004 * 1.18f32.powi(i as i32);
        [x, 1.0 - (-2.0 * x).exp()]
    }));
    let raw = SourceInfo { raw: true, camera_tone: curve, ..info() };
    // two inverts leave the pixels as they were: only the tone after them differs
    let out = |info: &SourceInfo, plugin: &str| render(&src, info, &with(&[FilterStage::Source, FilterStage::Source], plugin), &req);
    let (plain, toned) = (out(&info(), "t.invert"), out(&raw, "t.invert"));
    assert!(toned.warnings.is_empty(), "{:?}", toned.warnings);
    assert!(max_diff(&plain.image, &toned.image) > 10, "a raw gets its base tone");
    let rendered = out(&raw, "t.rendered");
    assert!(rendered.warnings.is_empty(), "{:?}", rendered.warnings);
    assert!(max_diff(&out(&info(), "t.rendered").image, &rendered.image) <= 1, "no base tone after a rendering filter");
}

/// Filter whose `pc_filter` checks whether `params` JSON contains `needle`, filling the band with
/// pure green (`[0, 1, 0]`) if found or pure red (`[1, 0, 0]`) if not.
fn contains_module(id: &str, needle: &str, overlap: u32) -> Vec<u8> {
    let manifest = format!(r#"{{"id":"{id}","name":"Inspect","version":"1","kind":"filter","overlap":{overlap},"params":{{}}}}"#);
    let m_esc = manifest.replace('\\', "\\\\").replace('"', "\\\"");
    let n_esc = needle.replace('\\', "\\\\").replace('"', "\\\"");
    let src = format!(
        r#"(module
  (memory (export "memory") 1)
  (data (i32.const 16) "{m_esc}")
  (data (i32.const 4096) "{n_esc}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1)) (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  (func $has (param $pp i32) (param $pl i32) (result i32)
    (local $i i32) (local $j i32) (local $end i32)
    (if (i32.lt_u (local.get $pl) (i32.const {nlen})) (then (return (i32.const 0))))
    (local.set $end (i32.sub (local.get $pl) (i32.const {nlen})))
    (block $done (loop $outer
      (br_if $done (i32.gt_u (local.get $i) (local.get $end)))
      (local.set $j (i32.const 0))
      (block $mismatch (loop $inner
        (if (i32.ge_u (local.get $j) (i32.const {nlen})) (then (return (i32.const 1))))
        (br_if $mismatch (i32.ne
          (i32.load8_u (i32.add (local.get $pp) (i32.add (local.get $i) (local.get $j))))
          (i32.load8_u (i32.add (i32.const 4096) (local.get $j)))))
        (local.set $j (i32.add (local.get $j) (i32.const 1)))
        (br $inner)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $outer)))
    (i32.const 0))
  (func (export "pc_filter") (param $buf i32) (param $len i32) (param $w i32) (param $h i32)
    (param $ch i32) (param $fmt i32) (param $pp i32) (param $pl i32) (result i32)
    (local $found i32) (local $i i32) (local $p i32) (local $r f32) (local $g f32)
    (local.set $found (call $has (local.get $pp) (local.get $pl)))
    (local.set $r (if (result f32) (local.get $found) (then (f32.const 0)) (else (f32.const 1))))
    (local.set $g (if (result f32) (local.get $found) (then (f32.const 1)) (else (f32.const 0))))
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (local.set $p (i32.add (local.get $buf) (local.get $i)))
      (f32.store (local.get $p) (local.get $r))
      (f32.store offset=4 (local.get $p) (local.get $g))
      (f32.store offset=8 (local.get $p) (f32.const 0))
      (local.set $i (i32.add (local.get $i) (i32.const 12)))
      (br $l)))
    (i32.const 0)))"#,
        mlen = manifest.len(),
        nlen = needle.len(),
    );
    wat::parse_str(&src).unwrap()
}

fn host_with(modules: Vec<Vec<u8>>) -> SourceInfo {
    let filters = modules.into_iter().map(|m| Arc::new(lcc_filters::Filter::load(&m, lcc_filters::Limits::default()).unwrap())).collect();
    SourceInfo { filters: Some(FilterHostRef(Arc::new(Host(filters)))), ..Default::default() }
}

fn is_green(px: [u8; 4]) -> bool {
    px[1] > px[0].saturating_add(80) && px[1] > px[2].saturating_add(80)
}

fn is_red(px: [u8; 4]) -> bool {
    px[0] > px[1].saturating_add(80) && px[0] > px[2].saturating_add(80)
}

#[test]
fn draft_render_passes_draft_scale_and_native_canvas_size() {
    // 800 × 800 = 640_000 px > DRAFT_SOURCE_PIXELS (500_000) triggers downsampled draft Source pass.
    let src = Rgb32f::from_fn(800, 800, |_, _| [0.2, 0.2, 0.2]);
    let info = host_with(vec![
        contains_module("t.is_draft", r#""draft":true"#, 0),
        contains_module("t.has_scale", r#""draftScale":"#, 0),
        contains_module("t.scale_one", r#""draftScale":1.0"#, 0),
        contains_module("t.native_w", r#""nativeCanvasWidth":800"#, 0),
        contains_module("t.native_h", r#""nativeCanvasHeight":800"#, 0),
        contains_module("t.canvas_707", r#""canvasWidth":707"#, 0),
    ]);
    let req_draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(800, 800) };
    let req_full = RenderRequest::fit(800, 800);

    // On downsampled draft: draft=true, draftScale present (<1.0), nativeCanvasWidth/Height=800, canvasWidth=707.
    assert!(is_green(render(&src, &info, &with(&[FilterStage::Source], "t.is_draft"), &req_draft).image.get(0, 0)));
    assert!(is_green(render(&src, &info, &with(&[FilterStage::Source], "t.has_scale"), &req_draft).image.get(0, 0)));
    assert!(is_red(render(&src, &info, &with(&[FilterStage::Source], "t.scale_one"), &req_draft).image.get(0, 0)));
    assert!(is_green(render(&src, &info, &with(&[FilterStage::Source], "t.native_w"), &req_draft).image.get(0, 0)));
    assert!(is_green(render(&src, &info, &with(&[FilterStage::Source], "t.native_h"), &req_draft).image.get(0, 0)));
    assert!(is_green(render(&src, &info, &with(&[FilterStage::Source], "t.canvas_707"), &req_draft).image.get(0, 0)));

    // On non-draft full render: draft=false, draftScale and nativeCanvasWidth omitted.
    assert!(is_red(render(&src, &info, &with(&[FilterStage::Source], "t.is_draft"), &req_full).image.get(0, 0)));
    assert!(is_red(render(&src, &info, &with(&[FilterStage::Source], "t.has_scale"), &req_full).image.get(0, 0)));
    assert!(is_red(render(&src, &info, &with(&[FilterStage::Source], "t.native_w"), &req_full).image.get(0, 0)));

    // Small draft (96 × 64 <= DRAFT_SOURCE_PIXELS): draft=true, draftScale=1.0, nativeCanvasWidth=96.
    let small = source();
    let info_small = host_with(vec![
        contains_module("t.is_draft", r#""draft":true"#, 0),
        contains_module("t.scale_one", r#""draftScale":1.0"#, 0),
        contains_module("t.native_96", r#""nativeCanvasWidth":96"#, 0),
    ]);
    let req_small_draft = RenderRequest { quality: Quality::Draft, ..RenderRequest::fit(96, 64) };
    assert!(is_green(render(&small, &info_small, &with(&[FilterStage::Source], "t.is_draft"), &req_small_draft).image.get(0, 0)));
    assert!(is_green(render(&small, &info_small, &with(&[FilterStage::Source], "t.scale_one"), &req_small_draft).image.get(0, 0)));
    assert!(is_green(render(&small, &info_small, &with(&[FilterStage::Source], "t.native_96"), &req_small_draft).image.get(0, 0)));
}

#[test]
fn source_info_origin_and_bit_depth_propagate_to_filters_including_native() {
    let src = source();
    let req = RenderRequest::fit(96, 64);
    let base_host = host_with(vec![
        contains_module("t.untagged", r#""origin":"untagged""#, 0),
        contains_module("t.bits16", r#""bitsPerSample":16"#, 0),
        contains_module("t.has_float", r#""float":true"#, 0),
        contains_module("t.native_untagged", r#""origin":"untagged""#, 1),
        contains_module("t.native_not_draft", r#""draft":false"#, 1),
    ]);

    let info_untagged_16 = SourceInfo { space_origin: Some("untagged".into()), bits_per_sample: Some(16), float: false, ..base_host.clone() };
    assert!(is_green(render(&src, &info_untagged_16, &with(&[FilterStage::Source], "t.untagged"), &req).image.get(0, 0)));
    assert!(is_green(render(&src, &info_untagged_16, &with(&[FilterStage::Source], "t.bits16"), &req).image.get(0, 0)));
    assert!(is_red(render(&src, &info_untagged_16, &with(&[FilterStage::Source], "t.has_float"), &req).image.get(0, 0)));
    // Native pixel-scale run (overlap > 0) preserves source and keeps draft=false.
    assert!(is_green(render(&src, &info_untagged_16, &with(&[FilterStage::Source], "t.native_untagged"), &req).image.get(0, 0)));
    assert!(is_green(render(&src, &info_untagged_16, &with(&[FilterStage::Source], "t.native_not_draft"), &req).image.get(0, 0)));

    // Float source propagates `"float":true`.
    let info_float = SourceInfo {
        space_origin: Some("container".into()),
        bits_per_sample: Some(32),
        float: true,
        ..host_with(vec![contains_module("t.container", r#""origin":"container""#, 0), contains_module("t.has_float", r#""float":true"#, 0)])
    };
    assert!(is_green(render(&src, &info_float, &with(&[FilterStage::Source], "t.container"), &req).image.get(0, 0)));
    assert!(is_green(render(&src, &info_float, &with(&[FilterStage::Source], "t.has_float"), &req).image.get(0, 0)));

    // Partial/missing source metadata does not panic and omits `"source"`.
    let partial = SourceInfo { space_origin: Some("untagged".into()), bits_per_sample: None, ..base_host };
    let r = render(&src, &partial, &with(&[FilterStage::Source], "t.untagged"), &req);
    assert!(r.warnings.is_empty());
    assert!(is_red(r.image.get(0, 0)));
}
