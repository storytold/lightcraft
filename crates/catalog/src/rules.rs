//! Smart-album rules: a list of conditions matched all / any / none, with nested groups.
//!
//! Each [`Rule`] is `{field, op, value}`; [`FIELDS`] lists the fields with their kind, which
//! decides the operators ([`ops_for`]). Text compares case-insensitively; dates are ISO strings
//! compared by prefix; "in the last N days/weeks/months/years" is relative to [`now`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Catalog, Photo, Source};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Match {
    #[default]
    All,
    Any,
    None,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RuleSet {
    #[serde(default, rename = "match")]
    pub mode: Match,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Rule {
    /// A nested group with its own match mode.
    Group { group: RuleSet },
    Field {
        field: String,
        op: String,
        #[serde(default)]
        value: Value,
    },
}

/// How a field's value is compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// A list of names (keywords, people): text ops over each name (any name matches; `isEmpty` =
    /// none).
    Keywords,
    Number,
    Date,
    /// An album or a smart album (not a folder), by id: in it or not.
    Album,
    /// One of a fixed set: (id, label). Rules store the id; people see the label.
    Choice(&'static [(&'static str, &'static str)]),
    Bool,
}

/// The fields rules can test: (id, label, kind), in field-menu order ([`TOP_LEVEL_FIELDS`], then
/// each of [`FIELD_GROUPS`]).
pub const FIELDS: &[(&str, &str, Kind)] = &[
    ("rating", "Rating", Kind::Number),
    ("flag", "Pick Flag", Kind::Choice(&[("pick", "Picked"), ("reject", "Rejected"), ("none", "Unflagged")])),
    (
        "label",
        "Color Label",
        Kind::Choice(&[("red", "Red"), ("yellow", "Yellow"), ("green", "Green"), ("blue", "Blue"), ("purple", "Purple"), ("none", "No Label")]),
    ),
    ("text", "Any Searchable Text", Kind::Text),
    // Source
    ("album", "Album", Kind::Album),
    ("virtualCopy", "Virtual Copy", Kind::Bool),
    ("copyName", "Copy Name", Kind::Text),
    ("stacked", "In a Stack", Kind::Bool),
    // File
    ("fileName", "Filename", Kind::Text),
    ("extension", "File Extension", Kind::Text),
    ("filePath", "File Path", Kind::Text),
    ("kind", "File Type", Kind::Choice(&[("image", "Image"), ("raw", "Raw"), ("video", "Video")])),
    ("format", "File Format", Kind::Text),
    ("duration", "Video Duration", Kind::Number),
    // Date
    ("captureDate", "Capture Date", Kind::Date),
    ("importDate", "Import Date", Kind::Date),
    ("editDate", "Edit Date", Kind::Date),
    // Keywords & People
    ("keywords", "Keywords", Kind::Keywords),
    ("keywordCount", "Keyword Count", Kind::Number),
    ("person", "People", Kind::Keywords),
    ("personCount", "People Count", Kind::Number),
    // Description
    ("title", "Title", Kind::Text),
    ("caption", "Caption", Kind::Text),
    ("altText", "Alt Text", Kind::Text),
    ("creator", "Creator", Kind::Text),
    ("copyright", "Copyright", Kind::Text),
    (
        "copyrightStatus",
        "Copyright Status",
        Kind::Choice(&[("copyrighted", "Copyrighted"), ("publicDomain", "Public Domain"), ("unknown", "Unknown")]),
    ),
    // Camera Info
    ("camera", "Camera", Kind::Text),
    ("lens", "Lens", Kind::Text),
    ("focalLength", "Focal Length", Kind::Number),
    ("aperture", "Aperture", Kind::Number),
    ("shutterSpeed", "Shutter Speed", Kind::Number),
    ("iso", "ISO Speed", Kind::Number),
    // Location
    ("location", "Location", Kind::Text),
    ("city", "City", Kind::Text),
    ("state", "State / Province", Kind::Text),
    ("country", "Country", Kind::Text),
    ("hasGps", "Has GPS", Kind::Bool),
    // Size
    ("longEdge", "Long Edge", Kind::Number),
    ("shortEdge", "Short Edge", Kind::Number),
    ("aspect", "Aspect Ratio", Kind::Choice(&[("landscape", "Landscape (wide)"), ("portrait", "Portrait (tall)"), ("square", "Square")])),
    ("megapixels", "Megapixels", Kind::Number),
    // Develop
    ("edited", "Has Edits", Kind::Bool),
    ("cropped", "Cropped", Kind::Bool),
    ("treatment", "Treatment", Kind::Choice(&[("color", "In Color"), ("monochrome", "Black & White")])),
    // Assisted Culling
    ("sharpness", "Focus (assisted culling)", Kind::Number),
    ("bestOfGroup", "Best of Similar Shots", Kind::Bool),
];

/// The fields the field menu shows at its top level, before the groups.
pub const TOP_LEVEL_FIELDS: &[&str] = &["rating", "flag", "label", "text"];

/// The field menu's submenus: (label, fields), in order. Every field of [`FIELDS`] is either here
/// once or in [`TOP_LEVEL_FIELDS`].
pub const FIELD_GROUPS: &[(&str, &[&str])] = &[
    ("Source", &["album", "virtualCopy", "copyName", "stacked"]),
    ("File", &["fileName", "extension", "filePath", "kind", "format", "duration"]),
    ("Date", &["captureDate", "importDate", "editDate"]),
    ("Keywords & People", &["keywords", "keywordCount", "person", "personCount"]),
    ("Description", &["title", "caption", "altText", "creator", "copyright", "copyrightStatus"]),
    ("Camera Info", &["camera", "lens", "focalLength", "aperture", "shutterSpeed", "iso"]),
    ("Location", &["location", "city", "state", "country", "hasGps"]),
    ("Size", &["longEdge", "shortEdge", "aspect", "megapixels"]),
    ("Develop", &["edited", "cropped", "treatment"]),
    ("Assisted Culling", &["sharpness", "bestOfGroup"]),
];

/// The field-menu group `field` sits in; `None` for a top-level or unknown field.
pub fn field_group(field: &str) -> Option<&'static str> {
    FIELD_GROUPS.iter().find(|g| g.1.contains(&field)).map(|g| g.0)
}

/// A rule that can't mean anything ([`RuleSet::check`]): where it is, what is wrong in English
/// (for agents and logs), and the field and kind of issue, so the editor can say it in any language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// Positions from the top rule list down: `[1, 0]` is the first rule of the second rule's group.
    pub path: Vec<usize>,
    /// The rule's field; `None` for a group.
    pub field: Option<String>,
    pub issue: Issue,
    pub message: String,
}

impl std::fmt::Display for Problem {
    /// `rule 2.1: no rating 0–5 is 9` (counting from 1, as people do).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let at: Vec<String> = self.path.iter().map(|i| (i.saturating_add(1)).to_string()).collect();
        // a problem with the filter around the rules (its album field) has no rule to point at
        if at.is_empty() { write!(f, "{}", self.message) } else { write!(f, "rule {}: {}", at.join("."), self.message) }
    }
}

/// What kind of thing is wrong with a rule ([`Problem`]); [`Issue::text`] says it after the
/// field's name ("Title: needs something to look for").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Issue {
    UnknownField,
    NoSuchOperator,
    NotYesNo,
    NeedsNumber,
    NeedsShutterSpeed,
    NeedsDate,
    DatesReversed,
    NeedsTwoValues,
    NeedsCount,
    UnknownUnit,
    NotAChoice,
    NeedsTextOrEmpty,
    NeedsTextOrNotEmpty,
    /// Keywords and People: their operators say "are empty".
    NeedsNamesOrEmpty,
    NeedsNamesOrNotEmpty,
    NeedsText,
    NoRatingMatches,
    ChooseAlbum,
    NoSuchAlbum,
    AlbumLoop,
    FolderAlbum,
    EmptyGroup,
}

impl Issue {
    pub const ALL: [Issue; 22] = [
        Issue::UnknownField,
        Issue::NoSuchOperator,
        Issue::NotYesNo,
        Issue::NeedsNumber,
        Issue::NeedsShutterSpeed,
        Issue::NeedsDate,
        Issue::DatesReversed,
        Issue::NeedsTwoValues,
        Issue::NeedsCount,
        Issue::UnknownUnit,
        Issue::NotAChoice,
        Issue::NeedsTextOrEmpty,
        Issue::NeedsTextOrNotEmpty,
        Issue::NeedsNamesOrEmpty,
        Issue::NeedsNamesOrNotEmpty,
        Issue::NeedsText,
        Issue::NoRatingMatches,
        Issue::ChooseAlbum,
        Issue::NoSuchAlbum,
        Issue::AlbumLoop,
        Issue::FolderAlbum,
        Issue::EmptyGroup,
    ];

    /// The issue in a few words, to follow the field's name; a message for translation catalogs.
    pub fn text(self) -> &'static str {
        match self {
            Issue::UnknownField => "isn't a field rules know",
            Issue::NoSuchOperator => "can't be tested that way",
            Issue::NotYesNo => "is yes or no",
            Issue::NeedsNumber => "needs a number",
            Issue::NeedsShutterSpeed => "needs a time like 1/250 or 2",
            Issue::NeedsDate => "needs a date like 2026, 2026-08 or 2026-08-14",
            Issue::DatesReversed => "has the first date after the second",
            Issue::NeedsTwoValues => "needs two values",
            Issue::NeedsCount => "needs a number more than 0",
            Issue::UnknownUnit => "needs hours, days, weeks, months or years",
            Issue::NotAChoice => "isn't one of the choices",
            Issue::NeedsTextOrEmpty => "needs something to look for (or use “is empty”)",
            Issue::NeedsTextOrNotEmpty => "needs something to look for (or use “isn't empty”)",
            Issue::NeedsNamesOrEmpty => "needs something to look for (or use “are empty”)",
            Issue::NeedsNamesOrNotEmpty => "needs something to look for (or use “aren't empty”)",
            Issue::NeedsText => "needs text",
            Issue::NoRatingMatches => "can't match any rating from 0 to 5",
            Issue::ChooseAlbum => "needs an album",
            Issue::NoSuchAlbum => "names an album that no longer exists",
            Issue::AlbumLoop => "would make this album include itself through other smart albums",
            Issue::FolderAlbum => "can't test a folder; choose an album in it",
            Issue::EmptyGroup => "This group is empty: add a rule or remove it.",
        }
    }
}

/// An issue with its English message, or nothing wrong.
type Found = Option<(Issue, String)>;

/// What is wrong with one rule, if anything ([`RuleSet::check`]).
fn field_problem(field: &str, op: &str, value: &Value, cat: &Catalog, owner: Option<crate::AlbumId>) -> Found {
    let Some(kind) = field_kind(field) else { return Some((Issue::UnknownField, format!("unknown field `{field}`"))) };
    if !ops_for(kind).iter().any(|o| o.0 == op) {
        return Some((Issue::NoSuchOperator, format!("`{field}` has no operator `{op}`")));
    }
    if matches!(op, "isEmpty" | "isNotEmpty") {
        return None;
    }
    match kind {
        Kind::Bool => bool_value(value).is_none().then(|| (Issue::NotYesNo, format!("`{field}` is yes or no, not {value}"))),
        Kind::Album => album_problem(value, cat, owner),
        Kind::Number => {
            let each = match value {
                Value::Array(a) if op == "between" && a.len() == 2 => a.iter().find_map(|v| number_problem(field, v)),
                _ if op == "between" => Some((Issue::NeedsTwoValues, format!("`{field}` between needs two values, not {value}"))),
                v => number_problem(field, v),
            };
            // a rating rule no rating could meet ("is 9", "≥ 7"); "< 6" is fine, if broad
            each.or_else(|| {
                let label = ops_for(kind).iter().find(|o| o.0 == op).map_or(op, |o| o.1);
                (field == "rating" && !(0..=5).any(|r| num_op(op, Some(f64::from(r)), value)))
                    .then(|| (Issue::NoRatingMatches, format!("no rating 0–5 {label} {value}")))
            })
        }
        Kind::Date => date_problem(field, op, value),
        Kind::Choice(choices) => choice_problem(field, value, choices, cat),
        Kind::Text | Kind::Keywords => match value {
            Value::String(s) if !s.trim().is_empty() => None,
            Value::String(_) | Value::Null => {
                // the hint names the operator as the menu does: Keywords and People say "are empty"
                let not = matches!(op, "isNot" | "notContains");
                let (issue, instead) = match (kind, not) {
                    (Kind::Keywords, false) => (Issue::NeedsNamesOrEmpty, "are empty"),
                    (Kind::Keywords, true) => (Issue::NeedsNamesOrNotEmpty, "aren't empty"),
                    (_, false) => (Issue::NeedsTextOrEmpty, "is empty"),
                    (_, true) => (Issue::NeedsTextOrNotEmpty, "isn't empty"),
                };
                Some((issue, format!("`{field}` needs something to look for (or use “{instead}”)")))
            }
            Value::Number(_) => None,
            other => Some((Issue::NeedsText, format!("`{field}` needs text, not {other}"))),
        },
    }
}

fn number_problem(field: &str, v: &Value) -> Found {
    if field == "shutterSpeed" {
        return shutter_value(v).is_null().then(|| (Issue::NeedsShutterSpeed, format!("`shutterSpeed` needs a time like 1/250 or 2, not {v}")));
    }
    number(v).filter(|n| n.is_finite()).is_none().then(|| (Issue::NeedsNumber, format!("`{field}` needs a number, not {v}")))
}

/// The album an Album rule names: its id as a whole number, written as a number (`3`, `3.0`) or a
/// numeric string (`"3"`). The check, the matcher and the editor all read it this way.
pub fn album_rule_id(value: &Value) -> Option<crate::AlbumId> {
    number(value).filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0).map(|n| crate::AlbumId(n as u64))
}

/// An Album rule's value: an album that exists and holds photos (a plain or a smart album, not a
/// folder of albums), and, for the rules of `owner`, not one that leads back to it (a smart album
/// testing itself, directly or through others).
fn album_problem(v: &Value, cat: &Catalog, owner: Option<crate::AlbumId>) -> Found {
    if v.is_null() {
        return Some((Issue::ChooseAlbum, "choose an album".into()));
    }
    match album_rule_id(v).and_then(|id| cat.album(id)) {
        None => Some((Issue::NoSuchAlbum, format!("no album {v}"))),
        Some(a) if owner.is_some_and(|o| a.id == o || cat.album_reaches(a.id, o)) => {
            Some((Issue::AlbumLoop, format!("album {v} would make this album include itself")))
        }
        Some(a) if a.folder => Some((Issue::FolderAlbum, format!("album {v} is a folder; choose an album in it"))),
        Some(_) => None,
    }
}

/// A rule date in its one reading: `2026`, `2026-08`, `2026-08-14`, then optionally `T10`,
/// `T10:00` or `T10:00:00` (a space works as the `T`), and after whole seconds a fraction and a
/// time zone as imported capture times carry them (`.250`, `+02:00`, `Z`); a real date and time
/// (no month 13, no 30 February, no 25 o'clock). Anything else is [`Issue::NeedsDate`]. The check
/// and the matcher both read dates this way.
pub fn rule_date(s: &str) -> Result<String, Issue> {
    const PATTERN: &str = "dddd-dd-ddTdd:dd:dd";
    let s: String = s.trim().char_indices().map(|(i, c)| if i == 10 && c == ' ' { 'T' } else { c }).collect();
    let shaped = |t: &str| {
        matches!(t.len(), 4 | 7 | 10 | 13 | 16 | 19)
            && t.chars().zip(PATTERN.chars()).all(|(c, p)| if p == 'd' { c.is_ascii_digit() } else { c == p })
    };
    // after whole seconds, a fraction and a zone as imports write them (.250, +02:00 or Z)
    let tail_ok = |rest: &str| {
        let after_fraction = match rest.strip_prefix('.') {
            Some(f) => {
                let digits = f.bytes().take_while(u8::is_ascii_digit).count();
                if !(1..=9).contains(&digits) {
                    return false;
                }
                f.get(digits..).unwrap_or("")
            }
            None => rest,
        };
        let zone = after_fraction.as_bytes();
        match zone {
            [] | [b'Z' | b'z'] => true,
            [b'+' | b'-', h1, h2, b':', m1, m2] => [h1, h2, m1, m2].iter().all(|c| c.is_ascii_digit()),
            _ => false,
        }
    };
    if !(shaped(&s) || s.get(..19).is_some_and(shaped) && s.get(19..).is_some_and(tail_ok)) {
        return Err(Issue::NeedsDate);
    }
    let part = |a: usize, b: usize| s.get(a..b).and_then(|t| t.parse::<u32>().ok());
    let time_ok = part(11, 13).is_none_or(|h| h < 24) && part(14, 16).is_none_or(|m| m < 60) && part(17, 19).is_none_or(|x| x < 60);
    let day = match s.len().min(19) {
        4 => format!("{s}-01-01"),
        7 => format!("{s}-01"),
        _ => s.get(..10).unwrap_or("").to_string(),
    };
    if time_ok && crate::dates::normalize_iso(&day).is_some() { Ok(s) } else { Err(Issue::NeedsDate) }
}

fn date_problem(field: &str, op: &str, value: &Value) -> Found {
    let date = |v: &Value| match v.as_str().map(rule_date) {
        Some(Ok(_)) => None,
        _ => Some((Issue::NeedsDate, format!("`{field}` needs a date like 2026, 2026-08 or 2026-08-14, not {v}"))),
    };
    match op {
        "inLast" | "notInLast" => {
            let (n, unit) = match value {
                Value::Object(o) => (o.get("n").and_then(number), o.get("unit").cloned().unwrap_or(Value::Null)),
                v => (number(v), Value::Null),
            };
            if !n.is_some_and(|n| n.is_finite() && n > 0.0) {
                return Some((Issue::NeedsCount, format!("`{field}` needs a number of hours, days, weeks, months or years more than 0")));
            }
            unit_secs(&unit).is_none().then(|| (Issue::UnknownUnit, format!("`{field}`: unknown unit {unit} (hours, days, weeks, months or years)")))
        }
        "between" => match value {
            Value::Array(a) if a.len() == 2 => a.iter().find_map(date).or_else(|| {
                let bound = |i: usize| a.get(i).and_then(Value::as_str).and_then(|d| rule_date(d).ok()).unwrap_or_default();
                let (from, to) = (bound(0), bound(1));
                (from > to && !from.starts_with(&to)).then(|| (Issue::DatesReversed, format!("`{field}`: the first date is after the second")))
            }),
            _ => Some((Issue::NeedsTwoValues, format!("`{field}` between needs two dates, not {value}"))),
        },
        _ => date(value),
    }
}

fn choice_problem(field: &str, value: &Value, choices: &[(&str, &str)], cat: &Catalog) -> Found {
    let known = value.as_str().is_some_and(|s| {
        choices.iter().any(|c| c.0.eq_ignore_ascii_case(s.trim()))
            || match field {
                "flag" => matches!(s.trim().to_lowercase().as_str(), "picked" | "rejected"),
                // a colour's name or the custom name given to it
                "label" => cat.label_from_name(s).is_some(),
                "copyrightStatus" => crate::CopyrightStatus::parse(s).is_some(),
                _ => false,
            }
    });
    let ids: Vec<&str> = choices.iter().map(|c| c.0).collect();
    (!known).then(|| (Issue::NotAChoice, format!("`{field}` is one of {}, not {value}", ids.join(", "))))
}

thread_local! {
    /// The smart albums whose rules are being evaluated on this thread, outermost first.
    static EVALUATING: std::cell::RefCell<Vec<crate::AlbumId>> = const { std::cell::RefCell::new(Vec::new()) };
}

thread_local! {
    /// While one outermost question is answered (is this photo in album X?): what each smart album
    /// it leads to answered for each photo, so an album tested many times along the way (X tests Y
    /// twice, Y tests Z twice…) is worked out once, not 2^depth times. Cleared when it ends.
    static ANSWERS: std::cell::RefCell<std::collections::HashMap<(crate::AlbumId, crate::PhotoId), bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

thread_local! {
    /// While [`AlbumPass`] is alive on this thread: the photos of each smart album that was counted whole, with
    /// the longest chain of smart albums (itself included) its rules lead through.
    static PASS: std::cell::RefCell<Option<std::collections::HashMap<crate::AlbumId, (usize, crate::tally::Members)>>> =
        const { std::cell::RefCell::new(None) };
}

/// One pass over one catalog state that counts every smart album whole. Each album put in with `remember`
/// answers [`smart_album_holds`] from its photos, not from its rules, until the pass is dropped (it can't outlive
/// the state it was made of: it exists only inside the counting function).
///
/// Only an album whose rules lead to no loop may be remembered, and only with the height of its chain: its answer
/// then doesn't depend on which albums are being evaluated around it, as long as the chain still fits under
/// [`MAX_ALBUM_DEPTH`] from where it is asked (checked on every use; otherwise the rules are evaluated as before).
pub(crate) struct AlbumPass {
    outer: Option<std::collections::HashMap<crate::AlbumId, (usize, crate::tally::Members)>>,
}

impl AlbumPass {
    pub(crate) fn begin() -> AlbumPass {
        AlbumPass { outer: PASS.with_borrow_mut(|p| p.replace(std::collections::HashMap::new())) }
    }

    /// `id` holds exactly `members`, and its rules lead through a chain of `height` smart albums (itself included),
    /// none of them in a loop.
    pub(crate) fn remember(&self, id: crate::AlbumId, height: usize, members: crate::tally::Members) {
        PASS.with_borrow_mut(|p| {
            if let Some(p) = p {
                p.insert(id, (height, members));
            }
        });
    }

    /// Ends the pass, giving back what was remembered.
    pub(crate) fn finish(self) -> std::collections::HashMap<crate::AlbumId, crate::tally::Members> {
        PASS.with_borrow_mut(Option::take).unwrap_or_default().into_iter().map(|(id, (_, members))| (id, members)).collect()
    }
}

impl Drop for AlbumPass {
    fn drop(&mut self) {
        let outer = self.outer.take();
        PASS.with_borrow_mut(|p| *p = outer);
    }
}

/// What a counting pass knows of smart album `id` for `photo`, when asking now can't give another answer than
/// evaluating its rules would: the chain below it fits under [`MAX_ALBUM_DEPTH`] from the albums being evaluated.
fn pass_answer(id: crate::AlbumId, photo: crate::PhotoId) -> Option<bool> {
    PASS.with_borrow(|p| {
        let (height, members) = p.as_ref()?.get(&id)?;
        let depth = EVALUATING.with_borrow(Vec::len);
        (depth + height <= MAX_ALBUM_DEPTH).then(|| members.holds(photo))
    })
}

#[cfg(test)]
thread_local! {
    /// How many times a smart album's rules were worked out on this thread (tests count them).
    pub(crate) static EVALUATED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Whether `photo` is in smart album `id`, by `matches` (its rules), remembered for the rest of the
/// outermost question and guarded by [`evaluating`] (a loop or a chain too deep holds nothing).
pub(crate) fn smart_album_holds(id: crate::AlbumId, photo: crate::PhotoId, matches: impl FnOnce() -> bool) -> bool {
    if let Some(known) = pass_answer(id, photo) {
        return known;
    }
    if let Some(known) = ANSWERS.with_borrow(|a| a.get(&(id, photo)).copied()) {
        return known;
    }
    #[cfg(test)]
    EVALUATED.with(|n| n.set(n.get() + 1));
    // the outermost question forgets the answers when it ends, however it ends
    struct Forget(bool);
    impl Drop for Forget {
        fn drop(&mut self) {
            if self.0 {
                ANSWERS.with_borrow_mut(std::collections::HashMap::clear);
            }
        }
    }
    let _forget = Forget(EVALUATING.with_borrow(Vec::is_empty));
    let answer = evaluating(id, matches);
    if let Some(a) = answer {
        ANSWERS.with_borrow_mut(|known| known.insert((id, photo), a));
    }
    answer.unwrap_or(false)
}

/// How deep smart albums may test smart albums before the innermost counts as holding nothing.
pub(crate) const MAX_ALBUM_DEPTH: usize = 32;

/// Run `f` as the evaluation of smart album `id`'s rules: `None` (so it holds nothing) when `id`
/// is already being evaluated (a loop the check would refuse, saved anyway) or the chain is
/// deeper than [`MAX_ALBUM_DEPTH`]. Loops cost one pass round them, never a stack overflow.
pub(crate) fn evaluating<T>(id: crate::AlbumId, f: impl FnOnce() -> T) -> Option<T> {
    let entered = EVALUATING.with_borrow_mut(|stack| {
        let ok = !stack.contains(&id) && stack.len() < MAX_ALBUM_DEPTH;
        if ok {
            stack.push(id);
        }
        ok
    });
    if !entered {
        return None;
    }
    // leaves the stack as it was, however `f` ends
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            EVALUATING.with_borrow_mut(|stack| {
                stack.pop();
            });
        }
    }
    let _leave = Leave;
    Some(f())
}

/// How a summary names an Album rule's album: “Name”, or `#9` when there is no such album.
pub fn album_name(value: &Value, cat: Option<&Catalog>) -> String {
    let id = album_rule_id(value);
    match id.and_then(|id| cat.and_then(|c| c.album(id))) {
        Some(a) => format!("“{}”", a.name),
        None => id.map_or_else(|| value.to_string(), |id| format!("#{}", id.0)),
    }
}

/// What a yes/no rule's value means: `true`, `"yes"`, `1` (or `1.0`) or no value is yes; `false`,
/// `"no"`, `0` is no (strings in any case); `None` for anything else, which matches nothing.
pub fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(b) => Some(*b),
        Value::Null => Some(true),
        Value::Number(n) => match n.as_f64() {
            Some(1.0) => Some(true),
            Some(0.0) => Some(false),
            _ => None,
        },
        Value::String(s) => match s.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// The display label of a yes/no value.
pub fn bool_label(b: bool) -> &'static str {
    if b { "Yes" } else { "No" }
}

/// The display label of choice `id` of `field` ("Public Domain" for `publicDomain`); `None` when
/// `field` isn't a choice field or has no such choice.
pub fn choice_label(field: &str, id: &str) -> Option<&'static str> {
    let Some(Kind::Choice(choices)) = field_kind(field) else { return None };
    choices.iter().find(|c| c.0 == id).map(|c| c.1)
}

/// The display label of `field`; `None` for an unknown field.
pub fn field_label(field: &str) -> Option<&'static str> {
    FIELDS.iter().find(|f| f.0 == field).map(|f| f.1)
}

/// The operators for a field kind: (id, label).
pub fn ops_for(kind: Kind) -> &'static [(&'static str, &'static str)] {
    match kind {
        Kind::Text => &[
            ("contains", "contains"),
            ("notContains", "doesn't contain"),
            ("is", "is"),
            ("isNot", "isn't"),
            ("startsWith", "starts with"),
            ("endsWith", "ends with"),
            ("isEmpty", "is empty"),
            ("isNotEmpty", "isn't empty"),
        ],
        Kind::Keywords => &[
            ("contains", "contains"),
            ("notContains", "doesn't contain"),
            ("is", "is"),
            ("startsWith", "starts with"),
            ("isEmpty", "are empty"),
            ("isNotEmpty", "aren't empty"),
        ],
        Kind::Number => {
            &[("is", "is"), ("isNot", "isn't"), ("gte", "is ≥"), ("lte", "is ≤"), ("gt", "is >"), ("lt", "is <"), ("between", "is between")]
        }
        Kind::Date => &[
            ("is", "is"),
            ("after", "is after"),
            ("before", "is before"),
            ("between", "is between"),
            ("inLast", "is in the last"),
            ("notInLast", "isn't in the last"),
            ("isEmpty", "is unknown"),
        ],
        Kind::Choice(_) | Kind::Album => &[("is", "is"), ("isNot", "isn't")],
        Kind::Bool => &[("is", "is")],
    }
}

pub fn field_kind(field: &str) -> Option<Kind> {
    FIELDS.iter().find(|f| f.0 == field).map(|f| f.2)
}

thread_local! {
    static NOW: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Set the time "in the last…" rules count back from (the engine's clock; tests).
pub fn set_now(iso: Option<String>) {
    NOW.with(|n| *n.borrow_mut() = iso);
}

/// The current time as ISO 8601: [`set_now`]'s, else the system clock (UTC).
pub fn now() -> String {
    if let Some(n) = NOW.with(|n| n.borrow().clone()) {
        return n;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        crate::dates::civil(secs)
    }
    #[cfg(target_arch = "wasm32")]
    {
        "2026-01-01T00:00:00".to_string()
    }
}

/// A flag as rules name it (its variant, lower case), without building a string per photo.
fn flag_name(f: crate::Flag) -> &'static str {
    match f {
        crate::Flag::None => "none",
        crate::Flag::Pick => "pick",
        crate::Flag::Reject => "reject",
    }
}

/// A colour label's colour as rules name it (lower case).
fn label_colour(l: crate::ColorLabel) -> &'static str {
    match l {
        crate::ColorLabel::Red => "red",
        crate::ColorLabel::Yellow => "yellow",
        crate::ColorLabel::Green => "green",
        crate::ColorLabel::Blue => "blue",
        crate::ColorLabel::Purple => "purple",
    }
}

/// A media kind as rules name it (lower case).
fn kind_name(k: crate::MediaKind) -> &'static str {
    match k {
        crate::MediaKind::Image => "image",
        crate::MediaKind::Raw => "raw",
        crate::MediaKind::Video => "video",
    }
}

fn lower(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_lowercase(),
        Value::Null => String::new(),
        other => other.to_string().to_lowercase(),
    }
}

fn text_op(op: &str, have: &str, want: &str) -> bool {
    let have = have.trim().to_lowercase();
    match op {
        "contains" => want.split_whitespace().all(|w| have.contains(w)),
        "notContains" => !want.split_whitespace().any(|w| have.contains(w)),
        "is" => have == want,
        "isNot" => have != want,
        "startsWith" => have.starts_with(want),
        "endsWith" => have.ends_with(want),
        "isEmpty" => have.is_empty(),
        "isNotEmpty" => !have.is_empty(),
        _ => false,
    }
}

/// A [`Kind::Keywords`] op over a list of names: any name matches (none for `notContains`); `is`
/// also matches one level of a hierarchical keyword (`italy` in `travel|italy|rome`).
fn names_op<S: AsRef<str>>(op: &str, names: &[S], want: &str) -> bool {
    let names = names.iter().map(AsRef::as_ref);
    match op {
        "isEmpty" => names.clone().all(|n| n.trim().is_empty()),
        "isNotEmpty" => names.clone().any(|n| !n.trim().is_empty()),
        "notContains" => !names.clone().any(|n| text_op("contains", n, want)),
        _ => names.clone().any(|n| text_op(op, n, want) || (op == "is" && n.to_lowercase().split('|').any(|part| part.trim() == want))),
    }
}

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().trim_start_matches("f/").trim_end_matches("mm").trim().parse().ok(),
        _ => None,
    }
}

/// Widens an `f32` to the `f64` nearest its shortest decimal form (1.4f32 -> 1.4), so it compares equal to the value a user types.
fn f32_as_decimal(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

fn num_op(op: &str, have: Option<f64>, value: &Value) -> bool {
    if op == "between" {
        let (a, b) = match value {
            Value::Array(v) if v.len() == 2 => (number(&v[0]), number(&v[1])),
            _ => (None, None),
        };
        return matches!((have, a, b), (Some(h), Some(a), Some(b)) if h >= a.min(b) && h <= a.max(b));
    }
    let (Some(h), Some(w)) = (have, number(value)) else { return op == "isNot" && have.is_none() };
    match op {
        "is" => (h - w).abs() < 1e-6,
        "isNot" => (h - w).abs() >= 1e-6,
        "gte" => h >= w - 1e-9,
        "lte" => h <= w + 1e-9,
        "gt" => h > w,
        "lt" => h < w,
        _ => false,
    }
}

/// `landscape`, `portrait` or `square` (edges within 1 %) for a `w × h` frame; `None` when it has no area.
fn aspect((w, h): (f64, f64)) -> Option<&'static str> {
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    Some(if (w - h).abs() <= 0.01 * w.max(h) {
        "square"
    } else if w > h {
        "landscape"
    } else {
        "portrait"
    })
}

/// A shutter-speed rule's value in seconds: camera notation (`1/250`) or a number; an operand
/// that isn't a time becomes `null`, which matches nothing.
fn shutter_value(value: &Value) -> Value {
    let secs = |v: &Value| match v {
        Value::String(s) => crate::parse_shutter_seconds(s).map_or(Value::Null, Value::from),
        other => number(other).filter(|n| n.is_finite() && *n > 0.0).map_or(Value::Null, Value::from),
    };
    match value {
        Value::Array(a) => Value::Array(a.iter().map(secs).collect()),
        v => secs(v),
    }
}

/// Seconds in one "in the last…" unit: hours, days, weeks, months or years, in any case, singular
/// or plural; none means days. `None` for anything else, which the check refuses and the matcher
/// matches nothing with.
fn unit_secs(unit: &Value) -> Option<f64> {
    let day = 86_400.0;
    let name = match unit {
        Value::Null => return Some(day),
        Value::String(s) => s.trim().to_lowercase(),
        _ => return None,
    };
    match name.strip_suffix('s').unwrap_or(&name) {
        "hour" => Some(3600.0),
        "day" => Some(day),
        "week" => Some(7.0 * day),
        "month" => Some(30.4375 * day),
        "year" => Some(365.25 * day),
        _ => None,
    }
}

/// The longest "in the last…" span: 10,000 years, beyond any capture date ("ever").
const MAX_LAST_SECS: f64 = 10_000.0 * 365.25 * 86_400.0;

/// `value` for in-the-last rules: `{n, unit: days|weeks|months|years}` or a number of days, in
/// seconds. `None` for a zero, negative or unreadable `n`; a longer span than [`MAX_LAST_SECS`]
/// is that (so the arithmetic on it can't overflow).
fn last_secs(value: &Value) -> Option<i64> {
    let (n, unit) = match value {
        Value::Object(o) => (o.get("n").and_then(number)?, o.get("unit").unwrap_or(&Value::Null)),
        v => (number(v)?, &Value::Null),
    };
    let per = unit_secs(unit)?;
    let secs = n * per;
    if secs.is_nan() || secs <= 0.0 {
        return None;
    }
    Some(secs.min(MAX_LAST_SECS) as i64)
}

fn date_op(op: &str, have: Option<&str>, value: &Value) -> bool {
    if op == "isEmpty" {
        return have.is_none_or(str::is_empty);
    }
    let Some(h) = have.filter(|h| !h.is_empty()) else { return op == "notInLast" };
    // the rule's date as the check reads it (a space as the "T"); one it can't read as written
    let s = |v: &Value| v.as_str().map(|d| rule_date(d).unwrap_or_else(|_| d.trim().to_string())).unwrap_or_default();
    match op {
        // a prefix: 2026, 2026-04, 2026-04-12
        "is" => {
            let w = s(value);
            !w.is_empty() && h.starts_with(&w)
        }
        "after" => {
            let w = s(value);
            !w.is_empty() && h > w.as_str() && !h.starts_with(&w)
        }
        "before" => {
            let w = s(value);
            !w.is_empty() && h < w.as_str()
        }
        "between" => match value {
            Value::Array(v) if v.len() == 2 => {
                let (a, b) = (s(&v[0]), s(&v[1]));
                h >= a.as_str() && (h <= b.as_str() || h.starts_with(&b))
            }
            _ => false,
        },
        "inLast" | "notInLast" => {
            let Some(secs) = last_secs(value) else { return false };
            // a span reaching before year 0 is "ever": every dated photo is in it
            let Some(from) = crate::dates::shift_iso(&now(), -secs) else { return op == "inLast" };
            (h >= from.as_str()) == (op == "inLast")
        }
        _ => false,
    }
}

/// One byte as the `filePath` rules compare it: `/` and `\` both separate, case doesn't matter.
fn fold_path_byte(b: u8) -> u8 {
    if b == b'\\' { b'/' } else { b.to_ascii_lowercase() }
}

/// Whether `a` and `b` are equal under [`fold_path_byte`].
fn path_bytes_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| fold_path_byte(*x) == fold_path_byte(*y))
}

/// `needle` inside `hay`, under [`fold_path_byte`]; allocates nothing.
fn path_bytes_contains(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else { return true };
    // the spellings of the needle's first byte that fold to it: a plain two-way compare per byte
    let (a, b) = match fold_path_byte(first) {
        b'/' => (b'/', b'\\'),
        c if c.is_ascii_lowercase() => (c, c.to_ascii_uppercase()),
        c => (c, c),
    };
    let Some(starts) = hay.len().checked_sub(needle.len()).and_then(|last| hay.get(..=last)) else { return false };
    let at = |i: usize, c: u8| (c == a || c == b) && hay.get(i + 1..i + needle.len()).is_some_and(|tail| path_bytes_eq(tail, rest));
    // eight bytes at a time: does any of them spell the first byte (a zero byte after xor)?
    const ONES: u64 = 0x0101_0101_0101_0101;
    let has_zero = |v: u64| v.wrapping_sub(ONES) & !v & (ONES << 7) != 0;
    let (chunks, tail) = starts.as_chunks::<8>();
    let tail_from = starts.len() - tail.len();
    for (k, chunk) in chunks.iter().enumerate() {
        let w = u64::from_le_bytes(*chunk);
        if (has_zero(w ^ (ONES * u64::from(a))) || has_zero(w ^ (ONES * u64::from(b)))) && chunk.iter().enumerate().any(|(j, &c)| at(k * 8 + j, c)) {
            return true;
        }
    }
    tail.iter().enumerate().any(|(j, &c)| at(tail_from + j, c))
}

/// A `filePath` rule. The common case (an ASCII path and an ASCII text) is compared in place, folding
/// case and `\` as it goes, with nothing allocated per photo; any other text takes
/// [`file_path_matches_slow`], so Unicode lower-casing (ß, İ, final sigma…) behaves as it always did.
fn file_path_matches(op: &str, source: &Source, value: &Value) -> bool {
    let path = match source {
        Source::File { path } => path.as_str(),
        Source::Demo { .. } => "",
    };
    let Value::String(want) = value else { return file_path_matches_slow(op, path, value) };
    if !path.is_ascii() || !want.is_ascii() {
        return file_path_matches_slow(op, path, value);
    }
    let (path, want) = (path.trim().as_bytes(), want.trim().as_bytes());
    match op {
        // the whole string, spaces included (not word by word like the other text fields)
        "contains" => !want.is_empty() && path_bytes_contains(path, want),
        "notContains" => want.is_empty() || !path_bytes_contains(path, want),
        "is" => path_bytes_eq(path, want),
        "isNot" => !path_bytes_eq(path, want),
        "startsWith" => path.get(..want.len()).is_some_and(|head| path_bytes_eq(head, want)),
        "endsWith" => path.len().checked_sub(want.len()).and_then(|from| path.get(from..)).is_some_and(|tail| path_bytes_eq(tail, want)),
        "isEmpty" => path.is_empty(),
        "isNotEmpty" => !path.is_empty(),
        _ => false,
    }
}

/// [`file_path_matches`] for any text: the path and the text are normalised into new strings.
fn file_path_matches_slow(op: &str, path: &str, value: &Value) -> bool {
    // `/` and `\` both separate, whatever platform the catalog came from; demo photos have no path
    let path = path.replace('\\', "/");
    let want = lower(value).replace('\\', "/");
    match op {
        // the whole string, spaces included (not word by word like the other text fields)
        "contains" => !want.trim().is_empty() && path.to_lowercase().contains(want.trim()),
        "notContains" => want.trim().is_empty() || !path.to_lowercase().contains(want.trim()),
        _ => text_op(op, &path, &want),
    }
}

impl Rule {
    pub fn matches(&self, p: &Photo, cat: &Catalog) -> bool {
        let (field, op, value) = match self {
            Rule::Group { group } => return group.matches(p, cat),
            Rule::Field { field, op, value } => (field.as_str(), op.as_str(), value),
        };
        if field == "filePath" {
            return file_path_matches(op, &p.source, value);
        }
        let m = &p.meta;
        let want = lower(value);
        let text = |have: &str| text_op(op, have, &want);
        let yes = |have: bool| bool_value(value).is_some_and(|b| have == b);
        match field {
            "rating" => num_op(op, Some(p.rating as f64), value),
            "flag" => {
                let f = flag_name(p.flag);
                (f == want || (want == "picked" && f == "pick") || (want == "rejected" && f == "reject")) == (op == "is")
            }
            "label" => {
                let l = p.label.map_or("none", label_colour);
                // custom label names count too
                let named = p.label.is_some_and(|l| cat.custom_label_name(l).map_or(label_colour(l) == want, |n| n.to_lowercase() == want));
                (l == want || named) == (op == "is")
            }
            "kind" => (kind_name(p.kind) == want) == (op == "is"),
            "edited" => yes(p.is_edited()),
            "hasGps" => yes(m.gps.is_some()),
            "virtualCopy" => yes(p.copy_of.is_some()),
            "copyName" => text(p.copy_name.as_deref().unwrap_or("")),
            "stacked" => yes(cat.stack_of(p.id).is_some()),
            "cropped" => yes(p.is_cropped()),
            "treatment" => {
                let t = if p.develop.treatment == lightcraft_develop::Treatment::Bw { "monochrome" } else { "color" };
                (t == want) == (op == "is")
            }
            "keywords" => names_op(op, &m.keywords, &want),
            "keywordCount" => num_op(op, Some(p.keyword_count() as f64), value),
            "person" => names_op(op, &p.people(), &want),
            "personCount" => num_op(op, Some(p.people().len() as f64), value),
            "text" => {
                let all = [
                    p.file_name.as_str(),
                    &m.title,
                    &m.caption,
                    &m.camera,
                    &m.lens,
                    &m.location,
                    &m.city,
                    &m.state,
                    &m.country,
                    &m.alt_text,
                    &p.format,
                    &m.keywords.join(" "),
                    &p.people().join(" "),
                    p.copy_name.as_deref().unwrap_or(""),
                ]
                .join(" ");
                text(&all)
            }
            "fileName" => text(&p.file_name),
            "extension" => text_op(op, p.extension(), want.trim_start_matches('.')),
            "duration" => num_op(op, p.duration.filter(|d| d.is_finite()), value),
            "format" => text(&p.format),
            "title" => text(&m.title),
            "caption" => text(&m.caption),
            "altText" => text(&m.alt_text),
            "city" => text(&m.city),
            "state" => text(&m.state),
            "country" => text(&m.country),
            "camera" => text(&m.camera),
            "lens" => text(&m.lens),
            "location" => text(&[m.location.as_str(), &m.city, &m.state, &m.country].join(" ")),
            "creator" => text(&m.creator),
            "copyright" => text(&m.copyright),
            "copyrightStatus" => {
                let want = crate::CopyrightStatus::parse(&want);
                (want == Some(m.copyright_status)) == (op == "is")
            }
            "captureDate" => date_op(op, p.captured.as_deref(), value),
            "importDate" => date_op(op, Some(&p.imported), value),
            "editDate" => date_op(op, p.edited.as_deref(), value),
            "shutterSpeed" => num_op(op, crate::parse_shutter_seconds(&m.shutter), &shutter_value(value)),
            "iso" => num_op(op, m.iso.map(|v| v as f64), value),
            "aperture" => num_op(op, m.aperture.map(f32_as_decimal), value),
            "focalLength" => num_op(op, m.focal_mm.map(f32_as_decimal), value),
            "longEdge" | "shortEdge" => {
                let (w, h) = p.shown_size();
                num_op(op, Some(if field == "longEdge" { w.max(h) } else { w.min(h) }.round()), value)
            }
            "aspect" => (aspect(p.shown_size()) == Some(want.as_str())) == (op == "is"),
            "megapixels" => {
                let (w, h) = p.shown_size();
                num_op(op, Some(w * h / 1e6), value)
            }
            "sharpness" => num_op(op, p.analysis.map(|a| a.sharpness as f64), value),
            "bestOfGroup" => yes(p.analysis.is_some_and(|a| a.best || a.group.is_none())),
            "album" => {
                let id = album_rule_id(value);
                // a smart album's photos are its matches; one being evaluated (a loop) holds none
                id.is_some_and(|a| cat.album_contains(a, p)) == (op == "is")
            }
            _ => false,
        }
    }
}

impl RuleSet {
    pub fn matches(&self, p: &Photo, cat: &Catalog) -> bool {
        if self.rules.is_empty() {
            return self.mode != Match::Any;
        }
        match self.mode {
            Match::All => self.rules.iter().all(|r| r.matches(p, cat)),
            Match::Any => self.rules.iter().any(|r| r.matches(p, cat)),
            Match::None => !self.rules.iter().any(|r| r.matches(p, cat)),
        }
    }

    /// Whether matches depend on the clock ("in the last…" rules, nested groups included): the
    /// same photos can enter or leave the set without any catalog change.
    pub fn depends_on_now(&self) -> bool {
        self.rules.iter().any(|r| match r {
            Rule::Group { group } => group.depends_on_now(),
            Rule::Field { op, .. } => op == "inLast" || op == "notInLast",
        })
    }

    /// Bring rules saved by older versions up to date, keeping what they meant: an Album rule with
    /// an operator from when Album was a number field (≥, between…) always matched as "isn't", and
    /// now says so. Commands and the editor apply it before checking.
    pub fn upgrade(&mut self) {
        for rule in &mut self.rules {
            match rule {
                Rule::Group { group } => group.upgrade(),
                Rule::Field { field, op, .. } if field == "album" && op != "is" && op != "isNot" => *op = "isNot".into(),
                Rule::Field { .. } => {}
            }
        }
    }

    /// The rules that can't mean anything, in order: an unknown field, an operator the field
    /// doesn't have, a value that isn't one of the field's (a number, a date, a choice, yes or
    /// no, an album that exists and isn't a folder…), text with nothing to look for, an empty group. Commands refuse a rule
    /// set with problems; the editor shows them. `cat` knows the albums and colour-label names.
    pub fn check(&self, cat: &Catalog) -> Vec<Problem> {
        self.check_for(cat, None)
    }

    /// [`RuleSet::check`] for the rules of smart album `owner` (`None`: a new album or a filter),
    /// which also refuses an Album rule that would make `owner` include itself.
    pub fn check_for(&self, cat: &Catalog, owner: Option<crate::AlbumId>) -> Vec<Problem> {
        let mut out = Vec::new();
        self.check_into(cat, owner, &mut Vec::new(), &mut out);
        out
    }

    /// The albums the Album rules test, groups included, in order.
    pub fn albums_tested(&self) -> Vec<crate::AlbumId> {
        let mut out = Vec::new();
        for rule in &self.rules {
            match rule {
                Rule::Group { group } => out.extend(group.albums_tested()),
                Rule::Field { field, value, .. } if field == "album" => out.extend(album_rule_id(value)),
                Rule::Field { .. } => {}
            }
        }
        out
    }

    fn check_into(&self, cat: &Catalog, owner: Option<crate::AlbumId>, path: &mut Vec<usize>, out: &mut Vec<Problem>) {
        for (i, rule) in self.rules.iter().enumerate() {
            path.push(i);
            match rule {
                Rule::Group { group } if group.rules.is_empty() => {
                    out.push(Problem { path: path.clone(), field: None, issue: Issue::EmptyGroup, message: "an empty group".into() })
                }
                Rule::Group { group } => group.check_into(cat, owner, path, out),
                Rule::Field { field, op, value } => {
                    if let Some((issue, message)) = field_problem(field, op, value, cat, owner) {
                        out.push(Problem { path: path.clone(), field: Some(field.clone()), issue, message });
                    }
                }
            }
            path.pop();
        }
    }

    /// A short readable summary ("rating is ≥ 3 and keywords contains travel").
    pub fn describe(&self) -> String {
        self.describe_in(None)
    }

    /// [`RuleSet::describe`] naming the albums Album rules test ("album isn't “Excluded
    /// Photos”"); one that is gone shows as `#9`.
    pub fn describe_with(&self, cat: &Catalog) -> String {
        self.describe_in(Some(cat))
    }

    fn describe_in(&self, cat: Option<&Catalog>) -> String {
        let join = match self.mode {
            Match::All => " and ",
            Match::Any => " or ",
            Match::None => " nor ",
        };
        let parts: Vec<String> = self
            .rules
            .iter()
            .map(|r| match r {
                Rule::Group { group } => format!("({})", group.describe_in(cat)),
                Rule::Field { field, op, value } => {
                    let label = field_label(field).unwrap_or(field).to_lowercase();
                    let op = field_kind(field).and_then(|k| ops_for(k).iter().find(|o| o.0 == op)).map_or(op.as_str(), |o| o.1);
                    let v = match value {
                        _ if field == "album" && cat.is_some() => album_name(value, cat),
                        _ if field_kind(field) == Some(Kind::Bool) => {
                            bool_value(value).map_or_else(|| value.to_string(), |b| bool_label(b).to_lowercase())
                        }
                        Value::String(s) => choice_label(field, s).map_or_else(|| s.clone(), str::to_lowercase),
                        Value::Null => String::new(),
                        Value::Object(o) => format!(
                            "{} {}",
                            o.get("n").map(|n| n.to_string()).unwrap_or_default(),
                            o.get("unit").and_then(Value::as_str).unwrap_or("days")
                        ),
                        Value::Array(a) => a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string)).collect::<Vec<_>>().join(" – "),
                        other => other.to_string(),
                    };
                    format!("{label} {op} {v}").trim().to_string()
                }
            })
            .collect();
        let s = parts.join(join);
        if self.mode == Match::None { format!("none of: {s}") } else { s }
    }
}

#[cfg(test)]
mod tests;
