//! IMM-SYNC: two-way metadata sync between a catalog photo and its linked Immich asset — the pure
//! part (no I/O): what each side holds ([`Snapshot`]), the per-field settings ([`SyncConfig`]),
//! the three-way merge against the values of the last sync ([`reconcile`]), and what to write on
//! each side ([`local_ops`], [`remote_update`]). The engine runs it (`immich.sync`).
//!
//! Sources: own design (a field-wise three-way merge, as in version control: a side "changed" a
//! field when it differs from the value both sides had at the last sync); Immich's public API
//! documentation for the remote fields (plan/immich.md → Sync).
//!
//! Mapping (defaults in [`default_rule`]):
//!
//! | catalog | Immich | default |
//! |---|---|---|
//! | stars 0–5 | rating (0 = none) | two-way, newest wins |
//! | pick flag (or 5★) | favourite | two-way, newest wins |
//! | reject flag | archived | off |
//! | caption (or title) | description | two-way, newest wins |
//! | keywords `a\|b` (export-excluded never sent) | tags `a/b` | two-way, merged as sets |
//! | GPS | location | two-way, ask |
//! | capture time | date | two-way, ask |

use std::collections::{BTreeMap, BTreeSet};

use dac_catalog::{Flag, Op, Photo};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::mapping::tag_to_keyword;
use crate::types::{Asset, AssetUpdate};

/// A synced field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    Rating,
    Favorite,
    Archived,
    Description,
    Keywords,
    Location,
    Captured,
}

impl Field {
    pub const ALL: [Field; 7] =
        [Field::Rating, Field::Favorite, Field::Archived, Field::Description, Field::Keywords, Field::Location, Field::Captured];

    pub fn name(self) -> &'static str {
        match self {
            Field::Rating => "rating",
            Field::Favorite => "favorite",
            Field::Archived => "archived",
            Field::Description => "description",
            Field::Keywords => "keywords",
            Field::Location => "location",
            Field::Captured => "captured",
        }
    }

    pub fn parse(s: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|f| f.name() == s)
    }
}

/// Which way a field syncs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Direction {
    #[default]
    TwoWay,
    /// Catalog changes go to Immich; Immich changes are ignored.
    ToImmich,
    /// Immich changes come into the catalog; catalog changes are not sent.
    FromImmich,
    Off,
}

/// Who wins when both sides changed a field since the last sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Policy {
    Catalog,
    Immich,
    /// The later change (catalog: when the sync first saw the change; Immich: the asset's
    /// `updatedAt`); unknown times leave a conflict.
    #[default]
    Newest,
    /// Always a conflict for the user (Sync Conflicts).
    Ask,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FieldRule {
    pub direction: Direction,
    pub policy: Policy,
}

/// What Immich's favourite means in the catalog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FavoriteMap {
    #[default]
    Pick,
    FiveStars,
}

/// Which catalog text Immich's description is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DescriptionMap {
    #[default]
    Caption,
    Title,
}

/// Per-account sync settings (kept with the account in `connections.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncConfig {
    /// Per-field overrides of [`default_rule`].
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub rules: BTreeMap<Field, FieldRule>,
    pub favorite: FavoriteMap,
    pub description: DescriptionMap,
    /// Background sync every so many minutes (0: manual only).
    pub interval_minutes: u32,
    /// Sync after publishing to this account (IMM-PUBLISH).
    pub on_publish: bool,
    /// Send names given in the app back to Immich's people (IMM-PEOPLE).
    pub push_people: bool,
}

impl SyncConfig {
    pub fn rule(&self, f: Field) -> FieldRule {
        self.rules.get(&f).copied().unwrap_or_else(|| default_rule(f))
    }
}

/// The defaults of the plan's mapping table.
pub fn default_rule(f: Field) -> FieldRule {
    match f {
        Field::Archived => FieldRule { direction: Direction::Off, policy: Policy::Newest },
        Field::Location | Field::Captured => FieldRule { direction: Direction::TwoWay, policy: Policy::Ask },
        _ => FieldRule::default(),
    }
}

/// The synced fields of one side, normalized so equal values compare equal.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Snapshot {
    pub rating: u8,
    pub favorite: bool,
    pub archived: bool,
    pub description: String,
    /// Catalog form (`a|b`).
    pub keywords: BTreeSet<String>,
    /// Rounded to 1e-6° (≈ 10 cm).
    pub location: Option<(f64, f64)>,
    /// `YYYY-MM-DDTHH:MM:SS`, local time.
    pub captured: Option<String>,
}

impl Snapshot {
    /// One field as JSON (Sync Conflicts, dry runs).
    pub fn value(&self, f: Field) -> Value {
        match f {
            Field::Rating => json!(self.rating),
            Field::Favorite => json!(self.favorite),
            Field::Archived => json!(self.archived),
            Field::Description => json!(self.description),
            Field::Keywords => json!(self.keywords),
            Field::Location => json!(self.location.map(|(a, b)| [a, b])),
            Field::Captured => json!(self.captured),
        }
    }

    fn same(&self, other: &Snapshot, f: Field) -> bool {
        match f {
            Field::Rating => self.rating == other.rating,
            Field::Favorite => self.favorite == other.favorite,
            Field::Archived => self.archived == other.archived,
            Field::Description => self.description == other.description,
            Field::Keywords => self.keywords == other.keywords,
            Field::Location => self.location == other.location,
            Field::Captured => self.captured == other.captured,
        }
    }

    /// The field holds nothing (a side that never had a value didn't "change" it).
    fn blank(&self, f: Field) -> bool {
        match f {
            Field::Rating => self.rating == 0,
            Field::Favorite => !self.favorite,
            Field::Archived => !self.archived,
            Field::Description => self.description.is_empty(),
            Field::Keywords => self.keywords.is_empty(),
            Field::Location => self.location.is_none(),
            Field::Captured => self.captured.is_none(),
        }
    }

    fn copy_field(&mut self, from: &Snapshot, f: Field) {
        match f {
            Field::Rating => self.rating = from.rating,
            Field::Favorite => self.favorite = from.favorite,
            Field::Archived => self.archived = from.archived,
            Field::Description => self.description = from.description.clone(),
            Field::Keywords => self.keywords = from.keywords.clone(),
            Field::Location => self.location = from.location,
            Field::Captured => self.captured = from.captured.clone(),
        }
    }
}

fn round6(v: f64) -> f64 {
    (v * 1e6).round() / 1e6
}

fn location(lat: Option<f64>, lon: Option<f64>) -> Option<(f64, f64)> {
    let (lat, lon) = (lat?, lon?);
    (lat.is_finite() && lon.is_finite() && (-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then(|| (round6(lat), round6(lon)))
}

/// `YYYY-MM-DDTHH:MM:SS` (a space separator is accepted), or `None`.
fn norm_time(t: &str) -> Option<String> {
    let t: String = t.trim().chars().take(19).map(|c| if c == ' ' { 'T' } else { c }).collect();
    let b = t.as_bytes();
    let digits = |r: std::ops::Range<usize>| b.get(r).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    (b.len() == 19 && digits(0..4) && digits(5..7) && digits(8..10) && digits(11..13) && digits(14..16) && digits(17..19)).then_some(t)
}

/// `a|b` → `a/b` (a `/` inside a level becomes `-`, as Immich tag names can't hold one).
pub fn keyword_to_tag(k: &str) -> String {
    k.split('|').map(|l| l.trim().replace('/', "-")).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("/")
}

fn norm_keyword(k: &str) -> String {
    tag_to_keyword(&keyword_to_tag(k))
}

/// The catalog side of `p`. `excluded` says which keywords are export-excluded (never sent).
pub fn local(p: &Photo, cfg: &SyncConfig, excluded: &dyn Fn(&str) -> bool) -> Snapshot {
    let text = match cfg.description {
        DescriptionMap::Caption => &p.meta.caption,
        DescriptionMap::Title => &p.meta.title,
    };
    Snapshot {
        rating: p.rating.min(5),
        favorite: match cfg.favorite {
            FavoriteMap::Pick => p.flag == Flag::Pick,
            FavoriteMap::FiveStars => p.rating >= 5,
        },
        archived: p.flag == Flag::Reject,
        description: text.trim().to_string(),
        keywords: p.meta.keywords.iter().filter(|k| !excluded(k)).map(|k| norm_keyword(k)).filter(|k| !k.is_empty()).collect(),
        location: p.meta.gps.and_then(|(a, b)| location(Some(a), Some(b))),
        captured: p.captured.as_deref().and_then(norm_time),
    }
}

/// The Immich side of `a` (fetched with its tags: `GET /assets/{id}`).
pub fn remote(a: &Asset) -> Snapshot {
    let exif = a.exif_info.clone().unwrap_or_default();
    Snapshot {
        rating: match exif.rating {
            Some(r @ 1..=5) => r as u8,
            _ => 0,
        },
        favorite: a.is_favorite,
        archived: a.visibility.as_deref() == Some("archive"),
        description: exif.description.unwrap_or_default().trim().to_string(),
        keywords: a.tags.iter().map(|t| tag_to_keyword(&t.value)).filter(|k| !k.is_empty()).collect(),
        location: location(exif.latitude, exif.longitude),
        captured: a.local_date_time.as_deref().and_then(norm_time),
    }
}

/// Assets the sync never touches: locked-folder and hidden (live-photo video) assets.
pub fn untouchable(a: &Asset) -> bool {
    matches!(a.visibility.as_deref(), Some("locked") | Some("hidden"))
}

/// A user's decision for a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Side {
    Catalog,
    Immich,
}

impl Side {
    pub fn parse(s: &str) -> Option<Side> {
        match s {
            "catalog" | "local" => Some(Side::Catalog),
            "immich" | "remote" => Some(Side::Immich),
            _ => None,
        }
    }
}

/// Everything [`reconcile`] looks at for one linked pair.
#[derive(Clone, Copy, Debug)]
pub struct Sides<'a> {
    /// The values both sides had at the last sync (`None`: never synced).
    pub base: Option<&'a Snapshot>,
    pub local: &'a Snapshot,
    pub remote: &'a Snapshot,
    /// Fields left in conflict by an earlier run.
    pub stuck: &'a BTreeSet<Field>,
    /// Conflicts the user decided.
    pub forced: &'a BTreeMap<Field, Side>,
    /// When the sync first saw the catalog side differ from `base` (ISO 8601).
    pub local_changed_at: Option<&'a str>,
    /// The asset's `updatedAt`.
    pub remote_updated_at: Option<&'a str>,
}

/// What to do for one linked pair.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    /// The values both sides will hold (conflicted fields: the catalog's, unchanged).
    pub target: Snapshot,
    /// Fields to write into the catalog.
    pub to_local: BTreeSet<Field>,
    /// Fields to send to Immich.
    pub to_remote: BTreeSet<Field>,
    /// Fields both sides changed that no rule decided.
    pub conflicts: BTreeSet<Field>,
    /// Tags (catalog form) to add to / remove from the asset.
    pub add_tags: BTreeSet<String>,
    pub remove_tags: BTreeSet<String>,
}

impl Outcome {
    pub fn is_noop(&self) -> bool {
        self.to_local.is_empty() && self.to_remote.is_empty() && self.conflicts.is_empty()
    }
}

/// ISO-8601 instants compared as instants when both parse (`Z`/offsets), else as text.
fn later(a: &str, b: &str) -> Option<std::cmp::Ordering> {
    match (crate::sync::instant(a), crate::sync::instant(b)) {
        (Some(x), Some(y)) => Some(x.cmp(&y)),
        _ => None,
    }
}

/// Seconds since 1970 (plus milliseconds ×1000) of `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`; no zone
/// is UTC.
pub fn instant(t: &str) -> Option<i64> {
    let base = norm_time(t)?;
    let num = |r: std::ops::Range<usize>| base.get(r).and_then(|s| s.parse::<i64>().ok());
    let (y, mo, d, h, mi, s) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    // days from civil (Howard Hinnant's public-domain algorithm)
    let yy = if mo <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let rest = t.trim().get(19..).unwrap_or_default();
    let (frac, zone) = match rest.strip_prefix('.') {
        Some(r) => {
            let n = r.bytes().take_while(u8::is_ascii_digit).count();
            let digits = r.get(..n.min(3)).unwrap_or_default();
            let ms = format!("{digits:0<3}").parse::<i64>().unwrap_or(0);
            (ms, r.get(n..).unwrap_or_default())
        }
        None => (0, rest),
    };
    let offset = match zone.as_bytes().first() {
        Some(b'+') | Some(b'-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let z = zone.get(1..).unwrap_or_default().replace(':', "");
            let hh = z.get(0..2).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            let mm = z.get(2..4).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
            sign * (hh * 3600 + mm * 60)
        }
        _ => 0,
    };
    let secs = days * 86_400 + h * 3600 + mi * 60 + s - offset;
    Some(secs.saturating_mul(1000).saturating_add(frac))
}

/// The three-way merge of one linked pair (see the module doc).
pub fn reconcile(sides: Sides<'_>, cfg: &SyncConfig) -> Outcome {
    let Sides { base, local: l, remote: r, stuck, forced, local_changed_at, remote_updated_at } = sides;
    let mut out = Outcome { target: l.clone(), ..Outcome::default() };
    for f in Field::ALL {
        let rule = cfg.rule(f);
        if rule.direction == Direction::Off {
            continue;
        }
        if f == Field::Keywords && !forced.contains_key(&f) {
            merge_keywords(base, l, r, rule.direction, &mut out);
            continue;
        }
        if l.same(r, f) {
            continue;
        }
        let pull = rule.direction != Direction::ToImmich;
        let push = rule.direction != Direction::FromImmich;
        let winner = match forced.get(&f) {
            Some(s) => Some(*s),
            None => {
                let (lc, rc) = match base {
                    Some(b) => (!b.same(l, f), !b.same(r, f)),
                    // never synced: a blank side didn't change anything
                    None => (!l.blank(f), !r.blank(f)),
                };
                let both = (lc && rc) || stuck.contains(&f);
                if both && !(pull && push) {
                    // a one-way field: its one direction decides
                    Some(if push { Side::Catalog } else { Side::Immich })
                } else if both {
                    match rule.policy {
                        Policy::Catalog => Some(Side::Catalog),
                        Policy::Immich => Some(Side::Immich),
                        Policy::Ask => None,
                        Policy::Newest => match (local_changed_at, remote_updated_at) {
                            (Some(a), Some(b)) => match later(a, b) {
                                Some(std::cmp::Ordering::Greater) => Some(Side::Catalog),
                                Some(std::cmp::Ordering::Less) => Some(Side::Immich),
                                _ => None,
                            },
                            _ => None,
                        },
                    }
                } else if lc {
                    Some(Side::Catalog)
                } else if rc {
                    Some(Side::Immich)
                } else {
                    // neither changed yet they differ (a value the mapping can't hold): the catalog's
                    Some(Side::Catalog)
                }
            }
        };
        match winner {
            Some(Side::Catalog) if push => {
                out.to_remote.insert(f);
            }
            Some(Side::Immich) if pull => {
                out.target.copy_field(r, f);
                out.to_local.insert(f);
            }
            Some(_) => {}
            None => {
                out.conflicts.insert(f);
            }
        }
    }
    // a forced keyword decision replaces the set
    if let Some(side) = forced.get(&Field::Keywords) {
        let set = if *side == Side::Catalog { l.keywords.clone() } else { r.keywords.clone() };
        apply_keyword_set(set, l, r, cfg.rule(Field::Keywords).direction, &mut out);
    }
    out
}

/// Keywords merge as sets: a keyword is kept when both sides have it, or one side added it since
/// the last sync; removed when one side removed it. Never a conflict.
fn merge_keywords(base: Option<&Snapshot>, l: &Snapshot, r: &Snapshot, dir: Direction, out: &mut Outcome) {
    let set: BTreeSet<String> = match (dir, base) {
        (Direction::ToImmich, _) => l.keywords.clone(),
        (Direction::FromImmich, _) => r.keywords.clone(),
        (_, None) => l.keywords.union(&r.keywords).cloned().collect(),
        (_, Some(b)) => l
            .keywords
            .union(&r.keywords)
            .filter(|k| {
                let (inl, inr, inb) = (l.keywords.contains(*k), r.keywords.contains(*k), b.keywords.contains(*k));
                // (every keyword here is on at least one side)
                (inl && inr) || !inb
            })
            .cloned()
            .collect(),
    };
    apply_keyword_set(set, l, r, dir, out);
}

fn apply_keyword_set(set: BTreeSet<String>, l: &Snapshot, r: &Snapshot, dir: Direction, out: &mut Outcome) {
    if set != l.keywords && dir != Direction::ToImmich {
        out.to_local.insert(Field::Keywords);
        out.target.keywords = set.clone();
    }
    if set != r.keywords && dir != Direction::FromImmich {
        out.to_remote.insert(Field::Keywords);
        out.add_tags = set.difference(&r.keywords).cloned().collect();
        out.remove_tags = r.keywords.difference(&set).cloned().collect();
    }
    if dir == Direction::ToImmich {
        out.target.keywords = l.keywords.clone();
    }
}

/// The ops that write `fields` of `t` into photo `p`. Export-excluded keywords stay.
pub fn local_ops(p: &Photo, t: &Snapshot, fields: &BTreeSet<Field>, cfg: &SyncConfig, excluded: &dyn Fn(&str) -> bool) -> Vec<Op> {
    let mut rating = p.rating;
    let mut flag = p.flag;
    let mut meta = p.meta.clone();
    let mut captured = p.captured.clone();
    for f in fields {
        match f {
            Field::Rating => rating = t.rating.min(5),
            Field::Archived => {
                if t.archived {
                    flag = Flag::Reject;
                } else if flag == Flag::Reject {
                    flag = Flag::None;
                }
            }
            Field::Description => match cfg.description {
                DescriptionMap::Caption => meta.caption = t.description.clone(),
                DescriptionMap::Title => meta.title = t.description.clone(),
            },
            Field::Keywords => {
                let mut kept: Vec<String> = Vec::new();
                for k in &meta.keywords {
                    let n = norm_keyword(k);
                    if (excluded(k) || t.keywords.contains(&n)) && !kept.iter().any(|x| norm_keyword(x) == n) {
                        kept.push(k.clone());
                    }
                }
                for k in &t.keywords {
                    if !kept.iter().any(|x| norm_keyword(x) == *k) {
                        kept.push(k.clone());
                    }
                }
                meta.keywords = kept;
            }
            Field::Location => meta.gps = t.location,
            Field::Captured => captured = t.captured.clone(),
            Field::Favorite => {}
        }
    }
    // the favourite last: with "favourite = 5★" it adjusts the rating just set
    if fields.contains(&Field::Favorite) {
        match cfg.favorite {
            FavoriteMap::Pick => {
                if t.favorite && flag != Flag::Reject {
                    flag = Flag::Pick;
                } else if !t.favorite && flag == Flag::Pick {
                    flag = Flag::None;
                }
            }
            FavoriteMap::FiveStars => {
                if t.favorite {
                    rating = 5;
                } else if rating >= 5 {
                    rating = 4;
                }
            }
        }
    }
    let mut ops = Vec::new();
    if rating != p.rating {
        ops.push(Op::SetRating { id: p.id, rating });
    }
    if flag != p.flag {
        ops.push(Op::SetFlag { id: p.id, flag });
    }
    if meta != p.meta {
        ops.push(Op::SetMeta { id: p.id, meta: Box::new(meta) });
    }
    if captured != p.captured {
        ops.push(Op::SetCaptured { id: p.id, captured });
    }
    ops
}

/// The `PUT /assets/{id}` body that sends `fields` of `t` (tags go separately). `zone` is the
/// asset's own time-zone suffix (`+02:00`, `Z`), kept on a new capture time.
pub fn remote_update(t: &Snapshot, fields: &BTreeSet<Field>, cfg: &SyncConfig, zone: Option<&str>) -> AssetUpdate {
    let mut u = AssetUpdate::default();
    for f in fields {
        match f {
            Field::Rating => u.rating = Some((t.rating > 0).then_some(i32::from(t.rating.min(5)))),
            Field::Favorite => {
                u.is_favorite = Some(t.favorite);
                if cfg.favorite == FavoriteMap::FiveStars && t.favorite {
                    u.rating = Some(Some(5));
                }
            }
            Field::Archived => u.visibility = Some(if t.archived { "archive" } else { "timeline" }.into()),
            Field::Description => u.description = Some(t.description.clone()),
            Field::Location => {
                // Immich has no "no location" update: clearing is sent as nothing
                if let Some((a, b)) = t.location {
                    u.latitude = Some(a);
                    u.longitude = Some(b);
                }
            }
            Field::Captured => {
                if let Some(c) = &t.captured {
                    u.date_time_original = Some(format!("{c}{}", zone.unwrap_or("")));
                }
            }
            Field::Keywords => {}
        }
    }
    u
}

/// The time-zone suffix of an ISO time (`Z`, `+02:00`), if it has one.
pub fn zone_of(t: &str) -> Option<&str> {
    let rest = t.trim().get(19..)?;
    let rest = match rest.strip_prefix('.') {
        Some(r) => r.trim_start_matches(|c: char| c.is_ascii_digit()),
        None => rest,
    };
    (rest == "Z" || ((rest.starts_with('+') || rest.starts_with('-')) && rest.len() == 6)).then_some(rest)
}

// ---- the per-account state between runs

/// A conflict as Sync Conflicts shows it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ConflictValues {
    pub catalog: Value,
    pub immich: Value,
    pub detected: String,
}

/// What the sync remembers about one linked asset.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ItemState {
    pub photo: u64,
    /// The values both sides had after the last sync.
    pub base: Option<Snapshot>,
    /// When a run first saw the catalog differ from `base`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_changed_at: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub conflicts: BTreeMap<Field, ConflictValues>,
    /// Decisions waiting for the next run.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub forced: BTreeMap<Field, Side>,
}

/// A deletion on one side, waiting for the user (deletions never sync on their own).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Deletion {
    pub asset: String,
    pub photo: u64,
    /// `immich`: the asset is in Immich's trash; `catalog`: the photo is in Recently Deleted.
    pub side: String,
    pub file_name: String,
    pub detected: String,
}

/// One run, for the activity log.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RunLog {
    pub started: String,
    pub finished: String,
    pub dry_run: bool,
    pub checked: u64,
    pub pulled: u64,
    pub pushed: u64,
    pub conflicts: u64,
    pub errors: Vec<String>,
}

/// Everything kept between runs for one account (a JSON file next to the catalog).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SyncState {
    /// The newest `updatedAt` a complete run has seen (incremental listing).
    pub synced_until: Option<String>,
    pub items: BTreeMap<String, ItemState>,
    pub deletions: Vec<Deletion>,
    /// Assets whose deletion the user chose to keep (not asked again).
    pub kept: BTreeSet<String>,
    pub log: Vec<RunLog>,
}

/// Most activity-log entries kept.
pub const LOG_CAP: usize = 100;

impl SyncState {
    pub fn push_log(&mut self, r: RunLog) {
        self.log.push(r);
        if self.log.len() > LOG_CAP {
            let n = self.log.len() - LOG_CAP;
            self.log.drain(..n);
        }
    }

    pub fn conflict_count(&self) -> usize {
        self.items.values().map(|i| i.conflicts.len()).sum()
    }
}
