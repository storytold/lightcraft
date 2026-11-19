//! A smart album's rule (`ZCOLLECTION.ZSMARTCRITERIA`), as LightCraft smart album rules or, when
//! a condition has no LightCraft field, as the photos it selects in the catalog.
//!
//! The rule is XML: `<MatchOperator Kind="AND|OR">` ("all" / "any" of the rules in Capture One's
//! dialog) around groups and `<Condition Enabled="YES|NO">` elements, each with a `<Key>`, an
//! `<Operator>` and a `<Criterion>`. Keys are mostly `IB_<S|I|F>_<column>` after the catalog's
//! columns; operators are one numbering for every kind of field. Both were learned from smart
//! albums made in Capture One 16.3 and compared with its rule dialog: keys and operators not seen
//! there are not guessed, and a rule that uses one is named in a warning.

use std::collections::BTreeMap;

use lightcraft_catalog::{Match, Rule as LcRule, RuleSet};
use serde_json::{Value, json};

use super::{CatalogPhoto, ColorTag};

/// Longest rule read; Capture One's own are a few hundred bytes per condition.
const MAX_RULE_BYTES: usize = 256 * 1024;
/// Deepest nesting of groups (Capture One writes two levels).
const MAX_DEPTH: usize = 16;
/// Most elements one rule may hold.
const MAX_NODES: usize = 20_000;

/// A group of conditions: all of them, or any.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Group {
    pub(super) all: bool,
    pub(super) items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    Group(Group),
    Condition(Condition),
}

/// One line of Capture One's rule dialog.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Condition {
    pub(super) enabled: bool,
    pub(super) key: String,
    pub(super) operator: i64,
    pub(super) criterion: String,
}

/// What a smart album becomes.
#[derive(Debug, PartialEq)]
pub(super) enum Outcome {
    /// A LightCraft smart album with these rules.
    Smart(RuleSet),
    /// A plain album with the photos (indexes into the catalog's) the rule selects now, and the
    /// conditions that kept it from being a smart album.
    Selected(Vec<usize>, Vec<String>),
    /// Neither; the reason.
    Skipped(String),
}

/// Read a rule. An error says why it can't be.
pub(super) fn parse(xml: &str) -> Result<Group, String> {
    if xml.len() > MAX_RULE_BYTES {
        return Err(format!("its rule is longer than {} KB", MAX_RULE_BYTES / 1024));
    }
    let limits = craft_xml::Limits { max_depth: MAX_DEPTH.saturating_mul(2), max_nodes: MAX_NODES, ..craft_xml::Limits::DEFAULT };
    let options = craft_xml::Options { strict: true, drop_blank_text: true, ..craft_xml::Options::default() }.with_limits(limits);
    let root = options.parse(xml.as_bytes()).map_err(|e| format!("its rule can't be read ({e})"))?;
    group(&root, 0)
}

fn group(el: &craft_xml::El, depth: usize) -> Result<Group, String> {
    if el.local() != "MatchOperator" || depth > MAX_DEPTH {
        return Err("its rule has a form this version doesn't know".into());
    }
    let all = match el.attr("Kind").map(str::trim) {
        Some(kind) if kind.eq_ignore_ascii_case("AND") => true,
        Some(kind) if kind.eq_ignore_ascii_case("OR") => false,
        _ => return Err("its rule joins conditions in a way this version doesn't know".into()),
    };
    let mut items = Vec::new();
    for child in el.elements() {
        match child.local() {
            "MatchOperator" => items.push(Item::Group(group(child, depth.saturating_add(1))?)),
            "Condition" => items.push(Item::Condition(condition(child)?)),
            _ => return Err("its rule has a form this version doesn't know".into()),
        }
    }
    Ok(Group { all, items })
}

fn condition(el: &craft_xml::El) -> Result<Condition, String> {
    let text = |name: &str| el.child_text(name).map(|t| t.trim().to_string()).unwrap_or_default();
    let operator = text("Operator").parse::<i64>().map_err(|_| "a condition of its rule has no operator".to_string())?;
    Ok(Condition {
        enabled: !el.attr("Enabled").is_some_and(|e| e.trim().eq_ignore_ascii_case("NO")),
        key: text("Key"),
        operator,
        criterion: text("Criterion"),
    })
}

/// What `rule` becomes against the catalog's `photos`: LightCraft rules when every condition has a
/// LightCraft field, else the photos it selects when every condition can be told from the catalog.
pub(super) fn outcome(rule: &Group, photos: &[CatalogPhoto]) -> Outcome {
    if !has_enabled(rule) {
        return Outcome::Skipped("its rule has no condition switched on".into());
    }
    let mut missing = Vec::new();
    let rules = to_rules(rule, &mut missing);
    if missing.is_empty() {
        return Outcome::Smart(rules);
    }
    match select(rule, photos) {
        Some(selected) => Outcome::Selected(selected, missing),
        None => {
            let sample = photos.first().cloned().unwrap_or_default();
            Outcome::Skipped(format!("its rule tests {}, which the catalog import can't read", unknown(rule, &sample).join(", ")))
        }
    }
}

/// The photos (indexes into `photos`) the rule selects; `None` when a condition tests something
/// the catalog import doesn't read.
pub(super) fn select(rule: &Group, photos: &[CatalogPhoto]) -> Option<Vec<usize>> {
    let mut selected = Vec::new();
    for (at, photo) in photos.iter().enumerate() {
        if matches(rule, photo)? {
            selected.push(at);
        }
    }
    Some(selected)
}

fn has_enabled(group: &Group) -> bool {
    group.items.iter().any(|item| match item {
        Item::Group(g) => has_enabled(g),
        Item::Condition(c) => c.enabled,
    })
}

// ---- LightCraft rules

/// The rule as LightCraft rules; the names of the conditions that have no LightCraft field go to
/// `missing` (the rules are then incomplete).
fn to_rules(group: &Group, missing: &mut Vec<String>) -> RuleSet {
    let mut rules = Vec::new();
    for item in &group.items {
        match item {
            Item::Group(g) => {
                if has_enabled(g) {
                    let mut group = to_rules(g, missing);
                    // Capture One puts every condition in a group of its own
                    match (group.rules.len(), group.rules.pop()) {
                        (1, Some(rule)) => rules.push(rule),
                        (_, Some(rule)) => {
                            group.rules.push(rule);
                            rules.push(LcRule::Group { group });
                        }
                        (_, None) => {}
                    }
                }
            }
            Item::Condition(c) if !c.enabled => {}
            Item::Condition(c) => match to_rule(c) {
                Some(rule) => rules.push(rule),
                None => {
                    let name = name(c);
                    if !missing.contains(&name) {
                        missing.push(name);
                    }
                }
            },
        }
    }
    RuleSet { mode: if group.all { Match::All } else { Match::Any }, rules }
}

fn field(field: &str, op: &str, value: Value) -> LcRule {
    LcRule::Field { field: field.into(), op: op.into(), value }
}

/// A rule that holds when `rule` doesn't.
fn not(rule: LcRule) -> LcRule {
    LcRule::Group { group: RuleSet { mode: Match::None, rules: vec![rule] } }
}

fn to_rule(c: &Condition) -> Option<LcRule> {
    let key = KEYS.iter().find(|k| k.key == c.key)?;
    let lc = key.lightcraft?;
    let value = c.criterion.as_str();
    match key.kind {
        Kind::Text | Kind::Keywords => text_rule(lc, key.kind, c.operator, value),
        Kind::Number => {
            let n: f64 = value.parse().ok().filter(|n: &f64| n.is_finite())?;
            Some(field(lc, number_op(c.operator)?, json!(n)))
        }
        Kind::Date(_) => date_rule(lc, c.operator, value),
        Kind::Bool => Some(field(lc, "is", json!(bool_value(c.operator)?))),
        Kind::Label => label_rule(c.operator, value),
        Kind::Aspect => {
            let aspect = match value {
                "0" => "landscape",
                "1" => "portrait",
                "2" => "square",
                _ => return None,
            };
            match c.operator {
                0 => Some(field(lc, "is", json!(aspect))),
                1 => Some(field(lc, "isNot", json!(aspect))),
                _ => None,
            }
        }
        Kind::Size | Kind::Stored => None,
    }
}

fn number_op(operator: i64) -> Option<&'static str> {
    Some(match operator {
        0 => "is",
        1 => "isNot",
        2 => "gte",
        3 => "lte",
        4 => "lt",
        5 => "gt",
        _ => return None,
    })
}

fn bool_value(operator: i64) -> Option<bool> {
    match operator {
        12 => Some(true),
        13 => Some(false),
        _ => None,
    }
}

fn text_rule(lc: &str, kind: Kind, operator: i64, value: &str) -> Option<LcRule> {
    if value.is_empty() {
        return None;
    }
    let keywords = kind == Kind::Keywords;
    let op = match operator {
        0 | 16 => "is",
        1 | 17 if keywords => return Some(not(field(lc, "is", json!(value)))),
        1 | 17 => "isNot",
        6 => "contains",
        8 => "notContains",
        9 => "startsWith",
        10 if !keywords => "endsWith",
        _ => return None,
    };
    // LightCraft's "contains" wants every word somewhere; Capture One's, the text as typed
    if matches!(op, "contains" | "notContains") && value.split_whitespace().nth(1).is_some() {
        return None;
    }
    // "camera" is make and model together: only a part of it can be looked for
    if lc == "camera" && !matches!(op, "contains" | "notContains") {
        return None;
    }
    Some(field(lc, op, json!(value)))
}

fn date_rule(lc: &str, operator: i64, value: &str) -> Option<LcRule> {
    let date = date_criterion(value)?;
    match operator {
        0 => Some(field(lc, "is", json!(date))),
        1 => Some(not(field(lc, "is", json!(date)))),
        14 => Some(field(lc, "after", json!(date))),
        15 => Some(field(lc, "before", json!(date))),
        _ => None,
    }
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD`, as Capture One writes a year, a month or a day.
fn date_criterion(value: &str) -> Option<&str> {
    let ok = match value.len() {
        4 => value.bytes().all(|b| b.is_ascii_digit()),
        7 | 10 => value.bytes().enumerate().all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }),
        _ => false,
    };
    ok.then_some(value)
}

/// A colour tag rule: a LightCraft label, or for orange and pink the keyword the import gives
/// them.
fn label_rule(operator: i64, value: &str) -> Option<LcRule> {
    let tag = ColorTag::from_index(value.parse().ok()?)?;
    let negated = match operator {
        0 => false,
        1 => true,
        _ => return None,
    };
    if matches!(tag, ColorTag::Orange | ColorTag::Pink) {
        let is = field("keywords", "is", json!(format!("{}|{}", super::convert::KEYWORD_PARENT, tag.name()?)));
        return Some(if negated { not(is) } else { is });
    }
    let label = tag.name().map_or_else(|| "none".to_string(), str::to_lowercase);
    Some(field("label", if negated { "isNot" } else { "is" }, json!(label)))
}

/// How a condition is named in a warning: its field as Capture One's key says it.
fn name(c: &Condition) -> String {
    let key = c.key.trim_start_matches("IB_");
    let key = key.split_once('_').filter(|(kind, _)| matches!(*kind, "S" | "I" | "F")).map_or(key, |(_, rest)| rest);
    let key = key.trim_start_matches('_');
    if key.is_empty() { "an unnamed field".into() } else { format!("“{key}”") }
}

fn unknown(group: &Group, photo: &CatalogPhoto) -> Vec<String> {
    let mut out = Vec::new();
    collect_unknown(group, photo, &mut out);
    out
}

fn collect_unknown(group: &Group, photo: &CatalogPhoto, out: &mut Vec<String>) {
    for item in &group.items {
        match item {
            Item::Group(g) => collect_unknown(g, photo, out),
            Item::Condition(c) if c.enabled && matches_condition(c, photo).is_none() => {
                let name = name(c);
                if !out.contains(&name) {
                    out.push(name);
                }
            }
            Item::Condition(_) => {}
        }
    }
}

// ---- the photos a rule selects

/// Whether `photo` meets the rule; `None` when a condition tests something the catalog import
/// doesn't read.
pub(super) fn matches(group: &Group, photo: &CatalogPhoto) -> Option<bool> {
    let mut any = false;
    let mut all = true;
    for item in &group.items {
        let met = match item {
            Item::Group(g) if !has_enabled(g) => continue,
            Item::Group(g) => matches(g, photo)?,
            Item::Condition(c) if !c.enabled => continue,
            Item::Condition(c) => matches_condition(c, photo)?,
        };
        any |= met;
        all &= met;
    }
    Some(if group.all { all } else { any })
}

fn matches_condition(c: &Condition, photo: &CatalogPhoto) -> Option<bool> {
    let key = KEYS.iter().find(|k| k.key == c.key);
    let kind = key.map_or(Kind::Stored, |k| k.kind);
    match kind {
        Kind::Text => text_matches(c.operator, &text_of(&c.key, photo), &c.criterion),
        Kind::Stored => stored_matches(c, photo),
        Kind::Keywords => keywords_match(c.operator, &photo.keywords, &c.criterion),
        Kind::Number => {
            let want: f64 = c.criterion.parse().ok().filter(|n: &f64| n.is_finite())?;
            Some(number_matches(number_op(c.operator)?, number_of(&c.key, photo), want))
        }
        Kind::Size => {
            let want: f64 = c.criterion.parse().ok().filter(|n: &f64| n.is_finite())?;
            let have = match c.key.as_str() {
                "IB_S_FILE_SIZE" => photo.file_size as f64,
                "IB_I_WIDTH" => f64::from(photo.width),
                _ => f64::from(photo.height),
            };
            Some(number_matches(number_op(c.operator)?, Some(have), want))
        }
        Kind::Date(part) => date_matches(c.operator, photo.captured.as_deref(), date_criterion(&c.criterion)?, part),
        Kind::Bool => {
            let have = match c.key.as_str() {
                "isModified" => photo.adjusted,
                "ANNOTATED" => photo.annotated,
                _ => photo.gps.is_some(),
            };
            Some(have == bool_value(c.operator)?)
        }
        Kind::Label => {
            let want = ColorTag::from_index(c.criterion.parse().ok()?)?;
            match c.operator {
                0 => Some(photo.color_tag == want),
                1 => Some(photo.color_tag != want),
                _ => None,
            }
        }
        Kind::Aspect => {
            let have = match photo.width.cmp(&photo.height) {
                std::cmp::Ordering::Greater => "0",
                std::cmp::Ordering::Less => "1",
                std::cmp::Ordering::Equal => "2",
            };
            match c.operator {
                0 => Some(have == c.criterion),
                1 => Some(have != c.criterion),
                _ => None,
            }
        }
    }
}

/// A condition on a column the table of keys doesn't name: one of the image's camera and
/// exposure columns, or another IPTC field, compared as numbers where both are, else as text.
fn stored_matches(c: &Condition, photo: &CatalogPhoto) -> Option<bool> {
    let (kind, column) = c.key.strip_prefix("IB_")?.split_once('_')?;
    let have = match photo.exif.get(column) {
        Some(value) => value.as_str(),
        // only filled IPTC fields are kept, so only the metadata table's columns can be told
        // apart from a column this reader doesn't know
        None if kind == "S" && METADATA_GROUPS.iter().any(|g| column.starts_with(g)) => {
            photo.iptc.other.get(column).map(String::as_str).unwrap_or("")
        }
        None => return None,
    };
    let numeric = (0..=5).contains(&c.operator);
    match (kind, numeric, number(&c.criterion)) {
        ("I" | "F", true, Some(want)) => Some(number_matches(number_op(c.operator)?, number(have), want)),
        ("S", true, Some(want)) if number(have).is_some() || have.is_empty() => Some(number_matches(number_op(c.operator)?, number(have), want)),
        ("S", _, _) => text_matches(c.operator, have, &c.criterion),
        _ => None,
    }
}

/// A number as Capture One writes one: plain, or a fraction (`-512/256`).
fn number(text: &str) -> Option<f64> {
    let text = text.trim();
    let value = match text.split_once('/') {
        Some((n, d)) => {
            let d: f64 = d.trim().parse().ok().filter(|d: &f64| *d != 0.0)?;
            n.trim().parse::<f64>().ok()? / d
        }
        None => text.parse().ok()?,
    };
    value.is_finite().then_some(value)
}

fn text_matches(operator: i64, have: &str, want: &str) -> Option<bool> {
    let (have, want) = (have.trim().to_lowercase(), want.trim().to_lowercase());
    Some(match operator {
        0 | 16 => have == want,
        1 | 17 => have != want,
        6 => have.contains(&want),
        8 => !have.contains(&want),
        9 => have.starts_with(&want),
        10 => have.ends_with(&want),
        _ => return None,
    })
}

fn keywords_match(operator: i64, keywords: &[String], want: &str) -> Option<bool> {
    let any = |op: i64| -> Option<bool> {
        let mut met = false;
        for keyword in keywords {
            // a keyword or any of its levels, as Capture One lists the parents beside it
            for name in std::iter::once(keyword.as_str()).chain(keyword.split('|')) {
                met |= text_matches(op, name, want)?;
            }
        }
        Some(met)
    };
    match operator {
        1 | 17 => any(16).map(|met| !met),
        8 => any(6).map(|met| !met),
        _ => any(operator),
    }
}

fn number_matches(op: &str, have: Option<f64>, want: f64) -> bool {
    let Some(have) = have else { return op == "isNot" };
    let same = (have - want).abs() <= f64::EPSILON * have.abs().max(want.abs()).max(1.0);
    match op {
        "is" => same,
        "isNot" => !same,
        "gte" => have >= want || same,
        "lte" => have <= want || same,
        "lt" => have < want && !same,
        "gt" => have > want && !same,
        _ => false,
    }
}

fn date_matches(operator: i64, have: Option<&str>, want: &str, part: Part) -> Option<bool> {
    let Some(have) = have.filter(|h| h.len() >= 10) else { return Some(operator == 1) };
    let have = match part {
        Part::Day => have.get(..10)?,
        Part::Month => have.get(..7)?,
        Part::Year => have.get(..4)?,
    };
    let have = have.get(..want.len().min(have.len()))?;
    Some(match operator {
        0 => have == want,
        1 => have != want,
        14 => have > want,
        15 => have < want,
        _ => return None,
    })
}

fn text_of(key: &str, photo: &CatalogPhoto) -> String {
    let iptc = &photo.iptc;
    let value = match key {
        "_textSearch" => {
            let fields = [
                &photo.file_name,
                &iptc.title,
                &iptc.headline,
                &iptc.description,
                &iptc.creator,
                &iptc.copyright,
                &iptc.location,
                &iptc.city,
                &iptc.state,
                &iptc.country,
            ];
            let mut all: Vec<&str> = fields.iter().map(|s| s.as_str()).collect();
            all.extend(photo.keywords.iter().map(String::as_str));
            return all.join(" ");
        }
        "IB_S_CAMERA_SERIAL" => &photo.camera_serial,
        "IB_S_CAMERA_LENS" => &photo.lens,
        "IB_S_CAMERA_MAKE" => &photo.camera_make,
        "IB_S_CAMERA_MODEL" => &photo.camera_model,
        "IB_S_CONTACT_CREATOR" => &iptc.creator,
        "IB_S_CONTENT_DESCRIPTION" => &iptc.description,
        "IB_S_CONTENT_HEADLINE" => &iptc.headline,
        "IB_S_STATUS_TITLE" => &iptc.title,
        "IB_S_STATUS_COPYRIGHTNOTICE" => &iptc.copyright,
        "IB_S_STATUS_USAGETERMS" => &iptc.usage_terms,
        "IB_S_IMAGE_LOCATION" => &iptc.location,
        "IB_S_IMAGE_CITY" => &iptc.city,
        "IB_S_IMAGE_STATE" => &iptc.state,
        "IB_S_IMAGE_COUNTRY" => &iptc.country,
        "IB_S_IMAGE_ISOCOUNTRYCODE" => &iptc.country_code,
        _ => return String::new(),
    };
    value.clone()
}

fn number_of(key: &str, photo: &CatalogPhoto) -> Option<f64> {
    match key {
        "IB_I_BASIC_RATING" => Some(f64::from(photo.rating)),
        "IB_I_ISO" => photo.iso.map(f64::from),
        "IB_F_EXP_APERTURE" => photo.aperture.map(f64::from),
        "IB_F_EXP_FOCALLENGTH" => photo.focal_mm.map(f64::from),
        "IB_F_EXP_SHUTTERSPEED" => photo.shutter_seconds,
        "IB_S_EXP_EXPOSURECOMPENSATION" => photo.exposure_bias.map(f64::from),
        _ => None,
    }
}

// ---- the keys

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Text,
    Keywords,
    Number,
    Date(Part),
    Bool,
    Label,
    Aspect,
    /// File size or pixel size: no LightCraft rule, but the catalog has it.
    Size,
    /// Not in the table: an IPTC text field the catalog keeps (`IB_S_<column>`), else unknown.
    Stored,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Part {
    Day,
    Month,
    Year,
}

struct Key {
    key: &'static str,
    /// The LightCraft rule field, when there is one.
    lightcraft: Option<&'static str>,
    kind: Kind,
}

const fn key(key: &'static str, lightcraft: Option<&'static str>, kind: Kind) -> Key {
    Key { key, lightcraft, kind }
}

/// The column groups of `ZVARIANTMETADATA`, whose text the catalog import keeps with each photo.
const METADATA_GROUPS: &[&str] = &["CONTACT_", "CONTENT_", "IMAGE_", "STATUS_", "GETTY_"];

/// Every key seen in Capture One's rule dialog that the import understands.
const KEYS: &[Key] = &[
    key("_textSearch", Some("text"), Kind::Text),
    key("IB_I_BASIC_RATING", Some("rating"), Kind::Number),
    key("IB_S_BASIC_URGENCY", Some("label"), Kind::Label),
    key("IB_S_CONTENT_KEYWORDS", Some("keywords"), Kind::Keywords),
    key("IB_S_EXP_DATE", Some("captureDate"), Kind::Date(Part::Day)),
    key("_date_yearMonth", Some("captureDate"), Kind::Date(Part::Month)),
    key("_date_year", Some("captureDate"), Kind::Date(Part::Year)),
    key("orientation", Some("aspect"), Kind::Aspect),
    key("isGeoTagged", Some("hasGps"), Kind::Bool),
    // nothing is edited right after the import: Capture One's adjustments decide
    key("isModified", None, Kind::Bool),
    key("ANNOTATED", None, Kind::Bool),
    key("IB_S_FILE_SIZE", None, Kind::Size),
    key("IB_I_WIDTH", None, Kind::Size),
    key("IB_I_HEIGHT", None, Kind::Size),
    key("IB_S_CAMERA_SERIAL", Some("cameraSerial"), Kind::Text),
    key("IB_S_CAMERA_LENS", Some("lens"), Kind::Text),
    key("IB_S_CAMERA_MAKE", Some("camera"), Kind::Text),
    key("IB_S_CAMERA_MODEL", Some("camera"), Kind::Text),
    key("IB_I_ISO", Some("iso"), Kind::Number),
    key("IB_F_EXP_APERTURE", Some("aperture"), Kind::Number),
    key("IB_F_EXP_FOCALLENGTH", Some("focalLength"), Kind::Number),
    key("IB_F_EXP_SHUTTERSPEED", Some("shutterSpeed"), Kind::Number),
    key("IB_S_EXP_EXPOSURECOMPENSATION", Some("exposureBias"), Kind::Number),
    key("IB_S_CONTACT_CREATOR", Some("creator"), Kind::Text),
    key("IB_S_CONTENT_DESCRIPTION", Some("caption"), Kind::Text),
    key("IB_S_STATUS_TITLE", Some("title"), Kind::Text),
    key("IB_S_STATUS_COPYRIGHTNOTICE", Some("copyright"), Kind::Text),
    key("IB_S_IMAGE_LOCATION", Some("location"), Kind::Text),
    key("IB_S_IMAGE_CITY", Some("city"), Kind::Text),
    key("IB_S_IMAGE_STATE", Some("state"), Kind::Text),
    key("IB_S_IMAGE_COUNTRY", Some("country"), Kind::Text),
    key("IB_S_CONTENT_HEADLINE", None, Kind::Text),
    key("IB_S_STATUS_USAGETERMS", None, Kind::Text),
    key("IB_S_IMAGE_ISOCOUNTRYCODE", None, Kind::Text),
];

/// The rule's conditions as the recovery archive keeps them.
pub(super) fn archived(group: &Group) -> Value {
    let items: Vec<Value> = group
        .items
        .iter()
        .map(|item| match item {
            Item::Group(g) => archived(g),
            Item::Condition(c) => {
                let row: BTreeMap<&str, Value> =
                    [("enabled", json!(c.enabled)), ("key", json!(c.key)), ("operator", json!(c.operator)), ("criterion", json!(c.criterion))]
                        .into_iter()
                        .collect();
                json!(row)
            }
        })
        .collect();
    json!({ "all": group.all, "items": items })
}
