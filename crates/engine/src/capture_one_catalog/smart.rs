//! A smart album's rule (`ZCOLLECTION.ZSMARTCRITERIA`), as LightCraft smart album rules or, when
//! a condition has no LightCraft field, as the photos it selects in the catalog.
//!
//! The rule is XML: `<MatchOperator Kind="AND|OR">` ("all" / "any" of the rules in Capture One's
//! dialog) around groups and `<Condition Enabled="YES|NO">` elements, each with a `<Key>`, an
//! `<Operator>` and a `<Criterion>`. Keys are mostly `IB_<S|I|F>_<column>` after the catalog's
//! columns; operators are one numbering for every kind of field. Both were learned from smart
//! albums made in Capture One 16.3 and compared with its rule dialog: keys and operators not seen
//! there are not guessed, and a rule that uses one is named in a warning.

use std::collections::{BTreeMap, HashSet};

use lightcraft_catalog::{Match, Rule as LcRule, RuleSet};
use serde_json::{Value, json};

use super::notice::{self, Notice};
use super::{CatalogPhoto, ColorTag};

/// Longest rule read; Capture One's own are a few hundred bytes per condition.
pub(super) const MAX_RULE_BYTES: usize = 256 * 1024;
/// Deepest nesting of groups (Capture One writes two levels).
const MAX_DEPTH: usize = 16;
/// Most elements one rule may hold.
const MAX_NODES: usize = 20_000;

/// The columns of the catalog's `ZVARIANTMETADATA`, without the leading `Z`
/// ([`super::CaptureOneCatalog::metadata_columns`]).
pub(super) type Columns = std::collections::BTreeSet<String>;

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
    Selected(Vec<usize>, Vec<Notice>),
    /// Neither; the reason.
    Skipped(Notice),
}

/// Read a rule. An error says why it can't be.
pub(super) fn parse(xml: &str) -> Result<Group, Notice> {
    if xml.len() > MAX_RULE_BYTES {
        return Err(Notice::new(notice::RULE_TOO_LONG).count("kb", MAX_RULE_BYTES / 1024));
    }
    let limits = craft_xml::Limits { max_depth: MAX_DEPTH.saturating_mul(2), max_nodes: MAX_NODES, ..craft_xml::Limits::DEFAULT };
    let options = craft_xml::Options { strict: true, drop_blank_text: true, ..craft_xml::Options::default() }.with_limits(limits);
    let root = options.parse(xml.as_bytes()).map_err(|e| Notice::new(notice::RULE_UNREADABLE).text_value("error", e.to_string()))?;
    group(&root, 0)
}

fn group(el: &craft_xml::El, depth: usize) -> Result<Group, Notice> {
    if el.local() != "MatchOperator" || depth > MAX_DEPTH {
        return Err(Notice::new(notice::RULE_FORM));
    }
    let all = match el.attr("Kind").map(str::trim) {
        Some(kind) if kind.eq_ignore_ascii_case("AND") => true,
        Some(kind) if kind.eq_ignore_ascii_case("OR") => false,
        _ => return Err(Notice::new(notice::RULE_JOIN)),
    };
    let mut items = Vec::new();
    for child in el.elements() {
        match child.local() {
            "MatchOperator" => items.push(Item::Group(group(child, depth.saturating_add(1))?)),
            "Condition" => items.push(Item::Condition(condition(child)?)),
            _ => return Err(Notice::new(notice::RULE_FORM)),
        }
    }
    Ok(Group { all, items })
}

fn condition(el: &craft_xml::El) -> Result<Condition, Notice> {
    let text = |name: &str| el.child_text(name).map(|t| t.trim().to_string()).unwrap_or_default();
    let operator = text("Operator").parse::<i64>().map_err(|_| Notice::new(notice::RULE_NO_OPERATOR))?;
    Ok(Condition {
        enabled: !el.attr("Enabled").is_some_and(|e| e.trim().eq_ignore_ascii_case("NO")),
        key: text("Key"),
        operator,
        criterion: text("Criterion"),
    })
}

/// What `rule` becomes against the catalog's `photos`: LightCraft rules when every condition has a
/// LightCraft field, else the photos it selects when every condition can be told from the catalog.
pub(super) fn outcome(rule: &Group, photos: &[CatalogPhoto], columns: &Columns) -> Outcome {
    if !has_enabled(rule) {
        return Outcome::Skipped(Notice::new(notice::RULE_ALL_OFF));
    }
    let seen = Seen {
        // Capture One's "is" on keywords tells upper and lower case apart; LightCraft's doesn't
        spellings: photos.iter().flat_map(|p| p.keywords.iter()).flat_map(|k| std::iter::once(k.as_str()).chain(k.split('|'))).collect(),
        // LightCraft shows a photo turned as its file says, not as the user turned it
        turned_by_hand: photos.iter().any(|p| p.turned_by_hand),
    };
    let mut missing = Vec::new();
    let rules = to_rules(rule, &seen, &mut missing);
    if missing.is_empty() {
        return Outcome::Smart(rules);
    }
    match select(rule, photos, columns) {
        Some(selected) => Outcome::Selected(selected, missing),
        None => {
            let sample = photos.first().cloned().unwrap_or_default();
            Outcome::Skipped(Notice::new(notice::RULE_NOT_READ).notices("fields", unknown(rule, &sample, columns)))
        }
    }
}

/// The photos (indexes into `photos`) the rule selects; `None` when a condition tests something
/// the catalog import doesn't read.
pub(super) fn select(rule: &Group, photos: &[CatalogPhoto], columns: &Columns) -> Option<Vec<usize>> {
    let mut selected = Vec::new();
    for (at, photo) in photos.iter().enumerate() {
        if matches(rule, photo, columns)? {
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

/// What the catalog holds that decides whether a condition means the same in LightCraft.
struct Seen<'a> {
    /// Every keyword and level, as spelled.
    spellings: HashSet<&'a str>,
    /// A photo is shown turned by hand.
    turned_by_hand: bool,
}

/// The rule as LightCraft rules; the names of the conditions that have no LightCraft field go to
/// `missing` (the rules are then incomplete).
fn to_rules(group: &Group, seen: &Seen<'_>, missing: &mut Vec<Notice>) -> RuleSet {
    let mut rules = Vec::new();
    for item in &group.items {
        match item {
            Item::Group(g) => {
                if has_enabled(g) {
                    let mut group = to_rules(g, seen, missing);
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
            Item::Condition(c) => match to_rule(c, seen) {
                Some(rule) => rules.push(rule),
                None => {
                    let name = if same_but_case(c, &seen.spellings) {
                        Notice::new(notice::KEYWORDS_BY_CASE)
                    } else if turned_by_hand(c, seen) {
                        Notice::new(notice::ORIENTATION_BY_HAND)
                    } else {
                        name(c)
                    };
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

fn to_rule(c: &Condition, seen: &Seen<'_>) -> Option<LcRule> {
    let key = KEYS.iter().find(|k| k.key == c.key)?;
    let lc = key.lightcraft?;
    let value = c.criterion.as_str();
    if same_but_case(c, &seen.spellings) || turned_by_hand(c, seen) {
        return None;
    }
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
        Kind::Size | Kind::Stored | Kind::Unread => None,
    }
}

/// An orientation condition in a catalog with a photo turned by hand: LightCraft shows that photo
/// as its file is, so its aspect rule would select differently.
fn turned_by_hand(c: &Condition, seen: &Seen<'_>) -> bool {
    seen.turned_by_hand && KEYS.iter().any(|k| k.key == c.key && k.kind == Kind::Aspect)
}

/// A keyword "is" / "is not" condition whose keyword the catalog also has in another case
/// ("Nature" for "nature"): Capture One compares case by case, LightCraft doesn't, so the two would
/// select different photos.
fn same_but_case(c: &Condition, spellings: &HashSet<&str>) -> bool {
    let want = c.criterion.trim();
    c.key == "IB_S_CONTENT_KEYWORDS"
        && matches!(c.operator, 0 | 1 | 16 | 17)
        && spellings.iter().any(|have| have.trim() != want && have.trim().to_lowercase() == want.to_lowercase())
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
    // LightCraft's "camera" is make and model together, Capture One tests one of them: "contains
    // EOS" on the make finds nothing there, but a Canon EOS R5 here
    if lc == "camera" {
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

/// How a condition is named in a warning: by its field's name, LightCraft's where it has the
/// field, else as Capture One's key says it.
fn name(c: &Condition) -> Notice {
    let known = KEYS.iter().find(|k| k.key == c.key);
    if let Some(label) = known.and_then(|k| k.label.or_else(|| k.lightcraft.and_then(lightcraft_catalog::rules::field_label))) {
        return Notice::new(label);
    }
    let key = c.key.trim_start_matches("IB_");
    let key = key.split_once('_').filter(|(kind, _)| matches!(*kind, "S" | "I" | "F")).map_or(key, |(_, rest)| rest);
    if let Some((_, label)) = STORED_LABELS.iter().find(|(column, _)| *column == key) {
        return Notice::new(label);
    }
    Notice::new(notice::FIELD_KEY).text_value("key", key.trim_start_matches('_'))
}

/// The LightCraft field names a condition can be named by ([`name`]).
pub(super) fn lightcraft_labels() -> Vec<&'static str> {
    let mut out: Vec<&'static str> =
        KEYS.iter().filter(|k| k.label.is_none()).filter_map(|k| k.lightcraft.and_then(lightcraft_catalog::rules::field_label)).collect();
    out.dedup();
    out
}

fn unknown(group: &Group, photo: &CatalogPhoto, columns: &Columns) -> Vec<Notice> {
    let mut out = Vec::new();
    collect_unknown(group, photo, columns, &mut out);
    out
}

fn collect_unknown(group: &Group, photo: &CatalogPhoto, columns: &Columns, out: &mut Vec<Notice>) {
    for item in &group.items {
        match item {
            Item::Group(g) => collect_unknown(g, photo, columns, out),
            Item::Condition(c) if c.enabled && matches_condition(c, photo, columns).is_none() => {
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
pub(super) fn matches(group: &Group, photo: &CatalogPhoto, columns: &Columns) -> Option<bool> {
    let mut any = false;
    let mut all = true;
    for item in &group.items {
        let met = match item {
            Item::Group(g) if !has_enabled(g) => continue,
            Item::Group(g) => matches(g, photo, columns)?,
            Item::Condition(c) if !c.enabled => continue,
            Item::Condition(c) => matches_condition(c, photo, columns)?,
        };
        any |= met;
        all &= met;
    }
    Some(if group.all { all } else { any })
}

fn matches_condition(c: &Condition, photo: &CatalogPhoto, columns: &Columns) -> Option<bool> {
    let key = KEYS.iter().find(|k| k.key == c.key);
    let kind = key.map_or(Kind::Stored, |k| k.kind);
    match kind {
        Kind::Text => text_matches(c.operator, &text_of(&c.key, photo), &c.criterion),
        Kind::Stored => stored_matches(c, photo, columns),
        Kind::Unread => None,
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
            // as Capture One shows the photo: turned by the file's orientation or the user
            let (width, height) = if photo.turned? { (photo.height, photo.width) } else { (photo.width, photo.height) };
            let have = match width.cmp(&height) {
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
fn stored_matches(c: &Condition, photo: &CatalogPhoto, columns: &Columns) -> Option<bool> {
    let (kind, column) = c.key.strip_prefix("IB_")?.split_once('_')?;
    let have = match photo.exif.get(column) {
        Some(value) => value.as_str(),
        // only filled IPTC fields are kept with a photo: a column the catalog's metadata table
        // has is empty where a photo lacks it, one it doesn't have can't be told
        None if kind == "S" && columns.contains(column) => photo.iptc.other.get(column).map(String::as_str).unwrap_or(""),
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
    // an operator Capture One doesn't use on keywords is unknown for every photo, not only for
    // those that have keywords to compare
    if !matches!(operator, 0 | 1 | 6 | 8 | 9 | 10 | 16 | 17) {
        return None;
    }
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
    // "is" tells upper and lower case apart (Capture One's own dialog finds "Nature" with
    // "Nature" but not with "nature"); "contains" doesn't
    let exact = || keywords.iter().any(|keyword| std::iter::once(keyword.as_str()).chain(keyword.split('|')).any(|name| name.trim() == want.trim()));
    match operator {
        0 | 16 => Some(exact()),
        1 | 17 => Some(!exact()),
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
        "IB_F_EXP_APERTURE" => photo.aperture.map(decimal),
        "IB_F_EXP_FOCALLENGTH" => photo.focal_mm.map(decimal),
        "IB_F_EXP_SHUTTERSPEED" => photo.shutter_seconds,
        "IB_S_EXP_EXPOSURECOMPENSATION" => photo.exposure_bias.map(decimal),
        _ => None,
    }
}

/// An `f32` as the `f64` of its shortest decimal form (5.6f32 → 5.6, not 5.599999904632568), so it
/// equals the number typed in Capture One's dialog, as LightCraft's own rules compare it.
fn decimal(value: f32) -> f64 {
    value.to_string().parse().unwrap_or(f64::from(value))
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
    /// Known, but not read by the catalog import.
    Unread,
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
    /// How a warning names a field LightCraft has no rule for.
    label: Option<&'static str>,
}

const fn key(key: &'static str, lightcraft: Option<&'static str>, kind: Kind) -> Key {
    Key { key, lightcraft, kind, label: None }
}

const fn own(key: &'static str, kind: Kind, label: &'static str) -> Key {
    Key { key, lightcraft: None, kind, label: Some(label) }
}

/// Names for the image's columns a rule can test that have no LightCraft field, seen in Capture
/// One's dialog; others go by their key.
const STORED_LABELS: &[(&str, &str)] =
    &[("EXP_FLASHMODE", notice::FLASH_MODE), ("EXP_FLASHCOMPENSATION", notice::FLASH_COMPENSATION), ("CAMERA_OWNER", notice::CAMERA_OWNER)];

/// The keywords the rule's switched-on "is" / "is not" conditions name, as spelled: Capture One
/// tells case apart there, so the library must not hold them in another spelling either.
pub(super) fn exact_keywords(group: &Group) -> Vec<String> {
    let mut out = Vec::new();
    for item in &group.items {
        match item {
            Item::Group(g) => out.extend(exact_keywords(g)),
            Item::Condition(c) if c.enabled && c.key == "IB_S_CONTENT_KEYWORDS" && matches!(c.operator, 0 | 1 | 16 | 17) => {
                out.push(c.criterion.trim().to_string());
            }
            Item::Condition(_) => {}
        }
    }
    out
}

/// Whether a smart album's rule (as the catalog stores it) has a condition on orientation, which
/// needs [`CatalogPhoto::turned`].
pub(super) fn tests_orientation(rule: &str) -> bool {
    fn uses(group: &Group) -> bool {
        group.items.iter().any(|item| match item {
            Item::Group(g) => uses(g),
            Item::Condition(c) => c.enabled && KEYS.iter().any(|k| k.key == c.key && k.kind == Kind::Aspect),
        })
    }
    rule.contains("orientation") && parse(rule).is_ok_and(|group| uses(&group))
}

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
    own("isModified", Kind::Bool, notice::ADJUSTED_FIELD),
    own("ANNOTATED", Kind::Bool, notice::ANNOTATION),
    own("IB_S_FILE_SIZE", Kind::Size, notice::FILE_SIZE),
    own("IB_I_WIDTH", Kind::Size, notice::WIDTH),
    own("IB_I_HEIGHT", Kind::Size, notice::HEIGHT),
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
    own("IB_S_CONTENT_HEADLINE", Kind::Text, notice::HEADLINE),
    own("IB_S_STATUS_USAGETERMS", Kind::Text, notice::USAGE_TERMS),
    own("IB_S_IMAGE_ISOCOUNTRYCODE", Kind::Text, notice::ISO_COUNTRY_CODE),
    // Live Comments: not in the catalog's tables the import reads
    own("commentsKeys", Kind::Unread, notice::LIVE_COMMENTS),
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
