//! Library view state: what the grid shows and what is selected.

use lcc_catalog::{AlbumId, Catalog, Filter, PhotoId};
use serde::{Deserialize, Serialize};

/// The "My Photos" source in the left panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "camelCase")]
pub enum LibrarySource {
    #[default]
    All,
    /// Imported in the last import session (we treat "recent" as the latest import date).
    RecentlyAdded,
    Album(AlbumId),
    RecentlyDeleted,
    /// Photos with picks.
    Picks,
    /// A folder on disk ([`crate::Session::browse`]): its files, added to the library or not.
    Folder,
    /// Photos whose original file can't be found (`library.missing`).
    Missing,
    /// A folder the library's photos were imported from ([`crate::Session::library_folder`]),
    /// and the folders inside it (`library.folders`).
    LibraryFolder,
    /// The photos of the latest import (one import shares one timestamp).
    PreviousImport,
    /// The Quick Collection (empty until something was added to it).
    QuickCollection,
    /// The photos of the last export ([`crate::Session::previous_export`]).
    PreviousExport,
}

/// The sources `library.source` showed, oldest first, with the one shown now: Go Back / Go Forward walk it.
/// Each entry is the `library.source` parameters that show the source again, so a source that is gone by then
/// (a deleted collection, a folder no photo came from any more) fails to show and is skipped.
#[derive(Clone, Debug, Default)]
pub struct SourceHistory {
    entries: Vec<serde_json::Value>,
    at: usize,
}

impl SourceHistory {
    /// How many sources are kept; the oldest go first.
    pub const MAX: usize = 50;

    /// `library.source` parameters for the source `s` shows now; `None` for one it can't be asked for again
    /// (a browsed folder).
    pub fn params_of(s: &crate::Session) -> Option<serde_json::Value> {
        use serde_json::json;
        match s.source {
            LibrarySource::All => Some(json!({"kind": "all"})),
            LibrarySource::RecentlyAdded => Some(json!({"kind": "recentlyAdded"})),
            LibrarySource::RecentlyDeleted => Some(json!({"kind": "recentlyDeleted"})),
            LibrarySource::Picks => Some(json!({"kind": "picks"})),
            LibrarySource::Missing => Some(json!({"kind": "missing"})),
            LibrarySource::PreviousImport => Some(json!({"kind": "previousImport"})),
            LibrarySource::QuickCollection => Some(json!({"kind": "quickCollection"})),
            LibrarySource::PreviousExport => Some(json!({"kind": "previousExport"})),
            LibrarySource::Album(a) => Some(json!({"kind": "album", "id": a.0})),
            LibrarySource::LibraryFolder => s.library_folder.as_ref().map(|p| json!({"kind": "libraryFolder", "path": p})),
            LibrarySource::Folder => None,
        }
    }

    /// A source was chosen: `before` is what was shown until then, `after` what is shown now. The forward half goes.
    pub fn chose(&mut self, before: Option<serde_json::Value>, after: Option<serde_json::Value>) {
        if self.entries.is_empty() {
            self.entries.extend(before);
            self.at = 0;
        }
        let Some(after) = after else { return };
        if self.entries.get(self.at) == Some(&after) {
            return;
        }
        self.entries.truncate(self.at.saturating_add(1));
        self.entries.push(after);
        if self.entries.len() > Self::MAX {
            let extra = self.entries.len() - Self::MAX;
            self.entries.drain(..extra);
        }
        self.at = self.entries.len().saturating_sub(1);
    }

    pub fn can_back(&self) -> bool {
        self.at > 0
    }

    pub fn can_forward(&self) -> bool {
        self.at.saturating_add(1) < self.entries.len()
    }

    /// The entries to try, nearest first, as `(index, params)`, in direction `dir` (-1 back, 1 forward).
    pub fn candidates(&self, dir: i32) -> Vec<(usize, serde_json::Value)> {
        if dir < 0 {
            (0..self.at).rev().filter_map(|i| self.entries.get(i).map(|e| (i, e.clone()))).collect()
        } else {
            (self.at.saturating_add(1)..self.entries.len()).filter_map(|i| self.entries.get(i).map(|e| (i, e.clone()))).collect()
        }
    }

    /// The entry at `i` is shown now.
    pub fn arrived(&mut self, i: usize) {
        self.at = i.min(self.entries.len().saturating_sub(1));
    }
}

/// The folder a [`LibrarySource::Folder`] view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Browse {
    pub path: String,
    pub subfolders: bool,
    /// The Browse tab's quick filter on "Show Not Imported": only the files an import would take.
    /// Off (the default), the view is everything on disk, the files the library has already
    /// included — they carry an In library badge in the grid.
    pub not_imported: bool,
}

/// How far back Recently Added reaches from the latest import.
pub const RECENT_DAYS: i64 = 30;

impl LibrarySource {
    pub fn to_filter(&self, f: &Filter, cat: &Catalog) -> Filter {
        let mut f = f.clone();
        match self {
            LibrarySource::All => {}
            // everything imported in the 30 days up to the latest import (by import, newest first)
            LibrarySource::RecentlyAdded => {
                let latest = cat.photos().filter(|p| p.in_library()).map(|p| p.imported.clone()).max().unwrap_or_default();
                let from = lcc_catalog::stacks::iso_seconds(&latest)
                    .map(|s| lcc_catalog::dates::civil(s - RECENT_DAYS * 86_400))
                    .unwrap_or_else(|| latest.get(..10).unwrap_or("").to_string());
                f.imported_from = Some(from);
            }
            LibrarySource::Album(a) => f.album = Some(*a),
            LibrarySource::PreviousImport => {
                let latest = cat.photos().filter(|p| p.in_library()).map(|p| p.imported.clone()).max().unwrap_or_default();
                f.imported_from = Some(latest);
            }
            // no Quick Collection yet: an album id no album has, so nothing shows
            LibrarySource::QuickCollection => f.album = Some(cat.quick_collection().unwrap_or(AlbumId(u64::MAX))),
            LibrarySource::RecentlyDeleted => f.deleted = true,
            LibrarySource::Picks => f.flag = Some(lcc_catalog::Flag::Pick),
            // the folder itself is filled in by the session (it holds the path)
            LibrarySource::Folder | LibrarySource::Missing | LibrarySource::LibraryFolder | LibrarySource::PreviousExport => {}
        }
        f
    }

    pub fn label(&self, cat: &Catalog) -> String {
        match self {
            LibrarySource::All => "All Photos".into(),
            LibrarySource::RecentlyAdded => "Recently Added".into(),
            LibrarySource::Album(a) => cat.album(*a).map(|a| a.name.clone()).unwrap_or_else(|| "Collection".into()),
            LibrarySource::RecentlyDeleted => "Recently Deleted".into(),
            LibrarySource::Picks => "Picks".into(),
            LibrarySource::Folder => "Folder".into(),
            LibrarySource::LibraryFolder => "Folder".into(),
            LibrarySource::Missing => "Missing Photos".into(),
            LibrarySource::PreviousImport => "Previous Import".into(),
            LibrarySource::QuickCollection => "Quick Collection".into(),
            LibrarySource::PreviousExport => "Previous Export".into(),
        }
    }
}

/// Selected photos (ordered) and the active (most-selected) one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub ids: Vec<PhotoId>,
    pub active: Option<PhotoId>,
}

/// How a photo relates to the selection: what every view draws (grid cell, filmstrip cell…).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionState {
    NotSelected,
    /// Selected, but not the photo the loupe and panels act on.
    Selected,
    /// The most-selected photo.
    Active,
}

impl Selection {
    /// What views draw for `id`: active, selected or neither (one answer for grid, filmstrip, …).
    pub fn state_of(&self, id: PhotoId) -> SelectionState {
        if self.active == Some(id) {
            SelectionState::Active
        } else if self.contains(id) {
            SelectionState::Selected
        } else {
            SelectionState::NotSelected
        }
    }
    pub fn single(id: PhotoId) -> Selection {
        Selection { ids: vec![id], active: Some(id) }
    }
    pub fn contains(&self, id: PhotoId) -> bool {
        self.ids.contains(&id)
    }
    pub fn toggle(&mut self, id: PhotoId) {
        if let Some(i) = self.ids.iter().position(|x| *x == id) {
            self.ids.remove(i);
            // taking the active photo out: the nearest selected photo before it (else after it) is active
            if self.active == Some(id) {
                self.active = self.ids.get(i.saturating_sub(1)).or(self.ids.first()).copied();
            }
        } else {
            // Lightroom Classic: adding to a selection keeps the photo that was active (the
            // "most selected" one, e.g. the source of Sync Settings) active (#119).
            self.ids.push(id);
            self.active = self.active.or(Some(id));
        }
    }
    /// Shift-click: select the range between the active photo and `id` in `order`.
    pub fn extend_to(&mut self, id: PhotoId, order: &[PhotoId]) {
        let anchor = self.active.unwrap_or(id);
        let (Some(a), Some(b)) = (order.iter().position(|x| *x == anchor), order.iter().position(|x| *x == id)) else {
            *self = Selection::single(id);
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        // A set keeps this linear: `Vec::contains` per photo was quadratic on big ranges.
        let mut seen: std::collections::HashSet<PhotoId> = self.ids.iter().copied().collect();
        for x in order.get(lo..=hi).unwrap_or_default() {
            if seen.insert(*x) {
                self.ids.push(*x);
            }
        }
        // The anchor stays the active photo, as in Lightroom Classic (#119).
        self.active = Some(anchor);
    }
    /// A plain click on a photo's picture, as in Lightroom: on a photo of a selection of several it
    /// becomes the active photo and the selection stays; anywhere else the photo alone is selected.
    pub fn click(&mut self, id: PhotoId) {
        if self.ids.len() > 1 && self.contains(id) {
            self.active = Some(id);
        } else {
            *self = Selection::single(id);
        }
    }
}

/// One active filter, shown as a removable chip above the grid.
#[derive(Clone, Debug, PartialEq)]
pub struct FilterChip {
    pub label: String,
    /// `library.filter` patch that clears just this filter.
    pub clear: serde_json::Value,
}

/// The chips for every constraint in the filter bar / search / sidebar (not the source itself).
pub fn filter_chips(f: &Filter, cat: &Catalog) -> Vec<FilterChip> {
    use lcc_catalog::{MediaKind, RatingOp};
    use serde_json::{Value::Null, json};
    let mut v = Vec::new();
    let mut add = |label: String, clear: serde_json::Value| v.push(FilterChip { label, clear });
    if !f.text.trim().is_empty() {
        add(format!("Search: “{}”", f.text.trim()), json!({"text": ""}));
    }
    if f.rating_filters() {
        let op = match f.rating_op {
            RatingOp::AtLeast => "≥",
            RatingOp::Exactly => "=",
            RatingOp::AtMost => "≤",
        };
        // "= 0" and "≤ 0" are the unrated photos; clearing the chip takes the operator back too
        let label = if f.rating == 0 { "Unrated".to_string() } else { format!("Rating {op} {}★", f.rating) };
        add(label, json!({"rating": 0, "ratingOp": "atLeast"}));
    }
    if let Some(shown) = f.flags_shown() {
        let names = shown.iter().map(|fl| format!("{fl:?}").to_lowercase()).collect::<Vec<_>>().join(" or ");
        add(format!("Flag: {names}"), json!({"flag": Null, "flags": []}));
    }
    if let Some(shown) = f.labels_shown() {
        let or = |ls: &[lcc_catalog::ColorLabel]| ls.iter().map(|l| cat.label_name(*l)).collect::<Vec<_>>().join(" or ");
        // empty: `label` and `labels` exclude each other, so name both constraints
        let names = if shown.is_empty() {
            // parenthesised when plural, so "Red and Yellow or Green" cannot read as "(Red and Yellow) or Green"
            let plural = if f.labels.len() > 1 { format!("({})", or(&f.labels)) } else { or(&f.labels) };
            format!("{} and {plural}", f.label.map(|l| cat.label_name(l)).unwrap_or_default())
        } else {
            or(&shown)
        };
        add(format!("Label: {names}"), json!({"label": Null, "labels": []}));
    }
    if let Some(k) = f.kind {
        let n = match k {
            MediaKind::Image => "Photos",
            MediaKind::Raw => "Raw",
            MediaKind::Video => "Videos",
        };
        add(format!("Type: {n}"), json!({"kind": Null}));
    }
    if let Some(m) = &f.merged {
        let n = match m.as_str() {
            "hdr" => "HDR",
            "panorama" => "Panoramas",
            "hdrPanorama" => "HDR panoramas",
            "focusStack" => "Focus stacks",
            _ => "Merged",
        };
        add(format!("Type: {n}"), json!({"merged": Null}));
    }
    if let Some(e) = f.edited {
        add(if e { "Edited".into() } else { "Unedited".into() }, json!({"edited": Null}));
    }
    // a facet's single value and its multi-select list are one chip ("Keyword: travel or family")
    for (name, key, list_key, val, list) in [
        ("Camera", "camera", "cameras", &f.camera, &f.cameras),
        ("Lens", "lens", "lenses", &f.lens, &f.lenses),
        ("Keyword", "keyword", "keywords", &f.keyword, &f.keywords),
        ("Person", "person", "people", &f.person, &f.people),
    ] {
        let mut values: Vec<&str> = list.iter().map(String::as_str).collect();
        if let Some(x) = val.as_deref().filter(|x| !values.iter().any(|v| v.eq_ignore_ascii_case(x))) {
            values.insert(0, x);
        }
        if !values.is_empty() {
            add(format!("{name}: {}", values.join(" or ")), json!({key: Null, list_key: []}));
        }
    }
    if let Some(x) = &f.faces {
        add(format!("Faces: {x}"), json!({"faces": Null}));
    }
    // the Image Quality Scanner's verdict and score floor (issue #1313): the verdict chip's label
    // is one the catalogs translate as it stands (the filter bar's own options)
    if let Some(q) = f.quality.as_deref() {
        let shown = match q {
            "pending" => "to review",
            "unscanned" => "not scanned",
            other => other,
        };
        add(format!("Quality: {shown}"), json!({"quality": Null}));
    }
    if let Some(a) = f.quality_min {
        add(format!("Quality score at least: {a:.0}"), json!({"qualityMin": Null}));
    }
    if let Some(b) = f.quality_max {
        add(format!("Quality score at most: {b:.0}"), json!({"qualityMax": Null}));
    }
    if let Some(d) = &f.date {
        add(format!("Date: {}", date_label(d)), json!({"date": Null}));
    }
    if let Some(d) = f.library_folder.as_deref().filter(|d| !d.trim().is_empty()) {
        // the last two names, so two folders called "Pictures" are told apart
        let name = lcc_catalog::folders::folder_label(d);
        add(format!("Folder: {name}"), json!({"libraryFolder": Null}));
    }
    if let Some(d) = &f.imported {
        add(format!("Imported: {}", date_label(d)), json!({"imported": Null}));
    }
    match (&f.date_from, &f.date_to) {
        (Some(a), Some(b)) => add(format!("Captured {a} – {b}"), json!({"dateFrom": Null, "dateTo": Null})),
        (Some(a), None) => add(format!("Captured from {a}"), json!({"dateFrom": Null})),
        (None, Some(b)) => add(format!("Captured until {b}"), json!({"dateTo": Null})),
        _ => {}
    }
    if let Some(rs) = f.rule_set.as_ref().filter(|r| !r.rules.is_empty()) {
        add(format!("Rules: {}", rs.describe_with(cat)), json!({"ruleSet": Null}));
    }
    if let Some(q) = f.semantic.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        add(format!("AI search: “{q}”"), json!({"semantic": Null, "only": [], "onlyNone": false}));
    } else if f.only_none || !f.only.is_empty() {
        let n = if f.only_none { 0 } else { f.only.len() };
        add(format!("Only {n} photos"), json!({"only": [], "onlyNone": false}));
    }
    v
}

/// `2026-01-16` → "January 16, 2026"; `2026-01` → "January 2026"; anything else as is.
fn date_label(d: &str) -> String {
    const MONTHS: [&str; 12] =
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let parts: Vec<&str> = d.split('-').collect();
    let month = parts.get(1).and_then(|m| m.parse::<usize>().ok()).filter(|m| (1..=12).contains(m)).map(|m| MONTHS[m - 1]);
    match (parts.as_slice(), month) {
        ([y, _, day], Some(m)) => format!("{m} {}, {y}", day.trim_start_matches('0')),
        ([y, _], Some(m)) => format!("{m} {y}"),
        _ => d.to_string(),
    }
}

#[cfg(test)]
mod selection_state_tests {
    use super::*;
    use lcc_catalog::PhotoId;

    #[test]
    fn extend_to_selects_a_large_range_once_and_keeps_the_anchor() {
        let order: Vec<PhotoId> = (0..200_000).map(PhotoId).collect();
        let mut s = Selection::single(PhotoId(10));
        s.toggle(PhotoId(150_000)); // already selected, inside the range
        let t = std::time::Instant::now();
        s.extend_to(PhotoId(199_999), &order);
        // Quadratic took minutes here; linear takes milliseconds. Generous bound for slow CI.
        assert!(t.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(s.ids.len(), 199_990);
        let uniq: std::collections::HashSet<_> = s.ids.iter().collect();
        assert_eq!(uniq.len(), s.ids.len());
        assert_eq!(s.active, Some(PhotoId(10)));
        assert!(s.contains(PhotoId(199_999)) && !s.contains(PhotoId(9)));
    }

    #[test]
    fn select_all_marks_the_others_selected_and_one_active() {
        let ids: Vec<PhotoId> = (1..=3).map(PhotoId).collect();
        let s = Selection { ids: ids.clone(), active: Some(ids[0]) };
        assert_eq!(s.state_of(ids[0]), SelectionState::Active);
        assert_eq!(s.state_of(ids[1]), SelectionState::Selected);
        assert_eq!(s.state_of(PhotoId(9)), SelectionState::NotSelected);
    }

    /// #119: the first photo clicked stays active (primary) when ⌘/Ctrl- or Shift-clicking more.
    #[test]
    fn adding_to_a_selection_keeps_the_first_photo_active() {
        let order: Vec<PhotoId> = (1..=5).map(PhotoId).collect();
        let mut s = Selection::single(order[1]);
        s.toggle(order[3]);
        assert_eq!(s.active, Some(order[1]));
        s.extend_to(order[4], &order);
        assert_eq!(s.active, Some(order[1]));
        assert_eq!(s.ids.len(), 4);
        s.toggle(order[1]);
        assert!(s.active.is_some_and(|a| s.contains(a)));
    }

    fn ids(n: u64) -> Vec<PhotoId> {
        (1..=n).map(PhotoId).collect()
    }

    /// ⌘/Ctrl-click adds and removes photos without changing the active one; taking the active
    /// photo out makes a neighbour active.
    #[test]
    fn ctrl_click_adds_and_removes_without_moving_the_active_photo() {
        let order = ids(5);
        let mut s = Selection::single(order[0]);
        s.toggle(order[3]);
        assert_eq!(s.active, Some(order[0]));
        s.toggle(order[3]);
        assert_eq!((s.active, s.ids.len()), (Some(order[0]), 1));
        s.toggle(order[2]);
        s.toggle(order[0]);
        assert_eq!(s.active, Some(order[2]));
    }

    /// A click inside the selection makes that photo active and keeps the selection; a click
    /// outside it replaces the selection.
    #[test]
    fn a_click_inside_the_selection_keeps_it() {
        let order = ids(8);
        let mut s = Selection::single(order[1]);
        s.extend_to(order[5], &order);
        s.click(order[3]);
        assert_eq!((s.active, s.ids.len()), (Some(order[3]), 5), "the whole set stays");
        s.click(order[7]);
        assert_eq!(s, Selection::single(order[7]), "a photo outside it replaces the selection");
    }
}
