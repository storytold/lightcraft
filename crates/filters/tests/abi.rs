//! The filter ABI end to end with WAT fixtures: well-behaved filters (invert, a neighbourhood
//! filter needing overlap, a position-dependent gradient) and hostile modules (traps, infinite
//! loops, memory bombs, imports, bad manifests, NaN output), which must all fail with an error,
//! quickly, and leave the image untouched.

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use lcc_filters::{Error, Filter, FrameInfo, Limits, Registry, is_filter_module};
use serde_json::json;

/// A module implementing the ABI: `manifest` JSON, `version`, `filter` body (params
/// `$buf $len $w $h $ch $fmt $pp $pl`, result i32) and extra module items.
fn module(manifest: &str, version: i32, filter: &str, extra: &str) -> Vec<u8> {
    let escaped = manifest.replace('\\', "\\\\").replace('"', "\\\"");
    let src = format!(
        r#"(module
  {extra}
  (memory (export "memory") 1)
  (data (i32.const 16) "{escaped}")
  (func (export "pc_abi_version") (result i32) (i32.const {version}))
  (func (export "pc_manifest") (result i64)
    (i64.or (i64.shl (i64.const {len}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param $n i32) (result i32) (local $old i32)
    (local.set $old (memory.grow (i32.shr_u (i32.add (local.get $n) (i32.const 65535)) (i32.const 16))))
    (if (result i32) (i32.eq (local.get $old) (i32.const -1))
      (then (i32.const 0)) (else (i32.mul (local.get $old) (i32.const 65536)))))
  ;; The integer after `"K":` in the params JSON (K one ASCII char), or 0.
  (func $num (param $pp i32) (param $pl i32) (param $k i32) (result i32)
    (local $i i32) (local $end i32) (local $v i32) (local $neg i32) (local $c i32)
    (local.set $end (i32.add (local.get $pp) (i32.sub (local.get $pl) (i32.const 4))))
    (local.set $i (local.get $pp))
    (block $found (loop $scan
      (br_if $found (i32.and (i32.and
        (i32.eq (i32.load8_u (local.get $i)) (i32.const 34))
        (i32.eq (i32.load8_u offset=1 (local.get $i)) (local.get $k)))
        (i32.and (i32.eq (i32.load8_u offset=2 (local.get $i)) (i32.const 34))
                 (i32.eq (i32.load8_u offset=3 (local.get $i)) (i32.const 58)))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $scan (i32.lt_u (local.get $i) (local.get $end)))
      (return (i32.const 0))))
    (local.set $i (i32.add (local.get $i) (i32.const 4)))
    (if (i32.eq (i32.load8_u (local.get $i)) (i32.const 45))
      (then (local.set $neg (i32.const 1)) (local.set $i (i32.add (local.get $i) (i32.const 1)))))
    (block $done (loop $digits
      (local.set $c (i32.sub (i32.load8_u (local.get $i)) (i32.const 48)))
      (br_if $done (i32.gt_u (local.get $c) (i32.const 9)))
      (local.set $v (i32.add (i32.mul (local.get $v) (i32.const 10)) (local.get $c)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $digits)))
    (if (result i32) (local.get $neg) (then (i32.sub (i32.const 0) (local.get $v))) (else (local.get $v))))
  (func (export "pc_filter") (param $buf i32) (param $len i32) (param $w i32) (param $h i32)
    (param $ch i32) (param $fmt i32) (param $pp i32) (param $pl i32) (result i32)
    (local $i i32) (local $x i32) (local $y i32) (local $p i32)
    {filter})
)"#,
        len = manifest.len()
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}\n{src}"))
}

fn manifest(extra: &str) -> String {
    format!(r#"{{"id":"test.filter","name":"Test","version":"1","kind":"filter"{extra}}}"#)
}

fn ok_module(filter: &str) -> Vec<u8> {
    module(&manifest(""), 1, filter, "")
}

/// 1 - v for every sample.
const INVERT: &str = r#"
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (local.set $p (i32.add (local.get $buf) (local.get $i)))
      (f32.store (local.get $p) (f32.sub (f32.const 1) (f32.load (local.get $p))))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l)))
    (i32.const 0)"#;

/// Every sample takes the value of the pixel to its left (needs one pixel of overlap).
const SHIFT_RIGHT: &str = r#"
    (local.set $y (i32.const 0))
    (block $dy (loop $ly
      (br_if $dy (i32.ge_u (local.get $y) (local.get $h)))
      (local.set $x (i32.sub (local.get $w) (i32.const 1)))
      (block $dx (loop $lx
        (br_if $dx (i32.lt_s (local.get $x) (i32.const 1)))
        (local.set $p (i32.add (local.get $buf)
          (i32.mul (i32.const 12) (i32.add (i32.mul (local.get $y) (local.get $w)) (local.get $x)))))
        (f32.store (local.get $p) (f32.load (i32.sub (local.get $p) (i32.const 12))))
        (f32.store offset=4 (local.get $p) (f32.load offset=4 (i32.sub (local.get $p) (i32.const 12))))
        (f32.store offset=8 (local.get $p) (f32.load offset=8 (i32.sub (local.get $p) (i32.const 12))))
        (local.set $x (i32.sub (local.get $x) (i32.const 1)))
        (br $lx)))
      (local.set $y (i32.add (local.get $y) (i32.const 1)))
      (br $ly)))
    (i32.const 0)"#;

/// R = absolute column, G = absolute row in the full image (from `_image.x`/`_image.y`), B = 0.5.
const GRADIENT: &str = r#"
    (local $ox i32) (local $oy i32)
    (local.set $ox (call $num (local.get $pp) (local.get $pl) (i32.const 120)))
    (local.set $oy (call $num (local.get $pp) (local.get $pl) (i32.const 121)))
    (block $dy (loop $ly
      (br_if $dy (i32.ge_u (local.get $y) (local.get $h)))
      (local.set $x (i32.const 0))
      (block $dx (loop $lx
        (br_if $dx (i32.ge_u (local.get $x) (local.get $w)))
        (local.set $p (i32.add (local.get $buf)
          (i32.mul (i32.const 12) (i32.add (i32.mul (local.get $y) (local.get $w)) (local.get $x)))))
        (f32.store (local.get $p) (f32.convert_i32_s (i32.add (local.get $ox) (local.get $x))))
        (f32.store offset=4 (local.get $p) (f32.convert_i32_s (i32.add (local.get $oy) (local.get $y))))
        (f32.store offset=8 (local.get $p) (f32.const 0.5))
        (local.set $x (i32.add (local.get $x) (i32.const 1)))
        (br $lx)))
      (local.set $y (i32.add (local.get $y) (i32.const 1)))
      (br $ly)))
    (i32.const 0)"#;

const NAN_OUT: &str = r#"
    (block $d (loop $l
      (br_if $d (i32.ge_u (local.get $i) (local.get $len)))
      (f32.store (i32.add (local.get $buf) (local.get $i)) (f32.const nan))
      (local.set $i (i32.add (local.get $i) (i32.const 4)))
      (br $l)))
    (i32.const 0)"#;

fn image(w: usize, h: usize) -> Vec<f32> {
    (0..w * h * 3).map(|i| ((i * 37) % 101) as f32 / 100.0).collect()
}

fn small_bands() -> Limits {
    // About 40 px per band, so even small test images are cut into many bands.
    Limits { band_bytes: 40 * 12, ..Limits::default() }
}

fn run(f: &Filter, img: &mut [f32], w: u32, h: u32, frame: FrameInfo) -> lcc_filters::Result<()> {
    f.apply_rgb(img, w, h, frame, &json!({}), &AtomicBool::new(false))
}

#[test]
fn invert_inverts_every_sample() {
    let f = Filter::load(&ok_module(INVERT), small_bands()).unwrap();
    assert_eq!(f.id(), "test.filter");
    assert_eq!(f.module_hash().len(), 64);
    let (w, h) = (23, 17);
    let orig = image(w, h);
    let mut img = orig.clone();
    run(&f, &mut img, w as u32, h as u32, FrameInfo::whole(w as u32, h as u32)).unwrap();
    for (a, b) in img.iter().zip(&orig) {
        assert!((a - (1.0 - b)).abs() < 1e-6);
    }
}

#[test]
fn module_hash_tells_builds_apart() {
    let a = Filter::load(&ok_module(INVERT), Limits::default()).unwrap();
    let b = Filter::load(&ok_module(NAN_OUT), Limits::default()).unwrap();
    let a2 = Filter::load(&ok_module(INVERT), Limits::default()).unwrap();
    assert_ne!(a.module_hash(), b.module_hash());
    assert_eq!(a.module_hash(), a2.module_hash());
}

#[test]
fn overlap_makes_neighbourhood_filters_seamless_across_bands() {
    let bytes = module(&manifest(r#","overlap":1"#), 1, SHIFT_RIGHT, "");
    let (w, h) = (31, 19);
    let orig = image(w, h);
    // Expected: each pixel takes its left neighbour; the first column repeats itself (edge).
    let mut want = orig.clone();
    for y in 0..h {
        for x in 1..w {
            for c in 0..3 {
                want[(y * w + x) * 3 + c] = orig[(y * w + x - 1) * 3 + c];
            }
        }
    }
    for limits in [Limits::default(), small_bands()] {
        let f = Filter::load(&bytes, limits).unwrap();
        let mut img = orig.clone();
        run(&f, &mut img, w as u32, h as u32, FrameInfo::whole(w as u32, h as u32)).unwrap();
        assert_eq!(img, want);
    }
}

#[test]
fn position_dependent_filters_line_up_across_bands_and_tiles() {
    let f = Filter::load(&ok_module(GRADIENT), small_bands()).unwrap();
    let (w, h) = (29, 13);
    let frame = FrameInfo { x: 100, y: 40, full_w: 400, full_h: 300 };
    let mut img = image(w, h);
    run(&f, &mut img, w as u32, h as u32, frame).unwrap();
    for y in 0..h {
        for x in 0..w {
            let px = &img[(y * w + x) * 3..][..3];
            assert_eq!(px, [(100 + x) as f32, (40 + y) as f32, 0.5], "pixel {x},{y}");
        }
    }
}

#[test]
fn non_finite_output_keeps_the_original() {
    let f = Filter::load(&ok_module(NAN_OUT), Limits::default()).unwrap();
    let orig = image(8, 8);
    let mut img = orig.clone();
    run(&f, &mut img, 8, 8, FrameInfo::whole(8, 8)).unwrap();
    assert_eq!(img, orig);
}

#[test]
fn params_are_resolved_against_the_manifest() {
    let bytes = module(&manifest(r#","params":{"amount":{"type":"number","min":0,"max":1,"default":0.5}}"#), 1, INVERT, "");
    let f = Filter::load(&bytes, Limits::default()).unwrap();
    let mut img = image(4, 4);
    let no = AtomicBool::new(false);
    assert!(matches!(f.apply_rgb(&mut img, 4, 4, FrameInfo::whole(4, 4), &json!({"amount": "lots"}), &no), Err(Error::Params(_))));
    assert!(f.apply_rgb(&mut img, 4, 4, FrameInfo::whole(4, 4), &json!({"amount": 9}), &no).is_ok());
}

#[test]
fn cancellation_stops_the_run_and_keeps_the_image() {
    let f = Filter::load(&ok_module(INVERT), small_bands()).unwrap();
    let orig = image(20, 20);
    let mut img = orig.clone();
    let r = f.apply_rgb(&mut img, 20, 20, FrameInfo::whole(20, 20), &json!({}), &AtomicBool::new(true));
    assert_eq!(r, Err(Error::Cancelled));
    assert_eq!(img, orig);
}

#[test]
fn wrong_buffer_size_is_an_error() {
    let f = Filter::load(&ok_module(INVERT), Limits::default()).unwrap();
    let mut img = image(4, 4);
    assert!(run(&f, &mut img, 5, 4, FrameInfo::whole(5, 4)).is_err());
    assert!(run(&f, &mut img, u32::MAX, u32::MAX, FrameInfo::whole(1, 1)).is_err());
    let mut empty: Vec<f32> = Vec::new();
    assert!(run(&f, &mut empty, 0, 0, FrameInfo::default()).is_ok());
}

/// Hostile modules: loading or running each must fail with an error (not a panic or a hang),
/// leaving the image untouched.
#[test]
fn hostile_modules_fail_cleanly() {
    let limits = Limits { fuel_base: 2_000_000, fuel_per_sample: 1_000, wall_time_ms: 5_000, ..Limits::default() };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("garbage", b"definitely not wasm".to_vec()),
        ("truncated", ok_module(INVERT)[..20].to_vec()),
        ("trap", ok_module("(unreachable)")),
        ("infinite loop", ok_module("(loop $l (br $l)) (i32.const 0)")),
        ("infinite loop in manifest", module(&manifest(""), 1, INVERT, "(func $spin (loop $l (br $l))) (start $spin)")),
        ("memory bomb", ok_module("(block $d (loop $l (br_if $d (i32.eq (memory.grow (i32.const 1000)) (i32.const -1))) (br $l))) (i32.const 0)")),
        ("out of bounds", ok_module("(i32.store (i32.const -4) (i32.const 1)) (i32.const 0)")),
        ("error code", ok_module("(i32.const 7)")),
        ("imports", module(&manifest(""), 1, INVERT, r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#)),
        ("wrong abi version", module(&manifest(""), 2, INVERT, "")),
        ("bad manifest json", module("not json", 1, INVERT, "")),
        ("manifest bad id", module(r#"{"id":"a b","name":"x","kind":"filter"}"#, 1, INVERT, "")),
        ("manifest huge overlap", module(&manifest(r#","overlap":100000"#), 1, INVERT, "")),
        ("deep recursion", module(&manifest(""), 1, "(call $rec) (i32.const 0)", "(func $rec (call $rec))")),
    ];
    for (name, bytes) in cases {
        let t = Instant::now();
        let orig = image(32, 24);
        let mut img = orig.clone();
        let r = Filter::load(&bytes, limits.clone()).and_then(|f| run(&f, &mut img, 32, 24, FrameInfo::whole(32, 24)));
        assert!(r.is_err(), "{name}: hostile module must fail");
        assert_eq!(img, orig, "{name}: image must be untouched");
        assert!(t.elapsed() < Duration::from_secs(20), "{name}: took {:?}", t.elapsed());
    }
}

#[test]
fn infinite_loop_hits_the_instruction_budget() {
    let limits = Limits { fuel_base: 1_000_000, fuel_per_sample: 10, ..Limits::default() };
    let f = Filter::load(&ok_module("(loop $l (br $l)) (i32.const 0)"), limits).unwrap();
    let mut img = image(4, 4);
    assert!(matches!(run(&f, &mut img, 4, 4, FrameInfo::whole(4, 4)), Err(Error::Limit(_))));
}

#[test]
fn oversized_declared_memory_is_rejected() {
    let limits = Limits { max_memory_bytes: 1 << 20, ..Limits::default() };
    let bytes = wat::parse_str(r#"(module (memory (export "memory") 100))"#).unwrap();
    assert!(Filter::load(&bytes, limits).is_err());
}

#[test]
fn sniff_recognises_filter_modules() {
    assert!(is_filter_module(&ok_module(INVERT)));
    assert!(!is_filter_module(&wat::parse_str(r#"(module (func (export "run")))"#).unwrap()));
    assert!(!is_filter_module(b"\0asm"));
}

#[test]
fn registry_installs_lists_and_reports_bad_files() {
    let reg = Registry::new(Limits::default());
    let f = reg.install_bytes(&ok_module(INVERT)).unwrap();
    assert_eq!(reg.get("test.filter").map(|g| g.module_hash().to_string()), Some(f.module_hash().to_string()));
    assert_eq!(reg.list().len(), 1);
    assert!(reg.install_bytes(b"junk").is_err());
    assert!(reg.remove("test.filter"));
    assert!(!reg.remove("test.filter"));
    assert!(reg.get("test.filter").is_none());

    let dir = std::env::temp_dir().join(format!("lcc-filters-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("good.wasm"), ok_module(INVERT)).unwrap();
    std::fs::write(dir.join("bad.wasm"), b"nope").unwrap();
    std::fs::write(dir.join("readme.txt"), b"ignored").unwrap();
    let report = reg.load_folder(&dir).unwrap();
    assert_eq!(report.loaded, ["test.filter"]);
    assert_eq!(report.failed.len(), 1);
    assert!(reg.get("test.filter").and_then(|f| f.source().map(str::to_owned)).is_some());
    assert!(reg.install_path(&dir).is_err());
    assert!(reg.load_folder(&dir.join("missing")).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn pc_analyze_is_optional_and_returns_json() {
    let plain = Filter::load(&ok_module(INVERT), Limits::default()).unwrap();
    assert!(!plain.has_analyze());
    let img = vec![0.5f32; 4 * 4 * 3];
    assert!(plain.analyze_rgb(&img, 4, 4, &json!({}), &AtomicBool::new(false)).is_err());

    let json = r#"{"base_r":200}"#;
    let extra = format!(
        r#"(data (i32.const 4096) "{}")
  (func (export "pc_analyze") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64)
    (i64.or (i64.shl (i64.const {}) (i64.const 32)) (i64.const 4096)))"#,
        json.replace('"', "\\\""),
        json.len()
    );
    let f = Filter::load(&module(&manifest(""), 1, INVERT, &extra), Limits::default()).unwrap();
    assert!(f.has_analyze());
    let r = f.analyze_rgb(&img, 4, 4, &json!({"encoding": "linear"}), &AtomicBool::new(false)).unwrap();
    assert_eq!(r, Some(json!({"base_r": 200})));
    // hostile input: wrong buffer size is an error, not a panic
    assert!(f.analyze_rgb(&img, 5, 5, &json!({}), &AtomicBool::new(false)).is_err());

    // a pc_analyze pointing outside memory errors
    let bad = r#"(func (export "pc_analyze") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i64)
    (i64.or (i64.shl (i64.const 100) (i64.const 32)) (i64.const 0x7ffffff0)))"#;
    let f = Filter::load(&module(&manifest(""), 1, INVERT, bad), Limits::default()).unwrap();
    assert!(f.analyze_rgb(&img, 4, 4, &json!({}), &AtomicBool::new(false)).is_err());
}
