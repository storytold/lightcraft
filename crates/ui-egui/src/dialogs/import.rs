//! The import review dialog (File → Import Photos… / Import from Folder… / Import from Device):
//! the source it scanned (a scanned folder is *not* added to Local), the files found under it as
//! a grid of thumbnails with checkboxes (duplicates marked and unchecked; Shift-click sets a
//! range), the destination (add in place / copy or move into the library's `Originals/` or a
//! chosen folder, filed by day, by month, into one folder or by a custom folder template,
//! optionally renamed), an album (existing or new), a preset and keywords to apply. Importing
//! runs on a worker thread (files are probed, copied or moved there) and its batches join the
//! catalog between frames, with a row and ✕ in the activity stack (issue #345); the whole import is
//! one undo step.

use egui::{Color32, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use lightcraft_engine::import::{
    ImportCandidate, ImportUprightMode, ImportWarning, ImportWarningCode, ScanInput, ScanOutput, ScanProgress, scan_with,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::atomic::Ordering;

use crate::LightcraftApp;
use crate::render::Slot;
use crate::theme::Tokens;
use crate::widgets::register;

mod body;
mod scan;
mod tags;
mod task;

#[cfg(test)]
pub(crate) use body::grid_height;
#[cfg(test)]
pub(crate) use body::thumb_edge;
#[cfg(test)]
use body::{DEFAULT_BELOW_GRID, badge_rect, dup_counts};
pub use body::{DEFAULT_FOLDER_TEMPLATE, body, example_destination, source_summary};
pub(crate) use body::{DIALOG_SIZE, THUMB_SIZE_DEFAULT, note_dialog_bottom, thumb_size};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use scan::cards_seen;
pub use scan::{ScanTask, browse, open, poll_scan, running};
#[cfg(test)]
pub(crate) use scan::{stall_scan, stall_scan_of};
pub(crate) use tags::{tag_field_width, tag_help, tag_toggle, unknown_tags_warning};
pub use task::{ImportTask, start, start_paths, tick};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportDialog {
    /// The files / folders that were scanned (shown as the source).
    pub sources: Vec<String>,
    pub candidates: Vec<ImportCandidate>,
    pub checked: Vec<bool>,
    /// Copy into the library (else add in place).
    pub copy: bool,
    /// With `copy`: move instead — the originals are removed from the source once each copy is
    /// verified and catalogued (`library.import` mode `move`).
    pub move_files: bool,
    /// Existing album to add to.
    pub album: Option<u64>,
    /// …or a new album with this name.
    pub new_album: String,
    /// Preset id ("" = none).
    pub preset: String,
    /// Comma-separated keywords.
    pub keywords: String,
    /// Copy: destination folder ("" = the library's Originals/).
    pub destination: String,
    /// Copy: `date` (YYYY/YYYY-MM-DD), `month`, `flat` or `custom` ([`ImportDialog::folder_template`]).
    pub organize: String,
    /// Copy, `custom`: the folder template, e.g. `{date:%Y}/{date:%Y%m%d}`.
    pub folder_template: String,
    /// Copy: file-name template for the copies ("" = keep the names).
    pub rename: String,
    /// Metadata preset name ("" = none).
    pub metadata_preset: String,
    /// Copy: raws are copied as DNG.
    pub dng: bool,
    /// Per candidate: the library already has it in Recently Deleted ([`ImportDialog::set_trashed`]).
    pub trashed: Vec<bool>,
    /// What to do with those: `restore` (the default: the photo comes back with its edits), `fresh`
    /// (imported as a new photo, the old record deleted) or "" (leave them) — the `onDeleted` param.
    pub on_deleted: String,
    /// The candidate clicked last: where a Shift-click range starts ([`ImportDialog::click`]).
    #[serde(skip)]
    pub last_clicked: Option<usize>,
    /// The library the scan was made in ([`lightcraft_engine::Session::library_generation`]): its duplicate
    /// decisions and album ids belong to that catalog, so the review is not shown or confirmed in another.
    #[serde(skip)]
    pub library: Option<u64>,
}

/// A candidate's file type, as the review groups them: its format, or its extension when the
/// probe named none (`JPEG` and `JPG` are one type).
fn file_type(c: &ImportCandidate) -> String {
    let f = if c.format.trim().is_empty() {
        std::path::Path::new(&c.path).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default()
    } else {
        c.format.clone()
    };
    match f.trim().to_uppercase().as_str() {
        "JPEG" => "JPG".into(),
        "TIFF" => "TIF".into(),
        "" => "?".into(),
        other => other.to_string(),
    }
}

impl ImportDialog {
    pub fn new(candidates: Vec<ImportCandidate>) -> Self {
        let checked = candidates.iter().map(|c| c.duplicate.is_none() && c.error.is_none()).collect();
        ImportDialog { candidates, checked, on_deleted: "restore".into(), ..Default::default() }
    }

    /// Whether the library the scan was made in is still the open one (a review that names none counts).
    fn in_library(&self, app: &LightcraftApp) -> bool {
        self.library.is_none_or(|g| g == app.session.library_generation())
    }
    /// Mark the candidates whose photo is in Recently Deleted. They are checked when the choice
    /// for them (`on_deleted`) imports them — by default it restores them, so a photo deleted by
    /// mistake comes back by importing its file again (issue #706).
    pub fn set_trashed(&mut self, trashed: Vec<bool>) {
        self.trashed = trashed;
        for i in 0..self.candidates.len() {
            if !self.is_trashed(i) {
                continue;
            }
            let on = self.checked_by_default(i);
            if let Some(c) = self.checked.get_mut(i) {
                *c = on;
            }
        }
    }
    /// Candidate `i` can be checked: readable, and not a duplicate — except one by content (not of a
    /// file in Recently Deleted), which can be ticked by hand to import it anyway, and one in
    /// Recently Deleted (checked only by hand when "Leave them" is selected). A file already
    /// in the library from the same path never can.
    pub fn importable(&self, i: usize) -> bool {
        self.candidates.get(i).is_some_and(|c| {
            c.error.is_none()
                && match c.duplicate.as_deref() {
                    None => true,
                    Some(_) if self.is_trashed(i) => true,
                    Some(reason) => reason == "content",
                }
        })
    }
    /// Candidate `i` is checked by Check All and by file type: importable, and not a content
    /// duplicate (those are only ever ticked by hand).
    pub fn checked_by_default(&self, i: usize) -> bool {
        self.importable(i)
            && !(self.is_trashed(i) && self.on_deleted.is_empty())
            && self.candidates.get(i).is_some_and(|c| c.duplicate.is_none() || self.is_trashed(i))
    }
    /// The checked files that import although another photo has their content (`importAnyway`).
    pub fn import_anyway_paths(&self) -> Vec<String> {
        let anyway = |i: usize, c: &ImportCandidate| c.duplicate.as_deref() == Some("content") && !self.is_trashed(i);
        self.candidates
            .iter()
            .zip(&self.checked)
            .enumerate()
            .filter(|(i, (c, on))| **on && self.importable(*i) && anyway(*i, c))
            .map(|(_, (c, _))| c.path.clone())
            .collect()
    }
    /// The file types among the candidates (`ARW`, `JPG`…, upper case, sorted) and how many of each.
    pub fn file_types(&self) -> Vec<(String, usize)> {
        let mut types: std::collections::BTreeMap<String, usize> = Default::default();
        for c in &self.candidates {
            *types.entry(file_type(c)).or_default() += 1;
        }
        types.into_iter().collect()
    }
    /// Any file of type `ty` is checked.
    pub fn type_checked(&self, ty: &str) -> bool {
        self.candidates.iter().zip(&self.checked).any(|(c, on)| *on && file_type(c) == ty)
    }
    /// Check (the importable ones) or uncheck every file of type `ty`: the raws only, say, and not
    /// the JPEGs kept beside them (issue #344).
    pub fn check_type(&mut self, ty: &str, on: bool) {
        for i in 0..self.candidates.len() {
            let same = self.candidates.get(i).is_some_and(|c| file_type(c) == ty);
            let importable = self.checked_by_default(i);
            if same && let Some(c) = self.checked.get_mut(i) {
                *c = on && importable;
            }
        }
    }
    /// Candidate `i` is a file whose photo is in Recently Deleted.
    pub fn is_trashed(&self, i: usize) -> bool {
        self.trashed.get(i).copied().unwrap_or(false)
    }
    /// Choose what happens to the files in Recently Deleted (`""`, `restore` or `fresh`); they are
    /// checked once there is a choice and unchecked again without one.
    pub fn set_on_deleted(&mut self, choice: &str) {
        // switching between restore and fresh keeps the user's per-cell choices
        let changed = self.on_deleted.is_empty() != choice.is_empty();
        self.on_deleted = choice.to_string();
        for i in (0..self.candidates.len()).filter(|_| changed) {
            if self.is_trashed(i)
                && let Some(c) = self.checked.get_mut(i)
            {
                *c = !choice.is_empty();
            }
        }
    }
    /// A click on candidate `i` toggles its checkbox. With `shift`, every importable candidate
    /// from the last clicked one to `i` takes `i`'s new state, as in a file manager: click the
    /// first, Shift-click the last, and the whole range is checked (or unchecked).
    pub fn click(&mut self, i: usize, shift: bool) {
        if !self.importable(i) {
            return;
        }
        let Some(on) = self.checked.get(i).map(|c| !c) else { return };
        let from = if shift { self.last_clicked.filter(|a| *a < self.checked.len()).unwrap_or(i) } else { i };
        for k in from.min(i)..=from.max(i) {
            // (a range doesn't sweep in the content duplicates it passes over)
            if self.importable(k)
                && (k == i || !on || self.checked_by_default(k))
                && let Some(c) = self.checked.get_mut(k)
            {
                *c = on;
            }
        }
        self.last_clicked = Some(i);
    }
    /// How many of the checked files come back as the photo Recently Deleted holds: the import restores
    /// those without copying or moving their files (only "Import as new" imports them like any other file).
    pub fn restored_count(&self) -> usize {
        let restoring = self.on_deleted != "fresh";
        let checked = self.checked.iter().enumerate().filter(|(i, on)| **on && self.importable(*i));
        checked.filter(|(i, _)| restoring && self.is_trashed(*i)).count()
    }
    pub fn selected_paths(&self) -> Vec<String> {
        self.candidates
            .iter()
            .zip(&self.checked)
            .enumerate()
            .filter(|(i, (_, on))| **on && self.importable(*i))
            .map(|(_, (c, _))| c.path.clone())
            .collect()
    }
    /// The `organize` param of `library.import` (`None` = the default, by day); an unusable
    /// custom folder template is an error.
    pub fn organize_param(&self) -> Result<Option<String>, String> {
        match self.organize.as_str() {
            "" => Ok(None),
            "custom" => {
                let t = self.folder_template.trim();
                if let Some(e) = lightcraft_engine::rename::folder_template_error(t) {
                    return Err(e);
                }
                // a plain folder name ("Imports") is a one-level template too
                Ok(Some(if t.contains(['{', '/', '\\']) { t.to_string() } else { format!("{t}/") }))
            }
            o => Ok(Some(o.to_string())),
        }
    }
    fn keywords(&self) -> Vec<String> {
        self.keywords.split(',').map(str::trim).filter(|k| !k.is_empty()).map(str::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_height_follows_the_window() {
        // the grid takes what the options and buttons leave, in whole points
        assert_eq!(grid_height(700.0, 300.0, 142.0), 400.0);
        assert_eq!(grid_height(1000.5, 300.0, 142.0), 700.0);
        // never less than one row of thumbnails
        assert_eq!(grid_height(200.0, 300.0, 142.0), 142.0);
        // a bad measurement doesn't take the grid away or make it endless
        assert_eq!(grid_height(700.0, -50.0, 142.0), 700.0);
        assert_eq!(grid_height(700.0, f32::NAN, 142.0), 700.0);
        assert_eq!(grid_height(f32::INFINITY, 300.0, 142.0), DIALOG_SIZE[1] - DEFAULT_BELOW_GRID);
    }

    /// Six candidates, all checked; candidate 3 is a duplicate by content (unchecked until ticked by hand).
    fn dialog() -> ImportDialog {
        let mut candidates: Vec<ImportCandidate> = (0..6).map(|i| ImportCandidate { path: format!("img{i}.jpg"), ..Default::default() }).collect();
        candidates[3].duplicate = Some("content".into());
        ImportDialog::new(candidates)
    }

    /// Identical copies inside the source folder are not "already in the library" (a fresh library
    /// said "8 already in the library").
    #[test]
    fn copies_within_the_source_are_not_counted_as_in_the_library() {
        let mut d = dialog();
        assert_eq!(dup_counts(&d), (0, 1, 0), "same bytes as an earlier file, no library photo");
        d.candidates[4].duplicate = Some("content".into());
        d.candidates[4].existing = Some(7);
        d.candidates[5].duplicate = Some("path".into());
        d.candidates[5].existing = Some(8);
        assert_eq!(dup_counts(&d), (2, 1, 0));
    }

    /// A toggle per file type: the raws only, without the JPEGs beside them (issue #344).
    #[test]
    fn file_types_check_and_uncheck_together() {
        let mut candidates: Vec<ImportCandidate> =
            ["a.ARW", "a.JPG", "b.arw", "b.jpeg", "c.png"].iter().map(|p| ImportCandidate { path: p.to_string(), ..Default::default() }).collect();
        candidates[2].format = "ARW".into();
        candidates[3].format = "JPEG".into();
        candidates[2].duplicate = Some("content".into());
        let mut d = ImportDialog::new(candidates);
        assert_eq!(d.file_types(), [("ARW".to_string(), 2), ("JPG".to_string(), 2), ("PNG".to_string(), 1)]);
        assert!(d.type_checked("JPG"));
        d.check_type("JPG", false);
        assert_eq!(d.checked, [true, false, false, false, true]);
        assert!(!d.type_checked("JPG"));
        d.check_type("PNG", false);
        d.check_type("ARW", true);
        // the duplicate stays unchecked: it isn't importable
        assert_eq!(d.checked, [true, false, false, false, false]);
        d.check_type("JPG", true);
        assert_eq!(d.checked, [true, true, false, true, false]);
        assert_eq!(d.selected_paths(), ["a.ARW", "a.JPG", "b.jpeg"]);
        // an unknown type changes nothing
        d.check_type("CR3", false);
        assert_eq!(d.checked, [true, true, false, true, false]);
    }

    #[test]
    fn click_toggles_one_candidate() {
        let mut d = dialog();
        d.click(1, false);
        assert_eq!(d.checked, [true, false, true, false, true, true]);
        d.click(1, false);
        assert_eq!(d.checked, [true, true, true, false, true, true]);
        d.click(3, false);
        assert_eq!(d.checked, [true, true, true, true, true, true], "a duplicate by content can be ticked by hand");
        d.click(3, false);
        assert_eq!(d.checked, [true, true, true, false, true, true]);
    }

    /// Issue #1401: a duplicate by content is ticked by hand to import it anyway; one by path never is.
    #[test]
    fn a_content_duplicate_can_be_ticked_by_hand_but_a_path_duplicate_cannot() {
        let mut d = dialog();
        d.candidates[2].duplicate = Some("path".into());
        d.checked[2] = false;
        d.click(2, false);
        assert!(!d.checked[2] && !d.importable(2), "same path: not importable");
        assert!(d.importable(3) && !d.checked[3], "content duplicate: importable, unchecked by default");
        assert!(d.import_anyway_paths().is_empty());
        d.click(3, false);
        assert!(d.checked[3]);
        assert_eq!(d.import_anyway_paths(), ["img3.jpg"]);
        assert!(d.selected_paths().contains(&"img3.jpg".to_string()));
        // Check All and the file-type toggles leave it alone; only the hand ticks it
        d.checked[3] = false;
        for i in 0..d.candidates.len() {
            d.checked[i] = d.checked_by_default(i);
        }
        assert_eq!(d.checked, [true, true, false, false, true, true]);
        d.check_type("JPG", true);
        assert!(!d.checked[3]);
        // a click range passes over it
        d.checked.iter_mut().for_each(|c| *c = false);
        d.click(0, false);
        d.click(5, true);
        assert_eq!(d.checked, [true, true, false, false, true, true]);
        // one in Recently Deleted follows the choice for those files instead
        let mut d = dialog();
        d.set_trashed(vec![false, false, false, true, false, false]);
        assert!(d.checked[3] && d.import_anyway_paths().is_empty(), "restored, not imported anyway");
    }

    #[test]
    fn the_summary_counts_the_three_kinds_apart() {
        let mut d = dialog();
        d.candidates[1].duplicate = Some("path".into());
        d.candidates[4].duplicate = Some("content".into());
        d.candidates[4].earlier = Some("img0.jpg".into());
        let counts = lightcraft_engine::import::DuplicateCounts::of(&d.candidates);
        assert_eq!((counts.in_library, counts.copies, counts.repeated), (1, 1, 1));
    }
    #[test]
    fn shift_click_sets_the_range_to_the_clicked_state() {
        let mut d = dialog();
        d.click(1, false);
        d.click(4, true);
        assert_eq!(d.checked, [true, false, false, false, false, true], "1..=4 unchecked, the duplicate left alone");
        assert_eq!(d.selected_paths(), ["img0.jpg", "img5.jpg"]);
        // backwards from the new anchor (4) re-checks 2..=4
        d.click(2, true);
        assert_eq!(d.checked, [true, false, true, false, true, true]);
    }

    #[test]
    fn shift_click_without_an_earlier_click_toggles_one() {
        let mut d = dialog();
        d.click(2, true);
        assert_eq!(d.checked, [true, true, false, false, true, true]);
    }

    /// With "Leave them", a file in Recently Deleted stays out of Check All and the type toggles,
    /// but a click still checks it: it is imported (restored) on its own (issue #1285).
    #[test]
    fn leave_them_keeps_a_trashed_file_out_until_it_is_clicked() {
        let mut d = dialog();
        d.set_trashed(vec![false, false, false, true, false, false]);
        assert!(d.checked[3], "restored by default");
        d.set_on_deleted("");
        assert!(!d.checked[3]);
        d.check_type("JPG", true);
        assert!(!d.checked[3], "the type toggle leaves it out");
        d.click(3, false);
        assert!(d.checked[3], "a click checks it");
        assert!(d.selected_paths().contains(&"img3.jpg".to_string()));
        d.click(3, false);
        assert!(!d.checked[3]);
        // a path duplicate (not in Recently Deleted) still can't be checked
        d.set_trashed(vec![false; 6]);
        d.candidates[3].duplicate = Some("path".into());
        d.click(3, false);
        assert!(!d.checked[3]);
    }

    /// A badge too wide to fit beside the checkbox goes under it instead of covering it (issue #1285).
    #[test]
    fn a_badge_never_covers_the_checkbox() {
        let img = Rect::from_min_size(pos2(100.0, 50.0), vec2(116.0, 116.0));
        let cb = Rect::from_min_size(img.min + vec2(6.0, 6.0), vec2(16.0, 16.0));
        // "Will be restored" fits beside it, top right
        let short = badge_rect(img, cb, vec2(76.0, 12.0));
        assert_eq!(short.right(), img.right() - 4.0);
        assert_eq!(short.top(), img.top() + 6.0);
        // "In Recently Deleted" doesn't
        let long = badge_rect(img, cb, vec2(96.0, 12.0));
        assert!(!long.intersects(cb), "{long:?} covers {cb:?}");
        assert_eq!(long.right(), img.right() - 4.0);
        assert!(img.contains_rect(long));
    }

    #[test]
    fn shift_click_with_a_stale_anchor_toggles_one() {
        let mut d = dialog();
        d.last_clicked = Some(99);
        d.click(2, true);
        assert_eq!(d.checked, [true, true, false, false, true, true]);
    }
}
