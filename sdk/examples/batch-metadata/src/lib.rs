//! Example plug-in: batch metadata. Build: `cargo build --release --target wasm32-unknown-unknown`,
//! then install `target/wasm32-unknown-unknown/release/batch_metadata.wasm` from the Plug-in Manager
//! and grant it `catalog` (to read each photo) and `metadataWrite` (to write the result).
//!
//! - **Title / Caption from Template…** (menu command with a dialog): fills the title or the caption
//!   of every selected photo from a template such as `{name} · {date} · #{n}`, with a start number.
//!   Tokens: `{name}` (file name without extension), `{file}` (file name), `{date}` (capture date),
//!   `{year}`, `{camera}`, `{n}` (the sequence number). Unknown tokens are left as written.
//! - **Add Keywords…** (menu command with a dialog): adds comma-separated keywords to every
//!   selected photo, keeping the keywords it already has.
//!
//! Every write goes through `metadata.setStandard`, so the host checks the permission and the
//! field names, and the edit is one undoable catalog change like any other.

use lcc_plugin_sdk::serde_json::{Value, json};
use lcc_plugin_sdk::{Request, export_plugin, host};

const MANIFEST: &str = r#"{
  "id": "org.example.batch-metadata",
  "name": "Batch Metadata (example)",
  "version": "0.1.0",
  "description": "Titles and captions from a template, and keywords, for many photos at once",
  "author": "the app's authors",
  "permissions": {"catalog": true, "metadataWrite": true},
  "commands": [
    {"id": "template", "label": "Title / Caption from Template…", "menu": "Batch Metadata",
     "dialog": [
       {"key": "field", "label": "Field", "type": "choice", "options": ["title", "caption"], "default": "title"},
       {"key": "template", "label": "Template", "type": "text", "default": "{name} · {date}"},
       {"key": "start", "label": "Start number", "type": "number", "min": 0, "max": 1000000, "default": 1}
     ]},
    {"id": "keywords", "label": "Add Keywords…", "menu": "Batch Metadata",
     "dialog": [{"key": "keywords", "label": "Keywords (comma-separated)", "type": "text", "default": ""}]}
  ]
}"#;

/// Most photos one call changes (the host also caps the work a call may do).
const MAX_PHOTOS: usize = 5000;

fn handle(req: &Request) -> Result<Value, String> {
    match (req.hook(), req.command()) {
        ("command", "template") => {
            let field = match req.arg("field").as_str().unwrap_or("title") {
                f @ ("title" | "caption") => f,
                other => return Err(format!("field must be title or caption, not {other:?}")),
            };
            let template = req.arg("template").as_str().unwrap_or("{name}").to_string();
            let start = req.arg("start").as_f64().filter(|v| v.is_finite() && *v >= 0.0).unwrap_or(1.0) as u64;
            let ids = req.selection();
            if ids.is_empty() {
                return Err("select the photos to change first".into());
            }
            let mut changed = 0usize;
            for (i, id) in ids.iter().take(MAX_PHOTOS).enumerate() {
                let photo = host::call("photo.inspect", json!({"id": id}))?;
                let text = expand(&template, &photo, start.saturating_add(i as u64));
                host::call("metadata.setStandard", json!({"ids": [id], field: text}))?;
                changed += 1;
            }
            host::log(&format!("set the {field} of {changed} photos"));
            Ok(json!({"message": format!("{field} set on {changed} photos"), "changed": changed}))
        }
        ("command", "keywords") => {
            let words = split_keywords(req.arg("keywords").as_str().unwrap_or_default());
            if words.is_empty() {
                return Err("type at least one keyword".into());
            }
            let ids: Vec<String> = req.selection().into_iter().take(MAX_PHOTOS).collect();
            if ids.is_empty() {
                return Err("select the photos to change first".into());
            }
            host::call("metadata.setStandard", json!({"ids": ids, "addKeywords": words}))?;
            Ok(json!({"message": format!("{} keywords added to {} photos", words.len(), ids.len()), "changed": ids.len()}))
        }
        ("command", other) => Err(format!("unknown command {other}")),
        (other, _) => Err(format!("unsupported hook {other:?}")),
    }
}

/// Fills `{token}`s from a `photo.inspect` result; `n` is the sequence number.
fn expand(template: &str, photo: &Value, n: u64) -> String {
    let file = photo.get("file_name").and_then(Value::as_str).unwrap_or_default();
    let name = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let captured = photo.get("captured").and_then(Value::as_str).unwrap_or_default();
    let date = captured.split('T').next().unwrap_or_default();
    let year = date.split('-').next().unwrap_or_default();
    let camera = photo.get("meta").and_then(|m| m.get("camera")).and_then(Value::as_str).unwrap_or_default();
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return out;
        };
        let token = &after[..close];
        match token {
            "name" => out.push_str(name),
            "file" => out.push_str(file),
            "date" => out.push_str(date),
            "year" => out.push_str(year),
            "camera" => out.push_str(camera),
            "n" => out.push_str(&n.to_string()),
            _ => {
                out.push('{');
                out.push_str(token);
                out.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Trimmed, non-empty, de-duplicated keywords from `a, b ,c`.
fn split_keywords(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in text.split([',', ';']).map(str::trim).filter(|w| !w.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(w)) {
            out.push(w.to_string());
        }
    }
    out
}

export_plugin!(MANIFEST, handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_expand_from_the_photo() {
        let photo = json!({"file_name": "IMG_0042.CR3", "captured": "2026-09-20T05:40:00", "meta": {"camera": "X2"}});
        assert_eq!(expand("{name} · {date} · {year} · {camera} · #{n}", &photo, 7), "IMG_0042 · 2026-09-20 · 2026 · X2 · #7");
        assert_eq!(expand("{file} {unknown} {", &photo, 1), "IMG_0042.CR3 {unknown} {");
        assert_eq!(expand("plain", &json!({}), 1), "plain");
        assert_eq!(expand("{name}{date}", &json!({}), 1), "");
    }

    #[test]
    fn keywords_are_split_and_deduplicated() {
        assert_eq!(split_keywords(" beach, Sunset ;beach,, sunset "), vec!["beach", "Sunset"]);
        assert!(split_keywords(" , ").is_empty());
    }
}
