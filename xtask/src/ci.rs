use super::layers::cmd_layers;
use super::raw_samples::RAW_SAMPLES;
use super::wasm::{cmd_wasm, cmd_wasm_for};
use super::{Command, Path, PathBuf, USAGE, assets, cargo, ci_lock, ci_quick, ci_report, metadata, parity, root, run};

/// CI build jobs: one per 1.5 GB of RAM available when CI starts, at most one per CPU, 4 when it
/// is unknown. With line-table debuginfo, 4 jobs measured about 3 GB of RAM in use and 2 jobs
/// about 1.5 GB, so this leaves about half the available RAM to spare. Counting available rather
/// than installed RAM keeps a busy machine (browsers, VMs, editors) from being overcommitted.
/// Test threads are this, capped at 4.
pub(crate) fn ci_jobs(available_mb: Option<u64>, cpus: usize) -> usize {
    available_mb.map_or(4, |mb| usize::try_from(mb / 1536).unwrap_or(usize::MAX)).clamp(1, cpus.max(1))
}

/// RAM the OS could hand out now (free plus reclaimable cache) in MB, if it reports it.
pub(crate) fn available_ram_mb() -> Option<u64> {
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
pub(crate) fn total_ram_gb() -> Option<u64> {
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

pub(crate) fn cmd_ci(args: &[&str]) -> Result<(), String> {
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
