//! `cargo xtask bench [FILE] [--strict] [--threshold PCT]`: run the end-to-end render benchmark, record
//! the results in `target/bench/history.jsonl` (one JSON object per run, with git revision and load
//! average) and compare with the previous run.
//!
//! Comparisons use **process CPU time**, which reflects the work done even on a busy shared machine;
//! wall-clock is recorded for reference. `--strict` exits non-zero when a CPU metric regressed by more
//! than the threshold (default 20 %), or when a budgeted metric ([`BUDGETS`]) misses its budget by more
//! than the threshold. Where the platform reports no process CPU time (Windows: the `cpu` column is 0),
//! the regression check compares wall time instead.

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
        if !rest.contains(" ms ") {
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

/// Wall-time budgets (ms) of `render_bench` metrics (docs/perf.md; AGENTS.md: slider ≤ 16 ms draft,
/// loupe ≤ 60 ms). Lane 4.4 grows this into the full budget table.
pub const BUDGETS: &[(&str, f64)] = &[
    // a Luminance NR slider drag at draft size, CPU path (task 4.5)
    ("loupe draft NR drag (warm)/cpu/wall", 16.0),
    ("loupe draft highlights drag (warm)/cpu/wall", 16.0),
    ("loupe 1920×1280 NR drag (warm)/cpu/wall", 60.0),
];

/// Budgeted metrics over their budget by more than `threshold` (fraction): (key, budget, now).
pub fn over_budget(now: &BTreeMap<String, f64>, threshold: f64) -> Vec<(String, f64, f64)> {
    BUDGETS.iter().filter_map(|(k, b)| now.get(*k).map(|&v| (k.to_string(), *b, v))).filter(|(_, b, v)| *v > b * (1.0 + threshold)).collect()
}

/// CPU-time metrics that got slower than `threshold` (fraction) vs `prev`: (key, prev, now).
pub fn regressions(prev: &BTreeMap<String, f64>, now: &BTreeMap<String, f64>, threshold: f64) -> Vec<(String, f64, f64)> {
    now.iter()
        .filter(|(k, _)| k.ends_with("/cpu"))
        .filter_map(|(k, &v)| prev.get(k).map(|&p| (k.clone(), p, v)))
        // no process CPU time on this platform: compare wall time
        .map(|(k, p, v)| {
            if p == 0.0 && v == 0.0 {
                let w = format!("{}/wall", k.strip_suffix("/cpu").unwrap_or(&k));
                match (prev.get(&w), now.get(&w)) {
                    (Some(&pw), Some(&vw)) => (w, pw, vw),
                    _ => (k, p, v),
                }
            } else {
                (k, p, v)
            }
        })
        .filter(|(_, p, v)| *p > 1.0 && *v > p * (1.0 + threshold))
        .collect()
}

fn load_avg() -> Option<f64> {
    let out = Command::new("uptime").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let tail = s.rsplit_once("load average")?.1;
    tail.trim_start_matches(['s', ':', ' ']).split([',', ' ']).find(|t| !t.is_empty())?.parse().ok()
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

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let strict = args.contains(&"--strict");
    let threshold =
        args.iter().position(|a| *a == "--threshold").and_then(|i| args.get(i + 1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(20.0) / 100.0;
    let file = args.iter().find(|a| !a.starts_with("--") && a.parse::<f64>().is_err()).map(|s| s.to_string());
    let file = file.or_else(|| {
        let default = root.join("corpus/raw/arw-sony-a7m3-compressed.arw");
        default.exists().then(|| default.to_string_lossy().to_string())
    });
    let mut cmd = crate::cargo();
    cmd.args(["run", "--release", "-q", "-p", "lcc-engine", "--example", "render_bench", "--"]);
    if let Some(f) = &file {
        cmd.arg(f);
    }
    eprintln!("$ cargo run --release -p lcc-engine --example render_bench -- {}", file.as_deref().unwrap_or("(procedural 24 MP source)"));
    let out = cmd.output().map_err(|e| format!("render_bench: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    print!("{text}");
    if !out.status.success() {
        return Err(format!("render_bench failed:\n{}", String::from_utf8_lossy(&out.stderr)));
    }
    let metrics = parse(&text);
    if metrics.is_empty() {
        return Err("no metrics parsed from render_bench output".into());
    }
    let dir = root.join("target/bench");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let hist = dir.join("history.jsonl");
    let prev: Option<BTreeMap<String, f64>> = std::fs::read_to_string(&hist)
        .ok()
        .and_then(|s| s.lines().rev().find(|l| !l.trim().is_empty()).map(str::to_string))
        .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .and_then(|v| serde_json::from_value(v["metrics"].clone()).ok());
    let rec = serde_json::json!({
        "rev": git_rev(root),
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
    let over = over_budget(&metrics, threshold);
    for (k, b, v) in &over {
        eprintln!("bench: OVER BUDGET {k}: {v:.1} ms (budget {b:.0} ms)");
    }
    let reg = match &prev {
        Some(prev) => regressions(prev, &metrics, threshold),
        None => {
            eprintln!("bench: first run — nothing to compare");
            Vec::new()
        }
    };
    if prev.is_some() && reg.is_empty() {
        eprintln!("bench: no CPU-time regressions > {:.0}% vs previous run", threshold * 100.0);
    }
    for (k, p, v) in &reg {
        eprintln!("bench: REGRESSION {k}: {p:.1} → {v:.1} ms (+{:.0}%)", (v / p - 1.0) * 100.0);
    }
    if strict && !(reg.is_empty() && over.is_empty()) {
        Err(format!("{} CPU-time regression(s), {} budget miss(es)", reg.len(), over.len()))
    } else {
        Ok(())
    }
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
        *now.get_mut("export JPEG encode/cpu/wall").unwrap() = 400.0; // wall ignored
        *now.get_mut("loupe draft 1152×768 cold/gpu/cpu").unwrap() = 18.0; // +5 %
        let r = regressions(&prev, &now, 0.2);
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
        let r = regressions(&prev, &now, 0.2);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "loupe draft NR drag (warm)/cpu/wall");
    }

    #[test]
    fn the_cpu_nr_drag_has_a_budget() {
        let fast = parse(
            "loupe draft NR drag (warm):  cpu: 15.0 ms wall, 200.0 ms cpu
",
        );
        assert!(over_budget(&fast, 0.2).is_empty());
        let slow = parse(
            "loupe draft NR drag (warm):  cpu: 34.0 ms wall, 200.0 ms cpu
",
        );
        assert_eq!(over_budget(&slow, 0.2).len(), 1);
    }
}
