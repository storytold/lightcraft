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

use lightcraft_catalog::{Match, Rule, RuleSet};
use serde_json::{Value, json};

use crate::preset_import::{Lua, parse_lua};

const MAX_DEPTH: usize = 16;

/// The rule set a smart collection's `content` describes, or why it can't be expressed.
pub(crate) fn smart_rules(content: &str) -> Result<RuleSet, String> {
    let root = parse_lua(content)?;
    group(&root, 0)
}

fn group(t: &Lua, depth: usize) -> Result<RuleSet, String> {
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
            rules.push(criterion(item)?);
        } else {
            rules.push(Rule::Group { group: group(item, depth + 1)? });
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

/// Lightroom keeps apertures as APEX values (`f = 2^(av / 2)`), native rules compare the f-number.
/// The catalog's APEX values are rounded, so each bound is widened by 1% (an exact `==` / `!=`
/// can't be expressed and stays static).
fn aperture(op: &str, av: f64, av2: Option<f64>) -> Option<Rule> {
    const TOL: f64 = 0.01;
    let f = |a: f64| (a / 2.0).exp2();
    let (lo, hi) = (f(av), av2.map(f));
    if !lo.is_finite() || hi.is_some_and(|h| !h.is_finite()) {
        return None;
    }
    Some(match op {
        "==" => field("aperture", "between", json!([lo * (1.0 - TOL), lo * (1.0 + TOL)])),
        ">=" => field("aperture", "gte", json!(lo * (1.0 - TOL))),
        ">" => field("aperture", "gt", json!(lo * (1.0 + TOL))),
        "<=" => field("aperture", "lte", json!(lo * (1.0 + TOL))),
        "<" => field("aperture", "lt", json!(lo * (1.0 - TOL))),
        "in" => field("aperture", "between", json!([lo * (1.0 - TOL), hi? * (1.0 + TOL)])),
        _ => return None,
    })
}

/// Lightroom's `fileFormat` names (`RAW` is every proprietary raw, apart from `DNG`) as native rules.
fn file_format(op: &str, value: &str) -> Option<Rule> {
    let any = |rules: Vec<Rule>| Rule::Group { group: RuleSet { mode: Match::Any, rules } };
    let positive = match value.trim().to_ascii_uppercase().as_str() {
        "RAW" => Rule::Group {
            group: RuleSet { mode: Match::All, rules: vec![field("kind", "is", json!("raw")), field("format", "isNot", json!("dng"))] },
        },
        "DNG" => field("format", "is", json!("dng")),
        "VIDEO" => field("kind", "is", json!("video")),
        "JPG" | "JPEG" => any(vec![field("format", "is", json!("jpg")), field("format", "is", json!("jpeg"))]),
        "TIFF" | "TIF" => any(vec![field("format", "is", json!("tif")), field("format", "is", json!("tiff"))]),
        "PSD" => field("format", "is", json!("psd")),
        _ => return None,
    };
    match op {
        "==" | "" => Some(positive),
        "!=" => Some(Rule::Group { group: RuleSet { mode: Match::None, rules: vec![positive] } }),
        _ => None,
    }
}

fn criterion(c: &Lua) -> Result<Rule, String> {
    let name = c.get("criteria").and_then(Lua::str).unwrap_or_default();
    let op = c.get("operation").and_then(Lua::str).unwrap_or_default();
    let value = c.get("value");
    let unsupported = || format!("rule `{name} {op}` has no equivalent");
    match name {
        "rating" | "isoSpeedRating" | "focalLength" | "aperture" => {
            let target = match name {
                "rating" => "rating",
                "isoSpeedRating" => "iso",
                "focalLength" => "focalLength",
                _ => "aperture",
            };
            let n = num(value).ok_or_else(unsupported)?;
            if name == "aperture" {
                return aperture(op, n, num(c.get("value2"))).ok_or_else(unsupported);
            }
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
        "fileFormat" => file_format(op, value.and_then(Lua::str).unwrap_or_default()).ok_or_else(unsupported),
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
        // Every word must appear, each on its own (separate keywords can carry them).
        "all" | "words" if words.len() == 1 => field(target, "contains", json!(value)),
        "all" | "words" => {
            Rule::Group { group: RuleSet { mode: Match::All, rules: words.iter().map(|w| field(target, "contains", json!(w))).collect() } }
        }
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
    fn maps_common_rules_and_groups() {
        let set = smart_rules(
            r#"s = { { criteria = "rating", operation = ">=", value = 3, value2 = 0 },
                     { criteria = "pick", operation = "==", value = 1 },
                     { criteria = "keywords", operation = "any", value = "italy france" },
                     { { criteria = "labelColor", operation = "==", value = "red" },
                       { criteria = "isoSpeedRating", operation = "in", value = 100, value2 = 800 },
                       combine = "union" },
                     combine = "intersect" }"#,
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
        let cat = lightcraft_catalog::Catalog::default();
        let mut tmp = cat;
        let id = tmp.alloc_album_id();
        let mut album = lightcraft_catalog::Album::new(id, "s");
        album.smart = Some(Box::new(lightcraft_catalog::Filter { rule_set: Some(set), ..Default::default() }));
        assert!(tmp.album_filter_problem(album.smart.as_deref().unwrap(), None).is_none());
    }

    #[test]
    fn unexpressible_rules_are_reported_not_guessed() {
        for bad in [
            r#"s = { { criteria = "collection", operation = "all", value = "x" }, combine = "intersect" }"#,
            r#"s = { { criteria = "rating", operation = "~", value = 3 } }"#,
            r#"s = { combine = "intersect" }"#,
            r#"s = { { criteria = "rating", operation = ">=", value = "abc" } }"#,
            "not lua at all {",
        ] {
            assert!(smart_rules(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn rules_whose_values_differ_in_lightroom_are_translated() {
        let one = |c: &str| smart_rules(&format!("s = {{ {c} }}")).unwrap().rules.remove(0);
        // every word may sit on a different keyword
        let Rule::Group { group } = one(r#"{ criteria = "keywords", operation = "all", value = "beach sunset" }"#) else { panic!() };
        assert_eq!((group.mode, group.rules.len()), (Match::All, 2));
        // APEX 6 is f/8
        let Rule::Field { field, op, value } = one(r#"{ criteria = "aperture", operation = "==", value = 6 }"#) else { panic!() };
        let (lo, hi) = (value[0].as_f64().unwrap(), value[1].as_f64().unwrap());
        assert_eq!((field.as_str(), op.as_str()), ("aperture", "between"));
        assert!(lo < 8.0 && 8.0 < hi && hi < 8.2, "{lo} {hi}");
        let Rule::Field { value, .. } = one(r#"{ criteria = "aperture", operation = "in", value = 4, value2 = 6 }"#) else { panic!() };
        assert!(value[0].as_f64().unwrap() < 4.0 && value[1].as_f64().unwrap() > 8.0, "{value}");
        // RAW is the raw kind, not DNG
        let Rule::Group { group } = one(r#"{ criteria = "fileFormat", operation = "==", value = "RAW" }"#) else { panic!() };
        assert_eq!(group.rules, vec![field_rule("kind", "is", "raw"), field_rule("format", "isNot", "dng")]);
        assert!(smart_rules(r#"s = { { criteria = "fileFormat", operation = "==", value = "WEIRD" } }"#).is_err());
    }

    fn field_rule(f: &str, op: &str, v: &str) -> Rule {
        field(f, op, json!(v))
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let mut s = String::from("{ criteria = \"rating\", operation = \">=\", value = 1 }");
        for _ in 0..40 {
            s = format!("{{ {s} }}");
        }
        assert!(smart_rules(&s).is_err());
    }
}
