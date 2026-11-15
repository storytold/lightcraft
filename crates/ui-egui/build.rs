//! Compile the translated format strings into the `tr_format!` macro.
//!
//! `locales/<code>-formats.json` maps an English format string to its translation for one language.
//! Every English message becomes one `tr_format!` arm that picks the active language and formats
//! that language's translation with `format!` — so Rust checks every catalog's placeholders against
//! the call site's arguments (a wrong or forgotten placeholder is a compile error, and `{:.1}`,
//! `{:+}`… keep their meaning). The English source is the fallback arm. Adding a language means
//! adding its file (and its `language_table!` entry); this script picks the file up by name.
//!
//! A message a language lacks (or translates as an empty string) shows the English source: that
//! language has no arm for it, and a message no catalog has formats the English source through the
//! macro's last arm. `i18n::tests::translation_gaps_report` lists the gaps per language.
//!
//! And it joins What's New (`$OUT_DIR/whats-new.md`, read by `panels::dialogs::WHATS_NEW`): upstream's entries in
//! `docs/whats-new.md` plus ours, one file each in `docs/whats-new/` (`lcc_notes::join_whats_new`). The joined
//! text is never committed, so pull requests that each add an entry never touch the same file. A malformed entry
//! fails the build with its file name and what's wrong (`cargo xtask notes --check` says the same).
use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::PathBuf,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    // The directory itself, so a catalog added or removed reruns this script.
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=locales/fork");
    let out = PathBuf::from(env::var("OUT_DIR")?);
    fs::write(out.join("tr-formats.rs"), generate()?)?;
    fs::write(out.join("whats-new.md"), whats_new()?)?;
    Ok(())
}

/// What's New as the app shows it (see the top of this file).
fn whats_new() -> Result<String, Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let docs = manifest.parent().and_then(|crates| crates.parent()).ok_or("no workspace above the crate")?.join("docs");
    let (upstream, ours) = (docs.join("whats-new.md"), docs.join("whats-new"));
    // the folder itself too, so an entry added or removed reruns this script
    println!("cargo:rerun-if-changed={}", upstream.display());
    println!("cargo:rerun-if-changed={}", ours.display());
    let text = fs::read_to_string(&upstream).map_err(|e| format!("{}: {e}", upstream.display()))?;
    let entries = lcc_notes::read_entries(lcc_notes::Kind::WhatsNew, &ours).map_err(|errors| {
        // one per line on stderr, which cargo shows when the build fails (the returned error prints escaped)
        eprintln!("What's New entries to fix (docs/whats-new/README.md):");
        for e in &errors {
            eprintln!("  {e}");
        }
        format!("{} malformed What's New entr{} (listed above)", errors.len(), if errors.len() == 1 { "y" } else { "ies" })
    })?;
    Ok(lcc_notes::join_whats_new(&text, &entries))
}

/// `ja` → `Ja`, `zh-hans` → `ZhHans`: the `Locale` variant the `language_table!` entry declares.
/// The code must be a lowercase BCP-47-like tag (`^[a-z]+(-[a-z]+)*$`), so the variant is always a
/// valid Rust identifier.
fn variant_for(code: &str) -> Result<String, String> {
    let valid = !code.is_empty() && code.split('-').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()));
    if !valid {
        return Err(format!("locales/{code}-formats.json: the language code must match ^[a-z]+(-[a-z]+)*$"));
    }
    Ok(code.split('-').map(capitalize).collect())
}

fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn generate() -> Result<String, Box<dyn std::error::Error>> {
    let mut catalogs: Vec<(String, String, BTreeMap<String, String>)> = Vec::new();
    for entry in fs::read_dir("locales")? {
        let path = entry?.path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default().to_string();
        // `en` is the source language: it has no catalog, its formats are the source themselves.
        let Some(code) = name.strip_suffix("-formats.json").filter(|code| *code != "en") else { continue };
        println!("cargo:rerun-if-changed=locales/{name}");
        let variant = variant_for(code)?;
        let mut messages: BTreeMap<String, String> =
            serde_json::from_str(&fs::read_to_string(&path)?).map_err(|error| format!("locales/{name}: {error}"))?;
        messages.extend(fork_overlay(&name)?);
        messages.retain(|_, translated| !translated.is_empty());
        catalogs.push((code.to_string(), variant, messages));
    }
    catalogs.sort_by(|a, b| a.0.cmp(&b.0));

    let all: BTreeSet<&String> = catalogs.iter().flat_map(|(_, _, messages)| messages.keys()).collect();

    let mut source = String::from(concat!(
        "/// Translate a format string, checked by `format!` in every language.\n",
        "/// `build.rs` writes one arm per message from `locales/*-formats.json`.\n",
        "macro_rules! tr_format {\n",
    ));
    for english in all {
        let en = serde_json::to_string(english)?;
        let en_filled = serde_json::to_string(&brand_fill(english))?;
        source.push_str(&format!("    ({en} $(, $($args:tt)*)?) => {{\n        match $crate::i18n::language() {{\n"));
        for (_, variant, messages) in &catalogs {
            if let Some(translated) = messages.get(english) {
                let translated = serde_json::to_string(&brand_fill(translated))?;
                source.push_str(&format!("            $crate::i18n::Locale::{variant} => format!({translated} $(, $($args)*)?),\n"));
            }
        }
        source.push_str(&format!("            _ => format!({en_filled} $(, $($args)*)?),\n        }}\n    }};\n"));
    }
    source.push_str(concat!(
        "    ($english:literal $(, $($args:tt)*)?) => {\n",
        "        format!($english $(, $($args)*)?)\n",
        "    };\n",
        "}\n",
        "\n",
        "// `macro_rules!` is textual: the module re-export is what lets call sites say\n",
        "// `crate::i18n::tr_format!`.\n",
        "pub(crate) use tr_format;\n",
    ));
    Ok(source)
}

/// The brand placeholders (`{app}`, `{cli}`, `{env}`) are filled in here, at build time: they are not
/// `format!` arguments. The name is escaped for `format!` (`{` → `{{`).
fn brand_fill(text: &str) -> String {
    let esc = |s: &str| s.replace('{', "{{").replace('}', "}}");
    text.replace("{app}", &esc(lcc_brand::DISPLAY_NAME)).replace("{cli}", &esc(lcc_brand::CLI_BINARY)).replace("{env}", &esc(lcc_brand::ENV_PREFIX))
}

/// The fork's own messages for a catalog (`locales/fork/<name>`, fork-owned), over upstream's.
fn fork_overlay(name: &str) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let path = PathBuf::from("locales/fork").join(name);
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    println!("cargo:rerun-if-changed=locales/fork/{name}");
    Ok(serde_json::from_str(&fs::read_to_string(&path)?).map_err(|error| format!("locales/fork/{name}: {error}"))?)
}
