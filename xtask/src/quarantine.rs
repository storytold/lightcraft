//! Quarantined tests (`ci/quarantine.toml`): known-flaky tests that CI keeps running but does not gate on.
//!
//! - `cargo xtask quarantine check`: validate the file (also a `cargo xtask ci` step).
//! - `cargo xtask quarantine filter [--platform P]`: print the nextest filter that excludes the active entries.
//! - `cargo xtask quarantine test [--platform P]`: the CI test run. With cargo-nextest installed: the gating
//!   `cargo nextest run --workspace --profile ci` without the active entries, `cargo test --workspace --doc`, then
//!   each active entry on its own, whose failure prints a GitHub `::warning` instead of failing. Without nextest:
//!   plain `cargo test --workspace` (nothing excluded).
//!
//! An entry is active on its platforms until its `until` date; an expired entry is no longer excluded (it gates
//! again) and prints a warning. Every entry names a tracking issue. See docs/ci.md.

use std::path::Path;

pub const FILE: &str = "ci/quarantine.toml";
const PLATFORMS: [&str; 3] = ["linux", "macos", "windows"];

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// `<package>::<test path>`, e.g. `lcc-ui-egui::tests_dialog_fit::export_dialog_is_unchanged_in_a_tall_window`.
    pub name: String,
    /// Integration-test target (`tests/<binary>.rs`); `None` for the library's unit tests.
    pub binary: Option<String>,
    pub platforms: Vec<String>,
    /// `#N` (an issue in this repository) or a URL.
    pub issue: String,
    pub until: String,
    pub reason: String,
}

impl Entry {
    fn package(&self) -> &str {
        self.name.split_once("::").map_or("", |(p, _)| p)
    }
    fn test_path(&self) -> &str {
        self.name.split_once("::").map_or("", |(_, t)| t)
    }
    /// nextest filter expression matching exactly this test.
    pub fn filter(&self) -> String {
        let bin = match &self.binary {
            Some(b) => format!("{}::{b}", self.package()),
            None => self.package().to_string(),
        };
        format!("(binary_id(={bin}) & test(={}))", self.test_path())
    }
    /// The issue as a link: `#N` resolves against `$GITHUB_REPOSITORY` when CI sets it.
    pub fn issue_link(&self) -> String {
        match (self.issue.strip_prefix('#'), std::env::var("GITHUB_REPOSITORY")) {
            (Some(n), Ok(repo)) if !repo.is_empty() => format!("https://github.com/{repo}/issues/{n}"),
            _ => self.issue.clone(),
        }
    }
    pub fn expired(&self, today: &str) -> bool {
        self.until.as_str() < today
    }
    pub fn applies(&self, platform: &str) -> bool {
        self.platforms.iter().any(|p| p == platform)
    }
}

fn ident_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn date_ok(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|d| d.iter().all(u8::is_ascii_digit));
    let num = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<u32>().ok()).unwrap_or(0);
    b.len() == 10
        && digits(0..4)
        && digits(5..7)
        && digits(8..10)
        && b.get(4) == Some(&b'-')
        && b.get(7) == Some(&b'-')
        && (1..=12).contains(&num(5..7))
        && (1..=31).contains(&num(8..10))
}

/// Parse and validate the file's text; every problem is reported.
pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let table: toml::Table = text.parse().map_err(|e| format!("{FILE}: {e}"))?;
    let mut errors = Vec::new();
    for key in table.keys() {
        if key != "test" {
            errors.push(format!("unknown top-level key `{key}` (expected [[test]] entries)"));
        }
    }
    let empty = Vec::new();
    let tests = match table.get("test") {
        None => &empty,
        Some(toml::Value::Array(a)) => a,
        Some(_) => return Err(format!("{FILE}: `test` must be an array of tables ([[test]])")),
    };
    let mut entries = Vec::new();
    for (i, t) in tests.iter().enumerate() {
        let at = format!("[[test]] #{}", i + 1);
        let Some(t) = t.as_table() else {
            errors.push(format!("{at}: not a table"));
            continue;
        };
        for key in t.keys() {
            if !["name", "binary", "platforms", "issue", "until", "reason"].contains(&key.as_str()) {
                errors.push(format!("{at}: unknown key `{key}`"));
            }
        }
        let s = |k: &str| t.get(k).and_then(toml::Value::as_str).map(str::to_string);
        let name = s("name").unwrap_or_default();
        let at = if name.is_empty() { at } else { format!("{at} ({name})") };
        let ok_name = name.split_once("::").is_some_and(|(p, rest)| ident_ok(p) && !rest.is_empty() && rest.split("::").all(ident_ok));
        if !ok_name {
            errors.push(format!("{at}: `name` must be `<package>::<test path>` (letters, digits, _ and -)"));
        }
        let binary = s("binary");
        if t.contains_key("binary") && !binary.as_deref().is_some_and(ident_ok) {
            errors.push(format!("{at}: `binary` must be an integration-test target name"));
        }
        let platforms: Vec<String> = t
            .get("platforms")
            .and_then(toml::Value::as_array)
            .map(|a| a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
            .unwrap_or_default();
        if platforms.is_empty() || platforms.iter().any(|p| !PLATFORMS.contains(&p.as_str())) {
            errors.push(format!("{at}: `platforms` must be a non-empty list of {PLATFORMS:?}"));
        }
        let issue = s("issue").unwrap_or_default();
        let number = issue.strip_prefix('#').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        if !(number || issue.starts_with("https://")) || issue.chars().any(char::is_whitespace) {
            errors.push(format!("{at}: `issue` must be the tracking issue (`#N` in this repository, or its https:// URL)"));
        }
        let until = s("until").unwrap_or_default();
        if !date_ok(&until) {
            errors.push(format!("{at}: `until` must be a YYYY-MM-DD date"));
        }
        let reason = s("reason").unwrap_or_default();
        if reason.trim().is_empty() {
            errors.push(format!("{at}: `reason` is required"));
        }
        entries.push(Entry { name, binary, platforms, issue, until, reason });
    }
    for (i, e) in entries.iter().enumerate() {
        if entries.iter().take(i).any(|o| o.name == e.name && o.binary == e.binary) {
            errors.push(format!("duplicate entry `{}`", e.name));
        }
    }
    if errors.is_empty() { Ok(entries) } else { Err(format!("{FILE}:\n  {}", errors.join("\n  "))) }
}

pub fn load(root: &Path) -> Result<Vec<Entry>, String> {
    match std::fs::read_to_string(root.join(FILE)) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(format!("{FILE}: {e}")),
    }
}

/// Entries excluded from the gating run on `platform` (not expired), and the expired ones that apply there.
pub fn split<'a>(entries: &'a [Entry], platform: &str, today: &str) -> (Vec<&'a Entry>, Vec<&'a Entry>) {
    let here = entries.iter().filter(|e| e.applies(platform));
    let (expired, active): (Vec<_>, Vec<_>) = here.partition(|e| e.expired(today));
    (active, expired)
}

/// nextest `-E` expression for the gating run, or `None` when nothing is excluded.
pub fn gating_filter(active: &[&Entry]) -> Option<String> {
    if active.is_empty() {
        return None;
    }
    let any: Vec<String> = active.iter().map(|e| e.filter()).collect();
    Some(format!("not ({})", any.join(" | ")))
}

/// Civil date (UTC) for days since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

pub fn today() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    civil(i64::try_from(secs / 86_400).unwrap_or(0))
}

pub fn host_platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

fn platform_arg(rest: &[&str]) -> Result<String, String> {
    match rest.iter().position(|a| *a == "--platform") {
        None => Ok(host_platform().to_string()),
        Some(i) => match rest.get(i + 1) {
            Some(p) if PLATFORMS.contains(p) => Ok((*p).to_string()),
            _ => Err(format!("--platform takes one of {PLATFORMS:?}")),
        },
    }
}

fn warn(title: &str, msg: &str) {
    // GitHub turns this into an annotation; locally it is a readable line
    println!("::warning title={title}::{msg}");
}

pub fn nextest_installed() -> bool {
    crate::cargo().args(["nextest", "--version"]).output().is_ok_and(|o| o.status.success())
}

/// The CI test run (see the module docs).
pub fn ci_test(root: &Path, platform: &str) -> Result<(), String> {
    let entries = load(root)?;
    if !nextest_installed() {
        eprintln!("quarantine: cargo-nextest is not installed; running plain `cargo test --workspace` (no retries, nothing excluded)");
        let mut c = crate::cargo();
        c.args(["test", "--workspace"]);
        return crate::run(c, "cargo test --workspace");
    }
    let today = today();
    let (active, expired) = split(&entries, platform, &today);
    for e in &expired {
        warn(
            "Quarantine entry expired",
            &format!("{} expired on {} and gates CI again; fix it or renew the entry ({})", e.name, e.until, e.issue_link()),
        );
    }
    let mut c = crate::cargo();
    c.args(["nextest", "run", "--workspace", "--profile", "ci"]);
    let filter = gating_filter(&active);
    if let Some(f) = &filter {
        c.args(["-E", f]);
    }
    let what = match &filter {
        Some(f) => format!("cargo nextest run --workspace --profile ci -E '{f}'"),
        None => "cargo nextest run --workspace --profile ci".to_string(),
    };
    let gating = crate::run(c, &what);
    // nextest doesn't run doctests
    let mut c = crate::cargo();
    c.args(["test", "--workspace", "--doc"]);
    let doc = crate::run(c, "cargo test --workspace --doc");
    for e in &active {
        let mut c = crate::cargo();
        c.args(["nextest", "run", "--workspace", "--profile", "ci", "--no-tests", "warn", "-E", &e.filter()]);
        if crate::run(c, &format!("quarantined: {}", e.name)).is_err() {
            warn(
                "Quarantined test failed",
                &format!("{} failed on {platform} (quarantined until {}, {}): {}", e.name, e.until, e.issue_link(), e.reason),
            );
        }
    }
    gating?;
    doc
}

pub fn run(root: &Path, rest: &[&str]) -> Result<(), String> {
    match rest.first().copied() {
        Some("check") => {
            let n = load(root)?.len();
            println!("{FILE}: {n} entr{} ok", if n == 1 { "y" } else { "ies" });
            Ok(())
        }
        Some("filter") => {
            let entries = load(root)?;
            let (active, _) = split(&entries, &platform_arg(rest)?, &today());
            println!("{}", gating_filter(&active).unwrap_or_else(|| "all()".into()));
            Ok(())
        }
        Some("test") => ci_test(root, &platform_arg(rest)?),
        _ => Err("usage: cargo xtask quarantine check | filter [--platform P] | test [--platform P]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r##"
[[test]]
name = "lcc-ui-egui::tests_dialog_fit::export_dialog_is_unchanged_in_a_tall_window"
platforms = ["macos"]
issue = "https://github.com/o/r/issues/1"
until = "2026-10-27"
reason = "window metrics"

[[test]]
name = "lcc-ui-egui::export_hooks_run_off_the_ui_thread_with_an_activity_row"
binary = "plugin_worker"
platforms = ["windows", "linux"]
issue = "#2"
until = "2026-10-01"
reason = "timing"
"##;

    #[test]
    fn parses_and_builds_filters() {
        let e = parse(GOOD).unwrap();
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].filter(), "(binary_id(=lcc-ui-egui) & test(=tests_dialog_fit::export_dialog_is_unchanged_in_a_tall_window))");
        assert_eq!(e[1].filter(), "(binary_id(=lcc-ui-egui::plugin_worker) & test(=export_hooks_run_off_the_ui_thread_with_an_activity_row))");
    }

    #[test]
    fn platform_and_expiry_select_entries() {
        let e = parse(GOOD).unwrap();
        let (active, expired) = split(&e, "macos", "2026-10-13");
        assert_eq!((active.len(), expired.len()), (1, 0));
        assert!(gating_filter(&active).unwrap().starts_with("not ((binary_id(=lcc-ui-egui) &"));
        // the Windows entry expired on 2026-10-01: it gates again
        let (active, expired) = split(&e, "windows", "2026-10-13");
        assert_eq!((active.len(), expired.len()), (0, 1));
        assert_eq!(gating_filter(&active), None);
        // the last day still counts
        let (active, _) = split(&e, "macos", "2026-10-27");
        assert_eq!(active.len(), 1);
        let (active, expired) = split(&e, "macos", "2026-10-28");
        assert_eq!((active.len(), expired.len()), (0, 1));
    }

    #[test]
    fn bad_entries_fail() {
        for bad in [
            "[[test]]\nname = \"nocrate\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"beos\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nuntil=\"2026-01-01\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"soon\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-13-01\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"",
            "[[test]]\nname = \"a::b) | all()\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"\nextra=1",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"\n[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"https://x\"\nuntil=\"2026-01-01\"\nreason=\"r\"",
            "test = 3",
            "[[test]]\nname = \"a::b\"\nplatforms=[\"linux\"]\nissue=\"#x\"\nuntil=\"2026-01-01\"\nreason=\"r\"",
        ] {
            assert!(parse(bad).is_err(), "accepted: {bad}");
        }
        assert_eq!(parse("").unwrap(), Vec::new());
    }

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), "1970-01-01");
        assert_eq!(civil(20_739), "2026-10-13");
        assert_eq!(civil(11_016), "2000-02-29");
    }

    #[test]
    fn repository_file_is_valid() {
        load(&crate::root()).unwrap();
    }
}
