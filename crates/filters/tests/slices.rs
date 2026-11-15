//! A long `pc_filter` call runs in slices of fuel. A function the call runs for the first time
//! must not fail it at a slice's end: wasmi translates functions lazily by default and charges
//! the translation to the call's fuel, and running out of fuel during a translation can't be
//! resumed. The call failed with "instruction budget" with most of its budget left (#1200).
//! Modules are translated when loaded instead.

use std::sync::atomic::AtomicBool;

use lcc_filters::{Filter, FrameInfo, Limits};
use serde_json::json;

/// The host's slice (`runtime::FUEL_SLICE`).
const SLICE: f64 = 20_000_000.0;

/// A filter that counts `burn` down to 0, then calls `funcs` functions it hasn't run yet, each
/// `body` additions long, and leaves the image as it is.
fn module(burn: u64, funcs: usize, body: usize) -> Vec<u8> {
    let manifest = r#"{"id":"test.slices","name":"Slices","version":"1","kind":"filter"}"#;
    let mut defs = String::new();
    let mut calls = String::new();
    for k in 0..funcs {
        let adds: String = (0..body).map(|j| format!("(local.set $x (i32.add (local.get $x) (i32.const {})))\n", j * 7 + k)).collect();
        defs.push_str(&format!("(func $f{k} (result i32) (local $x i32) {adds} (local.get $x))\n"));
        calls.push_str(&format!("(drop (call $f{k}))\n"));
    }
    let src = format!(
        r#"(module
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (func (export "pc_abi_version") (result i32) (i32.const 1))
  (func (export "pc_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "pc_alloc") (param i32) (result i32) (i32.const 65536))
  {defs}
  (func (export "pc_filter") (param i32 i32 i32 i32 i32 i32 i32 i32) (result i32) (local $i i64)
    (local.set $i (i64.const {burn}))
    (block $done (loop $l
      (br_if $done (i64.eqz (local.get $i)))
      (local.set $i (i64.sub (local.get $i) (i64.const 1)))
      (br $l)))
    {calls}
    (i32.const 0))
)"#,
        m = manifest.replace('"', "\\22"),
        mlen = manifest.len()
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}"))
}

/// One band of 2 × 2 pixels with `fuel` instructions for the `pc_filter` call.
fn run(bytes: &[u8], fuel: u64) -> lcc_filters::Result<()> {
    let limits = Limits { fuel_base: fuel, fuel_per_sample: 0, ..Limits::default() };
    let f = Filter::load(bytes, limits)?;
    let mut img = vec![0.25f32; 2 * 2 * 3];
    f.apply_rgb(&mut img, 2, 2, FrameInfo::whole(2, 2), &json!({}), &AtomicBool::new(false))
}

#[test]
fn a_function_first_run_at_a_slices_end_does_not_fail_the_filter() {
    // fuel per count, from the smallest budget a count of 1M fits in
    let counting = module(1_000_000, 0, 0);
    let (mut lo, mut hi) = (1_000_000u64, 40_000_000u64);
    while hi - lo > 200_000 {
        let mid = (lo + hi) / 2;
        if run(&counting, mid).is_ok() { hi = mid } else { lo = mid }
    }
    let per = hi as f64 / 1e6;
    // the 300 first calls start at points spread over the last 600k fuel of the first slice, so
    // the slice ends during some of them
    for k in 0..8 {
        let start = SLICE - 600_000.0 * (k as f64 + 0.5) / 8.0;
        let burn = (start / per) as u64;
        let r = run(&module(burn, 300, 60), Limits::default().fuel_base);
        assert!(r.is_ok(), "count {burn}: {r:?}");
    }
}
