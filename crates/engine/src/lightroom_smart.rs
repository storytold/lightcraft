//! Lightroom Classic smart-collection rules → smart-album rules, where they can be expressed.
//!
//! A smart collection's rules are a Lua table in `AgLibraryCollectionContent.content`
//! (`owningModule = "ag.library.smart_collection"`): positional criteria tables
//! `{ criteria = "rating", operation = ">=", value = 3 }` (or nested groups with their own
//! `combine`) and a `combine` of `"intersect"` (all), `"union"` (any) or `"exclude"` (none).
//! Anything we can't express exactly returns `Err` with the reason; the caller then keeps the
//! collection's current membership as a regular album and archives the original rules.
//!
//! Sources: own design, from the structure of the Lua literal as described in public prose
//! write-ups of the catalog format; no Adobe code or data.

use lcc_catalog::{AlbumId, Match, Rule, RuleSet};
use serde_json::{Value, json};

use crate::preset_import::{Lua, parse_lua};

const MAX_DEPTH: usize = 16;

/// The rule set a smart collection's `content` describes, or why it can't be expressed.
/// `collections` are the imported (non-folder) collections, name and album, which a `collection`
/// rule (a text match on the names of the collections a photo is in) resolves against.
pub(crate) fn smart_rules(content: &str, collections: &[(&str, AlbumId)]) -> Result<RuleSet, String> {
    let root = parse_lua(content)?;
    group(&root, 0, collections)
}

fn group(t: &Lua, depth: usize, collections: &[(&str, AlbumId)]) -> Result<RuleSet, String> {
    if depth > MAX_DEPTH {
        return Err("smart rules nest too deeply".into());
    }
    let Lua::Table(items, _) = t else { return Err("smart rules are not a table".into()) };
    let mode = match t.get("combine").and_then(Lua::str).unwrap_or("intersect") {
        "intersect" => Match::All,
        "union" => Match::Any,
        "exclude" => Match::None,
        other => return Err(format!("unknown combine `{other}`")),
    };
    let mut rules = Vec::new();
    for item in items {
        if item.get("criteria").is_some() {
            rules.push(criterion(item, collections)?);
        } else {
            rules.push(Rule::Group { group: group(item, depth + 1, collections)? });
        }
    }
    if rules.is_empty() {
        return Err("smart collection has no rules".into());
    }
    Ok(RuleSet { mode, rules })
}

fn num(v: Option<&Lua>) -> Option<f64> {
    match v {
        Some(Lua::Num(n)) if n.is_finite() => Some(*n),
        Some(Lua::Str(s)) => s.trim().parse().ok().filter(|n: &f64| n.is_finite()),
        _ => None,
    }
}

fn field(field: &str, op: &str, value: Value) -> Rule {
    Rule::Field { field: field.into(), op: op.into(), value }
}

fn number_op(op: &str) -> Option<&'static str> {
    Some(match op {
        "==" => "is",
        "!=" => "isNot",
        ">=" => "gte",
        "<=" => "lte",
        ">" => "gt",
        "<" => "lt",
        _ => return None,
    })
}

/// A Lightroom date value (`2024-05-01`, optionally with a time) as a rule date, or `None`.
fn date(v: Option<&Lua>) -> Option<String> {
    let s = v.and_then(Lua::str)?.trim();
    let day = s.get(..10).unwrap_or(s);
    let ok = day.len() == 10 && day.char_indices().all(|(i, c)| if i == 4 || i == 7 { c == '-' } else { c.is_ascii_digit() });
    ok.then(|| day.to_string())
}

/// Whether a collection name matches Lightroom's text operation on collection names.
fn name_matches(name: &str, op: &str, value: &str) -> Option<bool> {
    let name = name.to_lowercase();
    let value = value.to_lowercase();
    let mut words = value.split_whitespace();
    Some(match op {
        "all" | "words" => words.all(|w| name.contains(w)),
        "any" => words.any(|w| name.contains(w)),
        "beginsWith" => name.starts_with(value.trim()),
        "endsWith" => name.ends_with(value.trim()),
        "==" => name == value.trim(),
        _ => return None,
    })
}

fn criterion(c: &Lua, collections: &[(&str, AlbumId)]) -> Result<Rule, String> {
    let name = c.get("criteria").and_then(Lua::str).unwrap_or_default();
    let op = c.get("operation").and_then(Lua::str).unwrap_or_default();
    let value = c.get("value");
    let unsupported = || format!("rule `{name} {op}` has no equivalent");
    match name {
        "collection" => {
            // Resolved now against the collections imported with it: a collection made later
            // isn't matched by name (the archive keeps the original rule).
            let text = value.and_then(Lua::str).unwrap_or_default();
            if text.trim().is_empty() {
                return Err(unsupported());
            }
            let mut rules = Vec::new();
            for (n, id) in collections {
                if name_matches(n, op, text).ok_or_else(unsupported)? {
                    rules.push(field("album", "is", json!(id)));
                }
            }
            match rules.len() {
                0 => Err(format!("rule `{name} {op}` matches no imported collection")),
                1 => Ok(rules.remove(0)),
                _ => Ok(Rule::Group { group: RuleSet { mode: Match::Any, rules } }),
            }
        }
        "captureTime" | "touchTime" => {
            let target = if name == "captureTime" { "captureDate" } else { "editDate" };
            match op {
                "==" => Ok(field(target, "is", json!(date(value).ok_or_else(unsupported)?))),
                ">" => Ok(field(target, "after", json!(date(value).ok_or_else(unsupported)?))),
                "<" => Ok(field(target, "before", json!(date(value).ok_or_else(unsupported)?))),
                "in" => {
                    let (a, b) = (date(value).ok_or_else(unsupported)?, date(c.get("value2")).ok_or_else(unsupported)?);
                    Ok(field(target, "between", json!([a, b])))
                }
                "inLast" | "notInLast" => {
                    let n = num(value).filter(|n| *n > 0.0).ok_or_else(unsupported)?;
                    let unit = match c.get("value_units").and_then(Lua::str).unwrap_or("days") {
                        u @ ("hours" | "days" | "weeks" | "months" | "years") => u.trim_end_matches('s').to_string(),
                        _ => return Err(unsupported()),
                    };
                    Ok(field(target, op, json!({"n": n, "unit": unit})))
                }
                // "is today", "is yesterday", "is in this week / month / year" [inferred operator names]
                op if lcc_catalog::rules::CALENDAR_OPS.contains(&op) => Ok(field(target, op, Value::Null)),
                _ => Err(unsupported()),
            }
        }
        "rating" | "isoSpeedRating" | "focalLength" | "aperture" => {
            let target = match name {
                "rating" => "rating",
                "isoSpeedRating" => "iso",
                "focalLength" => "focalLength",
                _ => "aperture",
            };
            let n = num(value).ok_or_else(unsupported)?;
            if op == "in" {
                let hi = num(c.get("value2")).ok_or_else(unsupported)?;
                return Ok(field(target, "between", json!([n, hi])));
            }
            let op = number_op(op).ok_or_else(unsupported)?;
            Ok(field(target, op, json!(n)))
        }
        "pick" => {
            let flag = match num(value).map(|n| n as i64) {
                Some(1) => "pick",
                Some(-1) => "reject",
                Some(0) => "none",
                _ => return Err(unsupported()),
            };
            match op {
                "==" => Ok(field("flag", "is", json!(flag))),
                "!=" => Ok(field("flag", "isNot", json!(flag))),
                _ => Err(unsupported()),
            }
        }
        "labelColor" => {
            let label = value.and_then(Lua::str).map(str::to_ascii_lowercase).ok_or_else(unsupported)?;
            if !["red", "yellow", "green", "blue", "purple", "none"].contains(&label.as_str()) {
                return Err(unsupported());
            }
            match op {
                "==" => Ok(field("label", "is", json!(label))),
                "!=" => Ok(field("label", "isNot", json!(label))),
                _ => Err(unsupported()),
            }
        }
        "hasAdjustments" => {
            let yes = match value {
                Some(Lua::Bool(b)) => *b,
                Some(Lua::Num(n)) => *n != 0.0,
                None => true,
                _ => return Err(unsupported()),
            };
            match op {
                "==" | "" => Ok(field("edited", "is", json!(yes))),
                "!=" => Ok(field("edited", "is", json!(!yes))),
                _ => Err(unsupported()),
            }
        }
        _ => {
            let target = match name {
                "keywords" => "keywords",
                "filename" => "fileName",
                "camera" => "camera",
                "lens" => "lens",
                "title" => "title",
                "caption" => "caption",
                "copyname" | "copyName" => "copyName",
                "creator" => "creator",
                "copyright" => "copyright",
                "city" => "city",
                "country" => "country",
                "location" => "location",
                "state" => "state",
                "fileFormat" => "format",
                _ => return Err(unsupported()),
            };
            text_rule(target, op, value.and_then(Lua::str).unwrap_or_default()).ok_or_else(unsupported)
        }
    }
}

fn text_rule(target: &str, op: &str, value: &str) -> Option<Rule> {
    let words: Vec<&str> = value.split_whitespace().collect();
    let keywords = target == "keywords";
    let rule = match op {
        "empty" => field(target, "isEmpty", Value::Null),
        "notEmpty" => field(target, "isNotEmpty", Value::Null),
        _ if words.is_empty() => return None,
        // Every word must appear: what "contains" already means here.
        "all" | "words" => field(target, "contains", json!(value)),
        "any" if words.len() == 1 => field(target, "contains", json!(value)),
        "any" => Rule::Group { group: RuleSet { mode: Match::Any, rules: words.iter().map(|w| field(target, "contains", json!(w))).collect() } },
        "noneOf" => {
            Rule::Group { group: RuleSet { mode: Match::All, rules: words.iter().map(|w| field(target, "notContains", json!(w))).collect() } }
        }
        "beginsWith" => field(target, "startsWith", json!(value)),
        "endsWith" if !keywords => field(target, "endsWith", json!(value)),
        "==" => field(target, "is", json!(value)),
        "!=" if !keywords => field(target, "isNot", json!(value)),
        _ => return None,
    };
    Some(rule)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_collection_and_date_rules() {
        let cols = [("Italy 2024", AlbumId(1)), ("France 2024", AlbumId(2)), ("Family", AlbumId(3))];
        let set = smart_rules(
            r#"s = { { criteria = "collection", operation = "all", value = "2024" },
                     { criteria = "collection", operation = "beginsWith", value = "fam" },
                     { criteria = "captureTime", operation = "in", value = "2024-01-01", value2 = "2024-12-31" },
                     { criteria = "touchTime", operation = "inLast", value = 3, value_units = "weeks" },
                     { criteria = "captureTime", operation = ">", value = "2023-06-01T10:00:00" },
                     { criteria = "touchTime", operation = "thisWeek" },
                     combine = "intersect" }"#,
            &cols,
        )
        .unwrap();
        let Rule::Group { group } = &set.rules[0] else { panic!() };
        assert_eq!(group.rules, vec![field("album", "is", json!(AlbumId(1))), field("album", "is", json!(AlbumId(2)))]);
        assert_eq!(set.rules[1], field("album", "is", json!(AlbumId(3))));
        assert_eq!(set.rules[2], field("captureDate", "between", json!(["2024-01-01", "2024-12-31"])));
        assert_eq!(set.rules[3], field("editDate", "inLast", json!({"n": 3.0, "unit": "week"})));
        assert_eq!(set.rules[4], field("captureDate", "after", json!("2023-06-01")));
        assert_eq!(set.rules[5], field("editDate", "thisWeek", Value::Null));
        for bad in [
            r#"s = { { criteria = "collection", operation = "all", value = "spain" } }"#,
            r#"s = { { criteria = "captureTime", operation = "==", value = "last tuesday" } }"#,
            r#"s = { { criteria = "touchTime", operation = "inLast", value = 2, value_units = "fortnights" } }"#,
        ] {
            assert!(smart_rules(bad, &cols).is_err(), "{bad}");
        }
    }

    #[test]
    fn maps_common_rules_and_groups() {
        let set = smart_rules(
            r#"s = { { criteria = "rating", operation = ">=", value = 3, value2 = 0 },
                     { criteria = "pick", operation = "==", value = 1 },
                     { criteria = "keywords", operation = "any", value = "italy france" },
                     { { criteria = "labelColor", operation = "==", value = "red" },
                       { criteria = "isoSpeedRating", operation = "in", value = 100, value2 = 800 },
                       combine = "union" },
                     combine = "intersect" }"#,
            &[],
        )
        .unwrap();
        assert_eq!(set.mode, Match::All);
        assert_eq!(set.rules.len(), 4);
        assert_eq!(set.rules[0], field("rating", "gte", json!(3.0)));
        assert_eq!(set.rules[1], field("flag", "is", json!("pick")));
        let Rule::Group { group } = &set.rules[2] else { panic!() };
        assert_eq!(group.mode, Match::Any);
        let Rule::Group { group } = &set.rules[3] else { panic!() };
        assert_eq!(group.rules[1], field("iso", "between", json!([100.0, 800.0])));
        let cat = lcc_catalog::Catalog::default();
        let mut tmp = cat;
        let id = tmp.alloc_album_id();
        let mut album = lcc_catalog::Album::new(id, "s");
        album.smart = Some(Box::new(lcc_catalog::Filter { rule_set: Some(set), ..Default::default() }));
        assert!(tmp.album_filter_problem(album.smart.as_deref().unwrap(), None).is_none());
    }

    #[test]
    fn unexpressible_rules_are_reported_not_guessed() {
        for bad in [
            r#"s = { { criteria = "metadataStatus", operation = "==", value = "x" }, combine = "intersect" }"#,
            r#"s = { { criteria = "rating", operation = "~", value = 3 } }"#,
            r#"s = { combine = "intersect" }"#,
            r#"s = { { criteria = "rating", operation = ">=", value = "abc" } }"#,
            "not lua at all {",
        ] {
            assert!(smart_rules(bad, &[]).is_err(), "{bad}");
        }
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let mut s = String::from("{ criteria = \"rating\", operation = \">=\", value = 1 }");
        for _ in 0..40 {
            s = format!("{{ {s} }}");
        }
        assert!(smart_rules(&s, &[]).is_err());
    }
}
