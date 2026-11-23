//! Input schemas and argument repair for the generated `cmd_*` tools.
//!
//! A command's parameters are documented as a free-form string (`{id: albumId, ids?: [photoIds],
//! on?: bool}`), not a JSON schema. With nothing but `{"type": "object"}` to go on, MCP clients send
//! every top-level argument as a string: `{"id": "11"}`, `{"filter": "{\"album\": 8}"}`, which the
//! commands then reject (`missing album id`). So each generated tool lists the keys its doc names,
//! typed where the doc is unambiguous ([`schema`]), and [`repair_args`] turns a stringified number,
//! boolean, list or object back into JSON before the command sees it.

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

/// Most keys one doc lists; a longer doc is cut off there (bounds the schema a hostile backend can make).
const MAX_KEYS: usize = 64;

/// The top-level keys a params doc names, each with its description (`""` when the doc gives none):
/// `{id: albumId, ids?: [photoIds]} …` → `[("id", "albumId"), ("ids", "[photoIds]")]`. Alternatives
/// (`name | names`) give each key. Items that aren't `key` or `key: description` are skipped.
pub fn doc_keys(doc: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for item in top_level_items(doc) {
        let (head, desc) = match item.split_once(':') {
            Some((h, d)) => (h, d.trim()),
            None => (item, ""),
        };
        let names: Vec<&str> = head.split('|').map(|k| k.trim().trim_end_matches('?')).collect();
        if !names.iter().all(|k| is_identifier(k)) {
            continue;
        }
        for k in names {
            if out.len() < MAX_KEYS && !out.iter().any(|(o, _)| o == k) {
                out.push((k.to_string(), desc.to_string()));
            }
        }
    }
    out
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

/// The input schema of a generated command tool: the doc's keys as properties (described by the
/// doc's words for them; typed `boolean` / `string` where that is certain), any other key allowed.
pub fn schema(doc: &str) -> Value {
    let mut props = Map::new();
    for (key, desc) in doc_keys(doc) {
        let mut p = Map::new();
        if desc.starts_with("bool") {
            p.insert("type".into(), json!("boolean"));
        } else if is_text_key(&key) && !desc.starts_with('[') {
            p.insert("type".into(), json!("string"));
        }
        if !desc.is_empty() {
            p.insert("description".into(), json!(desc));
        }
        props.insert(key, Value::Object(p));
    }
    json!({"type": "object", "properties": props, "additionalProperties": true})
}

/// Undo a client's stringifying of top-level arguments: a string that is a JSON list or object
/// becomes that value, and one that is a JSON number, boolean or null does too unless the key holds
/// text ([`TEXT_KEYS`]). Anything else (real text, nested values) is left alone.
pub fn repair_args(args: &Value) -> Value {
    let Some(map) = args.as_object() else { return args.clone() };
    let repaired = map
        .iter()
        .map(|(k, v)| {
            let fixed = v.as_str().and_then(|s| serde_json::from_str::<Value>(s.trim()).ok()).filter(|parsed| match parsed {
                Value::Array(_) | Value::Object(_) => true,
                Value::Number(_) | Value::Bool(_) | Value::Null => !is_text_key(k),
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
        let fixed = repair_args(&json!({
            "id": "11", "on": "true", "ids": "[1, 2]", "filter": "{\"album\": 8}", "parent": "null",
            "name": "2008", "title": "true", "names": "[\"a\"]", "rename": "{date:%Y}", "note": "hello", "n": 3
        }));
        assert_eq!(
            fixed,
            json!({
                "id": 11, "on": true, "ids": [1, 2], "filter": {"album": 8}, "parent": null,
                "name": "2008", "title": "true", "names": ["a"], "rename": "{date:%Y}", "note": "hello", "n": 3
            })
        );
        assert_eq!(repair_args(&json!("x")), json!("x"));
    }
}
