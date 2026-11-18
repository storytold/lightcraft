//! A long call runs in slices of fuel; a function the call runs for the first time must not fail
//! it at a slice's end. wasmi compiles functions lazily by default, charging the compilation to the
//! call's fuel, and an out-of-fuel during a compilation can't be resumed: the call failed with
//! "instruction budget" with most of its budget left. Modules are compiled when loaded instead.

use lcc_plugins::{Limits, Manager, NoHost};
use serde_json::json;

/// The host's slice (`runtime::FUEL_SLICE`).
const SLICE: f64 = 20_000_000.0;

/// A plug-in whose command counts `burn` down to 0, then calls `funcs` functions it hasn't run
/// yet, each `body` additions long, and replies `{}`.
fn module(burn: u64, funcs: usize, body: usize) -> Vec<u8> {
    let manifest = r#"{"id":"test.slices","name":"Slices","version":"1.0.0","commands":[{"id":"go","label":"Go"}]}"#;
    let mut defs = String::new();
    let mut calls = String::new();
    for k in 0..funcs {
        let adds: String = (0..body).map(|j| format!("(local.set $x (i32.add (local.get $x) (i32.const {})))\n", j * 7 + k)).collect();
        defs.push_str(&format!("(func $f{k} (result i32) (local $x i32) {adds} (local.get $x))\n"));
        calls.push_str(&format!("(drop (call $f{k}))\n"));
    }
    let src = format!(
        r#"(module
  (import "host" "call" (func (param i32 i32) (result i32)))
  (import "host" "response" (func (param i32)))
  (import "host" "log" (func (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{m}")
  (data (i32.const 4096) "{{}}")
  (func (export "lcc_abi_version") (result i32) (i32.const 1))
  (func (export "lcc_manifest") (result i64) (i64.or (i64.shl (i64.const {mlen}) (i64.const 32)) (i64.const 16)))
  (func (export "lcc_alloc") (param i32) (result i32) (i32.const 8192))
  {defs}
  (func (export "lcc_handle") (param i32 i32) (result i64) (local $i i64)
    (local.set $i (i64.const {burn}))
    (block $done (loop $l
      (br_if $done (i64.eqz (local.get $i)))
      (local.set $i (i64.sub (local.get $i) (i64.const 1)))
      (br $l)))
    {calls}
    (i64.or (i64.shl (i64.const 2) (i64.const 32)) (i64.const 4096)))
)"#,
        m = manifest.replace('"', "\\22"),
        mlen = manifest.len()
    );
    wat::parse_str(&src).unwrap_or_else(|e| panic!("bad test WAT: {e}"))
}

fn run(bytes: &[u8], fuel: u64) -> lcc_plugins::Result<serde_json::Value> {
    let mut m = Manager::in_memory();
    m.set_limits(Limits { fuel, ..Limits::default() });
    m.install_bytes(bytes, None, false).unwrap();
    m.run_command("test.slices", "go", &json!({}), &[], &mut NoHost)
}

#[test]
fn a_function_first_run_at_a_slices_end_does_not_fail_the_call() {
    // fuel per count, from the smallest budget a count of 1M fits in
    let counting = module(1_000_000, 0, 0);
    let (mut lo, mut hi) = (1_000_000u64, 40_000_000u64);
    while hi - lo > 200_000 {
        let mid = (lo + hi) / 2;
        if run(&counting, mid).is_ok() { hi = mid } else { lo = mid }
    }
    let per = hi as f64 / 1e6;
    // the 300 first calls start at points spread over the last 600k fuel of the first slice, so
    // the slice ends during some of them (with lazy compilation, 38 of 40 such calls failed)
    for k in 0..3 {
        let start = SLICE - 600_000.0 * (k as f64 + 0.5) / 3.0;
        let burn = (start / per) as u64;
        let r = run(&module(burn, 300, 60), Limits::default().fuel);
        assert_eq!(r.unwrap_or_else(|e| panic!("count {burn}: {e}")), json!({}));
    }
}
