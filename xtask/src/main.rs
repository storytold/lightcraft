//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json; flate2/brotli for the web bundle). External tools (`cargo`, `curl`, `tar`) are
//! invoked through `std::process::Command`.

mod assets;
mod bench;
mod brand;
mod dist;
mod docs;
mod ico;
mod immich;
mod layers;
mod parity;
mod rename;
mod stats;
mod version;
mod web;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  assets          every image/icon/font/media file is attributed in assets/ATTRIBUTION.md; no Adobe assets
  brand check|test|vars
                  check: no product name (brand.toml's display_name/binary/env_prefix, or a legacy
                  name) in any source file outside the allowlist (xtask/src/brand.rs);
                  test: build the CLI with xtask/test-brand.toml and check help, UI snapshot,
                  MCP serverInfo and config/log paths show only that brand; vars: list template keys
  bench [FILE] [--strict] [--threshold PCT]
                  run the render benchmark, append to target/bench/history.jsonl, compare CPU time with
                  the previous run (default input: corpus/raw/arw-sony-a7m3-compressed.arw)
  bench-catalog [--photos N,N…] [--backend v3|v4] [--dir DIR]
                  catalog scale benchmark: import throughput, snapshot time, then (in a fresh process)
                  open time, peak RSS and filter latency (default 250000,1000000 photos, backend v4)
  deny            cargo deny check licenses (deny.toml), skipped with a message if cargo-deny is missing
  docs [--check]  render README.md.in and docs/**/*.md.in ({{app}}, {{binary}}, … from brand.toml) into
                  the .md next to each; --check fails when one is out of date
  install [--prefix DIR] [--skip-build]
                  release build, installed as brand.binary / brand.cli_binary into DIR/bin (default
                  ~/.local), with man page, shell completions and (Linux) desktop entry and icons
  package [--skip-build] [--render-only]
                  release build + rendered packaging/**/*.in templates, branded binaries, man page and
                  completions into target/package/ (installers: packaging/<os>/package.*)
  rename-crates <prefix> [--from OLD] | --upstream [--since REV]
                  change the internal crate prefix (dac-*) everywhere; --upstream rewrites incoming
                  upstream crate paths (UPSTREAM_PREFIX-*) in files changed since REV (fork-base)
  run [--release] [ARGS…]
                  build and run the app under its brand.binary name (target/<profile>/branded/)
  immich up | down [--volumes] | seed
                  pinned local Immich test server (xtask/immich/compose.yml) on 127.0.0.1:2284; seed creates an admin, an API key (target/immich/api-key) and a fixture album
  ico <out.ico> <in.png>...
                  pack square PNGs (<= 256 px) into a Windows .ico (see packaging/icons.sh)
  layers          enforce the crate dependency layering (plan/architecture.md §3)
  parity [--write]
                  check docs/parity.md (every cmd:/ctl: id and path it cites exists) and print the
                  Lightroom parity summary; --write refreshes the summary table in the document
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates (+ the web app)
  web [--serve [port]] [--dev]
                  build the browser app (apps/web) into <target>/web/;
                  --serve serves it on http://127.0.0.1:<port> (default 8080)
  ci              fmt --check, brand check, docs --check, clippy -D warnings, heif, test, parity refs, layers,
                  assets, deny, wasm (stops at first failure)
  corpus [--download]
                  show where test corpora live; --download fetches PngSuite and CC0 raw samples (raw.pixls.us) into corpus/ and checks their sha256
  stats [--exact] count tests and lines per crate (--exact: ask the test harness via `-- --list`)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let result = match args.first().map(String::as_str) {
        Some("brand") => brand::run(&root(), &rest),
        Some("docs") => docs::run(&root(), rest.contains(&"--check")),
        Some("deny") => cmd_deny(),
        Some("run") => dist::cmd_run(&root(), &rest),
        Some("install") => dist::cmd_install(&root(), &rest),
        Some("package") => dist::cmd_package(&root(), &rest),
        Some("rename-crates") => rename::run(&root(), &rest),
        Some("immich") => immich::run(&root(), &rest),
        Some("ico") => ico::run(&rest),
        Some("version") => version::run(&root(), &rest),
        Some("layers") => cmd_layers(),
        Some("assets") => assets::run(&root()),
        Some("bench") => bench::run(&root(), &rest),
        Some("bench-catalog") => cmd_bench_catalog(&rest),
        Some("parity") => parity::run(&root(), rest.contains(&"--write")),
        Some("wasm") => cmd_wasm(),
        Some("web") => web::run(&rest),
        Some("ci") => cmd_ci(),
        Some("corpus") => cmd_corpus(rest.contains(&"--download")),
        Some("stats") => stats::run(&root(), rest.contains(&"--exact")),
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
        for (key, value) in
            [("CARGO_PROFILE_DEV_DEBUG", "line-tables-only"), ("CARGO_BUILD_JOBS", build.as_str()), ("RUST_TEST_THREADS", threads.as_str())]
        {
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
        .chain(std::iter::once("dac-web".to_string()))
        .collect())
}

fn cmd_wasm() -> Result<(), String> {
    let set = wasm_set()?;
    let mut results = Vec::new();
    for pkg in &set {
        let mut c = cargo();
        c.args(["check", "--target", "wasm32-unknown-unknown", "-p", pkg]);
        let ok = run(c, &format!("cargo check --target wasm32-unknown-unknown -p {pkg}")).is_ok();
        results.push((pkg.clone(), ok));
    }
    println!("\nwasm32-unknown-unknown check:");
    for (p, ok) in &results {
        println!("  {:<6} {p}", if *ok { "ok" } else { "FAIL" });
    }
    let failed = results.iter().filter(|r| !r.1).count();
    if failed == 0 { Ok(()) } else { Err(format!("{failed} crate(s) failed the wasm check")) }
}

/// `cargo deny check licenses` with deny.toml; a clear skip when cargo-deny isn't installed (CI installs it).
/// `bench-catalog`: the catalog crate's `bench_catalog` example, release build (see its docs).
fn cmd_bench_catalog(rest: &[&str]) -> Result<(), String> {
    let mut c = cargo();
    c.args(["run", "--release", "-q", "-p", "dac-catalog", "--example", "bench_catalog", "--"]);
    if !rest.contains(&"--dir") {
        c.args(["--dir", "target/bench-catalog"]);
    }
    c.args(rest);
    run(c, "bench_catalog")
}

fn cmd_deny() -> Result<(), String> {
    let installed = cargo().args(["deny", "--version"]).output().is_ok_and(|o| o.status.success());
    if !installed {
        eprintln!("deny: SKIPPED, cargo-deny is not installed (`cargo install --locked cargo-deny`); CI runs it");
        return Ok(());
    }
    let mut c = cargo();
    c.args(["deny", "check", "licenses"]);
    run(c, "cargo deny check licenses")
}

fn cmd_ci() -> Result<(), String> {
    type Step = (&'static str, Box<dyn Fn() -> Result<(), String>>);
    let steps: Vec<Step> = vec![
        (
            "fmt",
            Box::new(|| {
                let mut c = cargo();
                c.args(["fmt", "--all", "--", "--check"]);
                run(c, "cargo fmt --all -- --check")
            }),
        ),
        ("brand", Box::new(|| brand::check(&root()))),
        ("docs", Box::new(|| docs::run(&root(), true))),
        (
            "clippy",
            Box::new(|| {
                let mut c = cargo();
                c.args(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy --workspace --all-targets -- -D warnings")
            }),
        ),
        (
            "heif",
            Box::new(|| {
                // the optional HEIC/HEIF decoder is off in the workspace build above
                let mut c = cargo();
                c.args(["clippy", "-p", "dac-codecs", "--features", "heif", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy -p dac-codecs --features heif --all-targets -- -D warnings")?;
                let mut c = cargo();
                c.args(["test", "-p", "dac-codecs", "-p", "dac-heif", "--features", "dac-codecs/heif"]);
                run(c, "cargo test -p dac-codecs -p dac-heif --features dac-codecs/heif")
            }),
        ),
        (
            "test",
            Box::new(|| {
                let mut c = cargo();
                c.args(["test", "--workspace"]);
                run(c, "cargo test --workspace")
            }),
        ),
        ("parity", Box::new(|| parity::run(&root(), false))),
        ("layers", Box::new(cmd_layers)),
        ("assets", Box::new(|| assets::run(&root()))),
        ("deny", Box::new(cmd_deny)),
        ("wasm", Box::new(cmd_wasm)),
    ];
    let mut done = Vec::new();
    for (name, f) in &steps {
        eprintln!("\n=== ci: {name} ===");
        if let Err(e) = f() {
            println!("\nCI summary:");
            for d in &done {
                println!("  ok    {d}");
            }
            println!("  FAIL  {name}: {e}");
            for (n, _) in steps.iter().skip(done.len() + 1) {
                println!("  skip  {n}");
            }
            return Err(format!("ci failed at `{name}`"));
        }
        done.push(*name);
    }
    println!("\nCI summary: all {} steps passed ({})", done.len(), done.join(", "));
    Ok(())
}

/// CC0 raw samples (raw.pixls.us). Small, representative set; extend freely (CC0 only).
/// CC0 raw samples from raw.pixls.us (each verified CC0 on the site).
/// The third field is the SHA-256 of the file as downloaded; `cargo xtask corpus --download` checks it.
/// One per format / compression variant we decode or deliberately report as unsupported (preview only).
const RAW_SAMPLES: &[(&str, &str, &str)] = &[
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
    // pre-2017 bodies: white balance only in the enciphered maker note, black level only in the SR2SubIFD (#148)
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
];

fn cmd_corpus(download: bool) -> Result<(), String> {
    let corpus = root().join("corpus");
    println!(
        "Test corpora live under {} (git-ignored, never committed).
Tests that use a corpus skip cleanly when it is absent.

  corpus/raw/        raw.pixls.us samples (CC0) — dac-raw decodes every file.
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
