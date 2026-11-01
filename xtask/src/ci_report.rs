//! What a `cargo xtask ci` run covered on this machine: the capability probe (OS, CPUs, RAM, GPU
//! adapters per backend, corpus, craft-fonts), each step's result and time, and the tests that
//! skipped for want of hardware or data. Written to `target/ci/report.json` and
//! `target/ci/summary.md` (a block to paste into a PR) and printed after the CI summary.
//!
//! Tests record skips with `lightcraft_testkit::skip!`; each test step points
//! `LIGHTCRAFT_SKIP_LOG` at `target/ci/skips-<step>.jsonl`. The GPU adapters come from the ignored
//! `gpu_probe` test of `lightcraft-gpu` (xtask has no wgpu). Nothing here is sent anywhere.
//!
//! The report holds no host name, user name or absolute path: the workspace root and the home
//! directory are cut from skip reasons.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// Bumped when a field changes meaning.
pub const SCHEMA: u64 = 1;

/// The environment variable `lightcraft-testkit` appends skips to.
pub const SKIP_LOG_ENV: &str = "LIGHTCRAFT_SKIP_LOG";

/// `target/ci` under the workspace root (not `CARGO_TARGET_DIR`: one place to look, like
/// `target/bench`).
pub fn dir(root: &Path) -> PathBuf {
    root.join("target").join("ci")
}

/// Create `target/ci` and remove the previous run's skip logs, report and summary.
pub fn prepare(root: &Path) -> Result<PathBuf, String> {
    let dir = dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if (name.starts_with("skips-") && name.ends_with(".jsonl")) || name == "report.json" || name == "summary.md" {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(dir)
}

/// The skip log of one step.
pub fn skip_log(root: &Path, step: &str) -> PathBuf {
    let safe: String = step.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' }).collect();
    dir(root).join(format!("skips-{safe}.jsonl"))
}

fn output(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut c = Command::new(program);
    c.args(args);
    if let Some(cwd) = cwd {
        c.current_dir(cwd);
    }
    let out = c.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Commit, branch and whether the work tree has uncommitted changes.
pub fn git(root: &Path) -> Value {
    let commit = output("git", &["rev-parse", "HEAD"], Some(root));
    let branch = output("git", &["rev-parse", "--abbrev-ref", "HEAD"], Some(root));
    let dirty = output("git", &["status", "--porcelain"], Some(root)).map(|s| !s.is_empty());
    json!({"commit": commit, "branch": branch, "dirty": dirty})
}

/// OS, version, architecture, CPUs and RAM.
pub fn machine(ram_total_gb: Option<u64>, ram_available_mb: Option<u64>) -> Value {
    json!({
        "os": std::env::consts::OS,
        "os_version": os_version(),
        "arch": std::env::consts::ARCH,
        "cpus": std::thread::available_parallelism().map(|n| n.get()).ok(),
        "ram_total_gb": ram_total_gb,
        "ram_available_mb": ram_available_mb,
    })
}

fn os_version() -> Option<String> {
    if cfg!(target_os = "linux") {
        let release = std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).map(|v| v.trim_matches('"').to_owned()));
        let kernel = output("uname", &["-r"], None);
        match (release, kernel) {
            (Some(r), Some(k)) => Some(format!("{r}, kernel {k}")),
            (r, k) => r.or(k),
        }
    } else if cfg!(target_os = "macos") {
        output("sw_vers", &["-productVersion"], None).map(|v| format!("macOS {v}"))
    } else if cfg!(windows) {
        output("cmd", &["/C", "ver"], None)
    } else {
        output("uname", &["-sr"], None)
    }
}

/// `rustc -V` of the toolchain cargo uses.
pub fn toolchain() -> Option<String> {
    output(&std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()), &["-V"], None)
}

/// Test data present: how many of the pinned raw samples, other corpus files, craft-fonts.
pub fn data(root: &Path, pinned_raw: &[&str]) -> Value {
    let corpus = std::env::var_os("LIGHTCRAFT_CORPUS").map(PathBuf::from).unwrap_or_else(|| root.join("corpus"));
    let count = |sub: &str| -> usize { std::fs::read_dir(corpus.join(sub)).map(|d| d.flatten().filter(|e| e.path().is_file()).count()).unwrap_or(0) };
    let pinned = pinned_raw.iter().filter(|name| corpus.join("raw").join(name).is_file()).count();
    let fonts = std::env::var_os("CRAFT_FONTS_DIR").filter(|d| !d.is_empty()).map(PathBuf::from);
    json!({
        "corpus": {
            "custom_dir": std::env::var_os("LIGHTCRAFT_CORPUS").is_some(),
            "raw_pinned": pinned,
            "raw_pinned_total": pinned_raw.len(),
            "raw_files": count("raw"),
            "images": count("images"),
            "pngsuite": count("pngsuite"),
        },
        "craft_fonts": {
            "set": fonts.is_some(),
            // crates/engine/build.rs embeds the fonts of `fonts/manifest.txt` (a relative dir may mean the root)
            "found": fonts.as_deref().is_some_and(|d| [d.to_path_buf(), root.join(d)].iter().any(|d| d.join("fonts/manifest.txt").is_file())),
        },
    })
}

/// Backends wgpu can probe, in the order reports list them.
const BACKENDS: [&str; 4] = ["vulkan", "metal", "dx12", "gl"];

/// Run `gpu_probe` (the pipeline's device), then once per backend: the backends the pipeline may
/// use, or with `all` every backend in the build (`--hardware`; on Windows that loads the Vulkan
/// driver, which the app avoids by default, issue #136).
pub fn gpu(mut probe: impl FnMut(Option<&str>) -> Result<String, String>, all: bool) -> Value {
    let pipeline = match probe(None).and_then(|s| first_json(&s)) {
        Ok(v) => v.get("pipeline").cloned().unwrap_or(Value::Null),
        Err(e) => return json!({"error": e}),
    };
    let names = |key: &str| -> Vec<String> {
        pipeline[key].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default()
    };
    let (compiled, used) = (names("compiled"), names("backends"));
    let mut backends = Vec::new();
    for b in BACKENDS.iter().filter(|b| compiled.iter().any(|c| c == *b) && (all || used.iter().any(|u| u == *b))) {
        backends.push(match probe(Some(b)).and_then(|s| first_json(&s)) {
            Ok(v) => v,
            Err(e) => json!({"backend": b, "adapters": [], "error": e}),
        });
    }
    json!({"pipeline": pipeline, "backends": backends})
}

/// The JSON of the `gpu_probe: {…}` line in the probe's output (libtest prints around it).
fn first_json(s: &str) -> Result<Value, String> {
    let line = s.lines().find_map(|l| l.trim().strip_prefix("gpu_probe: ")).ok_or_else(|| format!("gpu_probe printed no result: {s:?}"))?;
    serde_json::from_str(line).map_err(|e| format!("gpu_probe: bad JSON ({e}): {line}"))
}

/// Backends with at least one hardware (not software) adapter: `--hardware` runs the GPU tests
/// on each.
pub fn hardware_backends(gpu: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for b in gpu["backends"].as_array().into_iter().flatten() {
        let Some(name) = b["backend"].as_str() else { continue };
        let hw = b["adapters"].as_array().into_iter().flatten().find(|a| a["type"].as_str().is_some_and(|t| t != "Cpu"));
        if let Some(a) = hw {
            out.push((name.to_owned(), a["name"].as_str().unwrap_or("?").to_owned()));
        }
    }
    out
}

/// One test that skipped, possibly in several steps.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Skip {
    pub need: String,
    pub reason: String,
    pub test: String,
    pub binary: String,
    pub location: String,
    pub steps: Vec<String>,
}

/// The skips of every `skips-<step>.jsonl` in `dir`, deduplicated (the HEIF step reruns some test
/// binaries), sorted by need, then reason. Unreadable lines are dropped.
pub fn read_skips(dir: &Path, root: &Path) -> Vec<Skip> {
    let mut by_key: BTreeMap<(String, String, String, String, String), Vec<String>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    files.sort();
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(step) = name.strip_prefix("skips-").and_then(|n| n.strip_suffix(".jsonl")) else { continue };
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
            let field = |k: &str| v[k].as_str().unwrap_or("").to_owned();
            let key = (field("need"), sanitize(&field("reason"), root), field("test"), field("binary"), field("location"));
            let steps = by_key.entry(key).or_default();
            if !steps.iter().any(|s| s == step) {
                steps.push(step.to_owned());
            }
        }
    }
    by_key.into_iter().map(|((need, reason, test, binary, location), steps)| Skip { need, reason, test, binary, location, steps }).collect()
}

/// Cut the workspace root and the home directory from `s`, and fold `dir/../` in paths.
pub fn sanitize(s: &str, root: &Path) -> String {
    let mut out = s.to_owned();
    let root = root.display().to_string();
    if !root.is_empty() {
        for sep in ['/', '\\'] {
            out = out.replace(&format!("{root}{sep}"), "");
        }
        out = out.replace(&root, ".");
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| h.to_string_lossy().into_owned());
    if let Some(home) = home.filter(|h| h.len() > 1) {
        out = out.replace(&home, "~");
    }
    out.split(' ').map(fold_parent_dirs).collect::<Vec<_>>().join(" ")
}

/// `crates/raw/../../corpus/raw` → `corpus/raw` (lexically; leading `..` stay).
fn fold_parent_dirs(token: &str) -> String {
    if !token.contains("/../") && !token.contains("\\..\\") {
        return token.to_owned();
    }
    let sep = if token.contains("/../") { '/' } else { '\\' };
    let mut parts: Vec<&str> = Vec::new();
    for part in token.split(sep) {
        if part == ".." && parts.last().is_some_and(|p| *p != ".." && !p.is_empty()) {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    parts.join(&sep.to_string())
}

pub fn skips_json(skips: &[Skip]) -> Value {
    Value::Array(
        skips
            .iter()
            .map(|s| json!({"need": s.need, "reason": s.reason, "test": s.test, "binary": s.binary, "location": s.location, "steps": s.steps}))
            .collect(),
    )
}

/// What an empty skip list means: no test skipped, or the tests never ran.
fn no_skips(report: &Value) -> &'static str {
    let test_ran = report["steps"].as_array().into_iter().flatten().any(|s| s["name"] == "test" && s["status"] != "not run");
    if test_ran { "none recorded" } else { "unknown, the test step did not run" }
}

fn skips_from(report: &Value) -> Vec<Skip> {
    report["skips"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let f = |k: &str| v[k].as_str().unwrap_or("").to_owned();
            let steps = v["steps"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
            Skip { need: f("need"), reason: f("reason"), test: f("test"), binary: f("binary"), location: f("location"), steps }
        })
        .collect()
}

/// Skip counts per need, most first: `[("corpus", 40), ("gpu", 5)]`.
pub fn counts_by_need(skips: &[Skip]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for s in skips {
        *counts.entry(&s.need).or_default() += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().map(|(k, n)| (k.to_owned(), n)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// What gets the skipped tests of `need` running.
fn hint(need: &str) -> &'static str {
    match need {
        "gpu" => "a hardware GPU adapter the pipeline accepts",
        "corpus" => "the corpus (`cargo xtask corpus --download`; some files are private)",
        "fonts" => "craft-fonts (`CRAFT_FONTS_DIR`, see the README)",
        "model" => "the model files the reasons name",
        "input" => "the input files the reasons name",
        "system" => "what the operating system provides",
        "feature" => "support this build doesn't have yet",
        _ => "what the reasons name",
    }
}

/// The skips of `need`, grouped by reason, largest group first.
fn by_reason<'a>(skips: &'a [Skip], need: &str) -> Vec<(&'a str, Vec<&'a Skip>)> {
    let mut groups: BTreeMap<&str, Vec<&Skip>> = BTreeMap::new();
    for k in skips.iter().filter(|k| k.need == need) {
        groups.entry(&k.reason).or_default().push(k);
    }
    let mut groups: Vec<(&str, Vec<&Skip>)> = groups.into_iter().collect();
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
    groups
}

pub fn seconds(s: f64) -> String {
    if s < 60.0 { format!("{s:.1} s") } else { format!("{}m {:02}s", (s / 60.0).floor() as u64, (s % 60.0).floor() as u64) }
}

/// `binary::test`, or the binary and the location when the test's name is unknown (thread
/// `main`). A crate's integration tests share one binary, `it`, named here after the crate's
/// directory: `gpu/it::equivalence::camera_tone`.
fn test_name(s: &Skip) -> String {
    let mut dirs = s.location.split('/');
    let binary = match (s.binary.as_str(), dirs.next(), dirs.next()) {
        ("it", Some("crates" | "apps"), Some(dir)) => format!("{dir}/it"),
        _ => s.binary.clone(),
    };
    if s.test.is_empty() || s.test == "main" { format!("{binary} ({})", s.location) } else { format!("{binary}::{}", s.test) }
}

fn adapter_line(a: &Value) -> String {
    let name = a["name"].as_str().unwrap_or("?");
    let kind = a["type"].as_str().unwrap_or("?");
    let driver = [a["driver"].as_str().unwrap_or(""), a["driver_info"].as_str().unwrap_or("")]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if driver.is_empty() { format!("{name} ({kind})") } else { format!("{name} ({kind}; {driver})") }
}

/// "the pipeline's device" line.
fn gpu_line(report: &Value) -> String {
    let gpu = &report["capabilities"]["gpu"];
    if let Some(e) = gpu["error"].as_str() {
        return format!("probe failed: {e}");
    }
    let p = &gpu["pipeline"];
    match (p["available"].as_bool(), p["adapter"].as_str(), p["reason"].as_str()) {
        (Some(true), Some(a), _) => a.to_owned(),
        (_, _, Some(r)) => format!("none ({r})"),
        _ => "not probed".to_owned(),
    }
}

fn adapters_lines(report: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for b in report["capabilities"]["gpu"]["backends"].as_array().into_iter().flatten() {
        let name = b["backend"].as_str().unwrap_or("?");
        let list: Vec<String> = b["adapters"].as_array().into_iter().flatten().map(adapter_line).collect();
        let text = match (b["error"].as_str(), list.is_empty()) {
            (Some(e), _) => format!("probe failed: {e}"),
            (None, true) => "no adapter".to_owned(),
            (None, false) => list.join(", "),
        };
        out.push(format!("{name}: {text}"));
    }
    out
}

fn machine_line(report: &Value) -> String {
    let m = &report["machine"];
    let mut s = format!("{} {}", m["os"].as_str().unwrap_or("?"), m["arch"].as_str().unwrap_or("?"));
    if let Some(v) = m["os_version"].as_str() {
        let _ = write!(s, " ({v})");
    }
    if let Some(n) = m["cpus"].as_u64() {
        let _ = write!(s, ", {n} CPUs");
    }
    if let Some(gb) = m["ram_total_gb"].as_u64() {
        let _ = write!(s, ", {gb} GB RAM");
    }
    if let Some(mb) = m["ram_available_mb"].as_u64() {
        let _ = write!(s, " ({:.1} GB available)", mb as f64 / 1024.0);
    }
    s
}

fn data_line(report: &Value) -> String {
    let d = &report["capabilities"]["data"];
    let c = &d["corpus"];
    let n = |k: &str| c[k].as_u64().unwrap_or(0);
    let fonts = match (d["craft_fonts"]["set"].as_bool(), d["craft_fonts"]["found"].as_bool()) {
        (Some(true), Some(true)) => "CRAFT_FONTS_DIR set",
        (Some(true), _) => "CRAFT_FONTS_DIR set but not found",
        _ => "no CRAFT_FONTS_DIR",
    };
    format!("corpus: {} of {} pinned raws, {} images, {} PngSuite files; {fonts}", n("raw_pinned"), n("raw_pinned_total"), n("images"), n("pngsuite"))
}

fn commit_line(report: &Value) -> String {
    let g = &report["git"];
    let commit = g["commit"].as_str().map_or("unknown commit".to_owned(), |c| c.chars().take(10).collect());
    let state = match g["dirty"].as_bool() {
        Some(true) => "with uncommitted changes",
        Some(false) => "clean",
        None => "state unknown",
    };
    format!("{commit} ({state})")
}

/// The lines printed after the CI summary.
pub fn summary_text(report: &Value) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "\nMachine: {}", machine_line(report));
    let _ = writeln!(s, "GPU for the pipeline: {}", gpu_line(report));
    for line in adapters_lines(report) {
        let _ = writeln!(s, "  adapters on {line}");
    }
    let _ = writeln!(s, "Data: {}", data_line(report));
    for h in report["hardware"].as_array().into_iter().flatten() {
        let _ = writeln!(
            s,
            "GPU tests on {} ({}): {}",
            h["backend"].as_str().unwrap_or("?"),
            h["adapter"].as_str().unwrap_or("?"),
            h["status"].as_str().unwrap_or("?")
        );
    }
    let skips = skips_from(report);
    if skips.is_empty() {
        let _ = writeln!(s, "Skipped tests: {}", no_skips(report));
    } else {
        let counts: Vec<String> = counts_by_need(&skips).iter().map(|(k, n)| format!("{k} {n}")).collect();
        let _ = writeln!(s, "Skipped tests: {} passed without running their checks ({}):", skips.len(), counts.join(", "));
        for (need, n) in counts_by_need(&skips) {
            let _ = writeln!(s, "  {need} ({n}): needs {}", hint(&need));
            let groups = by_reason(&skips, &need);
            const SHOWN: usize = 4;
            for (reason, tests) in groups.iter().take(SHOWN) {
                let first = tests.first().map(|t| test_name(t)).unwrap_or_default();
                let more = if tests.len() > 1 { ", …" } else { "" };
                let _ = writeln!(s, "    {:>3}× {reason} ({first}{more})", tests.len());
            }
            if groups.len() > SHOWN {
                let rest: usize = groups.iter().skip(SHOWN).map(|(_, t)| t.len()).sum();
                let _ = writeln!(s, "    {rest:>3}× {} other reasons (target/ci/report.json)", groups.len() - SHOWN);
            }
        }
    }
    s
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

/// A Markdown block for a PR description or comment.
pub fn summary_md(report: &Value) -> String {
    let mut s = String::new();
    let result = report["result"].as_str().unwrap_or("?");
    let command = report["command"].as_str().unwrap_or("cargo xtask ci");
    let _ = writeln!(s, "<!-- lightcraft ci report, schema {SCHEMA} -->");
    let _ = writeln!(s, "**`{command}`: {result}** at {}\n", commit_line(report));
    let _ = writeln!(s, "| | |\n|---|---|");
    let _ = writeln!(s, "| Machine | {} |", cell(&machine_line(report)));
    if let Some(t) = report["toolchain"].as_str() {
        let _ = writeln!(s, "| Rust | {} |", cell(t));
    }
    let _ = writeln!(s, "| GPU for the pipeline | {} |", cell(&gpu_line(report)));
    let adapters = adapters_lines(report);
    if !adapters.is_empty() {
        let _ = writeln!(s, "| Adapters | {} |", cell(&adapters.join("; ")));
    }
    let _ = writeln!(s, "| Data | {} |", cell(&data_line(report)));
    let _ = writeln!(s, "\n| Step | Result | Time |\n|---|---|---|");
    for step in report["steps"].as_array().into_iter().flatten() {
        let time = step["seconds"].as_f64().map(seconds).unwrap_or_default();
        let _ = writeln!(s, "| {} | {} | {time} |", cell(step["name"].as_str().unwrap_or("?")), step["status"].as_str().unwrap_or("?"));
    }
    for h in report["hardware"].as_array().into_iter().flatten() {
        let time = h["seconds"].as_f64().map(seconds).unwrap_or_default();
        let name = format!("GPU tests on {} ({})", h["backend"].as_str().unwrap_or("?"), h["adapter"].as_str().unwrap_or("?"));
        let _ = writeln!(s, "| {} | {} | {time} |", cell(&name), h["status"].as_str().unwrap_or("?"));
    }
    let skips = skips_from(report);
    if skips.is_empty() {
        let _ = writeln!(s, "\nSkipped tests: {}.", no_skips(report));
    } else {
        let counts: Vec<String> = counts_by_need(&skips).iter().map(|(k, n)| format!("{k} {n}")).collect();
        let _ = writeln!(s, "\n<details><summary>{} tests skipped ({})</summary>\n", skips.len(), counts.join(", "));
        let _ = writeln!(s, "| Need | Tests | Reason | For example |\n|---|---|---|---|");
        for (need, _) in counts_by_need(&skips) {
            let groups = by_reason(&skips, &need);
            const SHOWN: usize = 12;
            for (reason, tests) in groups.iter().take(SHOWN) {
                let first = tests.first().map(|t| test_name(t)).unwrap_or_default();
                let _ = writeln!(s, "| {} | {} | {} | `{}` |", cell(&need), tests.len(), cell(reason), cell(&first));
            }
            if groups.len() > SHOWN {
                let rest: usize = groups.iter().skip(SHOWN).map(|(_, t)| t.len()).sum();
                let _ = writeln!(s, "| {} | {rest} | {} other reasons | |", cell(&need), groups.len() - SHOWN);
            }
        }
        let _ = writeln!(s, "\n</details>");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xtask-ci-report-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn skips_are_read_per_step_deduplicated_and_cut_free_of_the_root() {
        let root = Path::new("/work/lightcraft");
        let dir = tmp("skips");
        let gpu = r#"{"need":"gpu","reason":"no GPU adapter","test":"equivalence::camera_tone","binary":"it","location":"crates/gpu/tests/it/equivalence.rs:22"}"#;
        let corpus = r#"{"need":"corpus","reason":"skip /work/lightcraft/crates/raw/../../corpus/raw absent","test":"corpus::decodes","binary":"it","location":"crates/raw/tests/it/corpus.rs:28"}"#;
        std::fs::write(dir.join("skips-heif.jsonl"), format!("{corpus}\n")).unwrap();
        std::fs::write(dir.join("skips-test.jsonl"), format!("{gpu}\n{corpus}\nnot json\n")).unwrap();
        std::fs::write(dir.join("other.jsonl"), format!("{gpu}\n")).unwrap();
        let skips = read_skips(&dir, root);
        assert_eq!(skips.len(), 2, "{skips:?}");
        assert_eq!(skips[0].need, "corpus");
        assert_eq!(skips[0].reason, "skip corpus/raw absent");
        assert_eq!(skips[0].steps, ["heif", "test"]);
        assert_eq!(skips[1].steps, ["test"]);
        assert_eq!(counts_by_need(&skips), [("corpus".to_owned(), 1), ("gpu".to_owned(), 1)]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn integration_test_skips_are_named_after_their_crate() {
        let skip = |test: &str, binary: &str, location: &str| Skip {
            need: "gpu".into(),
            reason: String::new(),
            test: test.into(),
            binary: binary.into(),
            location: location.into(),
            steps: Vec::new(),
        };
        assert_eq!(test_name(&skip("equivalence::camera_tone", "it", "crates/gpu/tests/it/equivalence.rs:22")), "gpu/it::equivalence::camera_tone");
        assert_eq!(
            test_name(&skip("main", "it", "apps/lightcraft-cli/tests/it/cli.rs:9")),
            "lightcraft-cli/it (apps/lightcraft-cli/tests/it/cli.rs:9)"
        );
        assert_eq!(test_name(&skip("memory", "memory", "crates/gpu/tests/memory.rs:12")), "memory::memory");
        assert_eq!(test_name(&skip("tests::a", "lightcraft_engine", "crates/engine/src/tests.rs:3")), "lightcraft_engine::tests::a");
    }

    #[test]
    fn prepare_clears_the_last_runs_files_only() {
        let root = tmp("prepare");
        let dir = dir(&root);
        std::fs::create_dir_all(&dir).unwrap();
        for f in ["skips-test.jsonl", "report.json", "summary.md", "keep.txt"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        prepare(&root).unwrap();
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, ["keep.txt"]);
        assert_eq!(skip_log(&root, "gpu:dx12").file_name().unwrap(), "skips-gpu-dx12.jsonl");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn parent_dirs_fold_in_path_tokens_only() {
        assert_eq!(fold_parent_dirs("crates/raw/../../corpus/raw"), "corpus/raw");
        assert_eq!(fold_parent_dirs("../corpus/raw"), "../corpus/raw");
        assert_eq!(fold_parent_dirs("C:\\w\\crates\\raw\\..\\..\\corpus"), "C:\\w\\corpus");
        assert_eq!(fold_parent_dirs("absent"), "absent");
    }

    #[test]
    fn the_probe_asks_the_pipelines_backends_or_all_with_hardware() {
        let pipeline = r#"{"pipeline":{"available":false,"adapter":null,"reason":"software adapter (llvmpipe) skipped","backends":["vulkan","dx12"],"compiled":["vulkan","dx12","gl"]}}"#;
        let asked = std::cell::RefCell::new(Vec::new());
        let probe = |b: Option<&str>| -> Result<String, String> {
            asked.borrow_mut().push(b.map(str::to_owned));
            Ok(match b {
                None => format!("running 1 test\ntest gpu_probe ... \ngpu_probe: {pipeline}\nok\n"),
                Some("vulkan") => r#"gpu_probe: {"backend":"vulkan","adapters":[{"name":"llvmpipe","type":"Cpu","driver":"llvmpipe","driver_info":"Mesa"}]}"#.to_owned(),
                Some("dx12") => r#"gpu_probe: {"backend":"dx12","adapters":[{"name":"Intel(R) UHD Graphics 620","type":"IntegratedGpu","driver":"","driver_info":"31.0.101"}]}"#.to_owned(),
                Some(_) => return Err("crashed".into()),
            })
        };
        let gpu_default = gpu(probe, false);
        assert_eq!(*asked.borrow(), [None, Some("vulkan".to_owned()), Some("dx12".to_owned())]);
        assert_eq!(hardware_backends(&gpu_default), [("dx12".to_owned(), "Intel(R) UHD Graphics 620".to_owned())]);
        asked.borrow_mut().clear();
        let gpu_all = gpu(probe, true);
        assert_eq!(asked.borrow().len(), 4, "gl too");
        assert_eq!(gpu_all["backends"][2]["error"], "crashed");

        let report = json!({
            "command": "cargo xtask ci",
            "result": "pass",
            "git": {"commit": "0123456789abcdef", "dirty": false},
            "machine": {"os": "windows", "arch": "x86_64", "cpus": 8, "ram_total_gb": 16},
            "capabilities": {"gpu": gpu_all, "data": {"corpus": {"raw_pinned": 3, "raw_pinned_total": 30}, "craft_fonts": {"set": false}}},
            "steps": [{"name": "fmt", "status": "ok", "seconds": 1.25}, {"name": "test", "status": "ok", "seconds": 125.0}],
            "skips": [{"need": "gpu", "reason": "no GPU | adapter", "test": "memory", "binary": "memory", "location": "x.rs:1", "steps": ["test"]}],
        });
        let text = summary_text(&report);
        assert!(text.contains("GPU for the pipeline: none (software adapter (llvmpipe) skipped)"), "{text}");
        assert!(text.contains("adapters on dx12: Intel(R) UHD Graphics 620 (IntegratedGpu; 31.0.101)"), "{text}");
        assert!(text.contains("adapters on gl: probe failed: crashed"), "{text}");
        assert!(text.contains("Skipped tests: 1 passed without running their checks (gpu 1)"), "{text}");
        assert!(text.contains("  gpu (1): needs a hardware GPU adapter"), "{text}");
        assert!(text.contains("      1× no GPU | adapter (memory::memory)"), "{text}");
        assert!(text.contains("corpus: 3 of 30 pinned raws"), "{text}");
        let md = summary_md(&report);
        assert!(md.contains("**`cargo xtask ci`: pass** at 0123456789 (clean)"), "{md}");
        assert!(md.contains("| test | ok | 2m 05s |"), "{md}");
        assert!(md.contains("| gpu | 1 | no GPU \\| adapter | `memory::memory` |"), "{md}");
        assert!(md.contains("1 tests skipped (gpu 1)"), "{md}");
    }

    #[test]
    fn no_skips_is_not_claimed_when_the_tests_never_ran() {
        let stopped = json!({"steps": [{"name": "clippy", "status": "fail"}, {"name": "test", "status": "not run"}], "skips": []});
        assert!(summary_text(&stopped).contains("Skipped tests: unknown, the test step did not run"));
        let ran = json!({"steps": [{"name": "test", "status": "ok"}], "skips": []});
        assert!(summary_md(&ran).contains("Skipped tests: none recorded."));
    }
}
