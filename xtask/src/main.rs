//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json; flate2/brotli for the web bundle). External tools (`cargo`, `curl`, `tar`) are
//! invoked through `std::process::Command`.

mod assets;
mod bench;
mod ci_lock;
mod ci_quick;
mod ci_report;
mod ico;
mod layers;
mod parity;
mod stats;
mod version;
mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

mod ci;
mod corpus;
mod raw_samples;
mod wasm;

use ci::{available_ram_mb, ci_jobs, cmd_ci, total_ram_gb};
use corpus::cmd_corpus;
use layers::cmd_layers;
use wasm::cmd_wasm;

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  assets          every image/icon/font/media file is attributed in assets/ATTRIBUTION.md; no Adobe assets
  bench [FILE] [--strict] [--threshold PCT]
                  run the render benchmark, append to target/bench/history.jsonl, compare CPU time with
                  the previous run (default input: corpus/raw/arw-sony-a7m3-compressed.arw)
  ico <out.ico> <in.png>...
                  pack square PNGs (<= 256 px) into a Windows .ico (see packaging/icons.sh)
  layers          enforce the crate dependency layering (plan/architecture.md §3)
  parity [--write]
                  check docs/parity-checklist.md (every cmd:/ctl: id and path it cites exists) and print the
                  Lightroom parity summary; --write refreshes the summary table in the document
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates (+ the web app), in one call
  web [--serve [port]] [--dev]
                  build the browser app (apps/lightcraft-web) into <target>/web-site/;
                  --serve serves it on http://127.0.0.1:<port> (default 8080)
  ci [--hardware] [--summary-md] [--quick [--base REF]]
                  fmt --check, clippy -D warnings, heif, test, GPU probe, parity refs, layers, assets, wasm
                  (stops at first failure). Tests run under cargo test (cargo nextest with
                  LIGHTCRAFT_NEXTEST=1), without examples, then doctests with cargo test --doc; after
                  them it prints the translation gaps report (never a failure). Writes
                  target/ci/report.json (machine, GPU adapters, data,
                  steps, skipped tests) and target/ci/summary.md; --hardware also runs the GPU tests on
                  every backend with a hardware adapter; --summary-md prints the Markdown summary.
                  --quick: only what the changes since the merge base with origin/main (or REF) can
                  break: clippy, the changed packages' tests, and heif / parity / wasm when they apply.
                  One ci runs at a time per machine; others wait (LIGHTCRAFT_CI_SLOTS=N for N at once,
                  LIGHTCRAFT_CI_LOCK=off to skip the lock)
  corpus [--download]
                  show where test corpora live; --download fetches PngSuite and CC0 raw samples (raw.pixls.us) into corpus/ and checks their sha256
  stats [--exact] count tests and lines per crate (--exact: ask the test harness via `-- --list`)
  raw-frame [--json] FILE…
                  measure where each raw's embedded camera JPEG sits in its sensor data, which edge bands hold no
                  picture and what the decoder's crop keeps beyond the camera frame (any make with a full-size
                  embedded JPEG); print image-area entries for Nikon bodies that record no crop (MEASURED_BODIES in
                  crates/raw/src/vendor/nef.rs)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let result = match args.first().map(String::as_str) {
        Some("ico") => ico::run(&rest),
        Some("version") => version::run(&root(), &rest),
        Some("layers") => cmd_layers(),
        Some("assets") => assets::run(&root()),
        Some("bench") => bench::run(&root(), &rest),
        Some("parity") => parity::run(&root(), rest.contains(&"--write")),
        Some("wasm") => cmd_wasm(),
        Some("web") => web::run(&rest),
        Some("ci") => cmd_ci(&rest),
        Some("corpus") => cmd_corpus(rest.contains(&"--download")),
        Some("stats") => stats::run(&root(), rest.contains(&"--exact")),
        Some("raw-frame") => cmd_raw_frame(&rest),
        Some("-h" | "--help" | "help") | None => {
            print!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `cargo xtask raw-frame FILE…`: runs the `raw_frame` example of `lightcraft-engine` on the files.
fn cmd_raw_frame(files: &[&str]) -> Result<(), String> {
    if files.is_empty() {
        return Err("raw-frame needs one or more raw files (several per camera, with detail across the frame)".into());
    }
    eprintln!("$ cargo run --release -p lightcraft-engine --example raw_frame -- {}", files.join(" "));
    let status = cargo()
        .args(["run", "--release", "-q", "-p", "lightcraft-engine", "--example", "raw_frame", "--"])
        .args(files)
        .status()
        .map_err(|e| format!("raw_frame: {e}"))?;
    if status.success() { Ok(()) } else { Err("raw_frame failed".into()) }
}

/// Workspace root (parent of the xtask crate).
pub fn root() -> PathBuf {
    // `cargo run` sets it at run time; prefer that over the compile-time value, which goes stale
    // when the checkout moves and the cached xtask binary isn't rebuilt
    let dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from).filter(|d| d.join("Cargo.toml").is_file());
    let dir = dir.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    dir.parent().expect("xtask has a parent dir").to_path_buf()
}

pub fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    // Full Windows debuginfo plus one linker per CPU can exhaust RAM before any
    // tests run. Scope these overridable defaults to CI, including parity/WASM
    // subprocesses, without changing ordinary developer builds or GPU coverage.
    if std::env::args().nth(1).as_deref() == Some("ci") {
        static JOBS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();
        let (build, threads) = JOBS.get_or_init(|| {
            let ram_mb = available_ram_mb().or_else(|| total_ram_gb().map(|gb| gb.saturating_mul(1024) / 2));
            let jobs = ci_jobs(ram_mb, std::thread::available_parallelism().map_or(4, |n| n.get()));
            // More test threads barely shorten CI (a few heavy tests dominate) but make the
            // wall-clock frame-budget tests flaky on a loaded machine.
            (jobs.to_string(), jobs.min(4).to_string())
        });
        for (key, value) in [
            ("CARGO_PROFILE_DEV_DEBUG", "line-tables-only"),
            ("CARGO_BUILD_JOBS", build.as_str()),
            ("RUST_TEST_THREADS", threads.as_str()),
            ("NEXTEST_TEST_THREADS", threads.as_str()),
        ] {
            if std::env::var_os(key).is_none() {
                c.env(key, value);
            }
        }
    }
    c
}

pub fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    eprintln!("$ {what}");
    let status = cmd.status().map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    if status.success() { Ok(()) } else { Err(format!("{what}: exited with {status}")) }
}

pub fn metadata() -> Result<serde_json::Value, String> {
    let out = cargo().args(["metadata", "--format-version", "1", "--no-deps"]).output().map_err(|e| format!("cargo metadata: {e}"))?;
    if !out.status.success() {
        return Err(format!("cargo metadata failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("cargo metadata: bad JSON: {e}"))
}
