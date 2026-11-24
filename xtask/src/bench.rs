//! `cargo xtask bench [FILE] [--only P,P…] [--strict] [--threshold PCT] [--save-baseline]`: run the
//! performance probes in [`PROBES`] (render, Library grid at 100k, TIFF ZIP export, keyword tree, start-up at 100k, wheel-scroll at 100k), record
//! the results in `target/bench/history.jsonl` (one JSON object per run, with git revision, host and load
//! average), check them against the budgets in [`BUDGETS`] and compare with this machine's stored baseline
//! (`target/bench/baseline.json`, written by `--save-baseline`; the previous run on this host when there is
//! none).
//!
//! Comparisons use **process CPU time** where a probe reports it (it reflects the work done even on a busy
//! shared machine), wall-clock otherwise. Budgets are wall-clock: what a user waits for. `--strict` exits
//! non-zero when a metric regressed by more than the threshold (default 20 %) or an enforced budget is
//! breached by more than the same threshold (a noise allowance, see [`ceiling`]). A baseline from another host is never compared against. Where the platform reports no
//! process CPU time (Windows: the `cpu` column is 0), wall time is compared instead.
//!
//! Adding a probe or a budget is one line in [`PROBES`] / [`BUDGETS`]. A probe prints lines of the form
//! `label: N ms wall[, N ms cpu]` (or render_bench's `label: cpu: … | gpu: …`) on stdout or stderr.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Parse `render_bench` output lines such as
/// `loupe draft 1152×768 cold:  cpu: 379.5 ms wall, 139.5 ms cpu  |  gpu: 19.9 ms wall, 17.2 ms cpu`
/// or `export JPEG encode:  63.9 ms wall, 278.0 ms cpu` into `name/backend/{wall,cpu}` → ms.
pub fn parse(output: &str) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    for line in output.lines() {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if !rest.contains(" ms ") && !rest.trim_end().ends_with(" ms wall") {
            continue;
        }
        let name = name.trim();
        for seg in rest.split('|') {
            let seg = seg.trim();
            let (backend, body) = match seg.split_once(':') {
                Some((b, body)) if matches!(b.trim(), "cpu" | "gpu") => (b.trim(), body),
                _ => ("cpu", seg),
            };
            for part in body.split(',') {
                let toks: Vec<&str> = part.split_whitespace().collect();
                if let [v, "ms", kind] = toks[..]
                    && let Ok(v) = v.parse::<f64>()
                    && matches!(kind, "wall" | "cpu")
                {
                    out.insert(format!("{name}/{backend}/{kind}"), v);
                }
            }
        }
    }
    out
}

/// Metrics that got slower than `threshold` (fraction) vs `prev`: (key, prev, now). Pass both through
/// [`compared`] first, to look at CPU time where a row has it.
pub fn regressions(prev: &BTreeMap<String, f64>, now: &BTreeMap<String, f64>, threshold: f64) -> Vec<(String, f64, f64)> {
    now.iter().filter_map(|(k, &v)| prev.get(k).map(|&p| (k.clone(), p, v))).filter(|(_, p, v)| *p > 1.0 && *v > p * (1.0 + threshold)).collect()
}

/// The metrics compared between runs: CPU time where a row has it, wall-clock otherwise.
pub fn compared(m: &BTreeMap<String, f64>) -> BTreeMap<String, f64> {
    m.iter()
        .filter(|&(k, &v)| {
            (k.ends_with("/cpu") && v > 0.0) || k.strip_suffix("/wall").is_some_and(|base| !m.get(&format!("{base}/cpu")).is_some_and(|&c| c > 0.0))
        })
        .map(|(k, v)| (k.clone(), *v))
        .collect()
}

/// One probe: a cargo command run with `<PREFIX>_PROBES=1`; its metrics are keyed `name:label/backend/kind`.
pub struct Probe {
    pub name: &'static str,
    pub cargo: &'static [&'static str],
    /// Takes the optional FILE argument (a raw to render).
    pub takes_file: bool,
}

const fn probe(name: &'static str, cargo: &'static [&'static str]) -> Probe {
    Probe { name, cargo, takes_file: false }
}

/// The probes `bench` runs, in order. One line per probe.
pub const PROBES: &[Probe] = &[
    Probe { name: "render", cargo: &["run", "--release", "-q", "-p", "lcc-engine", "--example", "render_bench", "--"], takes_file: true },
    probe("grid", &["test", "--release", "-q", "-p", "lcc-ui-egui", "--lib", "tests_grid::scale_", "--", "--nocapture", "--test-threads=1"]),
    probe("tiff", &["test", "--release", "-q", "-p", "lcc-codecs", "--test", "it", "tiff_zip::bench", "--", "--nocapture"]),
    probe("keywords", &["test", "--release", "-q", "-p", "lcc-catalog", "--lib", "probe_keyword_tree_cost", "--", "--nocapture"]),
    probe("startup", &["run", "--release", "-q", "-p", "lcc-ui-egui", "--example", "startup_bench"]), // 100k-photo library (4.1)
    probe("wheel", &["test", "--release", "-q", "-p", "lcc-ui-egui", "--lib", "tests_viewperf::wheel_scroll_100k_frame", "--", "--nocapture"]), // 4.8
];

/// A wall-clock ceiling on one metric. `enforce: false` is reported but never fails `--strict`: a known miss
/// with its own plan item.
pub struct Budget {
    pub metric: &'static str,
    pub max_ms: f64,
    pub enforce: bool,
    pub why: &'static str,
}

const fn budget(metric: &'static str, max_ms: f64, why: &'static str) -> Budget {
    Budget { metric, max_ms, enforce: true, why }
}

const fn advisory(metric: &'static str, max_ms: f64, why: &'static str) -> Budget {
    Budget { metric, max_ms, enforce: false, why }
}

/// The budgets (docs/perf.md). One line per budget; a metric the run did not produce (no GPU, probe left out
/// with `--only`) is reported as not measured.
// Numbers in the notes: Windows, shared machine under heavy build load, 2026-11-24 (docs/perf.md). An
// advisory row is a known miss on the CPU fallback (or GPU NR) path; turn it into `budget` once it is met.
#[rustfmt::skip]
pub const BUDGETS: &[Budget] = &[
    budget("render:loupe draft highlights drag (warm)/gpu/wall", 16.0, "slider, draft preview (GPU 3.9)"),
    budget("render:loupe draft clarity drag (warm)/gpu/wall", 16.0, "slider, draft preview (GPU 4.1)"),
    advisory("render:loupe draft highlights drag (warm)/cpu/wall", 16.0, "slider, draft, CPU path (78)"),
    advisory("render:loupe draft clarity drag (warm)/cpu/wall", 16.0, "slider, draft, CPU path (51)"),
    advisory("render:loupe draft NR drag (warm)/gpu/wall", 16.0, "slider, draft NR, GPU (30)"),
    advisory("render:loupe draft NR drag (warm)/cpu/wall", 16.0, "slider, draft NR, CPU (33 in 4.5; 519 loaded)"),
    budget("render:loupe 1920×1280 exposure drag (warm)/gpu/wall", 60.0, "loupe @ 2.5 MP (GPU 13)"),
    advisory("render:loupe 1920×1280 exposure drag (warm)/cpu/wall", 60.0, "loupe @ 2.5 MP, CPU path (127)"),
    advisory("render:loupe 1920×1280 NR drag (warm)/gpu/wall", 60.0, "loupe @ 2.5 MP NR, GPU (255)"),
    advisory("render:loupe 1920×1280 NR drag (warm)/cpu/wall", 60.0, "loupe @ 2.5 MP NR, CPU (573)"),
    budget("grid:grid 100k | unchanged frame/cpu/wall", 16.0, "Library grid frame, 100k (13)"),
    advisory("grid:filter bar hidden | still  changes | wheel-scroll frame/cpu/wall", 16.0, "grid wheel-scroll, bar hidden (17.5)"),
    budget("grid:grid 100k | jump to photo 90k (select + settle)/cpu/wall", 1500.0, "grid far jump, 100k (971)"),
    budget("grid:grid 100k | filter rating >= 4 (command + next frame)/cpu/wall", 1000.0, "grid filter change, 100k (232)"),
    budget("tiff:tiff 24 MP 16-bit noise 1300 Deflate/cpu/wall", 3000.0, "TIFF ZIP export, 24 MP 16-bit (960)"),
    budget("keywords:keyword tree 58k (kept)/cpu/wall", 16.0, "Keyword List, kept tree (0.6)"),
    advisory("startup:startup library ready/cpu/wall", 3000.0, "start-up to a usable library, 100k photos (4.1; 1.0-8.2 s loaded)"),
    budget("wheel:wheel scroll 100k | frame median/cpu/wall", 16.0, "grid wheel-scroll frame, 100k (4.8)"),
    budget("keywords:keyword tree 58k (built with its index)/cpu/wall", 400.0, "Keyword List after an edit (198)"),
];

/// The noise allowance on a budget: a reading fails only above `max_ms * (1 + allowance)`, the same
/// fraction as the regression `--threshold` (default 20 %), so one noisy run just over a ceiling on a shared
/// machine does not fail `--strict`. A reading between the ceiling and the allowance is reported as over
/// (within noise). Negative or non-finite allowances count as 0.
pub fn ceiling(b: &Budget, allowance: f64) -> f64 {
    let a = if allowance.is_finite() { allowance.max(0.0) } else { 0.0 };
    b.max_ms * (1.0 + a)
}

/// Budgets breached in `now` beyond the noise allowance (see [`ceiling`]): (budget, measured ms).
pub fn breaches<'a>(now: &BTreeMap<String, f64>, budgets: &'a [Budget], allowance: f64) -> Vec<(&'a Budget, f64)> {
    budgets.iter().filter_map(|b| now.get(b.metric).filter(|&&v| v > ceiling(b, allowance)).map(|&v| (b, v))).collect()
}

fn load_avg() -> Option<f64> {
    let out = Command::new("uptime").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let tail = s.rsplit_once("load average")?.1;
    tail.trim_start_matches(['s', ':', ' ']).split([',', ' ']).find(|t| !t.is_empty())?.parse().ok()
}

fn host() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .or_else(|| Command::new("hostname").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()))
        .unwrap_or_default()
}

fn git_rev(root: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(root)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn metrics_of(v: &serde_json::Value) -> Option<BTreeMap<String, f64>> {
    serde_json::from_value(v.get("metrics")?.clone()).ok()
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let strict = args.contains(&"--strict");
    let save = args.contains(&"--save-baseline");
    let opt = |f: &str| args.iter().position(|a| *a == f).and_then(|i| args.get(i + 1)).copied();
    let threshold = opt("--threshold").and_then(|v| v.parse::<f64>().ok()).unwrap_or(20.0) / 100.0;
    let only: Option<Vec<&str>> = opt("--only").map(|v| v.split(',').map(str::trim).collect());
    if let Some(o) = &only
        && let Some(bad) = o.iter().find(|n| !PROBES.iter().any(|p| p.name == **n))
    {
        return Err(format!("bench: unknown probe `{bad}` (probes: {})", PROBES.iter().map(|p| p.name).collect::<Vec<_>>().join(", ")));
    }
    let file = args
        .iter()
        .enumerate()
        .find(|(i, a)| !a.starts_with("--") && !i.checked_sub(1).and_then(|j| args.get(j)).is_some_and(|p| matches!(*p, "--threshold" | "--only")))
        .map(|(_, s)| s.to_string());
    let file = file.or_else(|| {
        let default = root.join("corpus/raw/arw-sony-a7m3-compressed.arw");
        default.exists().then(|| default.to_string_lossy().to_string())
    });
    let mut metrics = BTreeMap::new();
    for p in PROBES.iter().filter(|p| only.as_ref().is_none_or(|o| o.contains(&p.name))) {
        let mut cmd = crate::cargo();
        cmd.args(p.cargo).env(crate::env_name("PROBES"), "1");
        let input = if p.takes_file { file.as_deref() } else { None };
        if let Some(f) = input {
            cmd.arg(f);
        }
        let shown = if p.takes_file { input.unwrap_or("(procedural 24 MP source)") } else { "" };
        eprintln!("$ cargo {} {shown}", p.cargo.join(" "));
        let out = cmd.output().map_err(|e| format!("bench {}: {e}", p.name))?;
        let (so, se) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        print!("{so}");
        for line in se.lines().filter(|l| !parse(l).is_empty()) {
            println!("{line}");
        }
        if !out.status.success() {
            return Err(format!("bench {} failed:\n{se}", p.name));
        }
        let got = parse(&format!("{so}\n{se}"));
        if got.is_empty() {
            return Err(format!("bench {}: no metrics parsed from its output\n{se}", p.name));
        }
        metrics.extend(got.into_iter().map(|(k, v)| (format!("{}:{k}", p.name), v)));
    }
    let dir = root.join("target/bench");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let hist = dir.join("history.jsonl");
    let base_path = dir.join("baseline.json");
    let host = host();
    let prev: Option<BTreeMap<String, f64>> = std::fs::read_to_string(&hist)
        .ok()
        .and_then(|s| s.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .filter(|v| v["host"].as_str() == Some(host.as_str()))
        .and_then(|v| metrics_of(&v));
    let baseline_json = std::fs::read_to_string(&base_path).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let same_host = baseline_json.as_ref().filter(|v| v["host"].as_str() == Some(host.as_str()));
    if let Some(v) = baseline_json.as_ref().filter(|_| same_host.is_none()) {
        eprintln!("bench: the baseline is from host `{}`, not this one: not compared", v["host"].as_str().unwrap_or("?"));
    }
    let baseline = same_host.and_then(metrics_of);
    let rec = serde_json::json!({
        "rev": git_rev(root),
        "host": host,
        "time": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
        "load": load_avg(),
        "input": file,
        "metrics": metrics,
    });
    let mut line = rec.to_string();
    line.push('\n');
    use std::io::Write;
    std::fs::OpenOptions::new().create(true).append(true).open(&hist).and_then(|mut f| f.write_all(line.as_bytes())).map_err(|e| e.to_string())?;
    eprintln!("bench: {} metrics recorded in {}", metrics.len(), hist.display());

    let mut failures = Vec::new();
    for b in BUDGETS {
        let status = match metrics.get(b.metric) {
            None => "not measured".to_string(),
            Some(&v) if v <= b.max_ms => format!("{v:.1} ms  ok"),
            Some(&v) if v <= ceiling(b, threshold) => format!("{v:.1} ms  over (within {:.0}% noise allowance)", threshold * 100.0),
            Some(&v) if b.enforce => format!("{v:.1} ms  OVER BUDGET"),
            Some(&v) => format!("{v:.1} ms  over (advisory)"),
        };
        eprintln!("budget ≤ {:>5.0} ms  {:<46} {status}", b.max_ms, b.why);
    }
    for (b, v) in breaches(&metrics, BUDGETS, threshold).into_iter().filter(|(b, _)| b.enforce) {
        failures.push(format!("{} over budget: {v:.1} ms > {:.0} ms + {:.0}% ({})", b.metric, b.max_ms, threshold * 100.0, b.why));
    }
    let against = match (baseline.clone(), prev) {
        (Some(b), _) => Some((b, "the baseline")),
        (None, Some(p)) => Some((p, "the previous run")),
        (None, None) => None,
    };
    match against {
        None => eprintln!("bench: no baseline or previous run on this host: nothing to compare"),
        Some((against, what)) => {
            let reg = regressions(&compared(&against), &compared(&metrics), threshold);
            if reg.is_empty() {
                eprintln!("bench: no regressions > {:.0}% vs {what}", threshold * 100.0);
            }
            for (k, p, v) in &reg {
                eprintln!("bench: REGRESSION vs {what} {k}: {p:.1} → {v:.1} ms (+{:.0}%)", (v / p - 1.0) * 100.0);
                failures.push(format!("{k} regressed {p:.1} → {v:.1} ms vs {what}"));
            }
        }
    }
    if save {
        let mut base = baseline.unwrap_or_default();
        base.extend(metrics);
        let v = serde_json::json!({ "host": host, "rev": git_rev(root), "metrics": base });
        std::fs::write(&base_path, serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        eprintln!("bench: baseline saved to {}", base_path.display());
    }
    if strict && !failures.is_empty() { Err(format!("{} failure(s):\n  {}", failures.len(), failures.join("\n  "))) } else { Ok(()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUT: &str = "gpu: Apple M4 Pro (Metal) (device + kernels: 322 ms)
gpu vs cpu, loupe 1920×1280 typical: max 1 LSB, mean 0.0001 LSB
loupe draft 1152×768 cold:             cpu: 379.5 ms wall, 139.5 ms cpu  |  gpu: 19.9 ms wall, 17.2 ms cpu
export JPEG encode:                    63.9 ms wall, 278.0 ms cpu
window 3072×2304 at (3328, 2048) of 9984×6656
window pan (new window, warm src):     cpu: 912.0 ms wall, 2210.5 ms cpu  |  gpu: 37.5 ms wall, 8.7 ms cpu
";

    #[test]
    fn parses_cpu_gpu_and_plain_lines() {
        let m = parse(OUT);
        assert_eq!(m["loupe draft 1152×768 cold/cpu/wall"], 379.5);
        assert_eq!(m["loupe draft 1152×768 cold/gpu/cpu"], 17.2);
        assert_eq!(m["export JPEG encode/cpu/cpu"], 278.0);
        // (a comma in the name is no column separator)
        assert_eq!(m["window pan (new window, warm src)/cpu/cpu"], 2210.5);
        assert_eq!(m["window pan (new window, warm src)/gpu/wall"], 37.5);
        assert_eq!(m.len(), 10, "{m:?}");
    }

    #[test]
    fn flags_only_cpu_time_regressions_over_threshold() {
        let prev = parse(OUT);
        let mut now = prev.clone();
        *now.get_mut("export JPEG encode/cpu/cpu").unwrap() = 400.0; // +44 %
        *now.get_mut("export JPEG encode/cpu/wall").unwrap() = 400.0; // wall ignored: the row has CPU time
        *now.get_mut("loupe draft 1152×768 cold/gpu/cpu").unwrap() = 18.0; // +5 %
        let r = regressions(&compared(&prev), &compared(&now), 0.2);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "export JPEG encode/cpu/cpu");
    }

    #[test]
    fn without_cpu_time_wall_time_is_compared() {
        let prev = parse(
            "loupe draft NR drag (warm):  cpu: 30.0 ms wall, 0.0 ms cpu
",
        );
        let now = parse(
            "loupe draft NR drag (warm):  cpu: 40.0 ms wall, 0.0 ms cpu
",
        );
        let r = regressions(&compared(&prev), &compared(&now), 0.2);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "loupe draft NR drag (warm)/cpu/wall");
    }

    #[test]
    fn probe_lines_parse_and_wall_only_rows_compare_on_wall() {
        let m = parse("grid 100k | wheel-scroll frame: 12.5 ms wall\ntiff 24 MP 16-bit noise 1300 Deflate: 900 ms wall, 40.1 MB\nnot a metric: 3 MB");
        assert_eq!(m["grid 100k | wheel-scroll frame/cpu/wall"], 12.5);
        assert_eq!(m["tiff 24 MP 16-bit noise 1300 Deflate/cpu/wall"], 900.0);
        assert_eq!(m.len(), 2, "{m:?}");
        let prev = m.clone();
        let mut now = m;
        *now.get_mut("grid 100k | wheel-scroll frame/cpu/wall").unwrap() = 20.0;
        let r = regressions(&compared(&prev), &compared(&now), 0.2);
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn breaches_report_only_metrics_over_their_ceiling() {
        let m: BTreeMap<String, f64> = [("a".to_string(), 20.0), ("b".to_string(), 10.0)].into();
        let budgets = [budget("a", 16.0, "x"), budget("b", 16.0, "y"), advisory("c", 1.0, "z")];
        let r = breaches(&m, &budgets, 0.0);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0.metric, "a");
    }

    #[test]
    fn a_reading_just_over_the_ceiling_is_within_the_noise_allowance() {
        let budgets = [budget("a", 16.0, "x")];
        let just_over: BTreeMap<String, f64> = [("a".to_string(), 17.5)].into();
        assert!(breaches(&just_over, &budgets, 0.2).is_empty());
        let well_over: BTreeMap<String, f64> = [("a".to_string(), 19.3)].into();
        assert_eq!(breaches(&well_over, &budgets, 0.2).len(), 1);
        assert_eq!(breaches(&just_over, &budgets, 0.0).len(), 1, "no allowance: the bare ceiling");
        for bad in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(ceiling(&budgets[0], bad), 16.0, "{bad}");
        }
    }

    #[test]
    fn every_budget_names_a_probe() {
        for b in BUDGETS {
            let name = b.metric.split_once(':').map(|(p, _)| p);
            assert!(PROBES.iter().any(|p| Some(p.name) == name), "{}", b.metric);
            assert!(b.max_ms > 0.0, "{}", b.metric);
        }
    }
}
