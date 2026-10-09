//! Workspace tooling: `cargo xtask <command>`.
//!
//! Pure Rust (std + serde_json; flate2/brotli for the web bundle). External tools (`cargo`, `curl`, `tar`) are
//! invoked through `std::process::Command`.

mod assets;
mod bench;
mod ico;
mod layers;
mod parity;
mod stats;
mod version;
mod web;

use std::path::PathBuf;
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
                  check docs/parity.md (every cmd:/ctl: id and path it cites exists) and print the
                  Lightroom parity summary; --write refreshes the summary table in the document
  wasm            cargo check --target wasm32-unknown-unknown for the wasm-safe crates (+ the web app)
  web [--serve [port]] [--dev]
                  build the browser app (apps/lightcraft-web) into <target>/web/;
                  --serve serves it on http://127.0.0.1:<port> (default 8080)
  ci              fmt --check, clippy -D warnings, heif, test, parity refs, layers, assets, wasm (stops at first failure)
  corpus [--download]
                  show where test corpora live; --download fetches PngSuite and CC0 raw samples (raw.pixls.us) into corpus/
  stats [--exact] count tests and lines per crate (--exact: ask the test harness via `-- --list`)
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
        .chain(std::iter::once("lightcraft-web".to_string()))
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
                c.args(["clippy", "-p", "lightcraft-codecs", "--features", "heif", "--all-targets", "--", "-D", "warnings"]);
                run(c, "cargo clippy -p lightcraft-codecs --features heif --all-targets -- -D warnings")?;
                let mut c = cargo();
                c.args(["test", "-p", "lightcraft-codecs", "-p", "lightcraft-heif", "--features", "lightcraft-codecs/heif"]);
                run(c, "cargo test -p lightcraft-codecs -p lightcraft-heif --features lightcraft-codecs/heif")
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
/// CC0 raw samples from raw.pixls.us (each verified CC0 on the site; files matched by their published SHA-256).
/// One per format / compression variant we decode or deliberately report as unsupported (preview only).
const RAW_SAMPLES: &[(&str, &str)] = &[
    (
        "arw-sony-a7m3-compressed.arw",
        "https://raw.pixls.us/getfile.php/2414/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20compressed%20%283:2%29.ARW",
    ),
    (
        "arw-sony-a7m3-uncompressed.arw",
        "https://raw.pixls.us/getfile.php/2418/nice/Sony%20-%20ILCE-7M3%20-%2014bit%2014bit%20uncompressed%20%283:2%29.ARW",
    ),
    ("arw-sony-a7m4-14bit.arw", "https://raw.pixls.us/getfile.php/6936/nice/Sony%20-%20ILCE-7M4%20-%2014bit%20%283:2%29.ARW"),
    // lossless compressed (Compression 7): L is 2×2 CFA cells per LJ92 sample; M and S are subsampled
    ("arw-sony-a7m4-lossless-l.arw", "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06674_FullFrame-LossLess-Compressed-Large.ARW"),
    ("arw-sony-a7m4-lossless-m.arw", "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06675_FullFrame-LossLess-Compressed-Medium.ARW"),
    ("arw-sony-a7m4-lossless-s.arw", "https://raw.pixls.us/data/Sony/ILCE-7M4/ILCE-7M4_DSC06676_FullFrame-LossLess-Compressed-Small.ARW"),
    // pre-2017 bodies: white balance only in the enciphered maker note, black level only in the SR2SubIFD (#148)
    ("arw-sony-rx100m3.arw", "https://raw.pixls.us/data/Sony/DSC-RX100M3/DSC00734.ARW"),
    ("arw-sony-rx100.arw", "https://raw.pixls.us/data/Sony/DSC-RX100/DSC00838.ARW"),
    ("arw-sony-a7rm2-12bit-uncompressed.arw", "https://raw.pixls.us/data/Sony/ILCE-7RM2/12-bit-uncompressed.ARW"),
    // CR2 colour-filter layouts differ by model (issue #85): CR2CFAPattern 3 (GBRG) and 1 (RGGB) samples
    ("cr2-canon-40d.cr2", "https://raw.pixls.us/data/Canon/EOS%2040D/_MG_0153.CR2"),
    ("cr2-canon-550d.cr2", "https://raw.pixls.us/data/Canon/EOS%20550D/IMG_4047.CR2"),
    ("cr2-canon-5d2.cr2", "https://raw.pixls.us/data/Canon/EOS%205D%20Mark%20II/08.canon.raw.cr2"),
    ("cr2-canon-5dsr.cr2", "https://raw.pixls.us/data/Canon/EOS%205DS%20R/_DSR2002.CR2"),
    ("cr2-canon-6d.cr2", "https://raw.pixls.us/data/Canon/EOS%206D/EOS_6D_RAW.CR2"),
    ("cr2-canon-7d.cr2", "https://raw.pixls.us/data/Canon/EOS%207D/RAW_CANON_EOS_7D-raw.CR2"),
    ("cr2-canon-5d3-sraw2.cr2", "https://raw.pixls.us/getfile.php/773/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%20sRAW2%20%28sRAW%29.CR2"),
    ("cr2-canon-5d3.cr2", "https://raw.pixls.us/getfile.php/771/nice/Canon%20-%20EOS%205D%20Mark%20III.CR2"),
    ("cr2-canon-80d.cr2", "https://raw.pixls.us/getfile.php/1294/nice/Canon%20-%20EOS%2080D%20-%20RAW%20%283:2%29.CR2"),
    ("cr3-canon-m50-craw.cr3", "https://raw.pixls.us/getfile.php/2663/nice/Canon%20-%20EOS%20M50%20-%20CRAW%20%283:2%29.CR3"),
    ("cr3-canon-m50-raw.cr3", "https://raw.pixls.us/getfile.php/4657/nice/Canon%20-%20EOS%20M50%20-%203:2.CR3"),
    ("cr3-canon-r100-raw.cr3", "https://raw.pixls.us/getfile.php/7896/nice/Canon%20-%20EOS%20R100%20-%20RAW%20%283:2%29.CR3"),
    ("cr3-canon-r100-craw.cr3", "https://raw.pixls.us/getfile.php/7897/nice/Canon%20-%20EOS%20R100%20-%20CRAW%20%283:2%29.CR3"),
    ("cr3-canon-r8-raw.cr3", "https://raw.pixls.us/getfile.php/6585/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3"),
    ("cr3-canon-r8-craw.cr3", "https://raw.pixls.us/getfile.php/6587/nice/Canon%20-%20EOS%20R8%20-%203:2.CR3"),
    (
        "dng-adobe-canon-5d3-linear-lj92.dng",
        "https://raw.pixls.us/getfile.php/1032/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossless%20JPEG%20compression%2C%20rgb%20%283:2%29.DNG",
    ),
    (
        "dng-adobe-canon-5d3-lj92.dng",
        "https://raw.pixls.us/getfile.php/1024/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20Lossless%20JPEG%20compression%20%283:2%29.DNG",
    ),
    (
        "dng-adobe-canon-5d3-lossy.dng",
        "https://raw.pixls.us/getfile.php/1023/nice/Adobe%20DNG%20Converter%20-%20Canon%20EOS%205D%20Mark%20III%20-%20Lossy%20JPEG%20compression%20%283:2%29.DNG",
    ),
    // Apple ProRAW (iPhone 12 Pro, iOS 14.3): LinearRaw LJ92 tiles, ProfileToneCurve, ProfileGainTableMap, a sky matte.
    // sha256 e91e77a4533ed7cce551d83330676ea5c47dd5e55fb38adda7819366afdbdfc2
    ("dng-apple-iphone12pro-proraw.dng", "https://raw.pixls.us/data/Apple/iPhone%2012%20Pro/IMG_1361.DNG"),
    (
        "dng-canon-5d3-14bit-small.dng",
        "https://raw.pixls.us/getfile.php/2204/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2014bit%2014bit%20%282.3471882640587%29.dng",
    ),
    ("dng-canon-5d3-16bit-169.dng", "https://raw.pixls.us/getfile.php/2649/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%20%2816:9%29.dng"),
    ("dng-canon-5d3-16bit.dng", "https://raw.pixls.us/getfile.php/885/nice/Canon%20-%20EOS%205D%20Mark%20III%20-%2016bit%2016bit%20RAW.dng"),
    ("dng-google-pixel2xl.dng", "https://raw.pixls.us/getfile.php/2206/nice/Google%20-%20Pixel%202%20XL%20-%2016bit%20%284:3%29.dng"),
    ("dng-ricoh-gr3.dng", "https://raw.pixls.us/getfile.php/3115/nice/Ricoh%20-%20GR%20III%20-%2014bit%20%283:2%29.DNG"),
    (
        "nef-nikon-d5100-lossless.nef",
        "https://raw.pixls.us/getfile.php/1597/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.nef",
    ),
    (
        "nef-nikon-d5100-uncompressed.nef",
        "https://raw.pixls.us/getfile.php/1598/nice/Nikon%20-%20D5100%20-%2014bit%2014bit%20uncompressed%20%283:2%29.nef",
    ),
    (
        "nef-nikon-d7000-lossy12.nef",
        "https://raw.pixls.us/getfile.php/961/nice/Nikon%20-%20D7000%20-%2012bit%2012bit%20compressed%20%28Lossy%20%28type%202%29%29%20%283:2%29.NEF",
    ),
    (
        "nef-nikon-d7500-lossless12.nef",
        "https://raw.pixls.us/getfile.php/1532/nice/Nikon%20-%20D7500%20-%2012bit%2012bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
    ),
    (
        "nef-nikon-d7500-lossless14.nef",
        "https://raw.pixls.us/getfile.php/1534/nice/Nikon%20-%20D7500%20-%2014bit%2014bit%20compressed%20%28Lossless%29%20%283:2%29.NEF",
    ),
    (
        "nrw-nikon-b700-uncompressed.nrw",
        "https://raw.pixls.us/getfile.php/1621/nice/Nikon%20-%20COOLPIX%20B700%20-%2012bit%2012bit%20uncompressed%20%284:3%29.NRW",
    ),
    ("orf-olympus-e1.orf", "https://raw.pixls.us/getfile.php/1800/nice/Olympus%20-%20E-1%20-%2016bit%20%284:3%29.ORF"),
    ("orf-olympus-e400.orf", "https://raw.pixls.us/getfile.php/2151/nice/Olympus%20-%20E-400%20-%2016bit%20%284:3%29.ORF"),
    ("orf-olympus-em1.orf", "https://raw.pixls.us/getfile.php/1051/nice/Olympus%20-%20E-M1%20-%2016bit%20%284:3%29.orf"),
    ("orf-olympus-em10iii.orf", "https://raw.pixls.us/getfile.php/1787/nice/Olympus%20-%20E-M10%20Mark%20III%20-%2016bit%20%284:3%29.ORF"),
    ("orf-olympus-xz2.orf", "https://raw.pixls.us/getfile.php/1432/nice/Olympus%20-%20XZ-2%20-%2012bit%20%284:3%29.orf"),
    ("pef-pentax-k10d.pef", "https://raw.pixls.us/getfile.php/2239/nice/Pentax%20-%20K10D%20-%2012bit%2012bit%20compressed%20%283:2%29.PEF"),
    ("pef-pentax-k3.pef", "https://raw.pixls.us/getfile.php/1075/nice/Pentax%20-%20K-3%20-%2014bit%20%283:2%29.PEF"),
    ("pef-pentax-k5iis.pef", "https://raw.pixls.us/getfile.php/1198/nice/Pentax%20-%20K-5%20II%20s%20-%2014bit%20%283:2%29.PEF"),
    (
        "raf-fuji-xa2-12bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2883/nice/Fujifilm%20-%20X-A2%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF",
    ),
    (
        "raf-fuji-xa5-14bit-bayer.raf",
        "https://raw.pixls.us/getfile.php/2526/nice/Fujifilm%20-%20X-A5%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF",
    ),
    ("raf-fuji-xe1-12bit.raf", "https://raw.pixls.us/getfile.php/3098/nice/Fujifilm%20-%20X-E1%20-%2012bit%2012bit%20uncompressed%20%283:2%29.RAF"),
    ("raf-fuji-xt20-14bit.raf", "https://raw.pixls.us/getfile.php/1177/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20uncompressed%20%283:2%29.RAF"),
    (
        "raf-fuji-xt20-compressed.raf",
        "https://raw.pixls.us/getfile.php/1178/nice/Fujifilm%20-%20X-T20%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
    ),
    ("raw-panasonic-fz50.raw", "https://raw.pixls.us/getfile.php/2234/nice/Panasonic%20-%20DMC-FZ50%20-%204:3.RAW"),
    ("raw-panasonic-fz8.raw", "https://raw.pixls.us/getfile.php/2282/nice/Panasonic%20-%20DMC-FZ8%20-%204:3.RAW"),
    ("rw2-panasonic-fz1000m2-4x3.rw2", "https://raw.pixls.us/getfile.php/4706/nice/Panasonic%20-%20DC-FZ10002%20-%204:3.RW2"),
    // Fujifilm predictive compression: older X-Trans, 40 MP X-Trans, 14/16-bit GFX; both compression modes.
    (
        "raf-fuji-gfx100-3773.raf",
        "https://raw.pixls.us/getfile.php/3773/nice/Fujifilm%20-%20GFX%20100%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
    ),
    (
        "raf-fuji-gfx100-3775.raf",
        "https://raw.pixls.us/getfile.php/3775/nice/Fujifilm%20-%20GFX%20100%20-%2014bit%2014bit%20compressed%20%284:3%29.RAF",
    ),
    ("raf-fuji-gfx100rf-8091.raf", "https://raw.pixls.us/getfile.php/8091/nice/Fujifilm%20-%20GFX100RF%20-%2016bit%20compressed%20%284:3%29.RAF"),
    (
        "raf-fuji-gfx100s-4495.raf",
        "https://raw.pixls.us/getfile.php/4495/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20lossless%20compressed%20%284:3%29.RAF",
    ),
    (
        "raf-fuji-gfx100s-4503.raf",
        "https://raw.pixls.us/getfile.php/4503/nice/Fujifilm%20-%20GFX100S%20-%2016bit%2016bit%20compressed%20%284:3%29.RAF",
    ),
    (
        "raf-fuji-gfx50s-1435.raf",
        "https://raw.pixls.us/getfile.php/1435/nice/Fujifilm%20-%20GFX%2050S%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF",
    ),
    ("raf-fuji-xe5-8509.raf", "https://raw.pixls.us/getfile.php/8509/nice/Fujifilm%20-%20X-E5%20-%2014bit%20lossy%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xh2-6001.raf", "https://raw.pixls.us/getfile.php/6001/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xh2-6002.raf", "https://raw.pixls.us/getfile.php/6002/nice/Fujifilm%20-%20X-H2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xm5-7748.raf", "https://raw.pixls.us/getfile.php/7748/nice/Fujifilm%20-%20X-M5%20-%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xt2-865.raf", "https://raw.pixls.us/getfile.php/865/nice/Fujifilm%20-%20X-T2%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    (
        "raf-fuji-xt4-3914.raf",
        "https://raw.pixls.us/getfile.php/3914/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20lossless%20compressed%20%283:2%29.RAF",
    ),
    ("raf-fuji-xt4-3918.raf", "https://raw.pixls.us/getfile.php/3918/nice/Fujifilm%20-%20X-T4%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xt5-6122.raf", "https://raw.pixls.us/getfile.php/6122/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xt5-6123.raf", "https://raw.pixls.us/getfile.php/6123/nice/Fujifilm%20-%20X-T5%20-%2014bit%2014bit%20compressed%20%283:2%29.RAF"),
    ("raf-fuji-xt50-7807.raf", "https://raw.pixls.us/getfile.php/7807/nice/Fujifilm%20-%20X-T50%20-%2014bit%20compressed%20%283:2%29.RAF"),
    // Sony ILCE-7RM6: ARW6 Compressed (RAW-C) / Compressed HQ (RAW-C-HD) and the lossless siblings; CC0 1.0, RawDB.
    ("arw6-sony-ilce7rm6-apsc-land-hq.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_land_RAW-C-HD.ARW"),
    ("arw6-sony-ilce7rm6-apsc-land-c.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_land_RAW-C.ARW"),
    ("arw6-sony-ilce7rm6-apsc-port-hq.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_port_RAW-C-HD.ARW"),
    ("arw6-sony-ilce7rm6-apsc-port-c.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_APSC_port_RAW-C.ARW"),
    ("arw6-sony-ilce7rm6-ff-land-hq.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_land_RAW-C-HD.ARW"),
    ("arw6-sony-ilce7rm6-ff-land-c.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_land_RAW-C.ARW"),
    ("arw6-sony-ilce7rm6-ff-port-hq.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_port_RAW-C-HD.ARW"),
    ("arw6-sony-ilce7rm6-ff-port-c.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/arw6/ILCE-7RM6_FF_port_RAW-C.ARW"),
    ("arw-sony-ilce7rm6-apsc-land-lossless.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/raw_modes/ILCE-7RM6_APSC_land_RAW.ARW"),
    ("arw-sony-ilce7rm6-apsc-port-lossless.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/raw_modes/ILCE-7RM6_APSC_port_RAW.ARW"),
    ("arw-sony-ilce7rm6-ff-land-lossless.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/raw_modes/ILCE-7RM6_FF_land_RAW.ARW"),
    ("arw-sony-ilce7rm6-ff-port-lossless.arw", "https://rawdb.dnglab.org/api/download/Sony/ILCE-7RM6/raw_modes/ILCE-7RM6_FF_port_RAW.ARW"),
    ("rw2-panasonic-g9-b.rw2", "https://raw.pixls.us/getfile.php/2348/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2"),
    ("rw2-panasonic-g9.rw2", "https://raw.pixls.us/getfile.php/2585/nice/Panasonic%20-%20DC-G9%20-%204:3.RW2"),
    ("rw2-panasonic-gh1.rw2", "https://raw.pixls.us/getfile.php/1323/nice/Panasonic%20-%20DMC-GH1%20-%204:3.RW2"),
    ("rw2-panasonic-gh5.rw2", "https://raw.pixls.us/getfile.php/1517/nice/Panasonic%20-%20DC-GH5%20-%204:3.RW2"),
    ("rw2-panasonic-gh5m2.rw2", "https://raw.pixls.us/getfile.php/5082/nice/Panasonic%20-%20DC-GH5M2%20-%204:3.RW2"),
    ("rw2-panasonic-gh5s.rw2", "https://raw.pixls.us/getfile.php/2603/nice/Panasonic%20-%20DC-GH5S%20-%204:3.RW2"),
    ("rw2-panasonic-gh6.rw2", "https://raw.pixls.us/getfile.php/5876/nice/Panasonic%20-%20DC-GH6%20-%204:3.RW2"),
    ("rw2-panasonic-gx80.rw2", "https://raw.pixls.us/getfile.php/1569/nice/Panasonic%20-%20DMC-GX80%20-%204:3.RW2"),
    ("rw2-panasonic-s1.rw2", "https://raw.pixls.us/getfile.php/3038/nice/Panasonic%20-%20DC-S1%20-%203:2.RW2"),
    ("rw2-panasonic-s5-format7.rw2", "https://raw.pixls.us/getfile.php/6339/nice/Panasonic%20-%20DC-S5%20-%203:2.RW2"),
    ("rw2-panasonic-s5m2.rw2", "https://raw.pixls.us/getfile.php/7790/nice/Panasonic%20-%20DC-S5M2%20-%2014bit%20%283:2%29.RW2"),
    ("rw2-panasonic-s9.rw2", "https://raw.pixls.us/getfile.php/7702/nice/Panasonic%20-%20DC-S9%20-%203:2.RW2"),
    ("rwl-leica-dlux7.rwl", "https://raw.pixls.us/getfile.php/4204/nice/Leica%20-%20D-Lux%207%20-%204:3.RWL"),
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
    for (name, url) in RAW_SAMPLES {
        let out = dest.join(name);
        if out.exists() {
            continue;
        }
        let mut curl = Command::new("curl");
        curl.args(["-fsSL", "-o"]).arg(&out).arg(url);
        run(curl, &format!("curl {url}"))?;
    }
    Ok(())
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
