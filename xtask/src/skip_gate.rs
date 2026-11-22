//! `cargo xtask skips check|write`: the nightly skip gate (docs/ci.md → Nightly).
//!
//! A full `cargo xtask ci` run lists the tests that skipped for want of hardware or data in
//! `target/ci/report.json` (`ci_report.rs`). The nightly corpus job brings the corpus, craft-fonts
//! and the photocraft-corpus, so most of those skips should vanish; this gate fails the job when
//! the skips of any need (`gpu`, `corpus`, `fonts`, …) exceed the caps in `ci/skip-allowlist.toml`,
//! so a test that quietly started skipping (a renamed corpus file, a font the build lost) turns the
//! nightly red instead of passing unseen. A need missing from the allow-list allows none.
//!
//! `write` sets every cap to the report's counts: run it on a green nightly's report (the job
//! uploads `target/ci`) to tighten the caps, never to make a red run pass without reading why.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::ci_report;

/// The allow-list, relative to the workspace root.
pub const FILE: &str = "ci/skip-allowlist.toml";

const USAGE: &str = "usage: cargo xtask skips check|write [--report FILE] [--allow FILE]";

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let mut report = root.join("target").join("ci").join("report.json");
    let mut allow = root.join(FILE);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match *a {
            "--report" => report = root.join(it.next().ok_or("skips: --report needs a file")?),
            "--allow" => allow = root.join(it.next().ok_or("skips: --allow needs a file")?),
            other => return Err(format!("skips: unknown option `{other}`\n{USAGE}")),
        }
    }
    let text = std::fs::read_to_string(&report).map_err(|e| format!("{}: {e} (run `cargo xtask ci` first)", report.display()))?;
    let report: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", report.display()))?;
    let counts = counts(&report)?;
    match args.first().copied() {
        Some("check") => {
            let text = std::fs::read_to_string(&allow).map_err(|e| format!("{}: {e}", allow.display()))?;
            let caps = parse_caps(&text).map_err(|e| format!("{}: {e}", allow.display()))?;
            for (need, n) in &counts {
                println!("skips: {need}: {n} (allowed {})", caps.get(need).copied().unwrap_or(0));
            }
            let over = over_caps(&counts, &caps);
            if over.is_empty() {
                println!("skips: within {FILE}");
                return Ok(());
            }
            let list: Vec<String> = over.iter().map(|(need, n, cap)| format!("{need}: {n} > {cap}")).collect();
            Err(format!("skips: more tests skipped than {FILE} allows ({}); see target/ci/summary.md for which and why", list.join(", ")))
        }
        Some("write") => {
            std::fs::write(&allow, render_caps(&counts)).map_err(|e| format!("{}: {e}", allow.display()))?;
            println!("skips: wrote {} caps to {}", counts.len(), allow.display());
            Ok(())
        }
        _ => Err(USAGE.to_string()),
    }
}

/// Skips per need in `report`; an error when the test step did not run (no skips would then
/// mean nothing).
pub fn counts(report: &Value) -> Result<BTreeMap<String, usize>, String> {
    let ran = report["steps"].as_array().into_iter().flatten().any(|s| s["name"] == "test" && s["status"] != "not run");
    if !ran {
        return Err("skips: the report's test step did not run (a `--fast` or failed-early run); nothing to check".into());
    }
    let skips: Vec<ci_report::Skip> = report["skips"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| {
            let f = |k: &str| v[k].as_str().unwrap_or("").to_owned();
            ci_report::Skip { need: f("need"), reason: f("reason"), test: f("test"), binary: f("binary"), location: f("location"), steps: Vec::new() }
        })
        .collect();
    Ok(ci_report::counts_by_need(&skips).into_iter().collect())
}

/// The `[max]` table: need → skips allowed.
pub fn parse_caps(text: &str) -> Result<BTreeMap<String, usize>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{e}"))?;
    let Some(max) = table.get("max") else { return Ok(BTreeMap::new()) };
    let max = max.as_table().ok_or("`max` must be a table of need = count")?;
    let mut caps = BTreeMap::new();
    for (need, v) in max {
        let n = v.as_integer().and_then(|n| usize::try_from(n).ok()).ok_or_else(|| format!("max.{need} must be a count (0 or more)"))?;
        caps.insert(need.clone(), n);
    }
    Ok(caps)
}

/// The needs whose skips exceed their cap: (need, skips, cap). A need without a cap allows none.
pub fn over_caps(counts: &BTreeMap<String, usize>, caps: &BTreeMap<String, usize>) -> Vec<(String, usize, usize)> {
    counts
        .iter()
        .filter_map(|(need, &n)| {
            let cap = caps.get(need).copied().unwrap_or(0);
            (n > cap).then(|| (need.clone(), n, cap))
        })
        .collect()
}

fn render_caps(counts: &BTreeMap<String, usize>) -> String {
    let mut s = String::from(
        "# Skips the nightly corpus run may have, per need (docs/ci.md → Nightly). `cargo xtask skips check`\n\
         # fails when target/ci/report.json has more; a need not listed allows none.\n\
         # Written by `cargo xtask skips write` from a green nightly's report; lower caps by hand as\n\
         # tests stop skipping, never raise one without saying why in the commit.\n\n[max]\n",
    );
    for (need, n) in counts {
        s.push_str(&format!("{need} = {n}\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(skips: &[&str]) -> Value {
        json!({
            "steps": [{"name": "test", "status": "ok"}],
            "skips": skips.iter().enumerate().map(|(i, n)| json!({"need": n, "test": format!("t{i}"), "reason": "r"})).collect::<Vec<_>>(),
        })
    }

    #[test]
    fn more_skips_than_allowed_fail_and_unlisted_needs_allow_none() {
        let counts = counts(&report(&["gpu", "gpu", "gpu", "corpus", "fonts"])).unwrap();
        let caps = parse_caps("[max]\ngpu = 2\ncorpus = 5\n").unwrap();
        let over = over_caps(&counts, &caps);
        let needs: Vec<&str> = over.iter().map(|o| o.0.as_str()).collect();
        assert_eq!(needs, ["fonts", "gpu"]);
    }

    #[test]
    fn skips_within_caps_pass() {
        let c = counts(&report(&["gpu", "model"])).unwrap();
        assert!(over_caps(&c, &parse_caps("[max]\ngpu = 1\nmodel = 3\n").unwrap()).is_empty());
        assert!(over_caps(&counts(&report(&[])).unwrap(), &BTreeMap::new()).is_empty());
    }

    #[test]
    fn a_run_without_tests_is_not_a_pass() {
        let r = json!({"steps": [{"name": "test", "status": "not run"}], "skips": []});
        assert!(counts(&r).is_err());
        assert!(counts(&json!({})).is_err());
    }

    #[test]
    fn written_caps_read_back_and_bad_caps_are_errors() {
        let counts = counts(&report(&["gpu", "corpus", "corpus"])).unwrap();
        let caps = parse_caps(&render_caps(&counts)).unwrap();
        assert!(over_caps(&counts, &caps).is_empty());
        for bad in ["[max]\ngpu = -1\n", "[max]\ngpu = \"many\"\n", "max = 3\n", "not toml ="] {
            assert!(parse_caps(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_committed_allow_list_parses() {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(FILE)).unwrap();
        parse_caps(&text).unwrap();
    }
}
