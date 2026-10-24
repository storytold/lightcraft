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

/// CI build jobs: one per 1.5 GB of RAM available when CI starts, at most one per CPU, 4 when it
/// is unknown. With line-table debuginfo, 4 jobs measured about 3 GB of RAM in use and 2 jobs
/// about 1.5 GB, so this leaves about half the available RAM to spare. Counting available rather
/// than installed RAM keeps a busy machine (browsers, VMs, editors) from being overcommitted.
/// Test threads are this, capped at 4.
fn ci_jobs(available_mb: Option<u64>, cpus: usize) -> usize {
    available_mb.map_or(4, |mb| usize::try_from(mb / 1536).unwrap_or(usize::MAX)).clamp(1, cpus.max(1))
}

/// RAM the OS could hand out now (free plus reclaimable cache) in MB, if it reports it.
fn available_ram_mb() -> Option<u64> {
    let output = |program: &str, args: &[&str]| {
        let out = Command::new(program).args(args).output().ok()?;
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if cfg!(target_os = "linux") {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb = info.lines().find_map(|l| l.strip_prefix("MemAvailable:"))?.trim().trim_end_matches("kB").trim();
        Some(kb.parse::<u64>().ok()? / 1024)
    } else if cfg!(windows) {
        let cmd = "(Get-CimInstance Win32_PerfFormattedData_PerfOS_Memory).AvailableMBytes";
        output("powershell", &["-NoProfile", "-Command", cmd])?.trim().parse().ok()
    } else {
        // macOS: free + inactive + speculative pages
        let stat = output("vm_stat", &[])?;
        let page: u64 = stat.split("page size of ").nth(1)?.split_whitespace().next()?.parse().ok()?;
        let pages = |name: &str| -> Option<u64> {
            let line = stat.lines().find(|l| l.starts_with(name))?;
            line.rsplit(':').next()?.trim().trim_end_matches('.').parse().ok()
        };
        let free = pages("Pages free")?.saturating_add(pages("Pages inactive")?).saturating_add(pages("Pages speculative").unwrap_or(0));
        Some(free.saturating_mul(page) >> 20)
    }
}

/// Installed physical memory in whole GB, if the OS reports it (the fallback when available RAM
/// can't be read: half of it is assumed available).
fn total_ram_gb() -> Option<u64> {
    let bytes: u64 = if cfg!(target_os = "linux") {
        let info = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb = info.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.trim().trim_end_matches("kB").trim();
        kb.parse::<u64>().ok()?.checked_mul(1024)?
    } else {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            ("powershell", &["-NoProfile", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
        } else {
            ("sysctl", &["-n", "hw.memsize"])
        };
        let out = Command::new(program).args(args).output().ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()?
    };
    // round to the nearest GB: "8 GB" machines report slightly less
    Some(bytes.saturating_add(1 << 29) >> 30)
}

/// The targets the test runs build: no examples (clippy `--all-targets` still checks them).
/// Doctests run on their own, [`doctests`]: nextest can't run them.
const TEST_TARGETS: [&str; 3] = ["--lib", "--bins", "--tests"];

/// `args` followed by [`TEST_TARGETS`].
fn with_test_targets<'a>(args: &[&'a str]) -> Vec<&'a str> {
    args.iter().copied().chain(TEST_TARGETS).collect()
}

/// Whether the tests run under `cargo nextest`: only when `LIGHTCRAFT_NEXTEST=1` asks for it and
/// nextest is installed. `cargo test` is the default because the demo library's generated scenes
/// are cached per process: libtest runs a binary's tests in one process and generates each scene
/// once, while nextest's process per test regenerates them (measured: the ui-egui tests take about
/// 30 min under nextest, under 5 min under cargo test).
fn nextest() -> bool {
    static FOUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FOUND.get_or_init(|| {
        if std::env::var_os("LIGHTCRAFT_NEXTEST").is_none_or(|v| v != "1") {
            return false;
        }
        let found = cargo()
            .args(["nextest", "--version"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !found {
            eprintln!("note: LIGHTCRAFT_NEXTEST=1 but cargo-nextest is not installed, so the tests run under cargo test (cargo install cargo-nextest --locked)");
        }
        found
    })
}

/// A test run of `args` (packages, features, targets, filters): `cargo nextest run`, or `cargo
/// test`. The command and how to print it.
fn tests(args: &[&str]) -> (Command, String) {
    let mut c = cargo();
    let tool = if nextest() { "nextest run" } else { "test" };
    c.args(tool.split(' ')).args(args);
    (c, format!("cargo {tool} {}", args.join(" ")))
}

/// The workspace's doctests.
fn doctests() -> (Command, String) {
    let mut c = cargo();
    c.args(["test", "--workspace", "--doc"]);
    (c, "cargo test --workspace --doc".to_owned())
}

/// `ci --quick`'s tests: build every workspace test target, then run only the test binaries of
/// `packages`. Building with `-p` instead would resolve features per selection: different
/// artifacts from `ci`'s workspace build, so it would recompile ~130 crates. Doc tests are left to
/// the full `ci`.
fn test_selected(root: &Path, packages: &std::collections::BTreeSet<String>, skip_log: &Path) -> Result<(), String> {
    let manifests: std::collections::BTreeMap<String, String> = metadata()?["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["manifest_path"].as_str()?.to_owned(), p["name"].as_str()?.to_owned())))
        .filter(|(_, name)| packages.contains(name))
        .collect();
    let what = "cargo test --workspace --no-run --lib --bins --tests";
    eprintln!("$ {what} (then the test binaries of {})", packages.iter().map(String::as_str).collect::<Vec<_>>().join(", "));
    let out = cargo()
        .args(with_test_targets(&["test", "--workspace", "--no-run", "--message-format=json-render-diagnostics"]))
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("{what}: failed to spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("{what}: exited with {}", out.status));
    }
    let mut binaries = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if msg["reason"] != "compiler-artifact" || msg["profile"]["test"] != true {
            continue;
        }
        let (Some(exe), Some(manifest)) = (msg["executable"].as_str(), msg["manifest_path"].as_str()) else { continue };
        if let Some(name) = manifests.get(manifest) {
            binaries.push((name.clone(), manifest.to_owned(), exe.to_owned()));
        }
    }
    if binaries.is_empty() {
        println!("no test binaries in {}", packages.iter().map(String::as_str).collect::<Vec<_>>().join(", "));
        return Ok(());
    }
    let mut failed = Vec::new();
    for (name, manifest, exe) in &binaries {
        let dir = Path::new(manifest).parent().unwrap_or(root);
        let mut c = Command::new(exe);
        c.current_dir(dir).env("CARGO_MANIFEST_DIR", dir).env("CARGO_PKG_NAME", name).env(ci_report::SKIP_LOG_ENV, skip_log);
        if std::env::var_os("RUST_TEST_THREADS").is_none() {
            // as the cargo runs of ci (`cargo()`): more threads make the frame-budget tests flaky
            c.env("RUST_TEST_THREADS", std::thread::available_parallelism().map_or(4, |n| n.get()).min(4).to_string());
        }
        let file = Path::new(exe).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if run(c, &format!("{name}: {file}")).is_err() {
            failed.push(format!("{name} ({file})"));
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(format!("tests failed in {}", failed.join(", "))) }
}

/// The test that prints the messages each language lacks (shown in English); it always passes.
const TRANSLATION_GAPS_TEST: &str = "i18n::tests::translation_gaps_report";

/// Print the translation gaps report into the log: [`TRANSLATION_GAPS_TEST`] of lightcraft-ui-egui,
/// run from the test binary the test step built (found with `--no-run` and the step's own
/// arguments, so nothing rebuilds). A report that can't run is a note, never a failed step.
fn translation_gaps() {
    eprintln!("\n=== ci: translation gaps ===");
    let mut c = cargo();
    c.args(["test", "--no-run", "--message-format=json"]).args(with_test_targets(&["--workspace"]));
    let binary = c.output().map_err(|e| format!("cargo test --no-run: {e}")).and_then(|out| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|m| {
                m["reason"] == "compiler-artifact"
                    && m["target"]["name"] == "lightcraft_ui_egui"
                    && m["profile"]["test"] == true
                    && m["target"]["kind"].as_array().is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
            })
            .and_then(|m| m["executable"].as_str().map(PathBuf::from))
            .ok_or_else(|| format!("no lightcraft-ui-egui test binary in the build (cargo exited with {})", out.status))
    });
    let ran = binary.and_then(|binary| {
        let mut c = Command::new(&binary);
        c.args(["--exact", TRANSLATION_GAPS_TEST, "--nocapture", "--quiet"]);
        run(c, &format!("{} --exact {TRANSLATION_GAPS_TEST} --nocapture --quiet", binary.display()))
    });
    if let Err(e) = ran {
        eprintln!("note: the translation gaps report did not run: {e}");
    }
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

fn cmd_layers() -> Result<(), String> {
    let crates = layers::from_metadata(&metadata()?)?;
    println!("Dependency layering (plan/architecture.md §3)\n");
    println!("{:<28} {:<14} workspace deps", "crate", "layer");
    for c in &crates {
        let ws: Vec<String> = c
            .deps
            .iter()
            .filter(|d| d.workspace)
            .map(|d| {
                let k = match d.kind {
                    layers::DepKind::Normal => "",
                    layers::DepKind::Dev => " (dev)",
                    layers::DepKind::Build => " (build)",
                };
                format!("{}{k}", layers::short_name(&d.name))
            })
            .collect();
        println!("{:<28} {:<14} {}", c.name, layers::describe(layers::classify(&c.name)), ws.join(", "));
    }
    let violations = layers::check(&crates);
    println!();
    if violations.is_empty() {
        println!("OK: {} crates, no layering violations.", crates.len());
        Ok(())
    } else {
        println!("{} violation(s):", violations.len());
        for v in &violations {
            println!("  - {v}");
        }
        Err(format!("{} layering violation(s)", violations.len()))
    }
}

/// Workspace packages that must build for wasm32: all L0–L5 crates plus
/// the egui shell and the web app.
fn wasm_set() -> Result<Vec<String>, String> {
    let crates = layers::from_metadata(&metadata()?)?;
    Ok(crates
        .into_iter()
        .filter(|c| match layers::classify(&c.name) {
            Some(layers::Class::Layer(l)) => l <= 5,
            Some(layers::Class::Standalone) => true,
            _ => false,
        })
        .map(|c| c.name)
        .chain(std::iter::once("lightcraft-web".to_string()))
        .collect())
}

/// One `cargo check` over the whole set; `--keep-going` reports every crate that fails.
fn cmd_wasm() -> Result<(), String> {
    cmd_wasm_for(None)
}

/// The wasm check of the wasm-safe crates, or only those of them in `only` (`ci --quick`).
fn cmd_wasm_for(only: Option<&std::collections::BTreeSet<String>>) -> Result<(), String> {
    let set: Vec<String> = wasm_set()?.into_iter().filter(|p| only.is_none_or(|o| o.contains(p))).collect();
    if set.is_empty() {
        println!("wasm32-unknown-unknown check: no changed crate builds for wasm32");
        return Ok(());
    }
    let mut c = cargo();
    c.args(["check", "--target", "wasm32-unknown-unknown", "--keep-going"]);
    for pkg in &set {
        c.args(["-p", pkg]);
    }
    run(c, &format!("cargo check --target wasm32-unknown-unknown --keep-going -p {}", set.join(" -p ")))
}

fn cmd_ci(args: &[&str]) -> Result<(), String> {
    let hardware = args.contains(&"--hardware");
    let print_md = args.contains(&"--summary-md");
    let quick = args.contains(&"--quick");
    let base_at = args.iter().position(|a| *a == "--base");
    let base = base_at.map(|i| args.get(i + 1).copied().ok_or("ci: --base needs a ref")).transpose()?;
    let unknown = args
        .iter()
        .enumerate()
        .find(|(i, a)| !matches!(**a, "--hardware" | "--summary-md" | "--quick" | "--base") && base_at.is_none_or(|b| *i != b + 1));
    if let Some((_, other)) = unknown {
        return Err(format!("ci: unknown option `{other}`\n\n{USAGE}"));
    }
    if base.is_some() && !quick {
        return Err("ci: --base only applies to --quick".into());
    }
    let root = root();
    let command = std::iter::once("cargo xtask ci").chain(args.iter().copied()).collect::<Vec<_>>().join(" ");
    // held until ci returns: one run at a time per machine (ci_lock.rs)
    let _slot = ci_lock::acquire(&root, &command)?;
    let plan = if quick {
        let (base, files) = ci_quick::changed_files(&root, base)?;
        let plan = ci_quick::plan(&files, &ci_quick::packages(&root, &metadata()?), base);
        let packages: Vec<&str> = plan.packages.iter().map(String::as_str).collect();
        eprintln!("=== ci --quick: {} changed file(s) since {} ===", plan.files, plan.base);
        eprintln!("  packages: {}", if packages.is_empty() { "none".to_owned() } else { packages.join(", ") });
        if plan.workspace_wide {
            eprintln!("  a workspace-wide file changed: testing every package");
        }
        Some(plan)
    } else {
        None
    };
    let report_dir = ci_report::prepare(&root)?;
    let started = std::time::Instant::now();
    let started_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // test runs append the skips their tests record (`lightcraft_testkit::skip!`) to
    // target/ci/skips-<step>.jsonl: a file, so it works the same under nextest and cargo test
    let test = |(mut c, what): (Command, String), step: &str| {
        c.env(ci_report::SKIP_LOG_ENV, ci_report::skip_log(&root, step));
        run(c, &what)
    };
    let gpu = std::cell::RefCell::new(serde_json::Value::Null);
    let hardware_runs = std::cell::RefCell::new(Vec::new());
    // What this machine offers the GPU tests, once, after the workspace tests (whose build the
    // probe reuses). Never fails the run: the report says what went wrong.
    let probe = || {
        if gpu.borrow().is_null() {
            eprintln!("\n=== ci: GPU probe ===");
            let t = std::time::Instant::now();
            let mut found = ci_report::gpu(probe_gpu, hardware);
            found["seconds"] = serde_json::json!(t.elapsed().as_secs_f64());
            *gpu.borrow_mut() = found;
        }
    };

    type Step<'a> = (&'static str, Box<dyn Fn() -> Result<(), String> + 'a>);
    // `--quick`: which steps apply (all of them in a full run) and the packages to test
    let wants = |step: &str| match &plan {
        None => true,
        Some(p) => match step {
            "clippy" | "test" | "wasm" => p.rust(),
            "heif" => p.heif,
            "parity" => p.parity,
            _ => true,
        },
    };
    let mut steps: Vec<Step> = vec![
        (
            "fmt",
            Box::new(|| {
                let mut c = cargo();
                c.args(["fmt", "--all", "--", "--check"]);
                run(c, "cargo fmt --all -- --check")
            }),
        ),
        (
            "clippy",
            Box::new(|| {
                let mut c = cargo();
                c.args(["clippy", "--workspace", "--all-targets", "--keep-going", "--", "-D", "warnings"]);
                run(c, "cargo clippy --workspace --all-targets --keep-going -- -D warnings")
            }),
        ),
        (
            "heif",
            Box::new(|| {
                // the optional HEIC/HEIF decoder is off in the workspace build above
                let mut c = cargo();
                c.args(["clippy", "-p", "lightcraft-codecs", "--features", "heif", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy -p lightcraft-codecs --features heif --all-targets -- -D warnings")?;
                test(
                    tests(&with_test_targets(&["-p", "lightcraft-codecs", "-p", "lightcraft-heif", "--features", "lightcraft-codecs/heif"])),
                    "heif",
                )?;
                // the engine's HEIC import test, with the decoder (the workspace run checks the error)
                test(tests(&["-p", "lightcraft-engine", "--lib", "--features", "lightcraft-codecs/heif", "heic"]), "heif")
            }),
        ),
        (
            "test",
            Box::new(|| match &plan {
                Some(p) if !p.workspace_wide => test_selected(&root, &p.packages, &ci_report::skip_log(&root, "test")),
                _ => test(tests(&with_test_targets(&["--workspace"])), "test").and_then(|()| test(doctests(), "test")),
            }),
        ),
    ];
    steps.retain(|(name, _)| wants(name));
    if hardware {
        steps.push((
            "gpu-backends",
            Box::new(|| {
                // the GPU tests once per backend with a hardware adapter (`LIGHTCRAFT_GPU_BACKEND`)
                probe();
                let backends = ci_report::hardware_backends(&gpu.borrow());
                if backends.is_empty() {
                    eprintln!("no hardware GPU adapter on any backend: nothing to run");
                }
                let mut failed = Vec::new();
                for (backend, adapter) in backends {
                    let step = format!("gpu-{backend}");
                    let (mut c, what) = tests(&with_test_targets(&["-p", "lightcraft-gpu", "--features", "denoise"]));
                    c.env("LIGHTCRAFT_GPU_BACKEND", &backend).env(ci_report::SKIP_LOG_ENV, ci_report::skip_log(&root, &step));
                    let t = std::time::Instant::now();
                    let ok = run(c, &format!("LIGHTCRAFT_GPU_BACKEND={backend} {what}")).is_ok();
                    hardware_runs.borrow_mut().push(serde_json::json!({
                        "backend": backend,
                        "adapter": adapter,
                        "status": if ok { "ok" } else { "fail" },
                        "seconds": t.elapsed().as_secs_f64(),
                    }));
                    if !ok {
                        failed.push(backend);
                    }
                }
                if failed.is_empty() { Ok(()) } else { Err(format!("the GPU tests failed on {}", failed.join(", "))) }
            }),
        ));
    }
    if wants("parity") {
        steps.push(("parity", Box::new(|| parity::run(&root, false))));
    }
    steps.push(("layers", Box::new(cmd_layers)));
    steps.push(("assets", Box::new(|| assets::run(&root))));
    if wants("wasm") {
        match plan.as_ref().filter(|p| !p.workspace_wide) {
            Some(p) => steps.push(("wasm", Box::new(|| cmd_wasm_for(Some(&p.packages))))),
            None => steps.push(("wasm", Box::new(cmd_wasm))),
        }
    }

    let mut results = Vec::new();
    let mut failure = None;
    for (name, f) in &steps {
        eprintln!("\n=== ci: {name} ===");
        let t = std::time::Instant::now();
        let r = f();
        let status = if r.is_ok() { "ok" } else { "fail" };
        results.push(serde_json::json!({"name": name, "status": status, "seconds": t.elapsed().as_secs_f64()}));
        if *name == "test" && plan.is_none() {
            // right after the workspace tests, before parity and wasm build other feature sets
            probe();
            translation_gaps();
        }
        if let Err(e) = r {
            failure = Some((*name, e));
            break;
        }
    }
    if gpu.borrow().is_null() {
        let why = if plan.is_some() { "not probed: --quick" } else { "not probed: the test step did not run" };
        *gpu.borrow_mut() = serde_json::json!({"error": why});
    }
    let done: Vec<&str> = steps.iter().take(results.len()).map(|(n, _)| *n).collect();
    match &failure {
        Some((name, e)) => {
            println!("\nCI summary:");
            for d in done.iter().filter(|d| *d != name) {
                println!("  ok    {d}");
            }
            println!("  FAIL  {name}: {e}");
            for (n, _) in steps.iter().skip(results.len()) {
                println!("  skip  {n}");
            }
        }
        None => println!("\nCI summary: all {} steps passed ({})", done.len(), done.join(", ")),
    }
    for (n, _) in steps.iter().skip(results.len()) {
        results.push(serde_json::json!({"name": n, "status": "not run"}));
    }

    let skips = ci_report::read_skips(&report_dir, &root);
    let ram_mb = available_ram_mb();
    let report = serde_json::json!({
        "schema": ci_report::SCHEMA,
        "command": command,
        "quick": plan.as_ref().map(|p| serde_json::json!({"base": p.base, "files": p.files, "packages": p.packages, "workspace_wide": p.workspace_wide})),
        "result": if failure.is_some() { "fail" } else { "pass" },
        "failed_step": failure.as_ref().map(|(n, _)| *n),
        "started_unix": started_unix,
        "seconds": started.elapsed().as_secs_f64(),
        "git": ci_report::git(&root),
        "machine": ci_report::machine(total_ram_gb(), ram_mb),
        "toolchain": ci_report::toolchain(),
        "capabilities": {"gpu": gpu.take(), "data": ci_report::data(&root, &RAW_SAMPLES.iter().map(|s| s.0).collect::<Vec<_>>())},
        "steps": results,
        "hardware": hardware_runs.take(),
        "skips": ci_report::skips_json(&skips),
    });
    print!("{}", ci_report::summary_text(&report));
    let md = ci_report::summary_md(&report);
    let written = serde_json::to_string_pretty(&report)
        .map_err(|e| e.to_string())
        .and_then(|json| std::fs::write(report_dir.join("report.json"), json + "\n").map_err(|e| e.to_string()))
        .and_then(|()| std::fs::write(report_dir.join("summary.md"), &md).map_err(|e| e.to_string()));
    match written {
        Ok(()) => println!("Report: target/ci/report.json; to paste into a PR: target/ci/summary.md (or run with --summary-md)"),
        Err(e) => eprintln!("ci: could not write the report to {}: {e}", report_dir.display()),
    }
    if print_md {
        println!("\n{md}");
    }
    match failure {
        Some((name, _)) => Err(format!("ci failed at `{name}`")),
        None => Ok(()),
    }
}

/// Run the ignored `gpu_probe` test of lightcraft-gpu: the pipeline's device (`None`) or one
/// backend's adapters. `--workspace`, like the test step, so its build is reused (every crate's `it`
/// binary starts; only lightcraft-gpu's has the test). Its stdout, or why
/// it failed (a driver that crashes while it is enumerated, issue #136, ends up here, not in xtask).
fn probe_gpu(backend: Option<&str>) -> Result<String, String> {
    let mut c = cargo();
    c.args(["test", "-q", "--workspace", "--test", "it", "--", "--ignored", "--exact", "gpu_probe::gpu_probe", "--nocapture"]);
    c.env("LIGHTCRAFT_GPU_PROBE", backend.unwrap_or(""));
    let out = c.output().map_err(|e| format!("gpu_probe: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().filter(|l| !l.trim().is_empty()).take(3).collect();
        return Err(format!(
            "gpu_probe {}exited with {}: {}",
            backend.map(|b| format!("{b} ")).unwrap_or_default(),
            out.status,
            tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// CC0 raw samples (raw.pixls.us). Small, representative set; extend freely (CC0 only).
/// CC0 raw samples from raw.pixls.us (each verified CC0 on the site).
/// The third field is the SHA-256 of the file as downloaded; `cargo xtask corpus --download` checks it.
/// One per format / compression variant we decode or deliberately report as unsupported (preview only).
const RAW_SAMPLES: &[(&str, &str, &str)] = &[
    // Camera-coverage sweep: CC0 files, sensor hashes and comparisons in docs/raw/coverage-sweep.tsv.
    (
        "coverage-915.dng",
        "https://raw.pixls.us/getfile.php/915/nice/Apple%20-%20iPhone%206s%20Plus%20-%2016bit%20%284:3%29.DNG",
        "845fee4f09c832af3728778737f5ae9b99124debb821b6e2066b94aed1daa91c",
    ),
    (
        "coverage-939.dng",
        "https://raw.pixls.us/getfile.php/939/nice/Apple%20-%20iPhone%207%20Plus%20-%2016bit%20%284:3%29.DNG",
        "e8c7ebd8f22f7281d165496b91c8fea8a8e79c8113172fb499e1d66d6045332e",
    ),
    (
        "coverage-3372.dng",
        "https://raw.pixls.us/getfile.php/3372/nice/Apple%20-%20iPhone%20XS%20-%2016bit%20%284:3%29.dng",
        "50bc64ee8cfbbfb6c0531e29ab428ff985506a16fe2d1d2e74656f21b54505f6",
    ),
    (
        "coverage-3009.dng",
        "https://raw.pixls.us/getfile.php/3009/nice/Hasselblad%20-%20L1D-20c%20-%2016bit%20%283:2%29.DNG",
        "7afecae0db80d832941eeeb3c0eb2dc03f5696637b6e78b3c592e8c002dce4ec",
    ),
    (
        "coverage-5140.dng",
        "https://raw.pixls.us/getfile.php/5140/nice/Hasselblad%20-%20L2D-20c%20-%2016bit%20%284:3%29.DNG",
        "dbe98cb202f884ff11e0144899613b26d33cf9cddd9f1a62226f4352397f18e4",
    ),
    (
        "coverage-8041.dng",
        "https://raw.pixls.us/getfile.php/8041/nice/DJI%20-%20FC9287%20-%2016bit%20%2816:9%29.DNG",
        "901b7970f02f7197c782f111363feef02108f75811342d8cc4a2b66757bb7af0",
    ),
    (
        "coverage-4785.dng",
        "https://raw.pixls.us/getfile.php/4785/nice/DJI%20-%20FC7303%20-%2016bit%20%284:3%29.DNG",
        "21a49e63966f82391ca1188c7559c3acc01a35a89a70194fda4bf44cbd64a851",
    ),
    (
        "coverage-4898.dng",
        "https://raw.pixls.us/getfile.php/4898/nice/DJI%20-%20FC7303%20-%2016bit%20%2816:9%29.DNG",
        "fb4f294e12cab7b507fd0b7c07f24e5d13e7c0dd127b534ce12c48d644c9363d",
    ),
    (
        "coverage-3502.dng",
        "https://raw.pixls.us/getfile.php/3502/nice/Google%20-%20Pixel%204%20XL%20-%2016bit%20%284:3%29.dng",
        "0c03a6c93b4b8593cd0e4426166e6389710007238b82eb23861ef191322f0c54",
    ),
    (
        "coverage-4176.dng",
        "https://raw.pixls.us/getfile.php/4176/nice/Google%20-%20Pixel%204a%20-%2016bit%20%284:3%29.dng",
        "11b38186ee3455376291bcdb3049728a3f9dcd94b9e82ff76855d9f3430e44be",
    ),
    (
        "coverage-5961.dng",
        "https://raw.pixls.us/getfile.php/5961/nice/Google%20-%20Pixel%206%20Pro%20-%2016bit%20%284:3%29.dng",
        "3a859e917a91698a3e15ce3b3721a48a1fb1296873e0697d3bf7c2793297200d",
    ),
    (
        "coverage-7751.dng",
        "https://raw.pixls.us/getfile.php/7751/nice/Google%20-%20Pixel%208%20Pro%20-%2016bit%20%284:3%29.dng",
        "45420c595401547cca950ae58d552bc82b32d31ce1d58c082df33004016197f5",
    ),
    (
        "coverage-7217.3fr",
        "https://raw.pixls.us/getfile.php/7217/nice/Hasselblad%20-%20CFV-50c%20-%2016bit%20%284:3%29.3FR",
        "e27ff177519298fe7bae64506b975190789dc0e6ad70bea58be7c78fd094eae0",
    ),
    (
        "coverage-7218.fff",
        "https://raw.pixls.us/getfile.php/7218/nice/Hasselblad%20-%20CFV-50c%20-%2016bit%20%284:3%29.fff",
        "c8d78912ae725a59acff42a4b4731350bbddd4a600423f319579261bd41da932",
    ),
    (
        "coverage-1640.fff",
        "https://raw.pixls.us/getfile.php/1640/nice/Hasselblad%20-%20H5D-40%20-%2016bit%20%284:3%29.fff",
        "209d464e13702de5f4fdb830ac26ac140c98a8d0dc3e81c5ba61d93677330832",
    ),
    (
        "coverage-1639.fff",
        "https://raw.pixls.us/getfile.php/1639/nice/Hasselblad%20-%20H5D-50c%20-%2016bit%20%284:3%29.fff",
        "1aad9319fcbab81b851a74c3eeaa2a07fc5013bf2c4e272fd31d6419b6b9adb0",
    ),
    (
        "coverage-2058.3fr",
        "https://raw.pixls.us/getfile.php/2058/nice/Hasselblad%20-%20X1D%20-%2016bit%20%284:3%29.3FR",
        "395acb2c706dc10270c58ec261e42a5e36f6ac8c39d2a767e86e83648aa318a3",
    ),
    (
        "coverage-3874.fff",
        "https://raw.pixls.us/getfile.php/3874/nice/Hasselblad%20-%20X1D%20II%2050C%20-%2016bit%20%284:3%29.fff",
        "8c2451b3478fb529b4e9bd51ed3267999ce26eb1fcb5c3b93c3ae7bc56267794",
    ),
    (
        "coverage-3941.3fr",
        "https://raw.pixls.us/getfile.php/3941/nice/Hasselblad%20-%20X1D%20II%2050C%20-%2016bit%20%284:3%29.3FR",
        "809d498263d6f07cbaa4b9a5be97933057b4ab21e7243bb7afbe3862f3b48b47",
    ),
    (
        "coverage-6568.fff",
        "https://raw.pixls.us/getfile.php/6568/nice/Hasselblad%20-%20X2D%20100C%20-%2016bit%20%284:3%29.fff",
        "44977fdaa3063eacbb2ac416dfb531bb4e3b96f3745d073dc49fb1ebcd78bedc",
    ),
    (
        "coverage-6565.3fr",
        "https://raw.pixls.us/getfile.php/6565/nice/Hasselblad%20-%20X2D%20100C%20-%2016bit%20%284:3%29.3FR",
        "dcc5a4abe3498e6f25e89bb491995fd12c3b669d9277c71b45b49210d3e56280",
    ),
    (
        "coverage-4726.nef",
        "https://raw.pixls.us/getfile.php/4726/nice/Nikon%20-%20D6%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "3a098e704d0e1af97efc37541b43a37e5b3457d2f4e7828c0d8d9af37332f233",
    ),
    (
        "coverage-4720.nef",
        "https://raw.pixls.us/getfile.php/4720/nice/Nikon%20-%20D6%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "827bcfff8a3cd86278382c353e4542faab52cf12c365540f30753bb4c6fabc0f",
    ),
    (
        "coverage-4721.nef",
        "https://raw.pixls.us/getfile.php/4721/nice/Nikon%20-%20D6%20-%2014bit%2014bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "081d5dbc1d6c97935020d33ffa486dcaa8d4564cbde280cfadcf26ff61da2838",
    ),
    (
        "coverage-3584.nef",
        "https://raw.pixls.us/getfile.php/3584/nice/Nikon%20-%20Z%206%20-%2012bit%2012bit%20compressed%20%283:2%29.NEF",
        "3cbd9acf9e9bfd7d0c62c46436109556a101f7535c5fd25f86dad6ea61bf3886",
    ),
    (
        "coverage-3581.nef",
        "https://raw.pixls.us/getfile.php/3581/nice/Nikon%20-%20Z%206%20-%2014bit%2014bit%20compressed%20%283:2%29.NEF",
        "8ca94584578b35a422744c1a610174ce37df0691588edfdecaec3111b3fbbb02",
    ),
    (
        "coverage-3582.nef",
        "https://raw.pixls.us/getfile.php/3582/nice/Nikon%20-%20Z%206%20-%2014bit%2014bit%20lossless%20compressed%20%283:2%29.NEF",
        "c079345fc93f53a4f0d322f8ddaae505f920c36015f25b58befccaa61db1af31",
    ),
    (
        "coverage-2792.nef",
        "https://raw.pixls.us/getfile.php/2792/nice/Nikon%20-%20Z%207%20-%2012bit%2012bit%20compressed%20%283:2%29.NEF",
        "c861546fbcf5826d5eb26d223e2143fa32968e89f7edbc5bb10c283ea5461ded",
    ),
    (
        "coverage-2789.nef",
        "https://raw.pixls.us/getfile.php/2789/nice/Nikon%20-%20Z%207%20-%2012bit%2012bit%20compressed%20%283:2%29.NEF",
        "e54a6c4264bf685214f538036fd9d39088e510b2bfedf288e568f0db1897880e",
    ),
    (
        "coverage-2790.nef",
        "https://raw.pixls.us/getfile.php/2790/nice/Nikon%20-%20Z%207%20-%2012bit%2012bit%20compressed%20%283:2%29.NEF",
        "f0e756dbfd00f3b71cb3d8aa5b9e2e1be32f1052a6c4380e09ebc7c5e7748114",
    ),
    (
        "coverage-6617.nef",
        "https://raw.pixls.us/getfile.php/6617/nice/Nikon%20-%20Z%208%20-%208bit%208bit%20compressed%20%283:2%29.NEF",
        "c13a8675294456b9c979e6c1e556951654f35377b2870af2874bfb556f21ce40",
    ),
    (
        "coverage-2912.dng",
        "https://raw.pixls.us/getfile.php/2912/nice/Parrot%20-%20Anafi%20-%2016bit%20%284:3%29.DNG",
        "a257a296047ff2d396109fd2e6947fa36aa7986695b6815f83979d59e98a84e6",
    ),
    (
        "coverage-6728.dng",
        "https://raw.pixls.us/getfile.php/6728/nice/Samsung%20-%20Galaxy%20S21%20Ultra%20-%204:3.dng",
        "5baeb5f0c4a125647df03b82c2d38b0f02784393bd39eff1a5b23d8f5308dc5e",
    ),
    (
        "coverage-6261.dng",
        "https://raw.pixls.us/getfile.php/6261/nice/Samsung%20-%20Galaxy%20S22%20Ulra%20-%204:3.dng",
        "53e7f83a94c965495ba32d3ea6021ef08ed02244ce64d111d1329bbd01fd5581",
    ),
    (
        "coverage-6697.dng",
        "https://raw.pixls.us/getfile.php/6697/nice/Samsung%20-%20Galaxy%20S23%20-%204:3.dng",
        "1c04574f32b5a139922f075291c52018b89d9981ee155a16958a231c1fdbb067",
    ),
    (
        "coverage-1297.dng",
        "https://raw.pixls.us/getfile.php/1297/nice/Samsung%20-%20G935F%20-%2016bit%20%2816:9%29.dng",
        "843617c8b572320b7badfb4ac2eda19479f5804a1dabb909c861bde23ed30561",
    ),
    (
        "coverage-2645.dng",
        "https://raw.pixls.us/getfile.php/2645/nice/Samsung%20-%20SM-G950F%20-%2016bit%20%284:3%29.dng",
        "6abdd353094964eaadddf69bba3a0d551ecce1bc0ed013a434691df4f64dcbf2",
    ),
    (
        "coverage-2310.dng",
        "https://raw.pixls.us/getfile.php/2310/nice/Samsung%20-%20SM-G955F%20-%2016bit%20%284:3%29.dng",
        "3cf5da4331d062bc6b31e0e8f5b7ff2cd21107de4262d79a2667f670516e8e8c",
    ),
    (
        "coverage-7273.dng",
        "https://raw.pixls.us/getfile.php/7273/nice/Sigma%20-%20fp%20-%2014bit%20%283:2%29.DNG",
        "0844bcf23a49e21b5c2f8489de55ce38fb7b594e3ed337d6b9af2dd9341354e9",
    ),
    (
        "coverage-6949.dng",
        "https://raw.pixls.us/getfile.php/6949/nice/Sigma%20-%20fp%20L%20-%2014bit%20%283:2%29.DNG",
        "8e7229f28be30bfa45289940eac04194af2b3474f6db286e4fa4070c145a0a70",
    ),
    (
        "arw-sony-a7rm4a-compressed.arw",
        "https://raw.pixls.us/getfile.php/4822/nice/Sony%20-%20ILCE-7RM4A%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "690c774f1d7bc1db3fa8c2489762743d8e7586e50b6f65d0b6a61cb467972c67",
    ),
    (
        "arw-sony-a9m2-compressed.arw",
        "https://raw.pixls.us/getfile.php/3989/nice/Sony%20-%20ILCE-9M2%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "161c2a9da2b5f1e50be6117a0b4da0ce249660b5d7de13568d301c4034716b97",
    ),
    (
        "arw-sony-a7m3-compressed.arw",
        "https://raw.pixls.us/getfile.php/2414/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "250784580ea527442c09004417bb0eead484f2bf3ee8f9121a776ac65bb50d0f",
    ),
    (
        "arw-sony-a7m3-uncompressed.arw",
        "https://raw.pixls.us/getfile.php/2418/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.ARW",
        "ece80551abf64949dbe826985a80f1d9b265401ee4ea6b989e1829549616fdcd",
    ),
    (
        "arw-sony-a7m4-14bit.arw",
        "https://raw.pixls.us/getfile.php/6936/nice/Sony%20-%20ILCE-7M4%20-%2014bit%20%283:2%29.ARW",
        "639a6d4db881f1359e3ea7e1137b314e59417d24e6d4d862b7202111a2c823b2",
    ),
    // lossless compressed (Compression 7): L is 2×2 CFA cells per LJ92 sample; M and S are subsampled
    (
        "arw-sony-a7m4-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW",
        "851b43c2116c4139104a5036f83ac3b6a148789b2142214dd7192c13972b25b6",
    ),
    (
        "arw-sony-a7m4-lossless-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06675_FullFrame-LossLess-Compressed-Medium.ARW",
        "d453005714327addd75bcb99c1c6223173dc92f2a59542d82e6761ef0e0e7571",
    ),
    (
        "arw-sony-a7m4-lossless-s.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06676_FullFrame-LossLess-Compressed-Small.ARW",
        "cbbd0930c7d8706dff84c68a2004454266e6fd0d8354f5f76a106b5d776e0223",
    ),
    // ILCE-7CR (61 MP), one scene in three codings: lossless compressed L (2×2 CFA cells per LJ92 sample) and M
    // (subsampled YCbCr), and compressed ARW2
    (
        "arw-sony-a7cr-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00795.ARW",
        "3e1642f3a1ae7c9f93228c5f09f27e362608e6a4ca1b3e5d8dd9eb8fae576997",
    ),
    (
        "arw-sony-a7cr-lossless-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00796.ARW",
        "5bcba4acc52a5b902074581b4a2f5fb3c79ec56bd0fdc1746b9e2c0f9d3e9c88",
    ),
    (
        "arw-sony-a7cr-compressed.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CR/DSC00798.ARW",
        "f5096e8fbccf0842c8a57763cabd2836608f019eb7fd54f7b22260c2f5bc4da5",
    ),
    // ILCE-7CM2 (33 MP, issue #697): lossless compressed L, and the compressed "M-size" coding, which is Bayer at
    // 4736×3132 with black 512 (only the lossless M/S codings are YCbCr with the 1024 pedestal)
    (
        "arw-sony-a7cm2-lossless-l.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CM2/DSC02431.ARW",
        "342d893bd71fd050c273df181e3039d3f12d88947b0662de5de5325c84cc449b",
    ),
    (
        "arw-sony-a7cm2-compressed-m.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7CM2/DSC02436.ARW",
        "002c86d38a16b08a2438780728b4fda8efcc745fa6c26231dad6af67b383b11e",
    ),
    // ILCA-99M2 (42 MP, compressed): checks that it takes its colour from the bundled ILCA-99M2 profile
    (
        "arw-sony-a99m2.arw",
        "https://raw.pixls.us/data/Sony/ILCA-99M2/BR_00086.ARW",
        "be898847ac6da485874d95469bf274df1f8f77f8b8542460464f0f393981b2d2",
    ),
    // a dusk sky clipped in green behind a poplar: the clipped-highlight colour of issue #523
    (
        "arw-sony-a7rm4-14bit-compressed.arw",
        "https://raw.pixls.us/getfile.php/3480/nice/Sony%20-%20ILCE-7RM4%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
        "e6dafe42643f69ab9d1fd00414b7a1f5104df354bb589201600ac934b29b5e4a",
    ),
    // pre-2017 bodies: white balance and black level only in the encrypted SR2SubIFD (#148)
    (
        "arw-sony-rx100m3.arw",
        "https://raw.pixls.us/data/Sony/DSC-RX100M3/DSC00734.ARW",
        "cb81392e3a8810231ce341fb45de3e4d1b9e2f87bc47f3c314d084041ccfd138",
    ),
    (
        "arw-sony-rx100.arw",
        "https://raw.pixls.us/data/Sony/DSC-RX100/DSC00838.ARW",
        "579a485b5126a25cbd55cbd5dadfa7d09cf021c99cc7d4869f9e56e3f759390b",
    ),
    (
        "arw-sony-a7rm2-12bit-uncompressed.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7RM2/12-bit-uncompressed.ARW",
        "71e0888396ef52c7e6a990b4e72ebb4d8413a1fc1c35bada449870539cef6e39",
    ),
    // older DSLRs whose SR2SubIFD keeps the black level elsewhere than the bodies above (#535): A500 (27152-byte
    // layout, like the A450/A550), A700 (62112 bytes)
    (
        "arw-sony-a500.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A500/DSC02421.ARW",
        "1407fb596a391df67c15b90026a1a76702a2e7386dbdff2f0815b903ea49cead",
    ),
    (
        "arw-sony-a700.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A700/DSC07249.ARW",
        "3159e28892bf0771d01525cd4b6190f9c15bbb19fa2fab6d2515ced0594e8f40",
    ),
    // SR2SubIFD white balance (#535): the A500 and A700 above, a third layout of a body that had none before
    // (SLT-A33, 29000 bytes), and two shots whose maker-note gains differ from the preset applied (5600 K, Shade)
    ("arw-sony-a33.arw", "https://raw.pixls.us/data/Sony/SLT-A33/DSC01867.ARW", "1a59856394f10d4fadb40f5ab9c6d1c89f473f4dbcdefd216fbbbbf1ad8d21f9"),
    (
        "arw-sony-a3500-5600k.arw",
        "https://raw.pixls.us/data/Sony/ILCE-3500/DSC06923.ARW",
        "04c4fe04425c3e6fff7c8af0f811923c35e750300373357bd54342150c5505d8",
    ),
    (
        "arw-sony-a7s-shade.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7S/DSC04125.ARW",
        "7cc338a0abc8fdad32d61006f1f8f412297b93e040c7efba572eb2bd8f8e8be2",
    ),
    // pre-2017 bodies in the camera's 16:9 mode (#535): the ILCE-7SM2 records it in FullImageSize, the DSLR-A580 only
    // in the Exif image size
    (
        "arw-sony-a7sm2-16x9.arw",
        "https://raw.pixls.us/data/Sony/ILCE-7SM2/DSC01005.ARW",
        "46044fb6a9c805f2b4915cd4970316b4d04b0566bcf5287afaad3ef648d7a3db",
    ),
    (
        "arw-sony-a580-16x9.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A580/RAW_SONY_A580.ARW",
        "5b0924d39151239dce19e92e08318f4f62a5a8ac276bd7180ec8463d2cfee709",
    ),
    // packed 12-bit ARW (two pixels per three bytes): the DSLR-A900
    (
        "arw-sony-a900-packed12.arw",
        "https://raw.pixls.us/data/Sony/DSLR-A900/_DSC7969.ARW",
        "ac7c1532df77c321e8010aa1be60c1b9f245b1ac0db6f38fe617a73aae81af50",
    ),
    // Sony ILCE-7RM6 lossless, CC0 1.0 (RawDB): the APS-C file, for its lens tables (`vendor::arw_lens_rm6`). The
    // full-frame lossless file is not pinned (at most two pins per PR); the two ARW6 pins below cover Compressed files.
    (
        "arw-sony-ilce7rm6-apsc-land-lossless.arw",
        "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/raw_modes/ILCE-7RM6_APSC_land_RAW.ARW",
        "7aaf0b3941509d7364a6f03844783155ebc912d246d1c456e5cdb4693ec09fbc",
    ),
    // CR2 colour-filter layouts differ by model (issue #85): CR2CFAPattern 3 (GBRG) and 1 (RGGB) samples
    (
        "cr2-canon-40d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%2040D/_MG_0153.CR2",
        "775c806358fedddec7622113a5e399a3330bd5a65e35e37ae1108d8bf58067be",
    ),
    (
        "cr2-canon-550d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%20550D/IMG_4047.CR2",
        "f390e0ba566f0be9af2d8e7f955b815996630c0c3cb13fcfb95d98baec1890f6",
    ),
    (
        "cr2-canon-5d2.cr2",
        "https://raw.pixls.us/data/Canon/EOS%205D%20Mark%20II/08.canon.raw.cr2",
        "12b3ad6aed9fbcd19ba3bfffcd0769aa5a041d610205eee840bf4b2c420dffae",
    ),
    (
        "cr2-canon-5dsr.cr2",
        "https://raw.pixls.us/data/Canon/EOS%205DS%20R/_DSR2002.CR2",
        "01b94b0586419d64894ccdf2e7b9f1defeedc7b51dfcd306c622fe784181a9b8",
    ),
    (
        "cr2-canon-6d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%206D/EOS_6D_RAW.CR2",
        "360842779d08fe4805b4a2fe4979f06fdc6da982c85c3acb181f25bcf4c9f536",
    ),
    (
        "cr2-canon-7d.cr2",
        "https://raw.pixls.us/data/Canon/EOS%207D/RAW_CANON_EOS_7D-raw.CR2",
        "b5e47c5fcf7332ac03e0134926f17a338a42e68c1fd7f83e16f45f4b767544e8",
    ),
    (
        "cr2-canon-5d3-sraw2.cr2",
        "https://raw.pixls.us/getfile.php/773/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%20sRAW2%20%28sRAW%29.CR2",
        "a79065e78a6c5bdbc8726e786ade02d54a7d496cc3cb4504d804c924a3d999da",
    ),
    (
        "cr2-canon-5d3.cr2",
        "https://raw.pixls.us/getfile.php/771/nice/Canon%20-%20EOS%205D%20Mark%20III.CR2",
        "ec069b178b9383d80c72f8cdc1b79bd54ca451e7c20793b109d8ad73bef6bdf6",
    ),
    (
        "cr2-canon-80d.cr2",
        "https://raw.pixls.us/getfile.php/1294/nice/Canon%20-%20EOS%2080D%20-%20RAW%20%283:2%29.CR2",
        "8f28465cee09844ffad95dbd3ec91923717a329c5ec4f3f405f5b4fce83aaea3",
    ),
    (
        "cr3-canon-m50-craw.cr3",
        "https://raw.pixls.us/getfile.php/2663/nice/Canon%20-%20EOS%20M50%20-%20CRAW%20%283:2%29.CR3",
        "15384b775867ec4c42b11882837f1e368cedc0561832ffab271221e6bb80be4c",
    ),
    (
        "cr3-canon-m50-raw.cr3",
        "https://raw.pixls.us/getfile.php/4657/nice/Canon%20-%20EOS%20M50%20-%203:2.CR3",
        "1ba9ad6b51b315b88820eb0fe5fd7f52bc136ba37b1a5ca76825ebfe25b18058",
    ),
    (
        "cr3-canon-r100-raw.cr3",
        "https://raw.pixls.us/getfile.php/7896/nice/Canon%20-%20EOS%20R100%20-%20RAW%20%283:2%29.CR3",
        "6d83217d58a5e6d2dabcf470430e91a16676b60531c025b81fa7453fc74b0521",
    ),
    (
        "cr3-canon-r100-craw.cr3",
        "https://raw.pixls.us/getfile.php/7897/nice/Canon%20-%20EOS%20R100%20-%20CRAW%20%283:2%29.CR3",
        "0b66842b2fe00329ebc05fe8ab9357ddbbe1a0a2d2e69a2893102de7fdf5c942",
    ),
    (
        "cr3-canon-r8-raw.cr3",
        "https://raw.pixls.us/getfile.php/6585/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3",
        "7d5c6dbb11ff7e6ee58715d90a20f5d801e8b103f4711ff6ea329ceafcef254c",
    ),
    (
        "cr3-canon-r8-craw.cr3",
        "https://raw.pixls.us/getfile.php/6587/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3",
        "df33cf394573645ce03dea1b2e9f0b5cc2b7734e9e3391ee7114151414cf2812",
    ),
    (
        "cr3-canon-rp-raw.cr3",
        "https://raw.pixls.us/getfile.php/4674/nice/Canon%20-%20EOS%20RP%20-%203:2.CR3",
        "dcbd0f6f9929de2e2331b0fd10a9d01e7cfd04c5a5e4038456de1647d714cdf2",
    ),
    (
        "cr3-canon-rp-craw.cr3",
        "https://raw.pixls.us/getfile.php/4672/nice/Canon%20-%20EOS%20RP%20-%203:2.CR3",
        "bd3270610593d37ff8a399908bb9406a4e32f9151d55bf19ac1cf8f10d787b46",
    ),
    (
        "cr3-canon-rp-raw-crop.cr3",
        "https://raw.pixls.us/getfile.php/4673/nice/Canon%20-%20EOS%20RP%20-%203:2.CR3",
        "7d4ba45dc9264d598336133593c677b09774bcdf13b602ee770e72415a652a16",
    ),
    (
        "cr3-canon-rp-craw-crop.cr3",
        "https://raw.pixls.us/getfile.php/4671/nice/Canon%20-%20EOS%20RP%20-%203:2.CR3",
        "4a859ab137f792aedaf3416b0aad1497ebaab02052a49453902e226699154aa7",
    ),
    // EOS R5 Mark II, full frame and 1.6x crop mode: odd-height (2825- / 1787-row) Bayer planes.
    (
        "cr3-canon-r5m2-raw.cr3",
        "https://raw.pixls.us/getfile.php/7881/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20RAW%20%283:2%29.CR3",
        "d6e6aa3b493ea8936dff0efb0cc17c87471437e7317bce37c8c82ba08b9cab7b",
    ),
    (
        "cr3-canon-r5m2-craw.cr3",
        "https://raw.pixls.us/getfile.php/7882/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20CRAW%20%283:2%29.CR3",
        "b3c08a5c7a97299dfd58896a12b939e7b43ec78682761812524825e6e5d087be",
    ),
    (
        "cr3-canon-r5m2-apsc-raw.cr3",
        "https://raw.pixls.us/getfile.php/7883/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20RAW%20%283:2%29.CR3",
        "9f88c0ff012924193b7f4497559d133dfe626766d600b5d67689156721739ead",
    ),
    (
        "cr3-canon-r5m2-apsc-craw.cr3",
        "https://raw.pixls.us/getfile.php/7884/nice/Canon%20-%20EOS%20R5%20Mark%20II%20-%20CRAW%20%283:2%29.CR3",
        "6a597497776a6abbfab8fcf12d0086ac7a3f8afea2879d61accf6bbf48d357d7",
    ),
    (
        "cr3-canon-m200.cr3",
        "https://raw.pixls.us/getfile.php/8469/nice/Canon%20-%20EOS%20M200%20-%20RAW%20%283:2%29.CR3",
        "9b900c04ae37ba827b7894e66907657a0b174a74855c40039a8ca0c95583f141",
    ),
    (
        "cr3-canon-r10.cr3",
        "https://raw.pixls.us/getfile.php/5871/nice/Canon%20-%20EOS%20R10%20-%203:2.CR3",
        "5d2e076d38e7835d4c4f13dc2030a913cc573a31d7199b60777d23a25ccc3ef9",
    ),
    (
        "cr3-canon-r50v.cr3",
        "https://raw.pixls.us/getfile.php/8403/nice/Canon%20-%20EOS%20R50%20V%20-%20RAW%20%283:2%29.CR3",
        "955ff98710505498ce7f98cbcc5875793ef2d6941b3b5692ed3baf2817ee7c9f",
    ),
    (
        "cr3-canon-r6m3.cr3",
        "https://raw.pixls.us/getfile.php/8961/nice/Canon%20-%20EOS%20R6%20Mark%20III%20-%20RAW%20%283:2%29.CR3",
        "e491e4bb960961b5fa299361bf698310a80cbe7b15d30d8dad3bb21bc5457dab",
    ),
    (
        "cr3-canon-powershot-v1-craw.cr3",
        "https://raw.pixls.us/getfile.php/8500/nice/Canon%20-%20PowerShot%20V1%20-%20CRAW%20%283:2%29.CR3",
        "0a0abfd81b3a34fa513ac8ed24c809275160d65b07e892c2eb4353b77c535a31",
    ),
    (
        "cr3-canon-1dxm3-craw.cr3",
        "https://raw.pixls.us/getfile.php/4654/nice/Canon%20-%20EOS-1D%20X%20Mark%20III%20-%203:2.CR3",
        "e5755d1414a2dd51fb0120b272058f00b0199861646070c00304d927eb119560",
    ),
    (
        "cr3-canon-250d-craw.cr3",
        "https://raw.pixls.us/getfile.php/3383/nice/Canon%20-%20EOS%20250D%20-%20C-RAW%20%283:2%29.CR3",
        "69bdd267defb3d676f41f62a5e5ea65651669a04191cf4e89740917c59cedbcc",
    ),
    (
        "cr3-canon-850d.cr3",
        "https://raw.pixls.us/getfile.php/4867/nice/Canon%20-%20EOS%20850D%20-%203:2.CR3",
        "47f7747b74073515bdbd65015738bc31947504d68255c5377baa21f179df2486",
    ),
    (
        "cr3-canon-90d-craw.cr3",
        "https://raw.pixls.us/getfile.php/4650/nice/Canon%20-%20EOS%2090D%20-%203:2.CR3",
        "c8c918b2d1a53e64d7d5009ebac89a920dbba5f766c2f396bb4ec95312b21034",
    ),
    (
        "cr3-canon-m50m2.cr3",
        "https://raw.pixls.us/getfile.php/4430/nice/Canon%20-%20EOS%20M50%20Mark%20II%20-%203:2.CR3",
        "1a3c11d85f48b8c644664b505f69fbdd75eb51270bb8ab556dbc660f0487e957",
    ),
    (
        "cr3-canon-m6m2-craw.cr3",
        "https://raw.pixls.us/getfile.php/3714/nice/Canon%20-%20EOS%20M6%20Mark%20II%20-%20C-RAW%20%283:2%29.CR3",
        "94635c6f91e7d743cee6881cc2519992ae9f9031fd86b49e5d0f16edf5961b55",
    ),
    (
        "cr3-canon-r3.cr3",
        "https://raw.pixls.us/getfile.php/5051/nice/Canon%20-%20EOS%20R3%20-%203:2.CR3",
        "295d46a4d5383895e63126f67000d4352a5bb425ee4d8b16eb6b6c284bdf7994",
    ),
    (
        "cr3-canon-r50-craw.cr3",
        "https://raw.pixls.us/getfile.php/6646/nice/Canon%20-%20EOS%20R50%20-%20CRAW%20%283:2%29.CR3",
        "b79af9ffa184dd740e8af7a9f451f7150053d7e456b3435ca2e43bbbf5f65026",
    ),
    (
        "cr3-canon-r5-craw.cr3",
        "https://raw.pixls.us/getfile.php/4698/nice/Canon%20-%20EOS%20R5%20-%203:2.CR3",
        "2deae46b816e56ba91ae7a3892ca121290e12d5b22afb11f5b05a3d8f09f759f",
    ),
    (
        "cr3-canon-r6-craw.cr3",
        "https://raw.pixls.us/getfile.php/4660/nice/Canon%20-%20EOS%20R6%20-%203:2.CR3",
        "4775df76433b1e5844bc7a42b95d93efc8438c0a4cd9681e120e23fe3e3007e8",
    ),
    (
        "cr3-canon-r6m2-craw.cr3",
        "https://raw.pixls.us/getfile.php/6409/nice/Canon%20-%20EOS%20R6%20Mark%20II%20-%203:2.cr3",
        "409eb724212abde94e18fbd3170abf3264951fab13384c36cbf6fb4f22ea4201",
    ),
    (
        "cr3-canon-r7.cr3",
        "https://raw.pixls.us/getfile.php/5633/nice/Canon%20-%20EOS%20R7%20-%203:2.CR3",
        "887439cb1a45becd6a5c85fe75ae10e6c520a9f13a8b9ed077c6cf5d7c37700c",
    ),
    (
        "cr3-canon-r-craw.cr3",
        "https://raw.pixls.us/getfile.php/4543/nice/Canon%20-%20EOS%20R%20-%203:2.CR3",
        "1b3efe0664a28a0decb53e225cbfecdbc0dd27d85abdc4683fc2a4add5129b4d",
    ),
    (
        "cr3-canon-g5xm2-craw.cr3",
        "https://raw.pixls.us/getfile.php/3506/nice/Canon%20-%20PowerShot%20G5%20X%20Mark%20II%20-%20C-RAW%20%283:2%29.CR3",
        "16d7b944344004c866bf2f84c5673969e76adfe16e5fb4388da48c72bdd58058",
    ),
    (
        "cr3-canon-g7xm3.cr3",
        "https://raw.pixls.us/getfile.php/6053/nice/Canon%20-%20PowerShot%20G7%20X%20Mark%20III%20-%203:2.CR3",
        "63ce623055dd2ac7374e0b1c645a4eabc7e181cd12c2039625f64b4e63a5ee86",
    ),
    (
        "cr3-canon-sx70.cr3",
        "https://raw.pixls.us/getfile.php/3276/nice/Canon%20-%20PowerShot%20SX70%20HS%20-%20RAW%2C%204:3%20%284:3%29.CR3",
        "3b346078836f933f505338ba45965be17a39cac82bfffc7006db07e3af944839",
    ),
    (
        "dng-adobe-canon-5d3-linear-lj92.dng",
        "https://raw.pixls.us/getfile.php/1032/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossless%20JPEG%20compression%2C%20rgb%20%283:2%29.DNG",
        "0608c9f277307b745530e61b589c465540611df22f9a1fe2d01237c5b2b4c13b",
    ),
    (
        "dng-adobe-canon-5d3-lj92.dng",
        "https://raw.pixls.us/getfile.php/1024/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20Lossless%20JPEG%20compression%20%283:2%29.DNG",
        "3118116735d4f6dd01fd9d6a80ee9aac52a2c2debdc6dcd112bd5e3059044a55",
    ),
    (
        "dng-adobe-canon-5d3-lossy.dng",
        "https://raw.pixls.us/getfile.php/1023/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossy%20JPEG%20compression%20%283:2%29.DNG",
        "159326856c29073e845c3c5a9ecf98c6474f43ca15798a88ad5e2baecd0664b7",
    ),
    // Apple ProRAW (iPhone 12 Pro, iOS 14.3): LinearRaw LJ92 tiles, ProfileToneCurve, ProfileGainTableMap, a sky matte.
    (
        "dng-apple-iphone12pro-proraw.dng",
        "https://raw.pixls.us/data/Apple/iPhone%2012%20Pro/IMG_1361.DNG",
        "e91e77a4533ed7cce551d83330676ea5c47dd5e55fb38adda7819366afdbdfc2",
    ),
    (
        "dng-canon-5d3-14bit-small.dng",
        "https://raw.pixls.us/getfile.php/2204/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2014bit%2014bit%20%282.3471882640587%29.dng",
        "1d77dcc6cdb839b10c1948f0b4471984ab48cf15e973fb77520584cee72b603e",
    ),
    (
        "dng-canon-5d3-16bit-169.dng",
        "https://raw.pixls.us/getfile.php/2649/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%20%2816:9%29.dng",
        "bc29ac8a37c800346749b9a5a1adc2722357f7f0e49df05a7104454881604b92",
    ),
    (
        "dng-canon-5d3-16bit.dng",
        "https://raw.pixls.us/getfile.php/885/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20RAW.dng",
        "6275226bfa2d86ba479130999d045ea0b8da6be4b011130c9eb91cd6a0eb2233",
    ),
    (
        "dng-google-pixel2xl.dng",
        "https://raw.pixls.us/getfile.php/2206/nice/Google%20-%20Pixel%202%20XL%20-%2016bit%20%284:3%29.dng",
        "5541093a369f0487fef92bcd1f5fef1be6e4e3cec2aa4d52ad38d2211f3309d7",
    ),
    (
        "dng-ricoh-gr3.dng",
        "https://raw.pixls.us/getfile.php/3115/nice/Ricoh%20-%20GR%20III%20-%2014bit%20%283:2%29.DNG",
        "05513ee72f7cac4165534c9f64995412084f414a033c80c473c79fd94154292e",
    ),
    (
        "nef-nikon-d5100-lossless.nef",
        "https://raw.pixls.us/getfile.php/1597/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.nef",
        "3ce328830942381648be26b372a54b317fdb27b63c1aa9c2ee1fce076a2e1b4f",
    ),
    (
        "nef-nikon-d5100-uncompressed.nef",
        "https://raw.pixls.us/getfile.php/1598/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20uncompressed%20%283:2%29.nef",
        "2fb2f1b87a3b89de5ba9e56527f21a912ca624f2fa2a594cef108ee8d7180412",
    ),
    (
        "nef-nikon-d7000-lossy12.nef",
        "https://raw.pixls.us/getfile.php/961/nice/Nikon%20-%20D7000%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "986db2bf9ed08cf095bb94cfbaf02f0e89d123edad28b63c05133e7815749486",
    ),
    (
        "nef-nikon-d7500-lossless12.nef",
        "https://raw.pixls.us/getfile.php/1532/nice/Nikon%20-%20D7500%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "1f4e58a961d3d317fa39a7e1c8948b825651e968c8cd06e49e4239f612faccad",
    ),
    (
        "nef-nikon-d7500-lossless14.nef",
        "https://raw.pixls.us/getfile.php/1534/nice/Nikon%20-%20D7500%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "430b4f1be4e53a011861b63294ad19fdc0353ec3ea62104f77d4c193e3dd3fc8",
    ),
    (
        "nef-nikon-zf-he.nef",
        "https://raw.pixls.us/download/data/Nikon/Z%20f/DSC_0043.NEF",
        "98d6ca8e6c98048ca7ffed68ccaeda7b2b9f03807f0d320d97d5678db21748c2",
    ),
    (
        "nef-nikon-zf-lossless.nef",
        "https://raw.pixls.us/download/data/Nikon/Z%20f/DSC_0040.NEF",
        "83c82be0be8865d796096dfbcc8ef2abf5af1bd37db44dfad6715070b0c99d15",
    ),
    (
        // Z 6II, NIKKOR Z 24-200 at 70 mm, Vivid picture control (not used to fit the bundled Z 6II profile)
        "nef-nikon-z6ii-lossless.nef",
        "https://raw.pixls.us/getfile.php/4160/nice/Nikon%20-%20Z%206_2%20-%2014bit%20compressed%20(Lossless)%20(3:2).NEF",
        "1b67d15817dbd4143a1f1089e2f09e319a2a2327c124e2643284509dd7621030",
    ),
    (
        // Z 8, NIKKOR Z 14-24 f/2.8 S (not used to fit the bundled Z 8 profile)
        "nef-nikon-z8-lossless.nef",
        "https://raw.pixls.us/data/Nikon/Z%208/Nikon_Z8_raw_14_bit_lossless_compression.NEF",
        "c13a8675294456b9c979e6c1e556951654f35377b2870af2874bfb556f21ce40",
    ),
    (
        "nrw-nikon-b700-uncompressed.nrw",
        "https://raw.pixls.us/getfile.php/1621/nice/Nikon%20-%20COOLPIX%20B700%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
        "a175d5892304e79282add07eacf4bdc498ca3f050af48cf989c080355b4735ec",
    ),
    (
        "orf-olympus-e1.orf",
        "https://raw.pixls.us/getfile.php/1800/nice/Olympus%20-%20E-1%20-%2016bit%20%284:3%29.ORF",
        "042286653fbae5b085bef4a4e626145385ea6e82e817f166b4904abcef64457c",
    ),
    (
        "orf-olympus-e400.orf",
        "https://raw.pixls.us/getfile.php/2151/nice/Olympus%20-%20E-400%20-%2016bit%20%284:3%29.ORF",
        "1354e0ac98ea90820227650031d8215637a2aaabd15de995c5a1e00b1986261b",
    ),
    (
        "orf-olympus-em1.orf",
        "https://raw.pixls.us/getfile.php/1051/nice/Olympus%20-%20E-M1%20-%2016bit%20%284:3%29.orf",
        "19ba17e778ae802d4039f0e88bb1cd68825780788b8058c479067e32b9a63eda",
    ),
    (
        "orf-olympus-em10iii.orf",
        "https://raw.pixls.us/getfile.php/1787/nice/Olympus%20-%20E-M10%20Mark%20III%20-%2016bit%20%284:3%29.ORF",
        "11d90cbb564ad2c660c7a8d5d81d99c2c018aa27fdec45ed6c461fa5d24f942b",
    ),
    (
        "orf-olympus-xz2.orf",
        "https://raw.pixls.us/getfile.php/1432/nice/Olympus%20-%20XZ-2%20-%2012bit%20%284:3%29.orf",
        "675fefe7c9e99281783b987889d723b5f46692f5c20e19fed5e422c7c6d2636b",
    ),
    (
        "pef-pentax-k10d.pef",
        "https://raw.pixls.us/getfile.php/2239/nice/Pentax%20-%20K10D%20-%2012bit%2012bit%20compressed%20%283:2%29.PEF",
        "e35ae4154a468be3154f5f462e884ba5941f010d3e8f23d347fbec14809f44d3",
    ),
    (
        "pef-pentax-k3.pef",
        "https://raw.pixls.us/getfile.php/1075/nice/Pentax%20-%20K-3%20-%2014bit%20%283:2%29.PEF",
        "abfb3907b53734b5f353e8de0c6173470c7aad07f9cecc10f723d222a49dec9a",
    ),
    (
        "pef-pentax-k5iis.pef",
        "https://raw.pixls.us/getfile.php/1198/nice/Pentax%20-%20K-5%20II%20s%20-%2014bit%20%283:2%29.PEF",
        "e579e7360c35e512f9c3a5c0eb9519575b87b489c6f86be82590f7706a7ed17b",
    ),
    // Samsung SRW, one file per packing: 12-bit MSB-first (NX10), 12-bit LSB-first (NX20), 16-bit words (EX1).
    (
        "srw-samsung-nx10.srw",
        "https://raw.pixls.us/getfile.php/5913/nice/Samsung%20-%20NX10%20-%2012bit%20%283:2%29.SRW",
        "dead7b4ad83b739ff7b8a1218882589f66cb14efc09f79eeba7a1dcbf7abd29f",
    ),
    (
        "srw-samsung-nx20.srw",
        "https://raw.pixls.us/getfile.php/5370/nice/Samsung%20-%20NX20%20-%2012bit%20%283:2%29.SRW",
        "8a367db506b8f7a6661a6f33a730467cfb3bfd7e46020a9f0df702330a7b45bb",
    ),
    (
        "srw-samsung-ex1.srw",
        "https://raw.pixls.us/getfile.php/1204/nice/Samsung%20-%20EX1%20-%2014bit%20%284:3%29.SRW",
        "a084890fd9d7995ceb746c18ecf5be48c95baf14286d5c0c480cc3daa25f3e37",
    ),
    (
        "raf-fuji-xa2-12bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2883/nice/Fujifilm%20-%20X-A2%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "58ad687b23138c3c49ebeb6c1f80daf13185349be364b947e2bf27e39a462d2f",
    ),
    (
        "raf-fuji-xa5-14bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2526/nice/Fujifilm%20-%20X-A5%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "46daaf8c2b07f40bd01f737181d048a497bb0a87615a1fde65809fdd5de86bf1",
    ),
    (
        "raf-fuji-xe1-12bit.raf",
        "https://raw.pixls.us/getfile.php/3098/nice/Fujifilm%20-%20X-E1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "0fa01fa1a674bcfa40f659225b4e66c5bedf15d2f78407ca1966dc8f38cd0e9f",
    ),
    (
        "raf-fuji-xt20-14bit.raf",
        "https://raw.pixls.us/getfile.php/1177/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "afe552f4a2794c7aa2376dd826165354b243dcf6f2296b01505dded8c8e4be76",
    ),
    (
        "raf-fuji-xt20-compressed.raf",
        "https://raw.pixls.us/getfile.php/1178/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "a23045101b2912e64f154100d646c228c0d16585d21e95e0d1a006e537f7784c",
    ),
    (
        "raw-panasonic-fz50.raw",
        "https://raw.pixls.us/getfile.php/2234/nice/Panasonic%20-%20DMC-FZ50%20-%204:3.RAW",
        "a776a941758214bf4d1342e2b89ed756bfcad5877e038cf9df3bdda502c432ea",
    ),
    (
        "raw-panasonic-fz8.raw",
        "https://raw.pixls.us/getfile.php/2282/nice/Panasonic%20-%20DMC-FZ8%20-%204:3.RAW",
        "a11f1534f3092a4d5f2b57d182f6b06f8d26cf766eb463c3e02b26a338b63b8b",
    ),
    (
        "rw2-panasonic-fz1000m2-4x3.rw2",
        "https://raw.pixls.us/getfile.php/4706/nice/Panasonic%20-%20DC-FZ10002%20-%204:3.RW2",
        "ab30922774bd829a992471d89eeefe1e829565a886d1477958130a2fa4fb77a1",
    ),
    // Fujifilm predictive compression: older X-Trans, 40 MP X-Trans, 14/16-bit GFX; both compression modes.
    (
        "raf-fuji-gfx100-3773.raf",
        "https://raw.pixls.us/getfile.php/3773/nice/Fujifilm%20-%20GFX%20100%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
        "018781e126842596fcf3876b3fb6cd8973dc3936ccc753ee8648618be8889287",
    ),
    (
        "raf-fuji-gfx100-3775.raf",
        "https://raw.pixls.us/getfile.php/3775/nice/Fujifilm%20-%20GFX%20100%20-%2014bit%2014bit%20compressed%20%284:3%29.RAF",
        "0e7dad9819a748bb3d3be26bbf5770d4968e2d80b9cb41d97036bf7c27f7bfe7",
    ),
    (
        "raf-fuji-gfx100rf-8091.raf",
        "https://raw.pixls.us/getfile.php/8091/nice/Fujifilm%20-%20GFX100RF%20-%2016bit%20compressed%20%284:3%29.RAF",
        "14bed5ac8191d6f2a7c82984e3bf6470ad4740df329293eac42b8018cc6a668a",
    ),
    (
        "raf-fuji-gfx100s-4495.raf",
        "https://raw.pixls.us/getfile.php/4495/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20lossless%20compressed%20%284:3%29.RAF",
        "a7bf8f061b603f7ef5fd29276eb6b2ac57355ca2f589a340117bfff0d03c065c",
    ),
    (
        "raf-fuji-gfx100s-4503.raf",
        "https://raw.pixls.us/getfile.php/4503/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
        "1bb4608b61861a3d9e85b0ca6c00689b8ba4620cbe8579374d958c21ed08edf0",
    ),
    (
        "raf-fuji-gfx50s-1435.raf",
        "https://raw.pixls.us/getfile.php/1435/nice/Fujifilm%20-%20GFX%2050S%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "a7124d13910e0241a6b035154bc1fcdb7be4439371d043b1e1950a0251feb51e",
    ),
    (
        "raf-fuji-xe5-8509.raf",
        "https://raw.pixls.us/getfile.php/8509/nice/Fujifilm%20-%20X-E5%20-%2014bit%20lossy%20compressed%20%283:2%29.RAF",
        "28f56cc9784164209e92fae209bdcc78267bd692aa2fb964595485f7e93692ab",
    ),
    (
        "raf-fuji-xh2-6001.raf",
        "https://raw.pixls.us/getfile.php/6001/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "caeadb2666f6c5d9bbe32fbe692c2079b6a7e028f8514a13173f360656d08507",
    ),
    (
        "raf-fuji-xh2-6002.raf",
        "https://raw.pixls.us/getfile.php/6002/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "109be38f59963c9b8ff289ebe1f5ec9168c52f609e5b601432bd194520b4605b",
    ),
    (
        "raf-fuji-xm5-7748.raf",
        "https://raw.pixls.us/getfile.php/7748/nice/Fujifilm%20-%20X-M5%20-%2014bit%20compressed%20%283:2%29.RAF",
        "c76ce546e15727abb4ba201471010c554403176963a1a6b8cc017462606429e5",
    ),
    (
        "raf-fuji-xt2-865.raf",
        "https://raw.pixls.us/getfile.php/865/nice/Fujifilm%20-%20X-T2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "f756ad5be551a8b3e1b503a8a6f7e8e637fba519c0b0f8c79dd58e94b0287602",
    ),
    (
        "raf-fuji-xt4-3914.raf",
        "https://raw.pixls.us/getfile.php/3914/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20lossless%20compressed%20%283:2%29.RAF",
        "07ea642656074834ffe5c661870a612ffa2e80fc1218a05166ee966bbe5f6fae",
    ),
    (
        "raf-fuji-xt4-3918.raf",
        "https://raw.pixls.us/getfile.php/3918/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "871836938ffa695f4c41765ddb107c2703ce755f7f3ced7e10f9e04746f1734e",
    ),
    (
        "raf-fuji-xt5-6122.raf",
        "https://raw.pixls.us/getfile.php/6122/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "b2a067e7fda16338f15c2532caa12b238b2415dc1c75a96a6d24d3445cfd0ae3",
    ),
    (
        "raf-fuji-xt5-6123.raf",
        "https://raw.pixls.us/getfile.php/6123/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "cddd7ae0c43f9280876e5fdcbdf8878718972ebd3d034d8affc8cfc6752d33dc",
    ),
    (
        "raf-fuji-xt50-7807.raf",
        "https://raw.pixls.us/getfile.php/7807/nice/Fujifilm%20-%20X-T50%20-%2014bit%20compressed%20%283:2%29.RAF",
        "3a8a9ac6d92274969aaa1f55ea669191b98bb8f588d00ec8ec17afd8b3963dea",
    ),
    // Sony ILCE-7RM6 ARW6, CC0 1.0 (RawDB): one full-frame Compressed (RAW-C) and one APS-C Compressed HQ (RAW-C-HD) file;
    // docs/arw6-compression.md lists the rest of the set.
    (
        "arw6-sony-ilce7rm6-apsc-land-hq.arw",
        "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_land_RAW-C-HD.ARW",
        "b60a7e752d2a8102ccbe6eebcfdbf533c6b0f2545739090873e9e8eb32cc0637",
    ),
    (
        "arw6-sony-ilce7rm6-ff-land-c.arw",
        "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_land_RAW-C.ARW",
        "d900d6cbe838be864269ad54de4a33fcac7eb3a3d4f91009739f770b8f85a23b",
    ),
    (
        "rw2-panasonic-g9-b.rw2",
        "https://raw.pixls.us/getfile.php/2348/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2",
        "16282357ce0143ff7d36737f82074662db7aa0790ecd309196f406baf0f586a7",
    ),
    (
        "rw2-panasonic-g9.rw2",
        "https://raw.pixls.us/getfile.php/2585/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2",
        "7a56babeb19f9bde3eaa532a4c8d1b11637a0f550963a1182ec845722c65583d",
    ),
    (
        "rw2-panasonic-gh1.rw2",
        "https://raw.pixls.us/getfile.php/1323/nice/Panasonic%20-%20DMC-GH1%20-%204:3.RW2",
        "2b846f932c665f196af9af97b18469730331b60b2c62811969be7db245ce6cce",
    ),
    (
        "rw2-panasonic-gh5.rw2",
        "https://raw.pixls.us/getfile.php/1517/nice/Panasonic%20-%20DC-GH5%20-%204:3.RW2",
        "dae2281df393d8c3c1b8c513d00985f1834ae0e69ce18fbd571370c25d95d22b",
    ),
    (
        "rw2-panasonic-gh5m2.rw2",
        "https://raw.pixls.us/getfile.php/5082/nice/Panasonic%20-%20DC-GH5M2%20-%204:3.RW2",
        "0ea0efd2a9442d8c68508e4196e29f1da4577407dc3df5add4cf360f70493c7b",
    ),
    (
        "rw2-panasonic-gh5s.rw2",
        "https://raw.pixls.us/getfile.php/2603/nice/Panasonic%20-%20DC-GH5S%20-%204:3.RW2",
        "b617151876e62b492641cef14b8c98ee3797e5014d24c6af1c780715a501fa53",
    ),
    (
        "rw2-panasonic-gh6.rw2",
        "https://raw.pixls.us/getfile.php/5876/nice/Panasonic%20-%20DC-GH6%20-%204:3.RW2",
        "07996bca904a178d02c5f56ee03a8dce775a0e1150c1beeaec51d72ff5e700d0",
    ),
    (
        "rw2-panasonic-gx80.rw2",
        "https://raw.pixls.us/getfile.php/1569/nice/Panasonic%20-%20DMC-GX80%20-%204:3.RW2",
        "6e9a419c70c2124630912b2934027c635d3871e3d22dd91fed0655d58cfb5382",
    ),
    (
        "rw2-panasonic-s1.rw2",
        "https://raw.pixls.us/getfile.php/3038/nice/Panasonic%20-%20DC-S1%20-%203:2.RW2",
        "45309a096d6010281e2fa310e6ec2fe1bcc02947fa37b1a2414cfc7377ce4ac6",
    ),
    (
        "rw2-panasonic-s5-format7.rw2",
        "https://raw.pixls.us/getfile.php/6339/nice/Panasonic%20-%20DC-S5%20-%203:2.RW2",
        "3615c85207097180b231f0c89d018eb3f1a71988780612921eb0a05246bca706",
    ),
    (
        "rw2-panasonic-s5m2.rw2",
        "https://raw.pixls.us/getfile.php/7790/nice/Panasonic%20-%20DC-S5M2%20-%2014bit%20%283:2%29.RW2",
        "53a171521a7f9af6910b4ab61c5a4a42de0cceb24166e15b07eee1e00675028f",
    ),
    (
        "rw2-panasonic-s9.rw2",
        "https://raw.pixls.us/getfile.php/7702/nice/Panasonic%20-%20DC-S9%20-%203:2.RW2",
        "290ad5b4e396cb241a59459b2613d2b0659a03ff654e5d5368b3376d35812e50",
    ),
    (
        "rwl-leica-dlux7.rwl",
        "https://raw.pixls.us/getfile.php/4204/nice/Leica%20-%20D-Lux%207%20-%204:3.RWL",
        "4f6db6492da83f7f2a5012591e370557249d1ed44d4c948784016605b799327c",
    ),
    (
        "mrw-minolta-dimage7hi-words16.mrw",
        "https://raw.pixls.us/data/Minolta/DiMAGE%207Hi/RAW_MINOLTA_DIMAGE_7HI.MRW",
        "2919a05757197a5424e02970c016893bca5aa29363a67b62b1a4b36326a10f6a",
    ),
    (
        "mrw-minolta-dimage-a1.mrw",
        "https://raw.pixls.us/data/Minolta/DiMAGE%20A1/RAW_MINOLTA_A1.MRW",
        "bde1e2cd788e49c9bef20dfdd41be948fb02c2ee198c6b0959a67a3f79fd41c9",
    ),
    (
        "mrw-minolta-dimage-a200-gbrg.mrw",
        "https://raw.pixls.us/data/Minolta/DiMAGE%20A200/RAW_MINOLTA_DIMAGE_A200.MRW",
        "2a95f6af2168e536dfb6d02b47e3b6119b35b497eebf179d7c20a6e496c26641",
    ),
    (
        "mrw-minolta-dynax7d.mrw",
        "https://raw.pixls.us/data/Minolta/Dynax%207D/PICT3410.MRW",
        "713819e33e5302ec864d576d57223cfc8dcf46e8d45e0ae744d1fec40290a89f",
    ),
    (
        "nef-nikon-d700.nef",
        "https://raw.pixls.us/getfile.php/2249/nice/Nikon%20-%20D700%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "00c3314f2ccf92aff17644943e6149e8793b517c24bb2c38a15ec41f3af109cb",
    ),
    (
        "raf-fujifilm-xf10.raf",
        "https://raw.pixls.us/getfile.php/3124/nice/Fujifilm%20-%20XF10%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "0486676fdbbdc7f3f4c63cd45aa76581cbc1316f4c9baef8781bced0fbc137db",
    ),
    (
        "raf-fujifilm-x-a10.raf",
        "https://raw.pixls.us/getfile.php/3570/nice/Fujifilm%20-%20X-A10%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "05309c5434500cc53d5782072cc0dcdd947d2f0a34f0ddcd4656c82cc7803272",
    ),
    (
        "nef-nikon-d5300.nef",
        "https://raw.pixls.us/getfile.php/8365/nice/Nikon%20-%20D5300%20-%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "0ae88fc81f082929d76dd1b932aacf7815cd6bc670b96ef55a81928e11084a6c",
    ),
    (
        "raf-fujifilm-x-t100.raf",
        "https://raw.pixls.us/getfile.php/2676/nice/Fujifilm%20-%20X-T100%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "0af086b9569b59317da42b8e136013245486073b6b8d409fc997a71788538c4d",
    ),
    (
        "raf-fujifilm-x-pro1.raf",
        "https://raw.pixls.us/getfile.php/1191/nice/Fujifilm%20-%20X-Pro1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "0b1046c299d9eb222a8f003bd047f2a34f507da0d1704ac32cefbc82f799d121",
    ),
    (
        "nef-nikon-1-v1.nef",
        "https://raw.pixls.us/getfile.php/2210/nice/Nikon%20-%201%20V1%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "0dc76bc274f9cb776eb930683a4e53a3ebede1719abafd1cba4b0c800f13f7e4",
    ),
    (
        "nef-nikon-1-j4.nef",
        "https://raw.pixls.us/getfile.php/5922/nice/Nikon%20-%201%20J4%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "0e2887892b8cfc80891b2c510721088fcac6d77eff9ba68089f382a0115b274c",
    ),
    (
        "raf-fujifilm-gfx100s-ii.raf",
        "https://raw.pixls.us/getfile.php/7772/nice/Fujifilm%20-%20GFX100S%20II%20-%2016bit%20uncompressed%20%284:3%29.RAF",
        "10e007f3d1aef2ac9bf4da063ea1983e44515af818bf0ab63e3703cd67803810",
    ),
    (
        "nef-nikon-d610.nef",
        "https://raw.pixls.us/getfile.php/888/nice/Nikon%20-%20D610%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "11f0a71bb4141a8ed44c7e1c64f9a12571f5f67a11571f2bde8516aa95a7bea3",
    ),
    (
        "nef-nikon-d5.nef",
        "https://raw.pixls.us/getfile.php/1636/nice/Nikon%20-%20D5%20-%2014bit%2014bit%20uncompressed%20%283:2%29.NEF",
        "13191560a6a9d9ed4156ec5d20863277c9618916aaba934758c5ae2be6654f6a",
    ),
    (
        "nef-nikon-d2h.nef",
        "https://raw.pixls.us/getfile.php/5227/nice/Nikon%20-%20D2H%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "155edb938f884ea7372ce98d4ff5f965c3e413b43b95bc9923da6e92082cf914",
    ),
    (
        "nef-nikon-z-6-2.nef",
        "https://raw.pixls.us/getfile.php/4160/nice/Nikon%20-%20Z%206_2%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "1b67d15817dbd4143a1f1089e2f09e319a2a2327c124e2643284509dd7621030",
    ),
    (
        "nrw-nikon-coolpix-p1000.nrw",
        "https://raw.pixls.us/getfile.php/3381/nice/Nikon%20-%20COOLPIX%20P1000%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
        "1d9470ee083f914e51aa0be782161b432f27f87dbef551a59ec435f5dbc289dd",
    ),
    (
        "raf-fujifilm-finepix-x100.raf",
        "https://raw.pixls.us/getfile.php/2392/nice/Fujifilm%20-%20FinePix%20X100%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "1f4aea4fa43dd314f8b90954d7823655d4906d3e468a9ea014652b872a68db16",
    ),
    (
        "raf-fujifilm-x30.raf",
        "https://raw.pixls.us/getfile.php/1144/nice/Fujifilm%20-%20X30%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "1f65f680600c85a6f561d2b0ad7cd1f9d93ee7f733aafe4ecd37667d984dd6c4",
    ),
    (
        "raf-fujifilm-finepix-x10.raf",
        "https://raw.pixls.us/getfile.php/1344/nice/Fujifilm%20-%20FinePix%20X10%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "206448688c49b0a9da74dd94f12aba143c2d8842cb5bbf64d4ecdc501874451b",
    ),
    (
        "nef-nikon-d2xs.nef",
        "https://raw.pixls.us/getfile.php/2871/nice/Nikon%20-%20D2Xs%20-%2012bit%2012bit%20uncompressed%20%283:2%29.NEF",
        "2204bc2e2178bf594e17886ca92a50a94350403a110846ca6e83bdc5f3352706",
    ),
    (
        "nef-nikon-d4.nef",
        "https://raw.pixls.us/getfile.php/1974/nice/Nikon%20-%20D4%20-%2014bit%2014bit%20uncompressed%20%283:2%29.NEF",
        "23aa2dd3c409b3cbbfeea3e3f7b6752180efa15430d6e059e87404e969875cd7",
    ),
    (
        "nef-nikon-d800.nef",
        "https://raw.pixls.us/getfile.php/750/nice/Nikon%20-%20D800%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "26d58c21ed3019af5a22ef6bc02f017b863aad9eafcf84252ed375cd495c0efb",
    ),
    (
        "nef-nikon-d780.nef",
        "https://raw.pixls.us/getfile.php/3825/nice/Nikon%20-%20D780%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "3178d0656e3b5fbe90176e38a1cd608d0b69292299445932dddc9a0b2d196b36",
    ),
    (
        "nef-nikon-d850.nef",
        "https://raw.pixls.us/getfile.php/1835/nice/Nikon%20-%20D850%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "31b7fe298a5c91694c6ca8b3284a0a016e572a02c3964b502d4732e8e9283da8",
    ),
    (
        "raf-fujifilm-x-e2s.raf",
        "https://raw.pixls.us/getfile.php/745/nice/Fujifilm%20-%20X-E2S%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "330fc84b597e7be6d7477aad5715705efaa99d4663eb05aa47e1250d6f421549",
    ),
    (
        "nef-nikon-d500.nef",
        "https://raw.pixls.us/getfile.php/2504/nice/Nikon%20-%20D500%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "33c96a2ba7167f780928767c0545719737d601883e5f98e5edba4f71bdc99d83",
    ),
    (
        "raf-fujifilm-gfx-50r.raf",
        "https://raw.pixls.us/getfile.php/2979/nice/Fujifilm%20-%20GFX%2050R%20-%2014bit%2014bit%20compressed%20%283:2%29.raf",
        "35109fc20a849026c43c47674902431ef5a2d54b09b616995263dc75df17912f",
    ),
    (
        "nef-nikon-df.nef",
        "https://raw.pixls.us/getfile.php/1385/nice/Nikon%20-%20Df%20-%2012bit%2012bit%20uncompressed%20%283:2%29.NEF",
        "38ea7aba6b7e0cdbd338fe6703d3c98291f4a9a28b0d1a81bfb1f77b21712c63",
    ),
    (
        "raf-fujifilm-x-s10.raf",
        "https://raw.pixls.us/getfile.php/4188/nice/Fujifilm%20-%20X-S10%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "3d080f9ce8ac65c4beab1e9c0f49f7c72cdec71f31fe9e27dd1d88e551a1c262",
    ),
    (
        "nef-nikon-1-v2.nef",
        "https://raw.pixls.us/getfile.php/1693/nice/Nikon%20-%201%20V2%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "3d29dfa6397b0aa4e72969bb5b85d040e8bebf2e02f61f6364f580ce39f30bf2",
    ),
    (
        "nef-nikon-1-v3.nef",
        "https://raw.pixls.us/getfile.php/2802/nice/Nikon%20-%201%20V3%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "3f5376481f9ae59e9712b537dcc4eaffa72e311a8768c5396414506ae2eb1d69",
    ),
    (
        "nef-nikon-d3s.nef",
        "https://raw.pixls.us/getfile.php/5203/nice/Nikon%20-%20D3S%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "402898e57007a4176b8686019ef2878c28481f7520c6136127f00148de9960b5",
    ),
    (
        "nef-nikon-z-fc.nef",
        "https://raw.pixls.us/getfile.php/4812/nice/Nikon%20-%20Z%20fc%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.nef",
        "4099ecc46ebaca26e2a66a96a8313fed5e8c707b3836ccf0e77a134844b041c1",
    ),
    (
        "nef-nikon-d810.nef",
        "https://raw.pixls.us/getfile.php/2029/nice/Nikon%20-%20D810%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "40a9df99865ad25a68b1131014cf954fb72da8bc32991b6a5e19d14937e21bc9",
    ),
    (
        "raf-fujifilm-x-a3.raf",
        "https://raw.pixls.us/getfile.php/1371/nice/Fujifilm%20-%20X-A3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "4187cb5caa72320a74e9b4355db01e8e21edd43f3084873f9b9b67169d8d4ad8",
    ),
    (
        "nef-nikon-d40.nef",
        "https://raw.pixls.us/getfile.php/3618/nice/Nikon%20-%20D40%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "44e88bc77b7a531b22647bcd07b9393c4568062e8f0906d3bbdecb42fbe29e03",
    ),
    (
        "nef-nikon-d800e.nef",
        "https://raw.pixls.us/getfile.php/1303/nice/Nikon%20-%20D800E%20-%2014bit%2014bit%20uncompressed%20%283:2%29.NEF",
        "4559b9614ab6d558192d9d30e1db4a4ec44752553e45b2bd26c1569c1820441a",
    ),
    (
        "nef-nikon-d3x.nef",
        "https://raw.pixls.us/getfile.php/3708/nice/Nikon%20-%20D3X%20-%2014bit%2014bit%20uncompressed%20%283:2%29.NEF",
        "48dc0339ef86541aabcc6bb68a6452f603e23a7d7a5597dad0d267f69e3e79d4",
    ),
    (
        "raf-fujifilm-x100v.raf",
        "https://raw.pixls.us/getfile.php/3812/nice/Fujifilm%20-%20X100V%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "48f2055ab497c687347eee84103d4f48deb016fff711d2810eaf1317e506f621",
    ),
    (
        "nef-nikon-d4s.nef",
        "https://raw.pixls.us/getfile.php/2220/nice/Nikon%20-%20D4S%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "49067c81374549465f3c621acd249bf05dbdef4e44b9201fef1fb11c93572983",
    ),
    (
        "raf-fujifilm-x-t30-iii.raf",
        "https://raw.pixls.us/getfile.php/8769/nice/Fujifilm%20-%20X-T30%20III%20-%2014bit%20compressed%20%283:2%29.RAF",
        "49f77d6162abfa5c94d2d8b90e4e926b7386c42bcf7e84a152c9ffe1ebd584da",
    ),
    (
        "raf-fujifilm-xq2.raf",
        "https://raw.pixls.us/getfile.php/1844/nice/Fujifilm%20-%20XQ2%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "4bce88593f5ae8fc45aeeec8a6dee9a84c773493cc6db9dabdc28744c8ac4b4d",
    ),
    (
        "raf-fujifilm-x-t10.raf",
        "https://raw.pixls.us/getfile.php/1192/nice/Fujifilm%20-%20X-T10%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "4c09401975cc6f95b1284693aac39fef85498f70b55e35bf458a133c6e61885d",
    ),
    (
        "nef-nikon-d2x.nef",
        "https://raw.pixls.us/getfile.php/9064/nice/Nikon%20-%20D2X%20-%2012bit%20uncompressed%20%283:2%29.NEF",
        "4d4b89f85bdfef233ef9b8aa7ecfeb609874ab592cb44e777f5b36814df5883c",
    ),
    (
        "raf-fujifilm-x70.raf",
        "https://raw.pixls.us/getfile.php/1301/nice/Fujifilm%20-%20X70%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "4f9c33fde578addebda991c37d26551b46fab8f592c74f7192560a05ec08d577",
    ),
    (
        "dng-nikon-d80.dng",
        "https://raw.pixls.us/getfile.php/1988/nice/Nikon%20-%20D80%20-%2016bit%20%283:2%29.dng",
        "5095ca264e763624cd3aa62940286dd1bcff4504eefa1ce61cbb72f50f8426bb",
    ),
    (
        "nef-nikon-1-aw1.nef",
        "https://raw.pixls.us/getfile.php/1171/nice/Nikon%20-%201%20AW1%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "519d354ec40907f1cd30329fc1a641ddec5b46e08e88883f3cf2fddc4ddd9974",
    ),
    (
        "raf-fujifilm-x-h1.raf",
        "https://raw.pixls.us/getfile.php/2537/nice/Fujifilm%20-%20X-H1%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "5210bf896d2757cecda88bab0a87ef0b534d4b36c31dc08d5db7cd8f5bbf9b4e",
    ),
    (
        "nef-nikon-1-j5.nef",
        "https://raw.pixls.us/getfile.php/1342/nice/Nikon%20-%201%20J5%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.nef",
        "56efc4f426fa489383a967102c6d30fa9d6b198ca0484c4725c91f0b0d47c063",
    ),
    (
        "nef-nikon-1-j1.nef",
        "https://raw.pixls.us/getfile.php/1017/nice/Nikon%20-%201%20J1%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "5965935729931b72ca8398041f8fc15cb0a226004db941d1f0626ccc2c5bb5f3",
    ),
    (
        "nef-nikon-d7200.nef",
        "https://raw.pixls.us/getfile.php/832/nice/Nikon%20-%20D7200%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "5dea8ff6f95f372e4eb0852f75e728f182d7aa4057b2b76631f0fd3066c6a4b5",
    ),
    (
        "raf-fujifilm-x-t30-ii.raf",
        "https://raw.pixls.us/getfile.php/5064/nice/Fujifilm%20-%20X-T30%20II%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "5e2678038377cc7f146ceb7693c08535fa1af162cec705d06b0446c3d3017d47",
    ),
    (
        "nef-nikon-z-50.nef",
        "https://raw.pixls.us/getfile.php/3647/nice/Nikon%20-%20Z%2050%20-%2014bit%2014bit%20compressed%20%283:2%29.NEF",
        "5e93735923229117abee2779d81f114f180f361e8ae553c9dd9bdaaedb0d9e02",
    ),
    (
        "nef-nikon-d5000.nef",
        "https://raw.pixls.us/getfile.php/1341/nice/Nikon%20-%20D5000%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "5f8e8005be87366bbf7746d35c3c88f82a12a1dd18206c939d8f1254c5922360",
    ),
    (
        "raf-fujifilm-xq1.raf",
        "https://raw.pixls.us/getfile.php/1741/nice/Fujifilm%20-%20XQ1%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "663c0bfcb8819c3931444acd9d1491de4d0c0aa1fd745b99e1faa68cc8de033b",
    ),
    (
        "raf-fujifilm-finepix-hs30exr.raf",
        "https://raw.pixls.us/getfile.php/2613/nice/Fujifilm%20-%20FinePix%20HS30EXR%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "6b95bb5133d026dbbb7361985e81d9a2b93000e2aea88bcd3f120412d0061da7",
    ),
    (
        "nef-nikon-d70s.nef",
        "https://raw.pixls.us/getfile.php/1117/nice/Nikon%20-%20D70s%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "6c46f9854033e7623ebbb5de8c951df6762ca44f05d45035ccc5cb7e9f4e5692",
    ),
    (
        "nef-nikon-d5200.nef",
        "https://raw.pixls.us/getfile.php/1691/nice/Nikon%20-%20D5200%20-%2014bit%2014bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "6c49ca47d189f9d6766f94be7c9d7d779ce45c79c0c6e48a0eba7d92f320fe0e",
    ),
    (
        "nrw-nikon-coolpix-p1100.nrw",
        "https://raw.pixls.us/getfile.php/7689/nice/Nikon%20-%20Coolpix%20P1100%20-%2012bit%20uncompressed%20%284:3%29.NRW",
        "706840f7a36c3852092d4aad0465e75920e04f1aa653adf31626c0c05ab41258",
    ),
    (
        "raf-fujifilm-x-s20.raf",
        "https://raw.pixls.us/getfile.php/6666/nice/Fujifilm%20-%20X-S20%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "737c1551843282d4963ccb351a4d6409f3eaf172928d563bbaf72fe0f1f74fdd",
    ),
    (
        "nef-nikon-z-30.nef",
        "https://raw.pixls.us/getfile.php/5812/nice/Nikon%20-%20Z%2030%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "75249c11a3a845367c7a322a1c4e7076f33c785ec2518fc6594c34c185b41161",
    ),
    (
        "raf-fujifilm-x-e2.raf",
        "https://raw.pixls.us/getfile.php/937/nice/Fujifilm%20-%20X-E2%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "7588c70af7fd6be3abcec52546aabcd04e0aafc6541e7f7f50bdc60b60513c57",
    ),
    (
        "nef-nikon-d60.nef",
        "https://raw.pixls.us/getfile.php/3698/nice/Nikon%20-%20D60%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "780ce4867e997143acbd1734a889575a5eaeeb40c6afb33b6006c266831caaa6",
    ),
    (
        "nef-nikon-1-j2.nef",
        "https://raw.pixls.us/getfile.php/923/nice/Nikon%20-%201%20J2%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "81051ee36135883acea55bd1e609d6d31212c0bf03640b7ae9fe177a1ca51219",
    ),
    (
        "nef-nikon-d5600.nef",
        "https://raw.pixls.us/getfile.php/1415/nice/Nikon%20-%20D5600%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "82e15f02c4037820e80f14bacfacdc52ea29721b3c70d2cf08b237b50ce8db05",
    ),
    (
        "nef-nikon-d3.nef",
        "https://raw.pixls.us/getfile.php/3516/nice/Nikon%20-%20D3%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "880c60c5f611adf6b70de3d099f8433492de3a3d96866d570122f17f0a651fc8",
    ),
    (
        "nef-nikon-d300.nef",
        "https://raw.pixls.us/getfile.php/924/nice/Nikon%20-%20D300%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "8b989f53c96995f7014ddd16124530cf6d8f3341ea9570d6312d9da1eb0a117a",
    ),
    (
        "raf-fujifilm-finepix-sl1000.raf",
        "https://raw.pixls.us/getfile.php/5760/nice/Fujifilm%20-%20FinePix%20SL1000%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "8bc9903cb1bff93376eb73b64f35861dcf3c66e2ca936cfac2d7c62435689dec",
    ),
    (
        "raf-fujifilm-x-pro3.raf",
        "https://raw.pixls.us/getfile.php/3700/nice/Fujifilm%20-%20X-Pro3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "8c50e965d7627932414e8790a5ef1d673fcb44b548b64130df6b9be42c418554",
    ),
    (
        "raf-fujifilm-x-pro2.raf",
        "https://raw.pixls.us/getfile.php/1680/nice/Fujifilm%20-%20X-Pro2%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "8cc12566e5c0fc676b713bb1612ce0510d47b1dc691e9a5a0bcf3799ad9b8af1",
    ),
    (
        "nef-nikon-1-s1.nef",
        "https://raw.pixls.us/getfile.php/5661/nice/Nikon%20-%201%20S1%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "8e540d40b842f61de69161cb0b8be72b024629c752fd82b6f8d10e460bce88cb",
    ),
    (
        "raf-fujifilm-x100vi.raf",
        "https://raw.pixls.us/getfile.php/7300/nice/Fujifilm%20-%20X100VI%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "8e5e4d8d2e0a865ca79b3d40734b6abc2f4b00281afea8b9bd3d8c29808f2fb3",
    ),
    (
        "raf-fujifilm-x-t200.raf",
        "https://raw.pixls.us/getfile.php/3899/nice/Fujifilm%20-%20X-T200%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "8fc8dfa5b4741246287c430010d9699fe6f1e512ac03e14abea72e63cb022051",
    ),
    (
        "nef-nikon-d3200.nef",
        "https://raw.pixls.us/getfile.php/743/nice/Nikon%20-%20D3200%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "91657dd8e9c7a086826683f6d20799685657dd7ca2056702b145851465f57cd3",
    ),
    (
        "raf-fujifilm-x-t3.raf",
        "https://raw.pixls.us/getfile.php/2783/nice/Fujifilm%20-%20X-T3%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "95b33021160b239ceb1a09d46a1b29cb60cb7dd47581feb126b337c219091754",
    ),
    (
        "raf-fujifilm-finepix-f550exr.raf",
        "https://raw.pixls.us/getfile.php/3869/nice/Fujifilm%20-%20FinePix%20F550EXR%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "95b6c8a9d5d885f60bbc965ddfd899d325f2225c99ab087d02739543a3ff7dd6",
    ),
    (
        "nef-nikon-d5500.nef",
        "https://raw.pixls.us/getfile.php/1469/nice/Nikon%20-%20D5500%20-%2014bit%2014bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "96d5b85dae24e190691c504781d8e7ee58cb9b63ea715760de9af3d345dc4c13",
    ),
    (
        "raf-fujifilm-x-a7.raf",
        "https://raw.pixls.us/getfile.php/3879/nice/Fujifilm%20-%20X-A7%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "97db251dd129663da00d33e8eb4d7932d222f58d99ff7311c09cc277de5b6f18",
    ),
    (
        "nef-nikon-d3300.nef",
        "https://raw.pixls.us/getfile.php/1734/nice/Nikon%20-%20D3300%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "97dfc95aecc00383d6da6197cb759bacb1e43c86071b4ca2d94748e498620f2f",
    ),
    (
        "nef-nikon-z-5.nef",
        "https://raw.pixls.us/getfile.php/4136/nice/Nikon%20-%20Z%205%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "9870248c45532080c2459f3b52cdcc4a07bc8846ad50cbb81c7c1c6dc5e36d91",
    ),
    (
        "nef-nikon-1-j3.nef",
        "https://raw.pixls.us/getfile.php/1814/nice/Nikon%20-%201%20J3%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "996d65705bf1ac9f83f055817ea92618ea44c822109196d7d97a803a0268b576",
    ),
    (
        "nef-nikon-z-9.nef",
        "https://raw.pixls.us/getfile.php/5146/nice/Nikon%20-%20Z%209%20-%208bit%208bit%20compressed%20%283:2%29.NEF",
        "99d0fe24a38f0f7623aeab21440d266a5ac09f9665cf5283f590337e84323c38",
    ),
    (
        "nrw-nikon-coolpix-a1000.nrw",
        "https://raw.pixls.us/getfile.php/4337/nice/Nikon%20-%20Coolpix%20A1000%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
        "9b4edd03bbb17d66eacc2a72b30f044969e67666446f3f1ed25e88bb947d4f8a",
    ),
    (
        "raf-fujifilm-x100s.raf",
        "https://raw.pixls.us/getfile.php/2115/nice/Fujifilm%20-%20X100S%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "9cea5b0d4dd3682f8850dc34e93f0045e0e28fa69f9715576d40c5a8a59e8ae9",
    ),
    (
        "nef-nikon-d2hs.nef",
        "https://raw.pixls.us/getfile.php/2852/nice/Nikon%20-%20D2Hs%20-%2012bit%2012bit%20uncompressed%20%283:2%29.NEF",
        "9ea1c3d5d6c8a11f33239079559f1f0c86bbdb9440a2c7d2578368647abf7ea4",
    ),
    (
        "nef-nikon-d90.nef",
        "https://raw.pixls.us/getfile.php/1325/nice/Nikon%20-%20D90%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "9eb3daa4e3d1c1614bccc4c6d136c4166960f936f17c437cc4d11a5786b2df06",
    ),
    (
        "nef-nikon-d50.nef",
        "https://raw.pixls.us/getfile.php/1282/nice/Nikon%20-%20D50%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "9f94c27672a63f946c92d43910ef0c266dffaa14dae0007ae00ac72d2c684935",
    ),
    (
        "nef-nikon-d3000.nef",
        "https://raw.pixls.us/getfile.php/2161/nice/Nikon%20-%20D3000%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "a07ee8a0b1ff7a1ad06085306ef8f9a197d95fec639994467317c56108eeb435",
    ),
    (
        "raf-fujifilm-x100t.raf",
        "https://raw.pixls.us/getfile.php/2137/nice/Fujifilm%20-%20X100T%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "a12bb7fa3585b636b7fa487d767ba0ba1ae10fe6aaa74db4147dc0ec2b180a0a",
    ),
    (
        "nef-nikon-z50-2.nef",
        "https://raw.pixls.us/getfile.php/7762/nice/Nikon%20-%20Z50_2%20-%208bit%20compressed%20%283:2%29.NEF",
        "a3314a026524396e603b461afc4fe61a8a5a3958232f3687bb1c8eca0dcbed93",
    ),
    (
        "nef-nikon-d600.nef",
        "https://raw.pixls.us/getfile.php/916/nice/Nikon%20-%20D600%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "a70c3e54501efa7e4391ec4cf5705912a10c356a029e984ee13be5f68bc7482d",
    ),
    (
        "raf-fujifilm-xf1.raf",
        "https://raw.pixls.us/getfile.php/3209/nice/Fujifilm%20-%20XF1%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "b17c389a59228881af8d50c22bdd8ce2295a54290d9a18447dd1e56d0743e913",
    ),
    (
        "nef-nikon-d7100.nef",
        "https://raw.pixls.us/getfile.php/1855/nice/Nikon%20-%20D7100%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "b241cb95b1008f25b8ed7d1443354e8cc7dee9c4d4e1edd0a5856d92063a48d0",
    ),
    (
        "raf-fujifilm-x-e3.raf",
        "https://raw.pixls.us/getfile.php/1864/nice/Fujifilm%20-%20X-E3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "b2aeb7fd72a9ea116b1dcc2c8a8cf5462b15757ba3aca33bad8ce36339a1642f",
    ),
    (
        "raf-fujifilm-x-a1.raf",
        "https://raw.pixls.us/getfile.php/1214/nice/Fujifilm%20-%20X-A1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "b2ddb5fe31a3b976d12961c72a2689de8742739df3d8f757614eca65f8df2a61",
    ),
    (
        "nef-nikon-d3500.nef",
        "https://raw.pixls.us/getfile.php/2902/nice/Nikon%20-%20D3500%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "b53395f74674588a63f6b667325c5a2b400f26cba72ff402a65ddd02c5f841d5",
    ),
    (
        "raf-fujifilm-finepix-hs50exr.raf",
        "https://raw.pixls.us/getfile.php/6018/nice/Fujifilm%20-%20FinePix%20HS50EXR%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "b5c4f3658dc686e535eeaf639729f2cb6b54e60a3e1ab206eabba1024c4dce83",
    ),
    (
        "raf-fujifilm-gfx100-ii.raf",
        "https://raw.pixls.us/getfile.php/7174/nice/Fujifilm%20-%20GFX100%20II%20-%2014bit%2014bit%20uncompressed%20%284:3%29.RAF",
        "b7b329bcbfcc07c5523a2b172a5173f4de4b841c8315078a1b69150ddaa16f30",
    ),
    (
        "raf-fujifilm-x100f.raf",
        "https://raw.pixls.us/getfile.php/1936/nice/Fujifilm%20-%20X100F%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
        "bca8ad027b0ee0038107c01792ccb5c6b776512b77b584c6f93c6f2ced5f15a3",
    ),
    (
        "nef-nikon-d40x.nef",
        "https://raw.pixls.us/getfile.php/804/nice/Nikon%20-%20D40X%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "c3e4956f7e2d1f1b3f846b6ec9552678073db0bcaeb51132ec4c60bd2229d56e",
    ),
    (
        "nef-nikon-coolpix-a.nef",
        "https://raw.pixls.us/getfile.php/2238/nice/Nikon%20-%20Coolpix%20A%20-%2014bit%2014bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "c5ca0333a7cda8a2fc33fb2157d18dfa89f1844548dc49c2fd2e67f502f72e25",
    ),
    (
        "raf-fujifilm-x20.raf",
        "https://raw.pixls.us/getfile.php/1549/nice/Fujifilm%20-%20X20%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "ccb4fad836f1561b52859c3ee01db0fef7db770980d4a84e95595da1eda72b2a",
    ),
    (
        "raf-fujifilm-x-e4.raf",
        "https://raw.pixls.us/getfile.php/4446/nice/Fujifilm%20-%20X-E4%20-%2014bit%2014bit%20compressed%20lossless%20%283:2%29.RAF",
        "ce4215079e4c6c8a86d493e15f6e574e4aa7684b346ef3439026c835ed2c8948",
    ),
    (
        "raf-fujifilm-gfx50s-ii.raf",
        "https://raw.pixls.us/getfile.php/5002/nice/Fujifilm%20-%20GFX50S%20II%20-%2014bit%2014bit%20uncompressed%20%284:3%29.RAF",
        "cf53a9018d5a77cd854ea0a91afee2a35ac8fab8f814020d79f728f99ba99892",
    ),
    (
        "nef-nikon-d1h.nef",
        "https://raw.pixls.us/getfile.php/5364/nice/Nikon%20-%20D1H%20-%2012bit%2012bit%20uncompressed%20%283:2%29.NEF",
        "d110b80a8b10a3a67b5cd1c15ef33f1a6397557b73d5d3f4e565739f0c072571",
    ),
    (
        "nef-nikon-d200.nef",
        "https://raw.pixls.us/getfile.php/824/nice/Nikon%20-%20D200%20-%2012bit%2012bit%20uncompressed%20%283:2%29.nef",
        "d468895b0b30a60d05a7f6468d69f2f15830dd49b2d82698b34245072a22e747",
    ),
    (
        "raf-fujifilm-finepix-hs33exr.raf",
        "https://raw.pixls.us/getfile.php/2907/nice/Fujifilm%20-%20FinePix%20HS33EXR%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "d72d5b67d67379fa310fc5a4615016d9456d086a8bfd32c74a3b256e0dfecee8",
    ),
    (
        "raf-fujifilm-finepix-s1.raf",
        "https://raw.pixls.us/getfile.php/1651/nice/Fujifilm%20-%20FinePix%20S1%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "d77f104a9fa4ce11c78b5078ac40acf8b150d67ea837d187e0903d208bf2dbae",
    ),
    (
        "nef-nikon-z-7-2.nef",
        "https://raw.pixls.us/getfile.php/4213/nice/Nikon%20-%20Z%207_2%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "dc83d3e49db54ca9c3a1e41dc847afd213c822d482c40158e42d01ee8968f33b",
    ),
    (
        "nef-nikon-d70.nef",
        "https://raw.pixls.us/getfile.php/2060/nice/Nikon%20-%20D70%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%201%29%29%20%283:2%29.NEF",
        "dd6405aeb33b0cd5bf66c98ba98ccbb478a765450cfd130810e470dab8d1f4b4",
    ),
    (
        "nef-nikon-d300s.nef",
        "https://raw.pixls.us/getfile.php/3049/nice/Nikon%20-%20D300S%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
        "de950a2263c87f085ee611a21e69fc4e0ecb6e417fff61ff6a23ea3121e21a81",
    ),
    (
        "raf-fujifilm-x-s1.raf",
        "https://raw.pixls.us/getfile.php/997/nice/Fujifilm%20-%20X-S1%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "e4652f3710cc972712a25370988ad8d72c8bf368956215348ea77b17d95f36f7",
    ),
    (
        "nef-nikon-d750.nef",
        "https://raw.pixls.us/getfile.php/895/nice/Nikon%20-%20D750%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "e4a3f85e014275fc0a47600a373af95ec850872a5f6cf87649bcaa42b77ef2e4",
    ),
    (
        "nef-nikon-d3100.nef",
        "https://raw.pixls.us/getfile.php/4229/nice/Nikon%20-%20D3100%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "e8113b6e7285155f9f2b88f81566dab63a59cf5559d6915113b74e8c3a8680f6",
    ),
    (
        "raf-fujifilm-x-t1.raf",
        "https://raw.pixls.us/getfile.php/2421/nice/Fujifilm%20-%20X-T1%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "e994a1fd6e87e392432fe146a35b0b88584dc2bd50bee2c8c7e886ac2b59fcde",
    ),
    (
        "nef-nikon-d3400.nef",
        "https://raw.pixls.us/getfile.php/1561/nice/Nikon%20-%20D3400%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
        "eced190786231f0081261b6c6b425fe23e42a53ade21dec3def5ae8551be71a5",
    ),
    (
        "raf-fujifilm-x-m1.raf",
        "https://raw.pixls.us/getfile.php/842/nice/Fujifilm%20-%20X-M1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
        "eef4e9ae629d2d81cef5a234fb853c14c88b3c0c12c39d635a1f9fa2a871bec3",
    ),
    (
        "raf-fujifilm-x-t30.raf",
        "https://raw.pixls.us/getfile.php/3068/nice/Fujifilm%20-%20X-T30%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
        "f0353544bc62a6816385de0850e1d37e2f3383892a7eb3928c73e4517fe47ff5",
    ),
    (
        "nrw-nikon-coolpix-p950.nrw",
        "https://raw.pixls.us/getfile.php/3806/nice/Nikon%20-%20COOLPIX%20P950%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
        "fc6136b9e5a7e133d7dc30fe65baa04c09ff8c18ca3c7ad82b4ffdbb71ce5bef",
    ),
    (
        "raf-fujifilm-finepix-f770exr.raf",
        "https://raw.pixls.us/getfile.php/3039/nice/Fujifilm%20-%20FinePix%20F770EXR%20-%2012bit%2012bit%20uncompressed%20%284:3%29.RAF",
        "ffc905e325a00c57a5335681654fdf226b724269cd5d26cd976983939f88043d",
    ),
    (
        "dng-leica-m-monochrom.dng",
        "https://raw.pixls.us/getfile.php/974/nice/Leica%20-%20M%20Monochrom%20-%2016bit%20%283:2%29.DNG",
        "149d5b3fbd5cd40d51c1f56d5e392a81870d129e047a6c8a596447590e7a0e4f",
    ),
    (
        "dng-leica-cl.dng",
        "https://raw.pixls.us/getfile.php/2489/nice/Leica%20-%20CL%20-%2014bit%2014bit%20uncompressed%20%283:2%29.DNG",
        "1f4cfa565d176d85838d96bd3eb598bbeed719cac8acb9999d5823c527635950",
    ),
    (
        "dng-leica-m10.dng",
        "https://raw.pixls.us/getfile.php/1603/nice/Leica%20-%20M10%20-%2016bit%2016bit%20uncompressed%20%283:2%29.dng",
        "26be5e06dfb53a2938dab3ca8f06024533a9c0dd0c652938f39eb48bc325f95e",
    ),
    (
        "rwl-leica-c-lux.rwl",
        "https://raw.pixls.us/getfile.php/3863/nice/Leica%20-%20C-Lux%20-%204:3.RWL",
        "286106ec2df65539912709c64e84a9b94686443fe5ea05b8ebada76daa1feb2d",
    ),
    (
        "arw-sony-zv-1m2.arw",
        "https://raw.pixls.us/getfile.php/8461/nice/Sony%20-%20ZV-1M2%20-%2012bit%20%283:2%29.ARW",
        "2a45845a93ab5e428868366d6575a264818b184541b1c114ab4de980872a7184",
    ),
    (
        "dng-leica-m-monochrom-typ-246.dng",
        "https://raw.pixls.us/getfile.php/1087/nice/Leica%20-%20M%20Monochrom%20%28Typ%20246%29%20-%2012bit%20%283:2%29.DNG",
        "3fb74d1fc402daaa48a9c3f1d6279d7e85fb58dfdaf816f036927d49ab228977",
    ),
    (
        "arw-sony-zv-e10m2.arw",
        "https://raw.pixls.us/getfile.php/8021/nice/Sony%20-%20ZV-E10M2%20-%2012bit%20%284:3%29.ARW",
        "49db06da969cf3894cf070d9171fe0ed4d0f125e16a87b5eba7eaec27cd75595",
    ),
    (
        "arw-sony-dsc-rx1rm3.arw",
        "https://raw.pixls.us/getfile.php/8544/nice/Sony%20-%20DSC-RX1RM3%20-%2014bit%20uncompressed%20%283:2%29.ARW",
        "6ef9956b343051748a6a16ba9ba7bd0d314a4ae0b4f8d8e2f1c13b4a1e50cfb8",
    ),
    (
        "arw-sony-ilce-1m2.arw",
        "https://raw.pixls.us/getfile.php/7827/nice/Sony%20-%20ILCE-1M2%20-%2014bit%20uncompressed%20%283:2%29.ARW",
        "7def3c8d80537f79af199052c6e13d86750203122dcb94888ada65291dabb793",
    ),
    (
        "dng-leica-m8.dng",
        "https://raw.pixls.us/getfile.php/2988/nice/Leica%20-%20M8%20-%208bit%208bit%20uncompressed%20%283:2%29.DNG",
        "7fdcca2371bb07fbf9483fcb08e3b5ab093eca1ca3924cbc86d6863736c31153",
    ),
    (
        "arw-sony-dsc-rx100m7a.arw",
        "https://raw.pixls.us/getfile.php/8096/nice/Sony%20-%20DSC-RX100M7A%20-%2012bit%20%283:2%29.ARW",
        "80adbc5b1cb3342058d5998cf2eda1e6b408565169462d6d26dfe02f0c140b30",
    ),
    (
        "arw-sony-ilce-7m5.arw",
        "https://raw.pixls.us/getfile.php/8845/nice/Sony%20-%20ILCE-7M5%20-%2014bit%20%284:3%29.ARW",
        "8c35cd1ab0ca36b487ae50ae5e0333a8e967122700fce5678e1e5b97561896a4",
    ),
    (
        "arw-sony-ilme-fx2.arw",
        "https://raw.pixls.us/getfile.php/8807/nice/Sony%20-%20ILME-FX2%20-%2014bit%20uncompressed%20%283:2%29.ARW",
        "924a6e4efd5c068883dd7de2bf2a2d155c7c13bdde668232077bdf9d92a201a8",
    ),
    (
        "dng-leica-x2.dng",
        "https://raw.pixls.us/getfile.php/882/nice/Leica%20-%20X2%20-%2012bit%20%283:2%29.DNG",
        "a67eca6271afb10510ffc9cf998b641d0e8a0ebfa4c779a7bc0bfed5d28b62dd",
    ),
    (
        "dng-leica-q2.dng",
        "https://raw.pixls.us/getfile.php/3204/nice/Leica%20-%20Q2%20-%2014bit%2014bit%20uncompressed%20%283:2%29.DNG",
        "a71f98405e9e9c1e4ac0f2d2e6c52d65daae3234fc0a9f6eed2a84ee4ce794ad",
    ),
    (
        "arw-sony-ilce-6400a.arw",
        "https://raw.pixls.us/getfile.php/7826/nice/Sony%20-%20ILCE-6400A%20-%2014bit%20%283:2%29.ARW",
        "a8a45e9b6c437eba6724fcda24c3efd084c05389711b955d84b2af67635e241b",
    ),
    (
        "dng-leica-sl-typ-601.dng",
        "https://raw.pixls.us/getfile.php/1951/nice/Leica%20-%20SL%20%28Typ%20601%29%20-%2014bit%20%283:2%29.DNG",
        "aa9fd9ef937ed34467d6f91bfa2603be00cb47542b7fffd7258b79652d3b7bef",
    ),
    (
        "dng-leica-m10-r.dng",
        "https://raw.pixls.us/getfile.php/7853/nice/Leica%20-%20M10-R%20-%2016bit%20compressed%20%283:2%29.DNG",
        "ba35d25d521c7ddbc4feca69c8cdbe071ac65dcd6cf2c93374391bf6f3662b8d",
    ),
    (
        "dng-leica-m-typ-240.dng",
        "https://raw.pixls.us/getfile.php/2317/nice/Leica%20-%20M%20%28Typ%20240%29%20-%2016bit%2016bit%20compressed%20%283:2%29.DNG",
        "bf35a4da121f853cb644253b69515b441748f95fcbd6fdea2de025a76e6c24cd",
    ),
    (
        "dng-leica-sl2.dng",
        "https://raw.pixls.us/getfile.php/7872/nice/Leica%20-%20SL2%20-%2014bit%20uncompressed%20%283:2%29.DNG",
        "d00f05468793e4509b53230e90dcdc078e823b254eee95f1c68b29e217df99bf",
    ),
    (
        "dng-leica-m9.dng",
        "https://raw.pixls.us/getfile.php/753/nice/Leica%20-%20M9%20-%208bit%208bit%20compressed%20%283:2%29.DNG",
        "d1d162fce62210b8951c862189a6098329b5163b7f7492dc397c6e53bba04239",
    ),
    (
        "dng-leica-q-typ-116.dng",
        "https://raw.pixls.us/getfile.php/1797/nice/Leica%20-%20Q%20%28Typ%20116%29%20-%2014bit%20%283:2%29.DNG",
        "f7b2a6b6d7684536511cf2db2e2bfa46fae1ad7e52214093714f35e7ca60acbf",
    ),
    (
        "srw-samsung-nx200.srw",
        "https://raw.pixls.us/getfile.php/839/nice/Samsung%20-%20NX200%20-%2012bit%20%283:2%29.srw",
        "03563eadfe4ef32b5bf2a2850abb40b29fcc1f8d56b3b2ed92ef307b90d7d9dc",
    ),
    (
        "orf-olympus-sp550uz.orf",
        "https://raw.pixls.us/getfile.php/8609/nice/Olympus%20-%20SP550UZ%20-%2012bit%20%284:3%29.ORF",
        "05f49918e1b1894c3abd2b5451b356f7a00adb1c8568abc5babff33fcade1bb2",
    ),
    (
        "orf-olympus-e-10.orf",
        "https://raw.pixls.us/getfile.php/5424/nice/Olympus%20-%20E-10%20-%2016bit%20%284:3%29.ORF",
        "2bfdade72439017a60a47aad1e5bbcb1aca36f2be7a21e94a4257a678fb6f4da",
    ),
    (
        "orf-olympus-c5060wz.orf",
        "https://raw.pixls.us/getfile.php/1799/nice/Olympus%20-%20C5060WZ%20-%2012bit%20%284:3%29.ORF",
        "3fc12bc3f6320cfab456111d7805b446e329fdd1907680be55e2e163e5e084bd",
    ),
    (
        "dng-samsung-gx10.dng",
        "https://raw.pixls.us/getfile.php/1165/nice/Samsung%20-%20GX10%20-%2012bit%20%283:2%29.DNG",
        "44cbb7980b320c0920d999758eb8a3f27e6b98dd14afc3dfc135bc8526abda58",
    ),
    (
        "orf-olympus-sp565uz.orf",
        "https://raw.pixls.us/getfile.php/3438/nice/Olympus%20-%20SP565UZ%20-%2012bit%20%284:3%29.ORF",
        "5088771f83ce08a001b44672e1dd9f013e603933c37ce1bfb7030a02abbe801c",
    ),
    (
        "dng-samsung-gx20.dng",
        "https://raw.pixls.us/getfile.php/1537/nice/Samsung%20-%20GX20%20-%2012bit%20%283:2%29.DNG",
        "5557016b2712b621befdb05731923562a802beafd13e0f366737390a8f7b3286",
    ),
    (
        "orf-olympus-e-m5-mark-ii.orf",
        "https://raw.pixls.us/getfile.php/2856/nice/Olympus%20-%20E-M5%20Mark%20II%20-%2016bit%2016bit%20%22high-res%22%20%284:3%29.ORF",
        "5c42fa75d6b549514b722e2c50726c03e34fac4909ec3640e147ea7fa825fc8d",
    ),
    (
        "orf-olympus-e-300.orf",
        "https://raw.pixls.us/getfile.php/2878/nice/Olympus%20-%20E-300%20-%2016bit%20%284:3%29.ORF",
        "6d818dbff78b3e2efa4835f568d04733052b9211e6bc65b0a3748a841170e95b",
    ),
    (
        "orf-olympus-sh-2.orf",
        "https://raw.pixls.us/getfile.php/792/nice/Olympus%20-%20SH-2%20-%2012bit%20%284:3%29.ORF",
        "6e6b9b7dd9ded7c4b364b83116c817ca2d6daac15f2e9ef7796159b08c3288a1",
    ),
    (
        "srw-samsung-nx5.srw",
        "https://raw.pixls.us/getfile.php/5684/nice/Samsung%20-%20NX5%20-%2012bit%20%283:2%29.SRW",
        "908fc61d5bbc1f0d0a5cea4f4e39e7753060c1af8673f7a4a9199284b70930f7",
    ),
    (
        "orf-olympus-c7070wz.orf",
        "https://raw.pixls.us/getfile.php/780/nice/Olympus%20-%20C7070WZ%20-%2012bit%20%284:3%29.ORF",
        "9d55506f9d6ee31d2d4c4b6df0dcd1d016d358b3e5eb4ed31edd7227dfefd5f8",
    ),
    (
        "orf-olympus-xz-10.orf",
        "https://raw.pixls.us/getfile.php/2431/nice/Olympus%20-%20XZ-10%20-%2012bit%20%284:3%29.ORF",
        "a25383acad9b5bb0bb404715e0957f2eb80aa1aaad6c448a71025c03cfa631be",
    ),
    (
        "srw-samsung-wb2000.srw",
        "https://raw.pixls.us/getfile.php/4262/nice/Samsung%20-%20WB2000%20-%2014bit%20%284:3%29.SRW",
        "b9a1149fb0dda9d23aee0ad8c402bf2f9ba42832366d46c4c1b739c0b0f48497",
    ),
    (
        "orf-olympus-e-330.orf",
        "https://raw.pixls.us/getfile.php/3624/nice/Olympus%20-%20E-330%20-%2016bit%20%284:3%29.ORF",
        "ba11eb970ab32e1e9cf3d0176041e4bd11c501d445720c0a344217e785ad65a6",
    ),
    (
        "orf-olympus-sp570uz.orf",
        "https://raw.pixls.us/getfile.php/5491/nice/Olympus%20-%20SP570UZ%20-%2012bit%20%284:3%29.ORF",
        "baab92391834076b427e406e98da79aaba48fb87243d6ccae60e08e299cfccf7",
    ),
    (
        "srw-samsung-nx11.srw",
        "https://raw.pixls.us/getfile.php/5985/nice/Samsung%20-%20NX11%20-%2012bit%20%283:2%29.SRW",
        "bdce6179b4180478057f4971b512e0303da71528db454cb3f399ab10e05b68fe",
    ),
    (
        "srw-samsung-nx1000.srw",
        "https://raw.pixls.us/getfile.php/2588/nice/Samsung%20-%20NX1000%20-%2012bit%20%283:2%29.SRW",
        "beef4d1e6f25ff28fb20bb3ad00ffadb62ab57d7041237e295b0b62f7de5309c",
    ),
    (
        "srw-samsung-nx1100.srw",
        "https://raw.pixls.us/getfile.php/4422/nice/Samsung%20-%20NX1100%20-%2012bit%20%283:2%29.srw",
        "d230af65ee55314792c5337ea8e0e757626a8541e7a25ed03d63b8df202ed9cb",
    ),
    (
        "srw-samsung-nx210.srw",
        "https://raw.pixls.us/getfile.php/1831/nice/Samsung%20-%20NX210%20-%2012bit%20%283:2%29.SRW",
        "d613503ddf9bedd2d2e84708749b1a94f0cccd4fa15492e01242368c5c07cfad",
    ),
    (
        "orf-olympus-sp510uz.orf",
        "https://raw.pixls.us/getfile.php/2420/nice/Olympus%20-%20SP510UZ%20-%2012bit%20%284:3%29.ORF",
        "f0737d78bb2e588963a3d5038fab493cdff1c9c48475704de96d797bbb81e62a",
    ),
    (
        "orf-olympus-pen-f.orf",
        "https://raw.pixls.us/getfile.php/2977/nice/Olympus%20-%20PEN-F%20-%2016bit%20%284:3%29.orf",
        "f87a5f3295aeb4d6ab27bb5b41c5e0f3a211ec9b4e3b1d9f105a650bee1a2f13",
    ),
    (
        "orf-olympus-e-500.orf",
        "https://raw.pixls.us/getfile.php/3540/nice/Olympus%20-%20E-500%20-%2016bit%20%284:3%29.ORF",
        "fa5a4759b2de2b573f59629ad1baf64ad04a85fb8fe1fb3cbf68e08a42fff4f0",
    ),
    (
        "cr2-canon-eos-70d.cr2",
        "https://raw.pixls.us/getfile.php/784/nice/Canon%20-%20EOS%2070D%20-%20RAW.CR2",
        "00e42517013bd043b7145001399bbc320a8ff235a6dcd16033580d8a130bee1e",
    ),
    (
        "cr2-canon-eos-rebel-t7i.cr2",
        "https://raw.pixls.us/getfile.php/3437/nice/Canon%20-%20EOS%20Rebel%20T7i%20-%20RAW%20%283:2%29.cr2",
        "03fb55af71f384ada6b6aa0ca93279d48cf5fbd13651a323408d2fd97e1d1f7c",
    ),
    (
        "cr2-canon-powershot-g9-x.cr2",
        "https://raw.pixls.us/getfile.php/2154/nice/Canon%20-%20PowerShot%20G9%20X%20-%20RAW%20%283:2%29.CR2",
        "05c041127f4fdecfd49a45da4b12192c8dbe695c3e50427043166fef49b39e83",
    ),
    (
        "pef-pentax-k-1.pef",
        "https://raw.pixls.us/getfile.php/2469/nice/Pentax%20-%20K-1%20-%2014bit%20%283:2%29.PEF",
        "068c902e7b38ac31c7c6eca5c55f69ca395ec6aa7170489281b692a893d0fee9",
    ),
    (
        "cr2-canon-eos-4000d.cr2",
        "https://raw.pixls.us/getfile.php/3241/nice/Canon%20-%20EOS%204000D%20-%20RAW%20%283:2%29.CR2",
        "06daaf663f42d3f6ceb1a837d3ca4db2cfcb5899888ed13510bfa01e3261a896",
    ),
    (
        "cr2-canon-eos-1000d.cr2",
        "https://raw.pixls.us/getfile.php/1616/nice/Canon%20-%20EOS%201000D%20-%20RAW%20%283:2%29.CR2",
        "08b0843a7968315b68bd3c71a96c952ebf275d5b5eba4b6094ae8b103522584c",
    ),
    (
        "cr2-canon-powershot-g1-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/2186/nice/Canon%20-%20PowerShot%20G1%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "08d861ca7fc2890f0217034970d4c900b09373b421106e4ba080f6904a48b37c",
    ),
    (
        "cr2-canon-eos-500d.cr2",
        "https://raw.pixls.us/getfile.php/911/nice/Canon%20-%20EOS%20500D%20-%20RAW%20%283:2%29.CR2",
        "0b57eb8b0dece6f63d4ea61660279488121c9a0aa250113852fa84c051c6da3e",
    ),
    (
        "pef-pentax-ist-dl.pef",
        "https://raw.pixls.us/getfile.php/1083/nice/Pentax%20-%20%2Aist%20DL%20-%2012bit%20%283:2%29.PEF",
        "0e6cd3c999a6dbf121591b2c58aaa8216e625f549db46af627f7346ccf1cf662",
    ),
    (
        "cr2-canon-eos-400d.cr2",
        "https://raw.pixls.us/getfile.php/1764/nice/Canon%20-%20EOS%20400D%20-%20RAW%20%283:2%29.CR2",
        "0e752ddaf81795885efc6443c78e5fee4d5f8f00c8d979ef20e235424f98d595",
    ),
    (
        "dng-ricoh-gx200.dng",
        "https://raw.pixls.us/getfile.php/2589/nice/Ricoh%20-%20GX200%20-%2012bit%20%284:3%29.DNG",
        "0f328905ee6f5b4823040a666103ced651178a13b8ccd0c4c60662f7265dd87f",
    ),
    (
        "cr2-canon-powershot-s110.cr2",
        "https://raw.pixls.us/getfile.php/909/nice/Canon%20-%20PowerShot%20S110%20-%20RAW%20%284:3%29.CR2",
        "14e440316c1d2b005a7f5d5c042e063b3e15d34242e38c3a0983db8ef85e4288",
    ),
    (
        "cr2-canon-eos-kiss-x80.cr2",
        "https://raw.pixls.us/getfile.php/3099/nice/Canon%20-%20EOS%20Kiss%20X80%20-%20RAW%20%283:2%29.CR2",
        "155878290528ff91f67b4f9ffed5d765e80ede5fb0c36fca40107c0b7f5c1d9d",
    ),
    (
        "dng-pentax-kp.dng",
        "https://raw.pixls.us/getfile.php/1824/nice/Pentax%20-%20KP%20-%2014bit%20%283:2%29.DNG",
        "16b09cb3032003989877659ae03cbb6985e3d7e84f1402863ccae8af10033c1c",
    ),
    (
        "dng-pentax-k-1-mark-ii.dng",
        "https://raw.pixls.us/getfile.php/3345/nice/Pentax%20-%20K-1%20Mark%20II%20-%2014bit%20%283:2%29.DNG",
        "190d5878675f4de07d246271ba411b152f3317b2d2a0e27b9c827fe7343b28ea",
    ),
    (
        "dng-pentax-k-3-mark-iii-monochrome.dng",
        "https://raw.pixls.us/getfile.php/7081/nice/Pentax%20-%20K-3%20Mark%20III%20Monochrome%20-%2014bit%20%283:2%29.DNG",
        "1afc1071451a3a3737c29ae319e1411551b6893d0e6a34db29903318266b0066",
    ),
    (
        "cr2-canon-eos-600d.cr2",
        "https://raw.pixls.us/getfile.php/1586/nice/Canon%20-%20EOS%20600D%20-%20RAW%20%283:2%29.CR2",
        "1b0892df5ba5fa418e945f0519d490aeecaaf7e74860a08b5dc47e3e9976a288",
    ),
    (
        "pef-pentax-k-70.pef",
        "https://raw.pixls.us/getfile.php/1141/nice/Pentax%20-%20K-70%20-%2014bit%20%283:2%29.PEF",
        "1d64c6aa71680642ba8f00b6f6105df3e66a4534e631f422fbf3316273c4408c",
    ),
    (
        "dng-ricoh-gr-iiix.dng",
        "https://raw.pixls.us/getfile.php/5818/nice/Ricoh%20-%20GR%20IIIx%20-%2014bit%20%283:2%29.DNG",
        "1e318af31be4baa9949f0ed4af5cfe8ffb2c180e2c12926cd75ac1d029bb0fae",
    ),
    (
        "cr2-canon-powershot-g1-x.cr2",
        "https://raw.pixls.us/getfile.php/1588/nice/Canon%20-%20PowerShot%20G1%20X%20-%20RAW%20%284:3%29.CR2",
        "1e486bbbdfb2c94accf0c5ef4558d1aa484e0ffd36fa03440e3ae34ba2aa338b",
    ),
    (
        "dng-ricoh-gxr.dng",
        "https://raw.pixls.us/getfile.php/2756/nice/Ricoh%20-%20GXR%20-%2012bit%20%284:3%29.DNG",
        "219a062f3d1eb8d66c9dd6088eecf495be082cf5c81f88622eb0dd28cc893cec",
    ),
    (
        "cr2-canon-powershot-g16.cr2",
        "https://raw.pixls.us/getfile.php/1114/nice/Canon%20-%20PowerShot%20G16%20-%20RAW%20%284:3%29.CR2",
        "2307e3cf643ab2f94d03f899467d8797ce1ef41bbffd6ad50af81bdd0ba34098",
    ),
    (
        "dng-pentax-k20d.dng",
        "https://raw.pixls.us/getfile.php/1143/nice/Pentax%20-%20K20D%20-%2012bit%20%283:2%29.DNG",
        "264465ef1d97f1599899778589f07f8aa485b88c0dfc9b1d4c006bf79d44b420",
    ),
    (
        "cr2-canon-powershot-g3-x.cr2",
        "https://raw.pixls.us/getfile.php/1509/nice/Canon%20-%20PowerShot%20G3%20X%20-%20RAW%20%283:2%29.CR2",
        "267641913613fd52532279ff16f03c135511fbcab5bbe8b276fc9cf91aebd7ca",
    ),
    (
        "pef-pentax-ist-ds.pef",
        "https://raw.pixls.us/getfile.php/4392/nice/Pentax%20-%20%2Aist%20DS%20-%2012bit%2012bit%20uncompressed%20%283:2%29.pef",
        "27809a7d252d515685ed4e6091b70ab25348502baeb75db03ce7b4f1d9549b76",
    ),
    (
        "cr2-canon-eos-m.cr2",
        "https://raw.pixls.us/getfile.php/1224/nice/Canon%20-%20EOS%20M%20-%20RAW%20%283:2%29.CR2",
        "27d151efe86f04bea8fcf6ec9f3a722ae3ee24a3f56a6ebbd27ee89b215cf569",
    ),
    (
        "cr2-canon-eos-1d-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1058/nice/Canon%20-%20EOS-1D%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "2e372710d546c9b8903a343696af4e7fd245794571f64258ab607a89dfa6aa97",
    ),
    (
        "pef-pentax-k-3-mark-iii.pef",
        "https://raw.pixls.us/getfile.php/4677/nice/Pentax%20-%20K-3%20Mark%20III%20-%2014bit%20%283:2%29.PEF",
        "351be88038b2dd28af39e11dbece6de74efd71119a9ef667d35cdf02dcff919f",
    ),
    (
        "cr2-canon-eos-750d.cr2",
        "https://raw.pixls.us/getfile.php/1982/nice/Canon%20-%20EOS%20750D%20-%20RAW%20%283:2%29.CR2",
        "38cdb173fe1ea083f4d475285d3080db49d7f3c7cdcb49bf1968fceeed66c85c",
    ),
    (
        "cr2-canon-eos-5ds.cr2",
        "https://raw.pixls.us/getfile.php/2083/nice/Canon%20-%20EOS%205DS%20-%20RAW%20%283:2%29.CR2",
        "3fdb8b7ada3b4a69ed5756fa814bee9bd51c1c22b40e70252a46890e2855df07",
    ),
    (
        "dng-canon-powershot-s2-is.dng",
        "https://raw.pixls.us/getfile.php/941/nice/Canon%20-%20PowerShot%20S2%20IS%20-%2010bit%2010bit%20DNG%2C%20CHDK%20ver.%201.0.0-1504%20%284:3%29.CR2",
        "445764716df165a1facd37c01d67ede44ca49671ed1a6112dd8ff7426324c757",
    ),
    (
        "cr2-canon-powershot-s100v.cr2",
        "https://raw.pixls.us/getfile.php/2492/nice/Canon%20-%20PowerShot%20S100V%20-%20RAW%20%284:3%29.CR2",
        "4775f347686f785104ee7f566df158247cdecef4de76fdbfa150e18b6b104c0a",
    ),
    (
        "cr2-canon-eos-kiss-digital-n.cr2",
        "https://raw.pixls.us/getfile.php/1185/nice/Canon%20-%20EOS%20Kiss%20Digital%20N%20-%20RAW%20%283:2%29.CR2",
        "49c78e5c36255cdd3f9fd219e86e3325bfa67be4d8d1ab5e21c21c3fd53c294e",
    ),
    (
        "dng-pentax-k-01.dng",
        "https://raw.pixls.us/getfile.php/1008/nice/Pentax%20-%20K-01%20-%2012bit%20%283:2%29.DNG",
        "4c9aac2ce2720154616ad7678385d1ec1f626d6464a1d5c6a5a4d821a78a608d",
    ),
    (
        "cr2-canon-eos-rebel-t5.cr2",
        "https://raw.pixls.us/getfile.php/1411/nice/Canon%20-%20EOS%20Rebel%20T5%20-%20RAW%20%283:2%29.CR2",
        "4f71c1416037d2212cc4847e978bffe874afc0f653260386e96dff704ca8bf51",
    ),
    (
        "pef-pentax-k110d.pef",
        "https://raw.pixls.us/getfile.php/1309/nice/Pentax%20-%20K110D%20-%2012bit%20%283:2%29.pef",
        "4f7c5574b0dbc3c1270b269126c2a3be030a86799f8b544610c74f31ef782f72",
    ),
    (
        "cr2-canon-powershot-g11.cr2",
        "https://raw.pixls.us/getfile.php/2755/nice/Canon%20-%20PowerShot%20G11%20-%20RAW%20%284:3%29.CR2",
        "50ef4d6712c7add1c65ef4c48c74ac5017f24ff1789f1d8a8d5d9accd3aab0fa",
    ),
    (
        "cr2-canon-eos-rebel-sl1.cr2",
        "https://raw.pixls.us/getfile.php/1201/nice/Canon%20-%20EOS%20Rebel%20SL1%20-%20RAW%20%283:2%29.CR2",
        "56faa7db3947c32c5e6e31bc0db027ebf95163533e043bd8c4f18166a129692c",
    ),
    (
        "cr2-canon-powershot-g15.cr2",
        "https://raw.pixls.us/getfile.php/1986/nice/Canon%20-%20PowerShot%20G15%20-%20RAW%20%284:3%29.CR2",
        "5756d1a6522134ee4833a9cbef04eb54b26a1f7935e58063733a264478385a26",
    ),
    (
        "dng-pentax-q7.dng",
        "https://raw.pixls.us/getfile.php/1760/nice/Pentax%20-%20Q7%20-%2012bit%20%284:3%29.DNG",
        "577eac8d03419bb82714169dd532d58efc82a0abf928d28e4641f1ed5d56c350",
    ),
    (
        "dng-pentax-k-s1.dng",
        "https://raw.pixls.us/getfile.php/1365/nice/Pentax%20-%20K-S1%20-%2012bit%20%283:2%29.DNG",
        "58abc43a8e025dcab0053bcee173d20197aa2b7983e5e62097147a1e8545f111",
    ),
    (
        "cr2-canon-eos-1d-mark-iv.cr2",
        "https://raw.pixls.us/getfile.php/4258/nice/Canon%20-%20EOS-1D%20Mark%20IV%20-%20RAW%20%283:2%29.CR2",
        "597bb01f2400b04bfe7ee024016992e8e897b7dffaa5e368b8375daf3b32b3ed",
    ),
    (
        "cr2-canon-eos-m5.cr2",
        "https://raw.pixls.us/getfile.php/2935/nice/Canon%20-%20EOS%20M5%20-%20RAW%20%283:2%29.CR2",
        "5af02dbad6cd90eeac2b0b1c0be0aeb9a5d2841c6dd984ee415ede9f2566667d",
    ),
    (
        "cr2-canon-powershot-s100.cr2",
        "https://raw.pixls.us/getfile.php/1357/nice/Canon%20-%20PowerShot%20S100%20-%20RAW%20%284:3%29.CR2",
        "5d1bb0597da59bc920442c55dbd7f70d090a9aa01b5642607bcdde523ae892c6",
    ),
    (
        "pef-pentax-ist-d.pef",
        "https://raw.pixls.us/getfile.php/1843/nice/Pentax%20-%20%2Aist%20D%20-%2012bit%20%283:2%29.PEF",
        "61904b594d16983070f3e56d663ae8f7b246b8bb554c4cf7bd3eb4b9f9a9f3aa",
    ),
    (
        "dng-pentax-k-30.dng",
        "https://raw.pixls.us/getfile.php/3094/nice/Pentax%20-%20K-30%20-%2012bit%20%283:2%29.DNG",
        "63eaa1ff93ad7c89947017332b10f78f17cd3853a5b495adf25410edaefbe618",
    ),
    (
        "cr2-canon-eos-rebel-t4i.cr2",
        "https://raw.pixls.us/getfile.php/2203/nice/Canon%20-%20EOS%20Rebel%20T4i%20-%20RAW%20%283:2%29.CR2",
        "65257f38bc68a11b9185ac710b3a442a19aa1c082afb01b668bf33e6836b6b2d",
    ),
    (
        "cr2-canon-powershot-sx50-hs.cr2",
        "https://raw.pixls.us/getfile.php/3152/nice/Canon%20-%20PowerShot%20SX50%20HS%20-%20RAW%20%284:3%29.CR2",
        "6c2ddd232f5432fb7bd0e28efc12d66995cedcd847f801fd854bfdfb180db50c",
    ),
    (
        "dng-pentax-k200d.dng",
        "https://raw.pixls.us/getfile.php/5246/nice/Pentax%20-%20K200D%20-%2012bit%2012bit%20uncompressed%20%283:2%29.DNG",
        "6d13579dd0a8ec784bfa3d8b53af5a3f936171bfa62d74eb96a17236a6fb9472",
    ),
    (
        "cr2-canon-eos-m2.cr2",
        "https://raw.pixls.us/getfile.php/5067/nice/Canon%20-%20EOS%20M2%20-%20RAW%20%283:2%29.cr2",
        "6d26eeb1e8a5978c7cb9d8beb6e6e34f3c27847055b76bc7f186503ca44508d0",
    ),
    (
        "cr2-canon-eos-m3.cr2",
        "https://raw.pixls.us/getfile.php/1002/nice/Canon%20-%20EOS%20M3%20-%20RAW%20%283:2%29.CR2",
        "6f12a02a21f64b284328ee6d3b2d1bab4439720cad2e881adf3da72592128b11",
    ),
    (
        "cr2-canon-powershot-g12.cr2",
        "https://raw.pixls.us/getfile.php/1767/nice/Canon%20-%20PowerShot%20G12%20-%20RAW%20%284:3%29.cr2",
        "6f2750c7cb144eee80a91431e2ce8df66dd229c3e1f1453bf80409ca669a8614",
    ),
    (
        "dng-pentax-k-5.dng",
        "https://raw.pixls.us/getfile.php/1479/nice/Pentax%20-%20K-5%20-%2014bit%20%283:2%29.DNG",
        "6f6419209340d07d597ccca66dc367d64cff3308ef2694c8f90c6259f3a5c431",
    ),
    (
        "cr2-canon-eos-6d-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1625/nice/Canon%20-%20EOS%206D%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "712815b910fa473f7aaeb97c0804292d996b16dd6b974a3ab9a7195a2e41d90c",
    ),
    (
        "cr2-canon-eos-m10.cr2",
        "https://raw.pixls.us/getfile.php/1758/nice/Canon%20-%20EOS%20M10%20-%20RAW%20%283:2%29.CR2",
        "71e27b9ebe7efba632e666db77b82c5129e0d1a402ff359a2b509774ebeb1b0f",
    ),
    (
        "dng-pentax-k-x.dng",
        "https://raw.pixls.us/getfile.php/830/nice/Pentax%20-%20K-x%20-%2012bit%20%283:2%29.DNG",
        "7a594b66b54f99bb5aba4627c78abfc41213d6911ea54e4f5d2dd6c8505f6968",
    ),
    (
        "cr2-canon-eos-rebel-t5i.cr2",
        "https://raw.pixls.us/getfile.php/2651/nice/Canon%20-%20EOS%20Rebel%20T5i%20-%20RAW%20%283:2%29.CR2",
        "7c3c9c3b8ec487d56040c21620aafe51e13e2c2fc1e1351929970720e10dda69",
    ),
    (
        "dng-ricoh-gr.dng",
        "https://raw.pixls.us/getfile.php/996/nice/Ricoh%20-%20GR%20-%2012bit%20%284:3%29.DNG",
        "7ceec1ea31c56b6e0414202504581b915f17818ffe018b4c8bba1393332c405e",
    ),
    (
        "dng-pentax-645d.dng",
        "https://raw.pixls.us/getfile.php/2401/nice/Pentax%20-%20645D%20-%2014bit%2014bit%20compressed%20%284:3%29.DNG",
        "7cfa4a6b3fc459157b0be94effaf98136027b1182dac7da0f91e29987ab6ae6c",
    ),
    (
        "cr2-canon-eos-rebel-t3.cr2",
        "https://raw.pixls.us/getfile.php/3920/nice/Canon%20-%20EOS%20Rebel%20T3%20-%20RAW%20%283:2%29.CR2",
        "8106f288e22ca0510b2ebe36e58833afbf847db92e9e1ec6f651605bac0beb24",
    ),
    (
        "cr2-canon-eos-77d.cr2",
        "https://raw.pixls.us/getfile.php/1332/nice/Canon%20-%20EOS%2077D%20-%20RAW%20%283:2%29.CR2",
        "811df582d97918987312fa0fceae3ad19a20532a64ebda9782764ac7e629c265",
    ),
    (
        "cr2-canon-eos-2000d.cr2",
        "https://raw.pixls.us/getfile.php/2612/nice/Canon%20-%20EOS%202000D%20-%20RAW%20%283:2%29.CR2",
        "84015493df96c877c0387071a627d6940187d75695b0598694901ac0ff83876a",
    ),
    (
        "cr2-canon-eos-5d-mark-iv.cr2",
        "https://raw.pixls.us/getfile.php/980/nice/Canon%20-%20EOS%205D%20Mark%20IV%20-%20RAW%20%283:2%29.CR2",
        "85bfec149261ffbae90797e45caae5e204e1e61ac62b92783925b5ee0f267d76",
    ),
    (
        "cr2-canon-eos-1d-mark-iii.cr2",
        "https://raw.pixls.us/getfile.php/2037/nice/Canon%20-%20EOS-1D%20Mark%20III%20-%20RAW%20%283:2%29.CR2",
        "860b5c9791548db8081823e3852c436a439e648ea863b0def05e99aac6e9e726",
    ),
    (
        "cr2-canon-eos-60d.cr2",
        "https://raw.pixls.us/getfile.php/993/nice/Canon%20-%20EOS%2060D%20-%20RAW%20%283:2%29.CR2",
        "865236ece3bf1f2f19a15001a8f0f99badf71e34b9eed1fead04d15daf87a154",
    ),
    (
        "pef-pentax-k100d.pef",
        "https://raw.pixls.us/getfile.php/2262/nice/Pentax%20-%20K100D%20-%2012bit%20%283:2%29.PEF",
        "86ec2383dbfda3eda38ca913b02874a70248efba6107aa304781c8d3779de8c9",
    ),
    (
        "cr2-canon-eos-1d-mark-ii-n.cr2",
        "https://raw.pixls.us/getfile.php/1781/nice/Canon%20-%20EOS-1D%20Mark%20II%20N%20-%20RAW%20%283:2%29.CR2",
        "8bf6591f64b0657bc6133cbd0ee1418ef8e6dd3dd595827f8e9910c0f235843c",
    ),
    (
        "dng-pentax-k-500.dng",
        "https://raw.pixls.us/getfile.php/1458/nice/Pentax%20-%20K-500%20-%2012bit%20%283:2%29.DNG",
        "91ccd6f29b8eaad88ae26d1c860ef1113e6dfc1bd2c8f5245b49f3a788295fb1",
    ),
    (
        "cr2-canon-eos-m100.cr2",
        "https://raw.pixls.us/getfile.php/3531/nice/Canon%20-%20EOS%20M100%20-%20RAW%20%283:2%29.CR2",
        "941d9e696b1d92b4b25a762474e3c5c127e7405dea099c266fa85e39a2454fcc",
    ),
    (
        "pef-pentax-k2000.pef",
        "https://raw.pixls.us/getfile.php/5239/nice/Pentax%20-%20K2000%20-%2012bit%2012bit%20compressed%20%283:2%29.PEF",
        "985003748c094b8d02f2517b4c07d66d48ea5380260235ccb3409f41ceb3ea84",
    ),
    (
        "dng-pentax-k-50.dng",
        "https://raw.pixls.us/getfile.php/2235/nice/Pentax%20-%20K-50%20-%2012bit%20%283:2%29.DNG",
        "9ea1dc1f92df19699aa8b620670c64a84d4ead0172a49cbbd47dea9257d14e67",
    ),
    (
        "cr2-canon-powershot-g5-x.cr2",
        "https://raw.pixls.us/getfile.php/4257/nice/Canon%20-%20PowerShot%20G5%20X%20-%20RAW%20%283:2%29.CR2",
        "a7d0ac7f3c7ddfa607a6d0a7433ef9ba664d260aa91616cd75cfbdf912b0da00",
    ),
    (
        "dng-pentax-645z.dng",
        "https://raw.pixls.us/getfile.php/2403/nice/Pentax%20-%20645Z%20-%2014bit%2014bit%20compressed%20%284:3%29.DNG",
        "a8203686c61962af591b374331b6eb9dace4f2b29fce9f166303fc66fd210bb0",
    ),
    (
        "dng-pentax-k-5-ii.dng",
        "https://raw.pixls.us/getfile.php/873/nice/Pentax%20-%20K-5%20II%20-%2014bit%20%283:2%29.DNG",
        "aa41f0d9ab8d3e21e468b4c61d4d1ddae6a4fbe37f629b78159246cbb94dd56b",
    ),
    (
        "cr2-canon-powershot-g9.cr2",
        "https://raw.pixls.us/getfile.php/760/nice/Canon%20-%20PowerShot%20G9%20-%20RAW%20%284:3%29.CR2",
        "ad27a4d751984cd709380e3be274a4a5243d36ca89c5fc711736ba8a9f5c932f",
    ),
    (
        "dng-pentax-q.dng",
        "https://raw.pixls.us/getfile.php/1610/nice/Pentax%20-%20Q%20-%2012bit%20%284:3%29.DNG",
        "ad79b4ce4b88e5ca8b03d0d5aeaba09f55559695234941a5a6ada0cd4825860d",
    ),
    (
        "dng-pentax-mx-1.dng",
        "https://raw.pixls.us/getfile.php/1731/nice/Pentax%20-%20MX-1%20-%2012bit%20%284:3%29.dng",
        "b00f3fd42479bbaba7a00b2b6fa3e5f5d213c51d591adff4f6eb4a768ecd1ce9",
    ),
    (
        "cr2-canon-eos-5d.cr2",
        "https://raw.pixls.us/getfile.php/1346/nice/Canon%20-%20EOS%205D%20-%20RAW%20%283:2%29.CR2",
        "b30089172973388d03e192a14a63f750e22914dfc01db5713b6b3bce3c9baeee",
    ),
    (
        "pef-pentax-k100d-super.pef",
        "https://raw.pixls.us/getfile.php/5804/nice/Pentax%20-%20K100D%20Super%20-%2012bit%2012bit%20uncompressed%20%283:2%29.PEF",
        "b391c84ff1cc8769ec5214f837d1e4b3ae80374673608601b12ee4484e003ddb",
    ),
    (
        "cr2-canon-eos-m6.cr2",
        "https://raw.pixls.us/getfile.php/1382/nice/Canon%20-%20EOS%20M6%20-%20RAW%20%283:2%29.CR2",
        "b3a7a0037600b61d4c26ad03d5cac1801e35fb43a55699db5b77708c96f7338a",
    ),
    (
        "cr2-canon-powershot-g7-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1682/nice/Canon%20-%20PowerShot%20G7%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "b81acebf5ac49b0389c354942d128d9423fdd59ff92217554f232f94e3e87ef5",
    ),
    (
        "cr2-canon-eos-7d-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1064/nice/Canon%20-%20EOS%207D%20Mark%20II%20-%20RAW%20%283:2%29.cr2",
        "b9f71c56adb6e2861e6653efb312fb5cc88ba4aa17fa809c6a18b8db044360c3",
    ),
    (
        "dng-ricoh-gr-ii.dng",
        "https://raw.pixls.us/getfile.php/1955/nice/Ricoh%20-%20GR%20II%20-%2012bit%20%283:2%29.DNG",
        "bc8e00ccfd8e4753e8ee0735255bff4dcca5fb8d1a1352adfa59b86806d03f44",
    ),
    (
        "cr2-canon-powershot-s120.cr2",
        "https://raw.pixls.us/getfile.php/1694/nice/Canon%20-%20PowerShot%20S120%20-%20RAW%20%284:3%29.CR2",
        "bcbfadad8ecdbcc783864a2b769aef37588e2a6339b9682dcd9642602bd2011f",
    ),
    (
        "cr2-canon-powershot-s95.cr2",
        "https://raw.pixls.us/getfile.php/1742/nice/Canon%20-%20PowerShot%20S95%20-%20RAW%20%284:3%29.cr2",
        "c4a55c61434b8403f948518deca22e8ab755099f124d5cea5bc1cd0ea86711ad",
    ),
    (
        "cr2-canon-powershot-g1-x-mark-iii.cr2",
        "https://raw.pixls.us/getfile.php/2695/nice/Canon%20-%20PowerShot%20G1%20X%20Mark%20III%20-%20RAW%20%283:2%29.CR2",
        "ca052ed025432fdacfba7347cb4374fa3b727155912a91c42ed9a275d13bedf5",
    ),
    (
        "cr2-canon-eos-50d.cr2",
        "https://raw.pixls.us/getfile.php/1792/nice/Canon%20-%20EOS%2050D%20-%20RAW%20%283:2%29.CR2",
        "ce460cf2b6762c8c22ebeb00e9e1edccd728edcb7c4e82c78f3bce61d21fa88d",
    ),
    (
        "dng-pentax-k-s2.dng",
        "https://raw.pixls.us/getfile.php/1626/nice/Pentax%20-%20K-S2%20-%2012bit%20%283:2%29.DNG",
        "d23916a31f09c961e6050e4120bbcd92f7847080f9e40fad8dc7047ead1dec2d",
    ),
    (
        "cr2-canon-eos-20d.cr2",
        "https://raw.pixls.us/getfile.php/1018/nice/Canon%20-%20EOS%2020D%20-%20RAW%20%283:2%29.CR2",
        "db78660d48589130d99e0bc621424851eb0f84df052634fb5bc89c7a02fe7033",
    ),
    (
        "pef-pentax-k-r.pef",
        "https://raw.pixls.us/getfile.php/8171/nice/Pentax%20-%20K-r%20-%2012bit%20compressed%20%283:2%29.PEF",
        "dd0d7b76d89039995d253adddcfb44d131414fad2179610d9ea4d8d4bb876b7b",
    ),
    (
        "pef-pentax-kf.pef",
        "https://raw.pixls.us/getfile.php/6664/nice/Pentax%20-%20KF%20-%2014bit%20%283:2%29.PEF",
        "ddc9cdf399a7210b203dee1765ab9f4165a2e695da59bce6bcabf75b77174a4b",
    ),
    (
        "cr2-canon-powershot-g7-x.cr2",
        "https://raw.pixls.us/getfile.php/1277/nice/Canon%20-%20PowerShot%20G7%20X%20-%20RAW%20%283:2%29.CR2",
        "e4b0a64df6f43252ab96b4789f698ca334e86586ab151d1e9f5616b80172abcd",
    ),
    (
        "cr2-canon-eos-450d.cr2",
        "https://raw.pixls.us/getfile.php/1353/nice/Canon%20-%20EOS%20450D%20-%20RAW%20%283:2%29.CR2",
        "e975f71429b939701dd618a7bac4709c1409f4bb6aa1c3607ceeaa86031c1c4a",
    ),
    (
        "cr2-canon-powershot-sx60-hs.cr2",
        "https://raw.pixls.us/getfile.php/4942/nice/Canon%20-%20PowerShot%20SX60%20HS%20-%20RAW%20%284:3%29.CR2",
        "f005e82fe8579876d7e9d39291c57f4410412904dfff0de5bbad9f32b3189612",
    ),
    (
        "dng-pentax-k-3-ii.dng",
        "https://raw.pixls.us/getfile.php/1363/nice/Pentax%20-%20K-3%20II%20-%2014bit%20%283:2%29.DNG",
        "f43b9dc35e4d1b43918d2d4bd0973bc202af48ed24c924e4aff61250c971b171",
    ),
    (
        "dng-pentax-k-7.dng",
        "https://raw.pixls.us/getfile.php/827/nice/Pentax%20-%20K-7%20-%2012bit%20%283:2%29.DNG",
        "f441c36678dfca4f38e25a580f4f8485d78d8067a054153bc36be71e56c92386",
    ),
    (
        "cr2-canon-eos-kiss-x9.cr2",
        "https://raw.pixls.us/getfile.php/2062/nice/Canon%20-%20EOS%20Kiss%20X9%20-%20RAW%20%283:2%29.CR2",
        "f50cea4641c97c1154a214a686226ba900002f00f4f844af858ef80b9ec8dea6",
    ),
    (
        "cr2-canon-powershot-g9-x-mark-ii.cr2",
        "https://raw.pixls.us/getfile.php/1575/nice/Canon%20-%20PowerShot%20G9%20X%20Mark%20II%20-%20RAW%20%283:2%29.CR2",
        "f91f1a3b6fa66b2e15fd26c1bd78e1eb19dfa7e6a3886aff36e7ab96fd1c319c",
    ),
    (
        "cr2-canon-eos-760d.cr2",
        "https://raw.pixls.us/getfile.php/2094/nice/Canon%20-%20EOS%20760D%20-%20RAW%20%283:2%29.cr2",
        "f98108c5efd57eef483009cdb490ada2071d1c1b58a82df3de997b4d86d849c2",
    ),
    (
        "cr2-canon-eos-30d.cr2",
        "https://raw.pixls.us/getfile.php/1791/nice/Canon%20-%20EOS%2030D%20-%20RAW%20%283:2%29.CR2",
        "fb216f8df8d7abca4eb9d9f8dc594c1d2b69d0a6369fb8b4107743343c37144f",
    ),
    ("erf-epson-rd1.erf", "https://raw.pixls.us/data/Epson/R-D1/_EPS0672.ERF", "09d8e533d93116294a9f3e161ed868e929454c7b946e083b644bd6bb75bcb5e4"),
    ("erf-epson-rd1s.erf", "https://raw.pixls.us/data/Epson/R-D1s/EPSN3575.ERF", "45bb0081baf1c813c7cd9e05b3e6ae734b90aba408dd1195118dc94dc6abbbb9"),
    ("erf-epson-rd1x.erf", "https://raw.pixls.us/data/Epson/R-D1x/_EPS1919.ERF", "fa1bff3e0b9b9a14f83b8db9fae022c2aca8d455896bf4f25f2c1a0034e295e6"),
    (
        "mef-mamiya-zd.mef",
        "https://raw.pixls.us/data/Mamiya/ZD/RAW_MAMIYA_ZD.MEF",
        "bcd63507c3c4cc3ea1bad2945e3f88cb4677a1a3c762e1acc38df4445714eb7c",
    ),
    // GoPro GPR (VC-5): one CC0 file per body (crates/raw/tests/it/gpr_corpus.rs)
    (
        "gpr-hero5-936.gpr",
        "https://raw.pixls.us/getfile.php/936/nice/GoPro%20-%20HERO5%20Black%20-%2016bit%20%284:3%29.GPR",
        "191901b64412083146256ccebeacd954783f6c40a4556ef355a067915136159b",
    ),
    (
        "gpr-hero6-2556.gpr",
        "https://raw.pixls.us/getfile.php/2556/nice/GoPro%20-%20HERO6%20Black%20-%2016bit%20%284:3%29.GPR",
        "ca9b79fcdfab1cc071405209063fb736cc0b7169a7a88fe091fceee803f546fb",
    ),
    (
        "gpr-hero7-2786.gpr",
        "https://raw.pixls.us/getfile.php/2786/nice/GoPro%20-%20HERO7%20Black%20-%2016bit%20%284:3%29.GPR",
        "0c2003137feb0ca7c9a8adf6cd832bc8fba7ff1b89cf5d77822b22558b9a20fe",
    ),
    (
        "gpr-hero8-3949.gpr",
        "https://raw.pixls.us/getfile.php/3949/nice/GoPro%20-%20HERO8%20Black%20-%2016bit%20%284:3%29.GPR",
        "bfd992594af380748f3d46e37814dae595356cb6fce182d4188230ca978e6a9f",
    ),
    (
        "gpr-hero9-4483.gpr",
        "https://raw.pixls.us/getfile.php/4483/nice/GoPro%20-%20HERO9%20Black%20-%2016bit%20%284:3%29.GPR",
        "bf6f67d0c7096f347b864310f6d58be35237c3d0e715b47a1b30e650e9d785b1",
    ),
    (
        "gpr-hero10-4883.gpr",
        "https://raw.pixls.us/getfile.php/4883/nice/GoPro%20-%20HERO10%20Black%20-%2016bit%20%284:3%29.GPR",
        "f462ea1e695a0491960a496e3998c8806a2a02a2830c9c696c7067c0039a437c",
    ),
    (
        "gpr-hero11-7319.gpr",
        "https://raw.pixls.us/getfile.php/7319/nice/GoPro%20-%20HERO11%20Black%20-%2016bit%20%285:4%29.GPR",
        "d7a6be163c847cb8456e6858a99aef3b85dc3d8836bca1da1b8886effe0d43e1",
    ),
    (
        "gpr-fusion-7370.gpr",
        "https://raw.pixls.us/getfile.php/7370/nice/GoPro%20-%20FUSION%20-%2016bit%20%281:1%29.GPR",
        "f90535bbb28f8b24093c6112bd982f89c47cb0181bcfaba97471e191861a1e80",
    ),
];

fn cmd_corpus(download: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    println!(
        "Test corpora live under {} (git-ignored, never committed).
Tests that use a corpus skip cleanly when it is absent.

  corpus/raw/        raw.pixls.us samples (CC0) — lightcraft-raw decodes every file.
  corpus/images/     CC0 / public-domain JPEG/PNG/TIFF/HEIC samples (optional)
",
        corpus.display()
    );
    if !download {
        return Ok(());
    }
    let dest = corpus.join("raw");
    std::fs::create_dir_all(&dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut bad = Vec::new();
    for (name, url, sha256) in RAW_SAMPLES {
        let out = dest.join(name);
        if !out.exists() {
            let mut curl = Command::new("curl");
            curl.args(["-fsSL", "-o"]).arg(&out).arg(url);
            run(curl, &format!("curl {url}"))?;
        }
        let actual = file_sha256(&out)?;
        if actual != *sha256 {
            // delete it, so the next run downloads it again instead of trusting it
            std::fs::remove_file(&out).map_err(|e| format!("remove {}: {e}", out.display()))?;
            bad.push(format!("{name}: sha256 is {actual}, expected {sha256} (file deleted; source {url})"));
        }
    }
    if bad.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{} corpus file(s) do not match their pinned sha256:
  {}",
        bad.len(),
        bad.join(
            "
  "
        )
    ))
}

fn file_sha256(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash).map_err(|e| format!("read {}: {e}", path.display()))?;
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
mod ci_jobs_tests {
    use super::ci_jobs;

    #[test]
    fn jobs_follow_available_ram_and_never_exceed_the_cpus() {
        assert_eq!(ci_jobs(Some(3 * 1024), 8), 2, "a 4 GB machine with 3 GB free");
        assert_eq!(ci_jobs(Some(6 * 1024), 8), 4, "an 8 GB machine with 6 GB free");
        assert_eq!(ci_jobs(Some(7 * 1024), 32), 4, "a busy 32 GB machine: what is free counts");
        assert_eq!(ci_jobs(Some(24 * 1024), 32), 16);
        assert_eq!(ci_jobs(Some(64 * 1024), 8), 8, "capped at the CPU count");
        assert_eq!(ci_jobs(Some(500), 8), 1, "at least one");
        assert_eq!(ci_jobs(None, 32), 4, "unknown RAM keeps the old default");
        assert_eq!(ci_jobs(None, 2), 2);
    }
}

#[cfg(test)]
mod corpus_tests {
    use super::RAW_SAMPLES;

    #[test]
    fn every_sample_has_a_unique_name_and_a_sha256() {
        let mut names: Vec<_> = RAW_SAMPLES.iter().map(|s| s.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), RAW_SAMPLES.len(), "duplicate file name");
        for (name, _, sha256) in RAW_SAMPLES {
            assert!(sha256.len() == 64 && sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')), "{name}: not a lowercase sha256");
        }
    }
}
