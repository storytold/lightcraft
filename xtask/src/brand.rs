//! `brand.toml` for xtask: a small reader (the same TOML subset `crates/brand/build.rs` accepts), `{{key}}`
//! template rendering, and the two brand commands:
//!
//! - `cargo xtask brand check`: no product name (current or legacy) in any source file outside the allowlist;
//! - `cargo xtask brand test`: build the CLI with the throw-away brand `xtask/test-brand.toml` and check that its
//!   help, UI snapshot, MCP `serverInfo` and config/log folders use that brand only.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The throw-away brand the CI job builds with.
pub const TEST_BRAND: &str = "xtask/test-brand.toml";

/// Names of earlier identities that must not appear in source files (matched case-insensitively, as substrings).
const LEGACY_NAMES: &[&str] = &["LightCraft", "LIGHTCRAFT_", "ArtCraft", "storyteller"];

/// Files that legitimately carry a product name: the config itself, licences, attributions, history and plans.
/// A trailing `/` matches a folder; `*` any run of characters other than `/`; a pattern without `/` matches the
/// file name in any folder.
const ALLOWED: &[&str] = &[
    "brand.toml",
    "xtask/test-brand.toml",
    "ATTRIBUTION.md",
    "NOTICE",
    "LICENSE*",
    "CHANGELOG*",
    "docs/upstream-merge.md",
    "/PLAN*.md",
    "plan/",
    "xtask/src/brand.rs",
    "xtask/src/rename.rs",
    // upstream contributor credits (attribution)
    "contributors/",
];

/// `[legacy]` handling: files that must name a previous identity to migrate from it (settings folders, env vars,
/// preset extensions, …). Same pattern syntax as `ALLOWED`. Keep this list short and specific.
pub const LEGACY_FILES: &[&str] = &[
    // XMP namespace already written into users' sidecars
    "crates/meta/src/legacy.rs",
    "crates/meta/README.md",
    // preset format tags, old preset-pack folder word, old binary name in library locks
    "crates/engine/src/legacy.rs",
    // IndexedDB name holding users' browser libraries, cross-tab lock name
    "apps/web/src/legacy.rs",
];

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Str(String),
    List(Vec<String>),
}

/// A parsed brand file: `(section, key) → value`.
#[derive(Clone, Debug, Default)]
pub struct Brand {
    entries: BTreeMap<(String, String), Value>,
}

impl Brand {
    pub fn load(path: &Path) -> Result<Brand, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Brand::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The workspace brand: `$BRAND_FILE` (relative to the workspace root) or `brand.toml`.
    pub fn current() -> Result<Brand, String> {
        let file = std::env::var_os("BRAND_FILE").filter(|v| !v.is_empty()).map_or_else(|| PathBuf::from("brand.toml"), PathBuf::from);
        Brand::load(&crate::root().join(file))
    }

    pub fn parse(text: &str) -> Result<Brand, String> {
        let mut section = String::new();
        let mut entries = BTreeMap::new();
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let err = |m: &str| format!("line {}: {m}: {raw}", n + 1);
            if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = name.trim().to_string();
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| err("expected `key = value`"))?;
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(err("bad key"));
            }
            let value = value.trim();
            let value = if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                let mut items = Vec::new();
                let mut rest = inner.trim();
                while !rest.is_empty() {
                    let (s, tail) = string(rest).ok_or_else(|| err("bad list item"))?;
                    items.push(s);
                    rest = tail.trim_start();
                    rest = rest.strip_prefix(',').unwrap_or(rest).trim_start();
                }
                Value::List(items)
            } else {
                let (s, tail) = string(value).ok_or_else(|| err("expected a quoted string"))?;
                if !tail.trim().is_empty() {
                    return Err(err("trailing text"));
                }
                Value::Str(s)
            };
            if section.is_empty() {
                return Err(err("key outside a [section]"));
            }
            entries.insert((section.clone(), key.to_string()), value);
        }
        Ok(Brand { entries })
    }

    /// A string from `[product]`, `[identity]` or `[stable]` (the flattened groups, as in `crates/brand`).
    pub fn get(&self, key: &str) -> Option<&str> {
        ["product", "identity", "stable"].iter().find_map(|s| match self.entries.get(&(s.to_string(), key.to_string())) {
            Some(Value::Str(v)) => Some(v.as_str()),
            _ => None,
        })
    }

    pub fn req(&self, key: &str) -> Result<&str, String> {
        self.get(key).ok_or_else(|| format!("brand.toml: missing `{key}`"))
    }

    pub fn list(&self, section: &str, key: &str) -> Vec<String> {
        match self.entries.get(&(section.to_string(), key.to_string())) {
            Some(Value::List(l)) => l.clone(),
            Some(Value::Str(s)) => vec![s.clone()],
            None => Vec::new(),
        }
    }

    /// Template variables: every flattened string key, plus `app` (= `display_name`) for docs.
    pub fn vars(&self) -> BTreeMap<String, String> {
        let mut vars = BTreeMap::new();
        for ((section, key), value) in &self.entries {
            if let ("product" | "identity" | "stable", Value::Str(v)) = (section.as_str(), value) {
                vars.insert(key.clone(), v.clone());
            }
        }
        if let Some(name) = self.get("display_name") {
            vars.insert("app".into(), name.to_string());
        }
        vars
    }
}

/// A basic `"…"` string with `\"` and `\\` escapes, and the rest of the input after it.
fn string(s: &str) -> Option<(String, &str)> {
    let s = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &s[i + 1..])),
            '\\' => out.push(chars.next()?.1),
            c => out.push(c),
        }
    }
    None
}

/// Drops a `#` comment that is not inside a string.
fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_str => escaped = true,
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Replaces every `{{name}}` (name: lower-case letters, digits, `_`) with `vars[name]`; an unknown name is an
/// error. Other uses of braces are left alone.
pub fn render(template: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let name_len = after.find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')).unwrap_or(after.len());
        if name_len > 0 && after[name_len..].starts_with("}}") {
            let name = &after[..name_len];
            let value = vars.get(name).ok_or_else(|| {
                let line = template[..template.len() - rest.len() + start].matches('\n').count() + 1;
                format!("line {line}: unknown placeholder {{{{{name}}}}}")
            })?;
            out.push_str(value);
            rest = &after[name_len + 2..];
        } else {
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    Ok(out)
}

// ---- brand check --------------------------------------------------------------------------------

/// `*` matches any run of characters except `/`; a trailing `/` matches a folder and everything below it; a
/// leading `/` anchors at the root; a pattern without `/` matches the file name anywhere.
fn allowed_by(pat: &str, path: &str) -> bool {
    if let Some(dir) = pat.strip_suffix('/') {
        return path.starts_with(&format!("{dir}/"));
    }
    if let Some(anchored) = pat.strip_prefix('/') {
        return crate::assets::glob(anchored, path);
    }
    if pat.contains('/') {
        return crate::assets::glob(pat, path);
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    crate::assets::glob(pat, name)
}

pub fn is_allowed(path: &str) -> bool {
    ALLOWED.iter().chain(LEGACY_FILES).any(|p| allowed_by(p, path))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Match {
    /// Case-sensitive, not inside a longer word (`[A-Za-z0-9_]`).
    Word,
    /// Case-sensitive, not preceded by a word character (`NONAMEYET_LOG` matches `NONAMEYET`).
    Prefix,
    /// Case-insensitive substring.
    Anywhere,
}

/// The names `brand check` looks for: `(needle, how, what)`.
fn needles(brand: &Brand) -> Result<Vec<(String, Match, &'static str)>, String> {
    let mut n = vec![
        (brand.req("display_name")?.to_string(), Match::Word, "display_name"),
        (brand.req("binary")?.to_string(), Match::Word, "binary"),
        (brand.req("env_prefix")?.to_string(), Match::Prefix, "env_prefix"),
    ];
    for legacy in LEGACY_NAMES {
        n.push((legacy.to_string(), Match::Anywhere, "legacy name"));
    }
    // earlier identities recorded in brand.toml itself
    for name in brand.list("legacy", "settings_dirs") {
        if !n.iter().any(|(s, _, _)| s.eq_ignore_ascii_case(&name)) {
            n.push((name, Match::Word, "legacy settings_dir"));
        }
    }
    for prefix in brand.list("legacy", "env_prefixes") {
        if !LEGACY_NAMES.iter().any(|l| l.eq_ignore_ascii_case(&format!("{prefix}_"))) {
            n.push((prefix, Match::Prefix, "legacy env_prefix"));
        }
    }
    n.retain(|(s, _, _)| !s.is_empty());
    Ok(n)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Byte offsets where `needle` occurs in `line` under the rule `how`.
fn find(line: &str, needle: &str, how: Match) -> Vec<usize> {
    let (hay, needle) = match how {
        Match::Anywhere => (line.to_ascii_lowercase(), needle.to_ascii_lowercase()),
        _ => (line.to_string(), needle.to_string()),
    };
    let bytes = hay.as_bytes();
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(i) = hay[from..].find(&needle) {
        let at = from + i;
        let end = at + needle.len();
        let before_ok = at == 0 || !is_word(bytes[at - 1]);
        let after_ok = end >= bytes.len() || !is_word(bytes[end]);
        let ok = match how {
            Match::Anywhere => true,
            Match::Prefix => before_ok,
            Match::Word => before_ok && after_ok,
        };
        if ok {
            hits.push(at);
        }
        from = at + needle.len().max(1);
    }
    hits
}

/// Every hit in `text` as `line: found `needle` (what)`.
fn scan(text: &str, needles: &[(String, Match, &'static str)]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        for (needle, how, what) in needles {
            if !find(line, needle, *how).is_empty() {
                out.push((n + 1, format!("`{needle}` ({what})")));
            }
        }
    }
    out
}

/// Tracked files plus untracked files that are not ignored.
pub fn source_files(root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .current_dir(root)
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard"])
        .output()
        .map_err(|e| format!("git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(format!("git ls-files failed: {}", String::from_utf8_lossy(&out.stderr)));
    }
    let mut files: Vec<String> = out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect();
    files.sort();
    files.dedup();
    Ok(files)
}

pub fn check(root: &Path) -> Result<(), String> {
    let brand = Brand::load(&root.join("brand.toml"))?;
    let needles = needles(&brand)?;
    let mut problems = Vec::new();
    let mut scanned = 0usize;
    for path in source_files(root)? {
        if is_allowed(&path) {
            continue;
        }
        // deleted-but-tracked files and binaries are skipped
        let Ok(bytes) = std::fs::read(root.join(&path)) else { continue };
        if bytes.contains(&0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else { continue };
        scanned += 1;
        // the file name counts as well (e.g. packaging/windows/lightcraft.wxs)
        for (_, what) in scan(&path, &needles) {
            problems.push(format!("{path}: file name contains {what}"));
        }
        for (line, what) in scan(&text, &needles) {
            problems.push(format!("{path}:{line}: {what}"));
        }
    }
    if problems.is_empty() {
        println!("brand check: OK ({scanned} files, no product names outside brand.toml and the allowlist)");
        return Ok(());
    }
    const SHOW: usize = 400;
    for p in problems.iter().take(SHOW) {
        println!("{p}");
    }
    if problems.len() > SHOW {
        println!("… and {} more", problems.len() - SHOW);
    }
    let mut per_file: BTreeMap<&str, usize> = BTreeMap::new();
    for p in &problems {
        let file = p.split(':').next().unwrap_or(p);
        *per_file.entry(file).or_default() += 1;
    }
    Err(format!(
        "brand check: {} product name(s) in {} file(s). Use the `dac_brand` constants (code), `{{{{app}}}}` templates \
         (docs, packaging) or neutral wording; legacy handling goes in LEGACY_FILES in xtask/src/brand.rs",
        problems.len(),
        per_file.len()
    ))
}

// ---- brand test ---------------------------------------------------------------------------------

/// Builds `dac-cli` with `xtask/test-brand.toml` into `target/brand-test` and checks the brand reaches its help,
/// UI snapshot, MCP `serverInfo` and config/log folders, with no other product name anywhere.
pub fn test(root: &Path) -> Result<(), String> {
    let test_brand = Brand::load(&root.join(TEST_BRAND))?;
    let real = Brand::load(&root.join("brand.toml"))?;
    let target = root.join("target/brand-test");
    let mut c = crate::cargo();
    c.env("BRAND_FILE", TEST_BRAND).env("CARGO_TARGET_DIR", &target).args(["build", "-p", "dac-cli"]);
    crate::run(c, &format!("BRAND_FILE={TEST_BRAND} CARGO_TARGET_DIR=target/brand-test cargo build -p dac-cli"))?;
    let cli = target.join("debug").join(format!("app-cli{}", std::env::consts::EXE_SUFFIX));

    let display = test_brand.req("display_name")?;
    let cli_name = test_brand.req("cli_binary")?;
    let settings_dir = test_brand.req("settings_dir")?;
    // names that must not show up in the test build's output
    let mut foreign: Vec<String> = LEGACY_NAMES.iter().map(|s| s.to_string()).collect();
    for key in ["display_name", "binary", "cli_binary", "settings_dir", "env_prefix"] {
        if let Some(v) = real.get(key).filter(|v| !v.is_empty() && test_brand.get(key) != Some(v)) {
            foreign.push(v.to_string());
        }
    }
    let foreign_in = |what: &str, text: &str| -> Vec<String> {
        // the checkout's own path may contain a product name (e.g. a folder named after it)
        let lower = text.replace(&*root.to_string_lossy(), "<root>").to_ascii_lowercase();
        foreign.iter().filter(|f| lower.contains(&f.to_ascii_lowercase())).map(|f| format!("{what}: contains `{f}`")).collect()
    };

    let home = target.join("home");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    let isolate = |c: &mut Command| {
        for (var, sub) in [
            ("HOME", ""),
            ("XDG_CONFIG_HOME", ".config"),
            ("XDG_DATA_HOME", ".local/share"),
            ("XDG_CACHE_HOME", ".cache"),
            ("XDG_STATE_HOME", ".local/state"),
            ("APPDATA", "AppData/Roaming"),
            ("LOCALAPPDATA", "AppData/Local"),
            ("USERPROFILE", ""),
            ("TMPDIR", "tmp"),
        ] {
            let dir = home.join(sub);
            let _ = std::fs::create_dir_all(&dir);
            c.env(var, dir);
        }
        // a developer's own settings must not leak in
        for (k, _) in std::env::vars() {
            if k.starts_with("LIGHTCRAFT_") || real.get("env_prefix").is_some_and(|p| k.starts_with(&format!("{p}_"))) {
                c.env_remove(k);
            }
        }
        c.current_dir(&home);
    };

    let mut failures = Vec::new();
    let mut checked = Vec::new();

    // 1. --help
    let mut c = Command::new(&cli);
    c.arg("--help");
    isolate(&mut c);
    let out = c.output().map_err(|e| format!("{}: {e}", cli.display()))?;
    let help = String::from_utf8_lossy(&out.stdout).into_owned();
    for want in [cli_name, display] {
        if !help.contains(want) {
            failures.push(format!("app-cli --help: does not mention `{want}`"));
        }
    }
    failures.extend(foreign_in("app-cli --help", &help));
    checked.push("CLI --help names the test brand");

    // 2. headless UI snapshot: menu tree and UI state through the control channel
    let script = home.join("brand-script.jsonl");
    std::fs::write(&script, "{\"method\": \"ui.menu.tree\"}\n{\"method\": \"ui.inspect\"}\n").map_err(|e| format!("{}: {e}", script.display()))?;
    let mut c = Command::new(&cli);
    c.args(["snapshot", "--demo", "--size", "1280x800", "--script"]).arg(&script).arg("-o").arg(home.join("snapshot.png"));
    isolate(&mut c);
    let out = c.output().map_err(|e| format!("app-cli snapshot: {e}"))?;
    // control-protocol replies go to stderr (stdout is free for the PNG path), so both are checked
    let snap = format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::write(home.join("snapshot.txt"), &snap);
    if !out.status.success() {
        failures.push(format!("app-cli snapshot exited with {}: {}", out.status, String::from_utf8_lossy(&out.stderr).trim()));
    }
    if !snap.contains(display) {
        failures.push(format!("UI snapshot (ui.menu.tree + ui.inspect): `{display}` not found"));
    }
    failures.extend(foreign_in("UI snapshot", &snap));
    checked.push("headless UI snapshot (menu tree, UI state) shows the test brand");

    // 3. MCP serverInfo
    let mut c = Command::new(&cli);
    c.args(["mcp", "--demo"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    isolate(&mut c);
    let mut child = c.spawn().map_err(|e| format!("app-cli mcp: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"xtask","version":"0"}}}"#;
        let _ = writeln!(stdin, "{init}");
        // dropping stdin ends the session
    }
    let out = child.wait_with_output().map_err(|e| format!("app-cli mcp: {e}"))?;
    let mcp_out = String::from_utf8_lossy(&out.stdout).into_owned();
    let server = mcp_out
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .find_map(|v| v.pointer("/result/serverInfo/name").and_then(|n| n.as_str()).map(str::to_string));
    let want = test_brand.req("mcp_server")?;
    match server {
        Some(name) if name == want => {}
        Some(name) => failures.push(format!("MCP serverInfo.name is `{name}`, expected `{want}`")),
        None => failures.push(format!("MCP initialize: no serverInfo in reply: {}", mcp_out.trim())),
    }
    failures.extend(foreign_in("MCP initialize reply", &mcp_out));
    checked.push("MCP serverInfo.name is the test brand's mcp_server");

    // 4. what the runs wrote under the isolated home: brand paths only
    let mut written = Vec::new();
    walk(&home, &home, &mut written);
    let lower_settings = settings_dir.to_ascii_lowercase();
    let files_written = written.iter().any(|p| home.join(p).is_file() && !p.starts_with("brand-script") && !p.starts_with("snapshot"));
    if files_written && !written.iter().any(|p| p.to_ascii_lowercase().contains(&lower_settings)) {
        failures.push(format!("nothing was written under a `{settings_dir}` folder (settings/logs) in {}", home.display()));
    }
    for p in &written {
        failures.extend(foreign_in(&format!("written path {p}"), p));
    }
    if !files_written {
        println!("note: the CLI wrote no settings or log files in this run, so their location was not checked");
    }
    checked.push("settings and logs (if any are written) go under the test brand's settings_dir");

    println!("\nbrand test ({}):", test_brand.req("display_name")?);
    for c in &checked {
        println!("  checked: {c}");
    }
    println!("  files written under the isolated HOME:");
    for p in written.iter().filter(|p| !p.starts_with("tmp/") && !p.starts_with("brand-script") && !p.starts_with("snapshot")).take(30) {
        println!("    {p}");
    }
    if failures.is_empty() {
        println!("brand test: OK");
        Ok(())
    } else {
        for f in &failures {
            println!("  FAIL {f}");
        }
        Err(format!("brand test: {} failure(s)", failures.len()))
    }
}

/// Every file and folder below `dir`, relative to `base`, with `/` separators.
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if let Ok(rel) = p.strip_prefix(base) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if p.is_dir() {
            walk(base, &p, out);
        }
    }
}

pub fn run(root: &Path, args: &[&str]) -> Result<(), String> {
    match args.first().copied() {
        Some("check") => check(root),
        Some("test") => test(root),
        Some("vars") => {
            for (k, v) in Brand::current()?.vars() {
                println!("{k} = {v}");
            }
            Ok(())
        }
        _ => Err("usage: cargo xtask brand check|test|vars".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[product]
display_name = "Zzyzx Test"   # comment
binary = "zzyzx"
env_prefix = "ZZYZX"
tagline = "a # in a string"
[legacy]
settings_dirs = ["A", "B"]
"#;

    #[test]
    fn parses_subset() {
        let b = Brand::parse(SAMPLE).unwrap();
        assert_eq!(b.get("display_name"), Some("Zzyzx Test"));
        assert_eq!(b.get("tagline"), Some("a # in a string"));
        assert_eq!(b.list("legacy", "settings_dirs"), vec!["A", "B"]);
        assert!(Brand::parse("key = \"outside\"").is_err());
        assert!(Brand::parse("[p]\nkey = bare").is_err());
    }

    #[test]
    fn renders_placeholders() {
        let vars = Brand::parse(SAMPLE).unwrap().vars();
        assert_eq!(render("Exec={{binary}} %F; {{app}}", &vars).unwrap(), "Exec=zzyzx %F; Zzyzx Test");
        assert_eq!(render("format!(\"{{}}\") {{ x }}", &vars).unwrap(), "format!(\"{{}}\") {{ x }}");
        assert!(render("a\n{{nope}}", &vars).unwrap_err().contains("line 2"));
    }

    #[test]
    fn matching_rules() {
        assert_eq!(find("run zzyzx-cli now", "zzyzx", Match::Word), vec![4]);
        assert!(find("zzyzxy", "zzyzx", Match::Word).is_empty());
        assert_eq!(find("ZZYZX_LOG=1", "ZZYZX", Match::Prefix), vec![0]);
        assert!(find("MY_ZZYZX_LOG", "ZZYZX", Match::Prefix).is_empty());
        assert_eq!(find("use lightcraft_engine", "LightCraft", Match::Anywhere), vec![4]);
        assert!(find("Zzyzx test", "Zzyzx Test", Match::Word).is_empty());
    }

    #[test]
    fn allowlist() {
        assert!(is_allowed("brand.toml"));
        assert!(is_allowed("assets/ATTRIBUTION.md"));
        assert!(is_allowed("crates/segment/LICENSE-APACHE"));
        assert!(is_allowed("crates/fetch/NOTICE"));
        assert!(is_allowed("PLAN_phase_0.md"));
        assert!(!is_allowed("docs/PLAN_x.md"));
        assert!(is_allowed("plan/STATUS.md"));
        assert!(!is_allowed("planner.md"));
        assert!(!is_allowed("crates/engine/src/lib.rs"));
    }

    #[test]
    fn workspace_and_test_brands_parse() {
        let root = crate::root();
        for f in ["brand.toml", TEST_BRAND] {
            let b = Brand::load(&root.join(f)).unwrap();
            for key in ["display_name", "binary", "cli_binary", "env_prefix", "app_id", "settings_dir", "mcp_server"] {
                assert!(b.get(key).is_some_and(|v| !v.is_empty()), "{f}: {key}");
            }
        }
    }
}
