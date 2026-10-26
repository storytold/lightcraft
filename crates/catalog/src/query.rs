//! Filtering, search and sorting.

use std::cmp::Ordering::Equal;

use serde::{Deserialize, Serialize};

use crate::{AlbumId, Catalog, ColorLabel, Flag, MediaKind, Photo, PhotoId};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RatingOp {
    #[default]
    AtLeast,
    Exactly,
    AtMost,
}

/// What the grid shows. Empty/None fields don't filter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Filter {
    /// Free-text search: every token must match filename, title, caption, keywords, camera, lens,
    /// location or format. Tokens like `rating:3`, `flag:pick`, `iso:>800`, `camera:x2` are fielded.
    pub text: String,
    pub rating: u8,
    pub rating_op: RatingOp,
    pub flag: Option<Flag>,
    pub label: Option<ColorLabel>,
    /// Only these photos (Find Similar results…); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<PhotoId>,
    /// Show nothing: an explicit empty result (Find Similar with no matches), since an empty
    /// `only` means "no constraint".
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub only_none: bool,
    /// Any of these labels (the filter bar's multi-select); empty = no constraint.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<ColorLabel>,
    pub kind: Option<MediaKind>,
    pub edited: Option<bool>,
    pub album: Option<AlbumId>,
    /// Show "Recently Deleted" instead of the library.
    pub deleted: bool,
    /// Capture date prefix (`2026`, `2026-04`, `2026-04-12`).
    pub date: Option<String>,
    /// A folder the library's photos were imported from: those photos and the ones imported from
    /// the folders inside it (see [`crate::folders`]). Photos only browsed in Local are never part
    /// of it, unlike [`Filter::folder`].
    pub library_folder: Option<String>,
    /// A keyword; hierarchical keywords match their children too (`travel` finds `travel|italy`).
    pub keyword: Option<String>,
    /// A person: photos with a named face region of this name (case-insensitive), as read from XMP.
    pub person: Option<String>,
    pub camera: Option<String>,
    /// Lens (case-insensitive substring).
    pub lens: Option<String>,
    /// Capture date range, inclusive: `dateFrom` is compared as a lower bound (`2026-04-01`),
    /// `dateTo` as a prefix upper bound (`2026-04` includes all of April).
    pub date_from: Option<String>,
    pub date_to: Option<String>,
    /// Import date prefix.
    pub imported: Option<String>,
    /// Imported at or after this time (ISO; Recently Added).
    pub imported_from: Option<String>,
    /// Photo Merge results: `hdr`, `panorama`, `hdrPanorama` or `any` (see [`merged_kind`]).
    pub merged: Option<String>,
    /// A folder on disk: its files only (browsed ones too); `subfolders` includes everything below.
    pub folder: Option<String>,
    pub subfolders: bool,
    /// Smart-album rules (all / any / none, nested groups; see [`crate::rules`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule_set: Option<crate::RuleSet>,
    /// Only these photos, each with a score (a search by photo contents, which the engine resolves outside the
    /// catalog), ANDed with everything else like [`Filter::only`]; [`SortKey::Relevance`] orders by the score.
    /// Never saved: it is worked out again from the query that made it.
    #[serde(skip)]
    pub ranked: Option<Ranked>,
}

/// Photos with scores, from outside the catalog ([`Filter::ranked`]): a photo passes when it is listed with a score of
/// at least `min`.
#[derive(Clone, Default, PartialEq)]
pub struct Ranked {
    pub ids: Vec<(PhotoId, f32)>,
    pub min: f32,
}

impl std::fmt::Debug for Ranked {
    // (a ranking can list thousands of photos; the view keys filters by their debug text)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sum: f64 = self.ids.iter().map(|(id, s)| id.0 as f64 * 1e-3 + f64::from(*s)).sum();
        write!(f, "Ranked({} photos, min {}, sum {sum})", self.ids.len(), self.min)
    }
}

impl Ranked {
    /// Score by photo, of the photos that pass (a photo listed twice keeps its best score).
    pub fn scores(&self) -> std::collections::HashMap<PhotoId, f32> {
        let mut m = std::collections::HashMap::with_capacity(self.ids.len());
        for (id, s) in self.ids.iter().filter(|(_, s)| *s >= self.min && !s.is_nan()) {
            let e = m.entry(*id).or_insert(*s);
            if *s > *e {
                *e = *s;
            }
        }
        m
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortKey {
    #[default]
    CaptureDate,
    ImportDate,
    EditDate,
    FileName,
    Rating,
    FileSize,
    /// A shuffle fixed by [`Sort::seed`]: the same seed always gives the same order, and photos
    /// added or edited later never reshuffle the others. Pick a new seed to reshuffle.
    Random,
    /// How well each photo matches a search by contents ([`Filter::ranked`]), best first (descending); without such a
    /// search, by capture date.
    Relevance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
    /// Date headers in the grid (date sort keys only).
    pub group: crate::GroupBy,
    /// Which shuffle [`SortKey::Random`] gives (ignored by the other keys).
    pub seed: u64,
}

impl Sort {
    /// Which of two photos comes first in this order (never `Equal` for two photos: the id decides last).
    pub fn order(&self, a: &Photo, b: &Photo) -> std::cmp::Ordering {
        self.order_ranked(a, b, None)
    }

    /// [`Sort::order`] with a search by contents' scores ([`Ranked::scores`]), which [`SortKey::Relevance`] orders by.
    pub fn order_ranked(&self, a: &Photo, b: &Photo, ranked: Option<&std::collections::HashMap<PhotoId, f32>>) -> std::cmp::Ordering {
        let score = |p: &Photo| ranked.and_then(|r| r.get(&p.id).copied()).unwrap_or(f32::NEG_INFINITY);
        let o = match self.key {
            SortKey::CaptureDate => a.captured.cmp(&b.captured).then_with(|| a.imported.cmp(&b.imported)),
            SortKey::ImportDate => a.imported.cmp(&b.imported),
            SortKey::EditDate => a.edited.cmp(&b.edited),
            SortKey::FileName => cmp_name(&a.file_name, &b.file_name),
            SortKey::Rating => a.rating.cmp(&b.rating),
            SortKey::FileSize => a.file_size.cmp(&b.file_size),
            SortKey::Random => shuffle_rank(self.seed, a.id).cmp(&shuffle_rank(self.seed, b.id)),
            // (descending, the default, is best first; equal scores fall back to the capture date)
            SortKey::Relevance if ranked.is_some() => score(a).total_cmp(&score(b)).then_with(|| a.captured.cmp(&b.captured)),
            SortKey::Relevance => a.captured.cmp(&b.captured).then_with(|| a.imported.cmp(&b.imported)),
        }
        // Ties (one import batch, a burst within a second, equal ratings) sort by file name, the
        // order cameras number frames, so the order doesn't depend on how ids were allocated;
        // the id only keeps it stable.
        .then_with(|| if self.key == SortKey::FileName { Equal } else { cmp_name(&a.file_name, &b.file_name) })
        .then(a.id.cmp(&b.id));
        if self.ascending { o } else { o.reverse() }
    }
}

impl Default for Sort {
    fn default() -> Self {
        Sort { key: SortKey::CaptureDate, ascending: false, group: crate::GroupBy::Auto, seed: 0 }
    }
}

/// splitmix64 finaliser: a stateless, platform-independent mix (no RNG state, no `rand` version
/// to drift), so a seed names the same shuffle on every machine.
pub fn mix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A photo's place in the shuffle for `seed`.
fn shuffle_rank(seed: u64, id: PhotoId) -> u64 {
    mix64(mix64(seed) ^ id.0)
}

/// The fields of a search text that ask what photos show (`contents:` and its synonym `shows:`), answered outside the
/// catalog ([`Filter::ranked`]); the catalog lets them through.
pub const CONTENT_FIELDS: [&str; 2] = ["contents", "shows"];

/// The content part of a search text: the values of its `contents:` / `shows:` words (`None` when it has none), and
/// the text without them.
pub fn split_contents(text: &str) -> (Option<String>, String) {
    let (mut asked, mut rest) = (Vec::new(), Vec::new());
    for tok in search_tokens(text) {
        match tok.split_once(':').filter(|_| !tok.starts_with('"')) {
            Some((field, value)) if CONTENT_FIELDS.iter().any(|f| f.eq_ignore_ascii_case(field)) => {
                let value = unquote(value).trim();
                if !value.is_empty() {
                    asked.push(value.to_string());
                }
            }
            // (an empty phrase, `""`, asks for nothing)
            _ if unquote(tok).trim().is_empty() => {}
            _ => rest.push(tok),
        }
    }
    ((!asked.is_empty()).then(|| asked.join(" ")), rest.join(" "))
}

/// What [`Filter::prepare`] works out once for a pass over many photos.
pub(crate) struct Prepared {
    root: Option<String>,
    only: Option<std::collections::HashSet<PhotoId>>,
    /// [`Filter::ranked`]'s scores ([`Ranked::scores`]).
    ranked: Option<std::collections::HashMap<PhotoId, f32>>,
}

/// The tokens of a search text: split at whitespace, except inside double quotes, so
/// `person:"Jane Doe"` is one token (issue #1449). An unclosed quote runs to the end of the text; a
/// backslash is an ordinary character. One pass over the text, nothing allocated.
pub(crate) fn search_tokens(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let mut quoted = false;
        let end = rest
            .char_indices()
            .find(|&(_, c)| {
                quoted ^= c == '"';
                !quoted && c.is_whitespace()
            })
            .map_or(rest.len(), |(i, _)| i);
        let (tok, tail) = rest.split_at(end);
        rest = tail;
        Some(tok)
    })
}

/// `s` without the double quotes around it (a missing closing quote is allowed).
fn unquote(s: &str) -> &str {
    s.strip_prefix('"').map_or(s, |inner| inner.strip_suffix('"').unwrap_or(inner))
}

fn token_matches(p: &Photo, tok: &str) -> bool {
    let t = tok.to_lowercase();
    // a token that starts with a quote is a phrase, even with a colon in it
    if let Some((field, quoted)) = t.split_once(':').filter(|_| !t.starts_with('"')) {
        let val = unquote(quoted);
        // `>3`, `>=3`, `<3`, `<=3`, `=3` or `3`
        let num = |s: &str| -> Option<(&str, f64)> {
            let op = ["<=", ">=", "<", ">", "="].into_iter().find(|op| s.starts_with(op)).unwrap_or("");
            s.get(op.len()..)?.parse().ok().map(|v| (op, v))
        };
        let cmp = |x: f64, s: &str| match num(s) {
            Some((">", v)) => x > v,
            Some(("<", v)) => x < v,
            Some((">=", v)) => x >= v - 1e-9,
            Some(("<=", v)) => x <= v + 1e-9,
            Some((_, v)) => (x - v).abs() < 1e-9,
            None => false,
        };
        // Fields stored as `f32` compare at f32 precision: 2.8f32 widened to f64 is not 2.8.
        let cmp32 = |x: f32, s: &str| match num(s) {
            Some((">", v)) => x > v as f32,
            Some(("<", v)) => x < v as f32,
            Some((">=", v)) => x >= v as f32,
            Some(("<=", v)) => x <= v as f32,
            Some((_, v)) => x == v as f32,
            None => false,
        };
        return match field {
            "rating" | "stars" => cmp(p.rating as f64, val),
            "flag" => Flag::parse(val) == Some(p.flag),
            "label" | "color" => p.label.is_some_and(|l| format!("{l:?}").eq_ignore_ascii_case(val)),
            "iso" => p.meta.iso.is_some_and(|i| cmp(i as f64, val)),
            "f" | "aperture" => p.meta.aperture.is_some_and(|a| cmp32(a, val)),
            "focal" => p.meta.focal_mm.is_some_and(|a| cmp32(a, val)),
            "camera" => p.meta.camera.to_lowercase().contains(val),
            "lens" => p.meta.lens.to_lowercase().contains(val),
            "keyword" | "kw" => p.meta.keywords.iter().any(|k| crate::keywords::is_under(k, val)),
            "person" | "who" => has_person(p, val),
            "type" | "kind" => format!("{:?}", p.kind).eq_ignore_ascii_case(val),
            "edited" => (val == "true" || val == "yes") == p.is_edited(),
            "date" => p.captured.as_deref().is_some_and(|c| c.starts_with(val)),
            "copy" | "virtual" => (val == "true" || val == "yes") == p.copy_of.is_some(),
            "name" | "file" => p.file_name.to_lowercase().contains(val),
            // answered outside the catalog, by `Filter::ranked`
            f if CONTENT_FIELDS.contains(&f) => true,
            // Not a known field: the colon is part of a plain word ("10:30", "Re:"), its value unquoted
            // (`re:"a b"` is the text `re:a b`).
            _ if val.len() != quoted.len() => text_matches(p, &format!("{field}:{val}")),
            _ => text_matches(p, &t),
        };
    }
    text_matches(p, unquote(&t))
}

/// Whether the lower-cased word `t` is in the file name, title, caption, camera, lens, location,
/// format, a keyword or the name of a person (a named face) of `p`.
fn text_matches(p: &Photo, t: &str) -> bool {
    let hay = [&p.file_name, &p.meta.title, &p.meta.caption, &p.meta.camera, &p.meta.lens, &p.meta.location, &p.format];
    hay.iter().any(|h| h.to_lowercase().contains(t))
        || p.meta.keywords.iter().any(|k| k.to_lowercase().contains(t))
        || p.people().iter().any(|n| n.to_lowercase().contains(t))
}

/// Whether `p` has a named face region called `name` (case-insensitive).
fn has_person(p: &Photo, name: &str) -> bool {
    let name = name.trim().to_lowercase();
    p.people().iter().any(|n| n.to_lowercase() == name)
}

impl Filter {
    /// The labels that `label` and `labels` let through together (they AND), in `labels`' order;
    /// `None` when neither is set, empty when they exclude each other. The chip and the filter
    /// bar's buttons show this, so they match what is filtered.
    pub fn labels_shown(&self) -> Option<Vec<ColorLabel>> {
        match (self.label, self.labels.is_empty()) {
            (None, true) => None,
            (Some(l), true) => Some(vec![l]),
            (l, false) => Some(self.labels.iter().copied().filter(|x| l.is_none_or(|l| l == *x)).collect()),
        }
    }

    /// Human-readable summary of the active rules (smart album tooltips, `album.list`).
    pub fn describe(&self) -> String {
        self.describe_in(None)
    }

    /// [`Filter::describe`] naming the albums its rules test (`RuleSet::describe_with`).
    pub fn describe_with(&self, cat: &crate::Catalog) -> String {
        self.describe_in(Some(cat))
    }

    fn describe_in(&self, cat: Option<&crate::Catalog>) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.rating > 0 {
            let op = match self.rating_op {
                RatingOp::AtLeast => "≥",
                RatingOp::Exactly => "=",
                RatingOp::AtMost => "≤",
            };
            v.push(format!("rating {op} {}", self.rating));
        }
        if let Some(f) = self.flag {
            v.push(format!("flag {}", format!("{f:?}").to_lowercase()));
        }
        if let Some(l) = self.label {
            v.push(format!("label {}", format!("{l:?}").to_lowercase()));
        }
        if !self.labels.is_empty() {
            v.push(format!("label {}", self.labels.iter().map(|l| format!("{l:?}").to_lowercase()).collect::<Vec<_>>().join(" or ")));
        }
        if let Some(k) = self.kind {
            v.push(format!("kind {}", format!("{k:?}").to_lowercase()));
        }
        if let Some(e) = self.edited {
            v.push(if e { "edited".into() } else { "unedited".into() });
        }
        for (name, val) in [
            ("keyword", &self.keyword),
            ("person", &self.person),
            ("camera", &self.camera),
            ("lens", &self.lens),
            ("date", &self.date),
            ("folder", &self.library_folder),
            ("imported", &self.imported),
        ] {
            if let Some(x) = val {
                v.push(format!("{name} {x}"));
            }
        }
        match (&self.date_from, &self.date_to) {
            (Some(a), Some(b)) => v.push(format!("captured {a} – {b}")),
            (Some(a), None) => v.push(format!("captured from {a}")),
            (None, Some(b)) => v.push(format!("captured until {b}")),
            _ => {}
        }
        if !self.text.trim().is_empty() {
            v.push(format!("“{}”", self.text.trim()));
        }
        if let Some(rs) = self.rule_set.as_ref().filter(|r| !r.rules.is_empty()) {
            v.push(cat.map_or_else(|| rs.describe(), |c| rs.describe_with(c)));
        }
        if v.is_empty() { "all photos".into() } else { v.join(", ") }
    }

    /// The albums this filter tests: its album field and its rules' Album rules.
    pub fn albums_tested(&self) -> Vec<crate::AlbumId> {
        let mut out: Vec<crate::AlbumId> = self.album.into_iter().collect();
        out.extend(self.rule_set.as_ref().map(crate::RuleSet::albums_tested).unwrap_or_default());
        out
    }

    /// Whether matches depend on the clock (only "in the last…" rules do).
    pub fn depends_on_now(&self) -> bool {
        self.rule_set.as_ref().is_some_and(crate::RuleSet::depends_on_now)
    }

    pub fn matches(&self, p: &Photo, cat: &Catalog) -> bool {
        self.matches_in(p, cat, &self.prepare_one())
    }

    /// [`Filter::prepare`] for testing one photo (the `only` ids are looked through, not hashed).
    pub(crate) fn prepare_one(&self) -> Prepared {
        Prepared { root: self.library_root(), only: None, ranked: self.ranked.as_ref().map(Ranked::scores) }
    }

    /// What [`Filter::matches_in`] needs worked out once for a pass over many photos rather than
    /// once per photo: the library folder's identity and the `only` ids as a set.
    pub(crate) fn prepare(&self) -> Prepared {
        Prepared {
            root: self.library_root(),
            only: (!self.only.is_empty()).then(|| self.only.iter().copied().collect()),
            ranked: self.ranked.as_ref().map(Ranked::scores),
        }
    }

    /// [`Filter::library_folder`] as a folder identity ([`folder_key`]); `None` without a choice.
    fn library_root(&self) -> Option<String> {
        // as given: a folder may be named with spaces at its edge, and trimming would make it
        // another folder
        self.library_folder.as_deref().filter(|d| !d.trim().is_empty()).map(folder_key)
    }

    /// [`Filter::matches`] with what [`Filter::prepare`] works out done already, so a query over a
    /// big library does that once, not once per photo.
    pub(crate) fn matches_in(&self, p: &Photo, cat: &Catalog, prepared: &Prepared) -> bool {
        if p.deleted != self.deleted {
            return false;
        }
        match &self.folder {
            // browsed photos only show in folder views, and in Recently Deleted once deleted (#1430)
            None if p.local && !p.deleted => return false,
            None => {}
            Some(dir) => {
                let crate::Source::File { path } = &p.source else { return false };
                if !in_folder(path, dir, self.subfolders) {
                    return false;
                }
            }
        }
        if self.rating > 0 {
            let ok = match self.rating_op {
                RatingOp::AtLeast => p.rating >= self.rating,
                RatingOp::Exactly => p.rating == self.rating,
                RatingOp::AtMost => p.rating <= self.rating,
            };
            if !ok {
                return false;
            }
        }
        if self.flag.is_some_and(|f| f != p.flag) || self.label.is_some_and(|l| Some(l) != p.label) || self.kind.is_some_and(|k| k != p.kind) {
            return false;
        }
        if self.edited.is_some_and(|e| e != p.is_edited()) {
            return false;
        }
        if let Some(rs) = &self.rule_set
            && !rs.matches(p, cat)
        {
            return false;
        }
        if !self.labels.is_empty() && !p.label.is_some_and(|l| self.labels.contains(&l)) {
            return false;
        }
        if self.only_none {
            return false;
        }
        if !self.only.is_empty() && !prepared.only.as_ref().map_or_else(|| self.only.contains(&p.id), |only| only.contains(&p.id)) {
            return false;
        }
        if prepared.ranked.as_ref().is_some_and(|r| !r.contains_key(&p.id)) {
            return false;
        }
        if let Some(want) = &self.merged {
            match merged_kind(&p.file_name) {
                Some(k) if want == "any" || want == k => {}
                _ => return false,
            }
        }
        if let Some(a) = self.album
            && !cat.album_contains(a, p)
        {
            return false;
        }
        if let Some(from) = &self.date_from
            && match p.captured.as_deref() {
                Some(c) => c < from.as_str(),
                None => true,
            }
        {
            return false;
        }
        if let Some(to) = &self.date_to
            && match p.captured.as_deref() {
                Some(c) => c.get(..to.len()).unwrap_or(c) > to.as_str(),
                None => true,
            }
        {
            return false;
        }
        if let Some(l) = &self.lens
            && !p.meta.lens.to_lowercase().contains(&l.to_lowercase())
        {
            return false;
        }
        if let Some(d) = &self.date
            && !p.captured.as_deref().is_some_and(|c| c.starts_with(d.as_str()))
        {
            return false;
        }
        if let Some(root) = prepared.root.as_deref()
            && !photo_in_root(p, root)
        {
            return false;
        }
        if let Some(d) = &self.imported
            && !p.imported.starts_with(d.as_str())
        {
            return false;
        }
        if let Some(d) = &self.imported_from
            && p.imported.as_str() < d.as_str()
        {
            return false;
        }
        if let Some(k) = &self.keyword
            && !p.meta.keywords.iter().any(|x| crate::keywords::is_under(x, k))
        {
            return false;
        }
        if let Some(n) = &self.person
            && !has_person(p, n)
        {
            return false;
        }
        if let Some(c) = &self.camera
            && !p.meta.camera.eq_ignore_ascii_case(c)
        {
            return false;
        }
        search_tokens(&self.text).all(|tok| token_matches(p, tok))
    }
}

/// Case-insensitive file name order without allocating (compares per-character lowercase via `char::to_lowercase`).
pub fn cmp_name(a: &str, b: &str) -> std::cmp::Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

impl Catalog {
    /// Photos matching `filter`, in `sort` order (ties broken by file name, then id).
    pub fn query(&self, filter: &Filter, sort: &Sort) -> Vec<PhotoId> {
        self.query_keeping(filter, sort, &std::collections::HashSet::new(), filter)
    }

    /// [`Catalog::query`], and with them the photos of `keep` that still match `within`, each at its place in `sort`
    /// order: a view that holds the photos it showed when an edit makes them stop matching `filter` (a smart
    /// album's rules, the filter bar), as long as they are still where the view looks (`within`: in the library,
    /// the album, the folder; not deleted).
    pub fn query_keeping(&self, filter: &Filter, sort: &Sort, keep: &std::collections::HashSet<PhotoId>, within: &Filter) -> Vec<PhotoId> {
        let prepared = filter.prepare();
        // `within` only decides for the few kept photos: worked out per photo, not hashed
        let within_prepared = within.prepare_one();
        let mut v: Vec<&Photo> = self
            .photos()
            .map(|p| p.as_ref())
            .filter(|p| filter.matches_in(p, self, &prepared) || (keep.contains(&p.id) && within.matches_in(p, self, &within_prepared)))
            .collect();
        v.sort_by(|a, b| sort.order_ranked(a, b, prepared.ranked.as_ref()));
        v.into_iter().map(|p| p.id).collect()
    }

    /// The list `was` (a [`Catalog::query`] of `filter` in `sort` order, or a [`Catalog::query_keeping`]), after ops
    /// that touched only the photos `touched` (sorted by id, each once: [`Catalog::touched_since`]): those photos
    /// taken out, and the ones that match `filter` now put back at their place in the order. With `within`, a
    /// touched photo that was in the list and still matches `within` stays, as `query_keeping` would keep it. What
    /// the query would give, without asking every photo again.
    pub fn follow(&self, was: &[PhotoId], touched: &[PhotoId], filter: &Filter, sort: &Sort, within: Option<&Filter>) -> Vec<PhotoId> {
        let listed: std::collections::HashSet<PhotoId> = was.iter().copied().filter(|id| touched.binary_search(id).is_ok()).collect();
        let mut ids: Vec<PhotoId> = was.iter().copied().filter(|id| !listed.contains(id)).collect();
        let (prepared, within_prepared) = (filter.prepare_one(), within.map(Filter::prepare_one));
        for id in touched {
            let Some(p) = self.photo(*id) else { continue };
            let kept = within.zip(within_prepared.as_ref()).is_some_and(|(w, wp)| listed.contains(id) && w.matches_in(p, self, wp));
            if filter.matches_in(p, self, &prepared) || kept {
                // (the list is in order without the touched photos, and stays so as each is put back)
                let at = ids.partition_point(|other| self.photo(*other).is_some_and(|o| sort.order_ranked(o, p, prepared.ranked.as_ref()).is_lt()));
                ids.insert(at, *id);
            }
        }
        ids
    }

    /// How many photos match `filter`: [`Catalog::query`]'s length without its sorting.
    pub fn count(&self, filter: &Filter) -> usize {
        let prepared = filter.prepare();
        self.photos().filter(|p| filter.matches_in(p, self, &prepared)).count()
    }

    /// Year → month → day counts for the "By Date" section (newest first).
    pub fn date_groups(&self) -> Vec<DateGroup> {
        use std::collections::BTreeMap;
        let mut years: BTreeMap<&str, (BTreeMap<&str, usize>, BTreeMap<&str, usize>)> = BTreeMap::new();
        for p in self.photos().filter(|p| p.in_library()) {
            let Some(d) = p.captured.as_deref() else { continue };
            // `get`, not `[..]`: a corrupt or foreign `captured` (a Lightroom catalog's
            // captureTime is copied verbatim) may not be ASCII, and slicing it on a byte index
            // would panic — the same rule as `dates::group_label`.
            if let (Some(year), Some(month), Some(day)) = (d.get(..4), d.get(..7), d.get(..10)) {
                let (months, days) = years.entry(year).or_default();
                *months.entry(month).or_default() += 1;
                *days.entry(day).or_default() += 1;
            }
        }
        let newest_first = |m: BTreeMap<&str, usize>| m.into_iter().rev().map(|(k, n)| (k.to_string(), n)).collect::<Vec<_>>();
        years
            .into_iter()
            .rev()
            .map(|(year, (months, days))| DateGroup {
                year: year.to_string(),
                count: months.values().sum(),
                months: newest_first(months),
                days: newest_first(days),
            })
            .collect()
    }

    /// All keywords with usage counts, sorted by name.
    pub fn keywords(&self) -> Vec<(String, usize)> {
        let mut m: std::collections::BTreeMap<String, usize> = Default::default();
        for p in self.photos().filter(|p| p.in_library()) {
            for k in &p.meta.keywords {
                *m.entry(k.clone()).or_default() += 1;
            }
        }
        m.into_iter().collect()
    }

    /// The people named on faces in the library (MWG regions read from XMP): how many photos each
    /// appears in and the photo showing their largest face (for a card's picture). Most photos first,
    /// then by name. Names that differ only in case are one person, shown as first seen; a person
    /// twice in one photo counts once.
    pub fn people(&self) -> Vec<Person> {
        self.people_in(&Filter::default())
    }

    /// [`Self::people`] among the photos `filter` lets through (its `person` is ignored): what the
    /// People view offers while other filters (a date, a rating, an album…) are active, so picking
    /// a person never ends in an empty grid.
    pub fn people_in(&self, filter: &Filter) -> Vec<Person> {
        let filter = Filter { person: None, ..filter.clone() };
        let mut m: std::collections::HashMap<String, (Person, f64)> = Default::default();
        let prepared = filter.prepare();
        for p in self.photos().filter(|p| filter.matches_in(p, self, &prepared)) {
            let mut seen: Vec<String> = Vec::new();
            for r in p.meta.regions.iter().filter(|r| r.kind == lightcraft_meta::RegionKind::Face) {
                let Some(name) = r.name.as_deref().map(str::trim).filter(|n| !n.is_empty()) else { continue };
                let key = name.to_lowercase();
                // the face's size in pixels of the photo
                let area = (r.rect.x1 - r.rect.x0) * (r.rect.y1 - r.rect.y0) * p.width as f64 * p.height as f64;
                let (person, best) =
                    m.entry(key.clone()).or_insert_with(|| (Person { name: name.to_string(), count: 0, photo: p.id, face: r.rect }, area));
                if !seen.contains(&key) {
                    person.count += 1;
                    seen.push(key);
                }
                if area > *best || (area == *best && p.id < person.photo) {
                    (person.photo, person.face, *best) = (p.id, r.rect, area);
                }
            }
        }
        let mut v: Vec<Person> = m.into_values().map(|(p, _)| p).collect();
        v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        v
    }
}

/// A person named on faces in the library ([`Catalog::people`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    /// Photos they appear in.
    pub count: usize,
    /// The photo with their largest face, and that face (normalized, in the photo's upright frame).
    pub photo: PhotoId,
    pub face: lightcraft_meta::Rect,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DateGroup {
    pub year: String,
    pub count: usize,
    /// `YYYY-MM` and counts, newest first.
    pub months: Vec<(String, usize)>,
    /// `YYYY-MM-DD` and counts, newest first.
    pub days: Vec<(String, usize)>,
}

/// Whether file `path` is directly in `dir` (or anywhere below it with `deep`). Both `/` and `\\`
/// separate.
pub fn in_folder(path: &str, dir: &str, deep: bool) -> bool {
    let dir = dir.trim_end_matches(['/', '\\']);
    let Some(rest) = path.strip_prefix(dir) else { return false };
    let Some(rest) = rest.strip_prefix(['/', '\\']) else { return false };
    !rest.is_empty() && (deep || !rest.contains(['/', '\\']))
}

/// A path read the way [`folder_key`] reads it: the part `..` can't climb above (`//server/share`,
/// `c:`; empty for `/` and relative paths), the names below it, and whether it is absolute.
pub(crate) struct SplitPath {
    pub prefix: String,
    pub parts: Vec<String>,
    pub absolute: bool,
}

/// Both separators, repeated separators (also between a server and its share), `.` and a
/// trailing separator dropped, `..` resolved lexically, a Windows verbatim prefix removed;
/// `lower_drive` lower-cases a drive letter (the identity does, a display name wants it as is).
pub(crate) fn split_path(path: &str, lower_drive: bool) -> SplitPath {
    let mut s = path.replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/").or_else(|| s.strip_prefix("//./")) {
        s = match rest.strip_prefix("UNC/") {
            Some(unc) => format!("//{unc}"),
            None => rest.to_string(),
        };
    }
    let (prefix, rest) = if let Some(unc) = s.strip_prefix("//").filter(|u| !u.is_empty() && !u.starts_with('/')) {
        let mut segs = unc.split('/').filter(|x| !x.is_empty());
        let (server, share) = (segs.next().unwrap_or(""), segs.next().unwrap_or(""));
        (format!("//{server}/{share}"), segs.collect::<Vec<_>>().join("/"))
    } else if let (true, Some(drive), Some(rest)) = (matches!(s.as_bytes(), [a, b':', ..] if a.is_ascii_alphabetic()), s.get(..2), s.get(2..)) {
        (if lower_drive { drive.to_ascii_lowercase() } else { drive.to_string() }, rest.to_string())
    } else {
        (String::new(), s.clone())
    };
    let absolute = rest.starts_with('/') || !prefix.is_empty();
    let mut parts: Vec<String> = Vec::new();
    for c in rest.split('/') {
        match c {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            c => parts.push(c.to_string()),
        }
    }
    SplitPath { prefix, parts, absolute }
}

/// A folder path's identity, for telling whether two spellings name the same folder: `/` and
/// `\\` are both separators, repeated separators, `.` and a trailing separator are dropped,
/// `..` is resolved lexically, a Windows verbatim prefix (`\\?\`) is removed and the drive
/// letter lower-cased; on Windows (case-insensitive file names) the whole path is lower-cased.
/// File names are otherwise compared as written, so on a case-insensitive disk elsewhere
/// (macOS, a Samba share) two spellings that differ in case are two folders here.
/// No file-system access: the paths compared should already be absolute.
pub fn folder_key(path: &str) -> String {
    let SplitPath { prefix, parts, absolute } = split_path(path, true);
    let key = if absolute { format!("{prefix}/{}", parts.join("/")) } else { parts.join("/") };
    if cfg!(windows) { key.to_lowercase() } else { key }
}

/// Whether `path` is folder `root` or lies somewhere inside it, however either is spelled
/// (see [`folder_key`]).
pub fn folder_within(path: &str, root: &str) -> bool {
    key_within(&folder_key(path), &folder_key(root))
}

/// The names below folder `root` that lead to `path`, as the caller spelled them (`None` when
/// `path` is not at or inside `root`; empty when it is `root`). Compared by [`folder_key`], but
/// the names keep their case, which the key lowers on Windows.
pub fn folder_rest(path: &str, root: &str) -> Option<Vec<String>> {
    if !key_within(&folder_key(path), &folder_key(root)) {
        return None;
    }
    let depth = split_path(root, false).parts.len();
    Some(split_path(path, false).parts.into_iter().skip(depth).collect())
}

/// Whether a library photo (not one only browsed in Local) lies in the folder whose
/// [`folder_key`] is `root`. A plain POSIX path (no `\`, `.`, `..`, repeated or trailing
/// separators) is its own identity, so it is read without building a key for it: the check runs
/// for every photo of the library on each refresh, and this keeps it cheap.
pub(crate) fn photo_in_root(p: &Photo, root: &str) -> bool {
    if p.local {
        return false;
    }
    let crate::Source::File { path } = &p.source else { return false };
    let plain = path.starts_with('/')
        && !path.starts_with("//")
        && !path.ends_with('/')
        && !path.contains(['\\'])
        && !path.contains("//")
        && !path.contains("/.");
    if !cfg!(windows) && plain && root.starts_with('/') && !root.starts_with("//") {
        let r = root.trim_end_matches('/');
        return path.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'));
    }
    crate::local::folder_of(p).is_some_and(|f| key_within(&f, root))
}

/// [`folder_within`] for two paths already turned into their [`folder_key`].
pub(crate) fn key_within(p: &str, r: &str) -> bool {
    // `.` or `a/..` name no folder: not "everything"
    if r.is_empty() {
        return false;
    }
    // a disk's root key ends in `/` (`c:/`, `//srv/share/`), a folder's does not: same folder
    let (p, r) = (p.trim_end_matches('/'), r.trim_end_matches('/'));
    p == r || p.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
}

/// The kind of Photo Merge result a file is, from the name merges give it (`IMG_1-HDR.dng`,
/// `IMG_1-Pano.dng`, `IMG_1-HDR-Pano.dng` — also the names other raw editors use):
/// `hdr`, `panorama` or `hdrPanorama`.
pub fn merged_kind(file_name: &str) -> Option<&'static str> {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s).to_ascii_lowercase();
    let stem = stem.trim_end_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ');
    if stem.ends_with("-hdr-pano") {
        Some("hdrPanorama")
    } else if stem.ends_with("-hdr") {
        Some("hdr")
    } else if stem.ends_with("-pano") {
        Some("panorama")
    } else {
        None
    }
}
