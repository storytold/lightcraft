//! Input schemas and argument repair for the generated `cmd_*` tools.
//!
//! A command's parameters are documented as a free-form string (`{id: albumId, ids?: [photoIds],
//! on?: bool}`), not a JSON schema. With nothing but `{"type": "object"}` to go on, MCP clients send
//! every top-level argument as a string: `{"id": "11"}`, `{"filter": "{\"album\": 8}"}`, which the
//! commands then reject (`missing album id`). So each generated tool lists the keys its doc names,
//! typed where the doc is unambiguous ([`schema`]), and [`repair_args`] turns a stringified number,
//! boolean, list or object back into JSON before the command sees it, reading the command's own doc
//! to tell text from numbers (`album` is an album id in `library.import` but a name in
//! `library.autoImport`).

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value, json};

/// Keys whose values are text: never turned into numbers or booleans (an album named `2008`, a
/// title `true`), only into a list or object (`names`, `synonyms`), and typed `string` in the schema
/// unless the doc names a list there.
const TEXT_KEYS: &[&str] = &[
    "addText",
    "albumName",
    "altText",
    "camera",
    "caption",
    "city",
    "copyright",
    "copyrightUrl",
    "country",
    "creator",
    "destination",
    "dir",
    "extendedDescription",
    "group",
    "keyword",
    "location",
    "name",
    "naming",
    "path",
    "prefix",
    "previewPath",
    "removeText",
    "rename",
    "template",
    "time",
    "title",
    "usageTerms",
];

/// Words a doc starts a key's description with when the value is text (`photosAlbum?: top-level album
/// name`, `watermark?: text | {…}`): a stringified number or boolean stays text there.
const TEXT_DESCRIPTIONS: &[&str] = &["text", "name", "string", "top-level album name"];

/// Most keys one doc lists; a longer doc is cut off there (bounds the schema a hostile backend can make).
const MAX_KEYS: usize = 64;

/// The top-level keys a params doc names, each with its description (`""` when the doc gives none):
/// `{id: albumId, ids?: [photoIds]} …` → `[("id", "albumId"), ("ids", "[photoIds]")]`. Alternatives
/// (`name | names`, `longEdge?|percent? (default …)`) give each key; a note after the last one, without
/// a `:`, is their description. Items that aren't keys (`...shape params`, `align=true`) are skipped.
pub fn doc_keys(doc: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for item in top_level_items(doc) {
        let (head, desc) = match split_description(item) {
            Some((h, d)) => (h, Some(d.trim())),
            None => (item, None),
        };
        let Some(parts) = head.split('|').map(key_and_note).collect::<Option<Vec<_>>>() else { continue };
        // a note may follow only the last alternative (`a?|b? (note)`), and only when there is no `: description`
        let (last, rest) = parts.split_last().map_or((None, &[][..]), |(l, r)| (Some(l), r));
        if rest.iter().any(|(_, note)| !note.is_empty()) || (desc.is_some() && last.is_some_and(|(_, n)| !n.is_empty())) {
            continue;
        }
        let desc = desc.or(last.map(|(_, n)| *n)).unwrap_or("");
        for (k, _) in parts {
            if out.len() < MAX_KEYS && !out.iter().any(|(o, _)| o == k) {
                out.push((k.to_string(), desc.to_string()));
            }
        }
    }
    out
}

/// `key: description` split at the first `:` outside brackets, so a note that holds one
/// (`hdr? (HDR output: gain map JPEG …)`) isn't taken for the description.
fn split_description(item: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, ch) in item.char_indices() {
        match ch {
            '{' | '[' | '(' => depth = depth.saturating_add(1),
            '}' | ']' | ')' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => return Some((item.get(..i)?, item.get(i + 1..)?)),
            _ => {}
        }
    }
    None
}

/// `percent? (default …)` → `("percent", "(default …)")`; `None` when it doesn't start with a key or
/// the key runs straight into other text (`align=true`).
fn key_and_note(part: &str) -> Option<(&str, &str)> {
    let part = part.trim();
    let end = part.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(part.len());
    let (key, rest) = part.split_at(end);
    if !is_identifier(key) {
        return None;
    }
    let rest = rest.strip_prefix('?').unwrap_or(rest);
    (rest.is_empty() || rest.starts_with([' ', '(', '\t', '\n'])).then_some((key, rest.trim()))
}

/// The comma-separated items of the doc's leading `{…}` (nested brackets kept whole); none when the
/// doc doesn't start with `{` or never closes it.
fn top_level_items(doc: &str) -> Vec<&str> {
    let Some(body) = doc.strip_prefix('{') else { return Vec::new() };
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in body.char_indices() {
        match ch {
            '{' | '[' | '(' => depth = depth.saturating_add(1),
            '}' | ']' | ')' if depth > 0 => depth -= 1,
            '}' => {
                items.extend(body.get(start..i));
                return items.into_iter().map(str::trim).filter(|s| !s.is_empty()).collect();
            }
            ',' if depth == 0 => {
                items.extend(body.get(start..i));
                start = i + 1;
            }
            _ => {}
        }
    }
    Vec::new()
}

fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_text_key(key: &str) -> bool {
    TEXT_KEYS.contains(&key)
}

/// Whether the doc describes the value as text (see [`TEXT_DESCRIPTIONS`]).
fn is_text_description(desc: &str) -> bool {
    let d = desc.to_ascii_lowercase();
    TEXT_DESCRIPTIONS.iter().any(|w| d.starts_with(w))
}

/// The params docs of the generated tools, by tool name, as `tools/list` last reported them.
static DOCS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Keep `doc` as the params doc of tool `name` (for [`doc_of`]).
pub fn remember_doc(name: &str, doc: &str) {
    DOCS.lock().unwrap_or_else(PoisonError::into_inner).insert(name.to_string(), doc.to_string());
}

/// The params doc [`remember_doc`] kept for tool `name`.
pub fn doc_of(name: &str) -> Option<String> {
    DOCS.lock().unwrap_or_else(PoisonError::into_inner).get(name).cloned()
}

/// The input schema of a generated command tool: the doc's keys as properties (described by the
/// doc's words for them; typed `boolean` / `string` where that is certain), any other key allowed.
pub fn schema(doc: &str) -> Value {
    let mut props = Map::new();
    for (key, desc) in doc_keys(doc) {
        let mut p = Map::new();
        let plain_text = is_text_description(&desc) && !desc.contains(['|', '{', '[']);
        if desc.starts_with("bool") {
            p.insert("type".into(), json!("boolean"));
        } else if (is_text_key(&key) && !desc.starts_with('[')) || plain_text {
            p.insert("type".into(), json!("string"));
        }
        if !desc.is_empty() {
            p.insert("description".into(), json!(desc));
        }
        props.insert(key, Value::Object(p));
    }
    json!({"type": "object", "properties": props, "additionalProperties": true})
}

/// Undo a client's stringifying of top-level arguments, given the command's params `doc`: a string
/// that is a JSON list or object becomes that value, and one that is a JSON number, boolean or null
/// does too, unless the key holds text. A key holds text when it is one of [`TEXT_KEYS`] or the doc
/// describes it as text ([`TEXT_DESCRIPTIONS`]); `"null"` still clears a text key whose doc offers
/// `null`. Anything else (real text, nested values) is left alone.
pub fn repair_args(args: &Value, doc: &str) -> Value {
    let Some(map) = args.as_object() else { return args.clone() };
    let keys = doc_keys(doc);
    let repaired = map
        .iter()
        .map(|(k, v)| {
            let desc = keys.iter().find(|(key, _)| key == k).map_or("", |(_, d)| d.as_str());
            let text = is_text_key(k) || is_text_description(desc);
            let fixed = v.as_str().and_then(|s| serde_json::from_str::<Value>(s.trim()).ok()).filter(|parsed| match parsed {
                Value::Array(_) | Value::Object(_) => true,
                Value::Number(_) | Value::Bool(_) => !text,
                Value::Null => !text || (!is_text_key(k) && desc.contains("null")),
                Value::String(_) => false,
            });
            (k.clone(), fixed.unwrap_or_else(|| v.clone()))
        })
        .collect();
    Value::Object(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_keys_reads_the_leading_object() {
        let keys = doc_keys("{id: albumId, ids?: [photoIds], on?: bool (toggles), rules?: {a, b}} → {added}");
        let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["id", "ids", "on", "rules"]);
        assert_eq!(keys[1].1, "[photoIds]");
        assert_eq!(keys[2].1, "bool (toggles)");
    }

    #[test]
    fn doc_keys_handles_alternatives_and_odd_docs() {
        let names = |d: &str| doc_keys(d).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
        assert_eq!(names("{name | names, favorite?: bool}"), ["name", "names", "favorite"]);
        assert_eq!(names("{}"), Vec::<String>::new());
        assert_eq!(names("{...shape params, align=true, ok}"), ["ok"]);
        assert!(names("partial Filter: {text?}").is_empty(), "not a leading object");
        assert!(names("{unclosed, list").is_empty());
        assert!(names("{}}}]]))").is_empty());
        assert_eq!(names("{ä: x, b}"), ["b"], "non-ASCII keys are skipped, never sliced");
        let long = format!("{{{}}}", (0..500).map(|i| format!("k{i}")).collect::<Vec<_>>().join(", "));
        assert_eq!(names(&long).len(), MAX_KEYS);
    }

    #[test]
    fn schema_types_only_what_is_certain() {
        let s = schema("{name, names?: [name], on?: bool, id: albumId}");
        assert_eq!(s["properties"]["name"]["type"], "string");
        assert!(s["properties"]["names"].get("type").is_none());
        assert_eq!(s["properties"]["on"]["type"], "boolean");
        assert!(s["properties"]["id"].get("type").is_none());
        assert_eq!(s["properties"]["id"]["description"], "albumId");
    }

    #[test]
    fn repair_args_undoes_stringified_values() {
        let fixed = repair_args(
            &json!({
            "id": "11", "on": "true", "ids": "[1, 2]", "filter": "{\"album\": 8}", "parent": "null",
            "name": "2008", "title": "true", "names": "[\"a\"]", "rename": "{date:%Y}", "note": "hello", "n": 3
            }),
            "",
        );
        assert_eq!(
            fixed,
            json!({
                "id": 11, "on": true, "ids": [1, 2], "filter": {"album": 8}, "parent": null,
                "name": "2008", "title": "true", "names": ["a"], "rename": "{date:%Y}", "note": "hello", "n": 3
            })
        );
        assert_eq!(repair_args(&json!("x"), ""), json!("x"));
    }

    /// `app.export` writes its resize options as alternatives with a note and no `:` (CodeRabbit on #4677).
    #[test]
    fn doc_keys_reads_alternatives_with_a_note() {
        let doc = "{path, longEdge?|shortEdge?|width?|height?|megapixels?|percent? (default longEdge 3000), \
                   hdr? (HDR output for HDR edits: gain map JPEG), quality?: 0..100}";
        let keys = doc_keys(doc);
        let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["path", "longEdge", "shortEdge", "width", "height", "megapixels", "percent", "hdr", "quality"]);
        assert_eq!(keys[1].1, "(default longEdge 3000)");
        assert_eq!(keys[7].1, "(HDR output for HDR edits: gain map JPEG)");
        // a note in the middle, or a note and a `:` description, isn't a key list
        assert!(doc_keys("{a? (x)|b}").is_empty());
        assert!(doc_keys("{a? (x): y}").is_empty());
    }

    /// The command's doc decides text from numbers: `photosAlbum` and `watermark` text stays text,
    /// `watermark` objects still decode, and `album` is a name in one command and an id in another.
    #[test]
    fn repair_args_reads_the_commands_doc() {
        let export = "{photosAlbum?: top-level album name (made when missing), watermark?: text | {text?, size?}, longEdge?|percent? (default)}";
        let fixed = repair_args(&json!({"photosAlbum": "2008", "watermark": "2026", "longEdge": "1600"}), export);
        assert_eq!(fixed, json!({"photosAlbum": "2008", "watermark": "2026", "longEdge": 1600}));
        let fixed = repair_args(&json!({"watermark": "{\"text\": \"©\", \"size\": 3}"}), export);
        assert_eq!(fixed, json!({"watermark": {"text": "©", "size": 3}}));
        let s = schema(export);
        assert_eq!(s["properties"]["photosAlbum"]["type"], "string");
        assert!(s["properties"]["watermark"].get("type").is_none(), "text or an object");
        assert!(s["properties"]["percent"].is_object());

        let auto_import = "{on?: bool, album?: name | null}";
        assert_eq!(repair_args(&json!({"album": "2008"}), auto_import), json!({"album": "2008"}));
        assert_eq!(repair_args(&json!({"album": "null"}), auto_import), json!({"album": null}), "null clears it");
        let import = "{paths: [file or folder], album?: albumId}";
        assert_eq!(repair_args(&json!({"album": "11"}), import), json!({"album": 11}));
        assert_eq!(repair_args(&json!({"name": "null"}), "{name: name | null}"), json!({"name": "null"}), "a text key keeps its text");
    }
}
