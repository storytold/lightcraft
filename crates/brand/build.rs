//! Reads `brand.toml` (workspace root, or `$BRAND_FILE`) and writes `brand.rs` with one `pub const` per key.
//!
//! Only the TOML subset `brand.toml` uses is understood: `[section]` headers, `key = "string"` and
//! `key = ["string", …]` (single line), and `#` comments. Anything else fails the build loudly.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=BRAND_FILE");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"));
    let path = match std::env::var_os("BRAND_FILE").filter(|v| !v.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            // relative paths are relative to the workspace root, like everything else in xtask
            if p.is_relative() { manifest.join("../..").join(p) } else { p }
        }
        None => manifest.join("../../brand.toml"),
    };
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read brand file {}: {e}", path.display()));
    let entries = parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));

    let mut out = String::new();
    for (section, key, value) in &entries {
        let name = format!("{}_{}", section, key).to_uppercase();
        let name = match section.as_str() {
            // the common groups are flattened: brand::DISPLAY_NAME, brand::APP_ID, brand::XMP_NAMESPACE_URI
            "product" | "identity" | "stable" => key.to_uppercase(),
            _ => name,
        };
        match value {
            Value::Str(s) => writeln!(out, "pub const {name}: &str = {s:?};").unwrap(),
            Value::List(l) => writeln!(out, "pub const {name}: &[&str] = &{l:?};").unwrap(),
        }
    }
    for required in REQUIRED {
        let (section, key) = required.split_once('.').unwrap();
        if !entries.iter().any(|(s, k, _)| s == section && k == key) {
            panic!("{}: missing required key `{required}`", path.display());
        }
    }
    let dest = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR")).join("brand.rs");
    std::fs::write(dest, out).expect("write brand.rs");
}

const REQUIRED: &[&str] = &[
    "product.display_name",
    "product.short_name",
    "product.tagline",
    "product.vendor",
    "product.homepage",
    "product.repository",
    "product.binary",
    "product.cli_binary",
    "product.env_prefix",
    "product.mcp_server",
    "product.icon_svg",
    "product.logo_svg",
    "identity.app_id",
    "identity.settings_dir",
    "identity.library_default",
    "identity.catalog_ext",
    "identity.preset_ext",
    "identity.url_scheme",
    "stable.xmp_namespace_uri",
    "stable.xmp_namespace_prefix",
    "stable.catalog_magic",
    "legacy.settings_dirs",
    "legacy.env_prefixes",
    "legacy.preset_exts",
    "legacy.catalog_exts",
    "legacy.app_ids",
];

enum Value {
    Str(String),
    List(Vec<String>),
}

fn parse(text: &str) -> Result<Vec<(String, String, Value)>, String> {
    let mut section = String::new();
    let mut out = Vec::new();
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
        out.push((section.clone(), key.to_string(), value));
    }
    Ok(out)
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
