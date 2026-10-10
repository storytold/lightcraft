//! The Keyword List's library-wide keyword attributes: synonyms, whether a keyword (its parents,
//! its synonyms) is written into exported files, person keywords, keywords created before any
//! photo has them, and keyword list text files (one keyword per line, children indented by a tab,
//! `[name]` = not exported, `{synonym}` lines under their keyword — the common plain-text keyword
//! list format).

use std::collections::BTreeMap;

use dac_catalog::keywords::{clean, is_under};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, bool_or, cmd, str_param};
use crate::{Result, Session};

/// What the library knows about a keyword beyond the photos that have it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct KeywordAttrs {
    /// Other words for it (written along on export, matched by search).
    pub synonyms: Vec<String>,
    /// Left out of exported files.
    pub exclude_on_export: bool,
    /// Its parents are not written along on export.
    pub no_parents_on_export: bool,
    /// Its synonyms are not written along on export.
    pub no_synonyms_on_export: bool,
    /// A person's name (People-style keyword).
    pub person: bool,
}

/// Limits for keyword list files (hostile input stays bounded).
const MAX_FILE: u64 = 8 << 20;
const MAX_LINES: usize = 100_000;
const MAX_DEPTH: usize = 32;
const MAX_SYNONYMS: usize = 64;

/// The attributes of `keyword` (case-insensitive), if any were set.
pub fn attrs<'a>(map: &'a BTreeMap<String, KeywordAttrs>, keyword: &str) -> Option<&'a KeywordAttrs> {
    map.iter().find(|(k, _)| k.eq_ignore_ascii_case(keyword)).map(|(_, v)| v)
}

fn entry<'a>(map: &'a mut BTreeMap<String, KeywordAttrs>, keyword: &str) -> &'a mut KeywordAttrs {
    let key = map.keys().find(|k| k.eq_ignore_ascii_case(keyword)).cloned().unwrap_or_else(|| keyword.to_string());
    map.entry(key).or_default()
}

/// The keywords to write into an exported file for a photo's `keywords`: excluded ones are left
/// out, a keyword whose parents aren't exported is written by its own name, synonyms are added.
/// Keywords without attributes are written as they are.
pub fn export_keywords(map: &BTreeMap<String, KeywordAttrs>, keywords: &[String]) -> Vec<String> {
    if map.is_empty() {
        return keywords.to_vec();
    }
    let mut out: Vec<String> = Vec::new();
    let push = |k: String, out: &mut Vec<String>| {
        if !k.is_empty() && !out.iter().any(|x| x.eq_ignore_ascii_case(&k)) {
            out.push(k);
        }
    };
    for k in keywords {
        let a = attrs(map, k);
        if a.is_some_and(|a| a.exclude_on_export) {
            continue;
        }
        let leaf = k.rsplit('|').next().unwrap_or(k).to_string();
        // parents an excluded ancestor hides are dropped from the path
        let written = if a.is_some_and(|a| a.no_parents_on_export) {
            leaf
        } else {
            let mut kept: Vec<&str> = Vec::new();
            let mut prefix = String::new();
            for part in k.split('|') {
                if !prefix.is_empty() {
                    prefix.push('|');
                }
                prefix.push_str(part);
                let hidden = !prefix.eq_ignore_ascii_case(k) && attrs(map, &prefix).is_some_and(|x| x.exclude_on_export);
                if !hidden {
                    kept.push(part);
                }
            }
            kept.join("|")
        };
        push(written, &mut out);
        if let Some(a) = a.filter(|a| !a.no_synonyms_on_export) {
            for s in &a.synonyms {
                push(s.clone(), &mut out);
            }
        }
    }
    out
}

/// Every keyword the library knows: on photos (with counts) and created ones (count 0).
fn all_keywords(s: &Session) -> Vec<(String, usize)> {
    fn walk(nodes: &[dac_catalog::KeywordNode], out: &mut Vec<(String, usize)>, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        for n in nodes {
            out.push((n.path.clone(), n.count));
            walk(&n.children, out, depth + 1);
        }
    }
    let mut out = Vec::new();
    walk(&s.catalog.keyword_tree(), &mut out, 0);
    for k in s.keyword_attrs.keys() {
        // a created keyword's parents are keywords too
        let mut path = String::new();
        for part in k.split('|') {
            if !path.is_empty() {
                path.push('|');
            }
            path.push_str(part);
            if !out.iter().any(|(x, _)| x.eq_ignore_ascii_case(&path)) {
                out.push((path.clone(), 0));
            }
        }
    }
    out.sort_by_key(|(k, _)| k.to_lowercase());
    out
}

/// The keyword list as text (tab-indented hierarchy, `[excluded]`, `{synonyms}`).
pub fn to_text(s: &Session) -> String {
    let mut out = String::new();
    for (k, _) in all_keywords(s) {
        let depth = k.matches('|').count();
        let name = k.rsplit('|').next().unwrap_or(&k);
        let a = attrs(&s.keyword_attrs, &k);
        let tabs = "\t".repeat(depth);
        if a.is_some_and(|a| a.exclude_on_export) {
            out.push_str(&format!("{tabs}[{name}]\n"));
        } else {
            out.push_str(&format!("{tabs}{name}\n"));
        }
        for syn in a.map(|a| a.synonyms.as_slice()).unwrap_or_default() {
            out.push_str(&format!("{tabs}\t{{{syn}}}\n"));
        }
    }
    out
}

/// Parse a keyword list: (keyword path, excluded, synonyms) per keyword, in file order.
pub fn parse_text(text: &str) -> Vec<(String, bool, Vec<String>)> {
    let mut out: Vec<(String, bool, Vec<String>)> = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    for line in text.lines().take(MAX_LINES) {
        let depth = line.chars().take_while(|c| *c == '\t').count();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('{') && t.ends_with('}') && t.len() >= 2 {
            let syn = t.get(1..t.len() - 1).unwrap_or("").trim().to_string();
            if let Some(last) = out.last_mut()
                && !syn.is_empty()
                && last.2.len() < MAX_SYNONYMS
            {
                last.2.push(syn);
            }
            continue;
        }
        let (name, excluded) =
            if t.starts_with('[') && t.ends_with(']') && t.len() >= 2 { (t.get(1..t.len() - 1).unwrap_or("").trim(), true) } else { (t, false) };
        // `|` would nest: a name never contains one
        let name = name.replace('|', " ");
        if name.is_empty() || depth > MAX_DEPTH {
            continue;
        }
        stack.truncate(depth.min(stack.len()));
        stack.push(name);
        out.push((stack.join("|"), excluded, Vec::new()));
    }
    out
}

fn set_attributes(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "keyword.setAttributes";
    let k = clean(str_param(p, "keyword").ok_or_else(|| bad(c, "missing keyword"))?);
    if k.is_empty() {
        return Err(bad(c, "empty keyword"));
    }
    let a = entry(&mut s.keyword_attrs, &k);
    if let Some(v) = p.get("synonyms") {
        let list: Vec<String> = match v {
            Value::String(x) => x.split(',').map(|x| x.trim().to_string()).collect(),
            Value::Array(xs) => xs.iter().filter_map(Value::as_str).map(|x| x.trim().to_string()).collect(),
            _ => return Err(bad(c, "synonyms: a list or comma-separated text")),
        };
        a.synonyms = list.into_iter().filter(|x| !x.is_empty()).take(MAX_SYNONYMS).collect();
    }
    if let Some(b) = p.get("includeOnExport").and_then(Value::as_bool) {
        a.exclude_on_export = !b;
    }
    if let Some(b) = p.get("exportParents").and_then(Value::as_bool) {
        a.no_parents_on_export = !b;
    }
    if let Some(b) = p.get("exportSynonyms").and_then(Value::as_bool) {
        a.no_synonyms_on_export = !b;
    }
    if let Some(b) = p.get("person").and_then(Value::as_bool) {
        a.person = b;
    }
    let out = attrs_json(&k, a);
    s.save_prefs()?;
    Ok(out)
}

fn attrs_json(k: &str, a: &KeywordAttrs) -> Value {
    json!({
        "keyword": k,
        "synonyms": a.synonyms,
        "includeOnExport": !a.exclude_on_export,
        "exportParents": !a.no_parents_on_export,
        "exportSynonyms": !a.no_synonyms_on_export,
        "person": a.person,
    })
}

fn catalog(s: &mut Session, p: &Value) -> Result<Value> {
    let filter = str_param(p, "filter").unwrap_or("").trim().to_lowercase();
    let kind = str_param(p, "kind").unwrap_or("all");
    if !matches!(kind, "all" | "person" | "other") {
        return Err(bad("keyword.catalog", "kind: all, person or other"));
    }
    let list: Vec<Value> = all_keywords(s)
        .into_iter()
        .filter_map(|(k, n)| {
            let a = attrs(&s.keyword_attrs, &k).cloned().unwrap_or_default();
            let person = a.person;
            let keep_kind = match kind {
                "person" => person,
                "other" => !person,
                _ => true,
            };
            let matches = filter.is_empty() || k.to_lowercase().contains(&filter) || a.synonyms.iter().any(|x| x.to_lowercase().contains(&filter));
            (keep_kind && matches).then(|| {
                let mut v = attrs_json(&k, &a);
                v["count"] = json!(n);
                v
            })
        })
        .collect();
    Ok(json!(list))
}

fn export_list(s: &mut Session, p: &Value) -> Result<Value> {
    let text = to_text(s);
    let n = text.lines().filter(|l| !l.trim_start().starts_with('{')).count();
    match str_param(p, "path").filter(|x| !x.is_empty()) {
        Some(path) => {
            std::fs::write(path, &text).map_err(|e| bad("keyword.exportList", format!("{path}: {e}")))?;
            Ok(json!({"path": path, "keywords": n}))
        }
        None => Ok(json!({"text": text, "keywords": n})),
    }
}

fn import_list(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "keyword.importList";
    let text = match (str_param(p, "path").filter(|x| !x.is_empty()), str_param(p, "text")) {
        (Some(path), _) => {
            let len = std::fs::metadata(path).map_err(|e| bad(c, format!("{path}: {e}")))?.len();
            if len > MAX_FILE {
                return Err(bad(c, format!("{path} is larger than 8 MB: not a keyword list")));
            }
            let bytes = std::fs::read(path).map_err(|e| bad(c, format!("{path}: {e}")))?;
            String::from_utf8_lossy(&bytes).into_owned()
        }
        (None, Some(t)) => t.to_string(),
        (None, None) => return Err(bad(c, "give path or text")),
    };
    let parsed = parse_text(&text);
    let n = parsed.len();
    for (k, excluded, synonyms) in parsed {
        let a = entry(&mut s.keyword_attrs, &k);
        if excluded {
            a.exclude_on_export = true;
        }
        for syn in synonyms {
            if !a.synonyms.iter().any(|x| x.eq_ignore_ascii_case(&syn)) && a.synonyms.len() < MAX_SYNONYMS {
                a.synonyms.push(syn);
            }
        }
    }
    s.save_prefs()?;
    Ok(json!({"keywords": n}))
}

/// After `keyword.rename`: attributes follow the keyword (and those below it).
pub(crate) fn rename_attrs(s: &mut Session, from: &str, to: &str) {
    let (f, t) = (clean(from), clean(to));
    let moved: Vec<String> = s.keyword_attrs.keys().filter(|k| is_under(k, &f)).cloned().collect();
    for k in moved {
        if let Some(a) = s.keyword_attrs.remove(&k) {
            let nk = format!("{t}{}", k.get(f.len().min(k.len())..).unwrap_or(""));
            s.keyword_attrs.insert(nk, a);
        }
    }
    let _ = s.save_prefs();
}

/// After `keyword.delete`: the keyword (and those below it) are forgotten.
pub(crate) fn delete_attrs(s: &mut Session, keyword: &str) {
    let k = clean(keyword);
    let before = s.keyword_attrs.len();
    s.keyword_attrs.retain(|x, _| !is_under(x, &k));
    if s.keyword_attrs.len() != before {
        let _ = s.save_prefs();
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "keyword.create",
            "Create Keyword Tag",
            [],
            None,
            "{keyword, synonyms?, person?} — a keyword in the list before any photo has it (`a|b` nests) → attributes",
            always,
            |s, p| {
                let k = clean(str_param(p, "keyword").ok_or_else(|| bad("keyword.create", "missing keyword"))?);
                if k.is_empty() {
                    return Err(bad("keyword.create", "empty keyword"));
                }
                entry(&mut s.keyword_attrs, &k);
                let mut q = p.clone();
                q["keyword"] = json!(k);
                set_attributes(s, &q)
            }
        ),
        cmd!(query "keyword.attributes", "Keyword Attributes", [], None, "{keyword} → {keyword, synonyms, includeOnExport, exportParents, exportSynonyms, person}", always, |s, p| {
            let k = str_param(p, "keyword").ok_or_else(|| bad("keyword.attributes", "missing keyword"))?;
            Ok(attrs_json(k, &attrs(&s.keyword_attrs, k).cloned().unwrap_or_default()))
        }),
        cmd!(
            "keyword.setAttributes",
            "Edit Keyword Tag",
            [],
            None,
            "{keyword, synonyms?: [..] or \"a, b\", includeOnExport?, exportParents?, exportSynonyms?, person?} — library-wide keyword attributes (saved with the library) → attributes",
            always,
            set_attributes
        ),
        cmd!(query "keyword.catalog", "Keyword List", [], None, "{filter?: text (names and synonyms), kind?: all|person|other} → [{keyword, count, synonyms, includeOnExport, exportParents, exportSynonyms, person}] every keyword, created ones included", always, catalog),
        cmd!(
            "keyword.exportList",
            "Export Keywords",
            [],
            None,
            "{path?} — the keyword list as text (tab-indented, [not exported], {synonym}); written to `path`, else returned → {keywords, path | text}",
            always,
            export_list
        ),
        cmd!(
            "keyword.importList",
            "Import Keywords",
            [],
            None,
            "{path | text} — read a keyword list (tab-indented, [not exported], {synonym}): its keywords join the list → {keywords}",
            always,
            import_list
        ),
        cmd!(
            "keyword.removeUnused",
            "Purge Unused Keywords",
            [],
            None,
            "{dryRun?} — forget created keywords no photo has → {removed}",
            always,
            |s, p| {
                let used: Vec<String> = all_keywords_used(s);
                let unused: Vec<String> = s.keyword_attrs.keys().filter(|k| !used.iter().any(|u| is_under(u, k))).cloned().collect();
                if !bool_or(p, "dryRun", false) && !unused.is_empty() {
                    for k in &unused {
                        s.keyword_attrs.remove(k);
                    }
                    s.save_prefs()?;
                }
                Ok(json!({"removed": unused}))
            }
        ),
    ]
}

fn all_keywords_used(s: &Session) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in s.catalog.photos() {
        for k in &p.meta.keywords {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(k)) {
                out.push(k.clone());
            }
        }
        if out.len() > 1_000_000 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_list_text_round_trips() {
        let text = "Places\n\tItaly\n\t\t{Italia}\n\t[Home]\nPeople\n\tAnna\n";
        let p = parse_text(text);
        assert_eq!(p[0].0, "Places");
        assert_eq!(p[1], ("Places|Italy".to_string(), false, vec!["Italia".to_string()]));
        assert_eq!(p[2], ("Places|Home".to_string(), true, vec![]));
        assert_eq!(p[4].0, "People|Anna");
    }

    #[test]
    fn hostile_lists_stay_bounded() {
        // deep indentation, braces alone, pipes, empty names
        let deep = format!("{}x\n", "\t".repeat(10_000));
        assert!(parse_text(&deep).is_empty());
        assert!(parse_text("{}\n[]\n{\n}\n|\n").iter().all(|(k, _, _)| !k.contains("||")));
        let wide = "a\n".repeat(MAX_LINES + 10);
        assert_eq!(parse_text(&wide).len(), MAX_LINES);
    }

    #[test]
    fn export_keywords_follow_the_attributes() {
        let mut m = BTreeMap::new();
        m.insert("Places|Italy".to_string(), KeywordAttrs { synonyms: vec!["Italia".into()], ..Default::default() });
        m.insert("Private".to_string(), KeywordAttrs { exclude_on_export: true, ..Default::default() });
        m.insert("People".to_string(), KeywordAttrs { exclude_on_export: true, ..Default::default() });
        m.insert(
            "Things|Car".to_string(),
            KeywordAttrs { no_parents_on_export: true, no_synonyms_on_export: true, synonyms: vec!["Auto".into()], ..Default::default() },
        );
        let k = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            export_keywords(&m, &k(&["Places|Italy", "Private", "People|Anna", "Things|Car", "sun"])),
            k(&["Places|Italy", "Italia", "Anna", "Car", "sun"])
        );
        assert_eq!(export_keywords(&BTreeMap::new(), &k(&["a|b"])), k(&["a|b"]));
    }
}
