//! `cargo xtask fuzz`: time-boxed runs of the pure-Rust fuzz targets (stable Rust, no cargo-fuzz).
//!
//! Each target is a `fuzz_target` test built on `lcc-fuzzkit` in the crate that owns the format. A plain
//! `cargo test` runs it briefly; this driver runs it in release mode with `LCC_FUZZ_SECS` (time box),
//! `LCC_FUZZ_RNG` (a fresh mutation stream per run), `LCC_FUZZ_CORPUS` (the gitignored `corpus/`, when
//! present) and `LCC_FUZZ_CRASH_DIR` (`target/fuzz/crashes/<target>/`). Docs: `docs/testing-sil.md`.

use std::path::Path;
use std::process::Command;

/// (name, package, test selector): `--test fuzz_target` for integration tests, `--lib` + a filter for
/// targets that need crate-private seeds.
pub const TARGETS: &[(&str, &str, &[&str])] = &[
    ("raw", "lcc-raw", &["--test", "fuzz_target"]),
    ("tiff", "lcc-tiff", &["--test", "fuzz_target"]),
    ("codecs", "lcc-codecs", &["--test", "fuzz_target"]),
    ("meta", "lcc-meta", &["--test", "fuzz_target"]),
    ("develop", "lcc-develop", &["--test", "fuzz_target"]),
    ("lightroom", "lcc-engine", &["--lib", "tests_fuzz_lightroom::fuzz_target_lightroom"]),
    ("peer", "lcc-peer", &["--test", "fuzz_target"]),
];

pub struct Options {
    pub targets: Vec<&'static str>,
    pub secs: u64,
    pub rng: u64,
    pub list: bool,
}

pub fn parse(args: &[&str]) -> Result<Options, String> {
    let mut o = Options { targets: Vec::new(), secs: 60, rng: rng_from_clock(), list: false };
    let mut i = 0;
    while i < args.len() {
        let a = args.get(i).copied().unwrap_or("");
        match a {
            "--secs" | "--rng" => {
                let v = args.get(i + 1).ok_or(format!("fuzz: {a} needs a value"))?;
                let n: u64 = v.parse().map_err(|_| format!("fuzz: {a} takes a whole number, not `{v}`"))?;
                if a == "--secs" {
                    o.secs = n.max(1);
                } else {
                    o.rng = n;
                }
                i += 1;
            }
            "--list" => o.list = true,
            "all" => o.targets.extend(TARGETS.iter().map(|t| t.0)),
            s if !s.starts_with('-') => {
                let t = TARGETS.iter().find(|t| t.0 == s).ok_or_else(|| format!("fuzz: unknown target `{s}` (targets: {})", names()))?;
                o.targets.push(t.0);
            }
            other => return Err(format!("fuzz: unknown option `{other}`")),
        }
        i += 1;
    }
    if o.targets.is_empty() {
        o.targets.extend(TARGETS.iter().map(|t| t.0));
    }
    o.targets.dedup();
    Ok(o)
}

fn names() -> String {
    TARGETS.iter().map(|t| t.0).collect::<Vec<_>>().join(", ")
}

fn rng_from_clock() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(1)
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    let o = parse(args)?;
    if o.list {
        for (name, pkg, _) in TARGETS {
            println!("{name:<10} {pkg}");
        }
        return Ok(());
    }
    let corpus = root.join("corpus");
    let crashes = root.join("target").join("fuzz").join("crashes");
    let mut failed = Vec::new();
    for name in &o.targets {
        let Some((_, pkg, sel)) = TARGETS.iter().find(|t| t.0 == *name) else { continue };
        println!("fuzz: {name} ({pkg}) for {}s, LCC_FUZZ_RNG={}", o.secs, o.rng);
        let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
        c.current_dir(root).args(["test", "--release", "-p", pkg]).args(*sel).args(["--", "--nocapture", "--test-threads", "1"]);
        c.env("LCC_FUZZ_SECS", o.secs.to_string()).env("LCC_FUZZ_RNG", o.rng.to_string()).env("LCC_FUZZ_CRASH_DIR", crashes.join(name));
        if corpus.is_dir() {
            c.env("LCC_FUZZ_CORPUS", &corpus);
        }
        let ok = c.status().map(|s| s.success()).map_err(|e| format!("cargo test: {e}"))?;
        if !ok {
            failed.push(*name);
        }
    }
    if failed.is_empty() {
        println!("fuzz: all {} target(s) ran {}s each without a crash", o.targets.len(), o.secs);
        Ok(())
    } else {
        Err(format!(
            "fuzz: {} crashed; inputs in {} — replay with the target's test, then add each to that format's hostile-input table",
            failed.join(", "),
            crashes.display()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_select_targets_and_time() {
        let o = parse(&["raw", "peer", "--secs", "1800", "--rng", "7"]).unwrap();
        assert_eq!((o.targets, o.secs, o.rng), (vec!["raw", "peer"], 1800, 7));
        assert_eq!(parse(&[]).unwrap().targets.len(), TARGETS.len());
        assert!(parse(&["nope"]).is_err());
        assert!(parse(&["--secs", "x"]).is_err());
    }

    #[test]
    fn every_target_names_an_existing_harness() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        for (name, pkg, sel) in TARGETS {
            let dir = root.join("crates").join(pkg.trim_start_matches("lcc-"));
            let file =
                if sel.first() == Some(&"--lib") { format!("src/{}.rs", sel[1].split("::").next().unwrap()) } else { "tests/fuzz_target.rs".into() };
            assert!(dir.join(&file).is_file(), "fuzz target {name}: {} missing", dir.join(file).display());
        }
    }
}
