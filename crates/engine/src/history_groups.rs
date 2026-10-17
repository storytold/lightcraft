//! Two-level Develop History: consecutive steps on one Develop section form a group, and an edit
//! made at a past step rewrites the history without losing the later groups.
//!
//! Steps keep storing full settings snapshots (so every catalog, old or new, loads unchanged);
//! a step's delta is the JSON difference from the step before it ([`crate::json_delta`]), so
//! groups and replay are derived on read. Flat history from before groups existed is grouped the
//! same way.

use std::sync::Arc;

use lcc_catalog::HistoryStep;
use lcc_develop::DevelopSettings;
use serde_json::Value;

/// Settings fields → the Develop section id they belong to. Fields not listed (the schema
/// `version`, the rendering `process`) aren't edits and belong to no section.
const SECTIONS: &[(&str, &str)] = &[
    ("profile", "basic"),
    ("treatment", "basic"),
    ("wb", "basic"),
    ("light", "basic"),
    ("hdr", "basic"),
    ("curve", "curve"),
    ("color", "color"),
    ("mixer", "color"),
    ("point_colors", "color"),
    ("bw_mix", "color"),
    ("bw", "color"),
    ("grading", "grading"),
    ("detail", "detail"),
    ("effects", "effects"),
    ("vignette", "effects"),
    ("grain", "effects"),
    ("optics", "lens"),
    ("geometry", "geometry"),
    ("crop", "crop"),
    ("orientation", "crop"),
    ("masks", "masking"),
    ("spots", "healing"),
    ("red_eye", "healing"),
    ("lens_blur", "lensBlur"),
    ("enhance", "enhance"),
    ("calibration", "calibration"),
    ("disabled_sections", "visibility"),
    ("plugin_filters", "pluginFilters"),
];

/// Step labels that apply several sections at once (presets, paste, sync, auto, restores…):
/// such a step is always a group of its own, whatever it happened to change.
const MULTI_LABELS: &[&str] = &[
    "Preset: ",
    "Paste",
    "Sync",
    "Same as Previous",
    "Match Total",
    "Auto",
    "Restore ",
    "Snapshot",
    "Swap Before",
    "Copy Before",
    "Quick Develop",
    "Cleared History",
    "Import",
    "Virtual Copy",
    "Lightroom",
    "History:",
];

/// One History group: steps `start..=end`, all on `section` (`None` = a multi-section step such as
/// a preset, which can't be edited inside but still replays).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Group {
    pub section: Option<String>,
    pub start: usize,
    pub end: usize,
}

/// The change from `old` to `new` as a partial settings object (`None` = no change).
pub fn delta(old: &DevelopSettings, new: &DevelopSettings) -> Option<Value> {
    crate::json_delta(&old.to_json(), &new.to_json())
}

/// The section a change with `label` from `old` to `new` belongs to: `None` when it spans several
/// sections, is a multi-section kind of step, or has no predecessor to compare with.
pub fn section_of(label: &str, old: Option<&DevelopSettings>, new: &DevelopSettings) -> Option<String> {
    let label_multi = MULTI_LABELS.iter().any(|m| label.starts_with(m)) || label == "Reset";
    if label_multi {
        return None;
    }
    // the first step has nothing to compare with: its label tells
    let Some(old) = old else { return control_section(label) };
    let d = delta(old, new)?;
    one_section(d.as_object()?.keys().map(String::as_str)).or_else(|| control_section(label))
}

/// The one section all of `fields` belong to (`None` if they span several, or none).
fn one_section<'a>(fields: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut found: Option<&str> = None;
    for k in fields {
        let Some((_, sec)) = SECTIONS.iter().find(|(f, _)| *f == k) else { continue };
        match found {
            Some(f) if f != *sec => return None,
            _ => found = Some(sec),
        }
    }
    found.map(str::to_string)
}

/// The section of the slider(s) a step label names (a slider edit is labelled with its control):
/// a step whose snapshot also carries changes made outside History still groups by its control.
fn control_section(label: &str) -> Option<String> {
    let ids = lcc_develop::controls::CONTROLS.iter().filter(|c| c.label == label).map(|c| c.id.split('.').next().unwrap_or_default());
    one_section(ids)
}

/// The section of each step of `history`.
pub fn step_sections(history: &[HistoryStep]) -> Vec<Option<String>> {
    let prev = std::iter::once(None).chain(history.iter().map(Some));
    history.iter().zip(prev).map(|(h, p)| section_of(&h.label, p.map(|p| &*p.settings), &h.settings)).collect()
}

/// Group `history`: runs of consecutive steps on the same section; a multi-section step is its own group.
pub fn groups(history: &[HistoryStep]) -> Vec<Group> {
    let mut out: Vec<Group> = Vec::new();
    for (i, sec) in step_sections(history).into_iter().enumerate() {
        match out.last_mut() {
            Some(g) if sec.is_some() && g.section == sec => g.end = i,
            _ => out.push(Group { section: sec, start: i, end: i }),
        }
    }
    out
}

/// The result of an edit made at a past step.
pub struct Rewrite {
    pub history: Vec<HistoryStep>,
    /// Index of the new step (where the photo's settings now are).
    pub at: usize,
}

/// An edit to `new` labelled `label`, made while the photo sits at step `cursor` of `history`.
///
/// When the edit is on the section of the (editable) group holding `cursor`, it replaces the
/// steps after `cursor` inside that group; otherwise it starts a new group right after `cursor`.
/// Every later group is kept and replayed on top: each of its steps' deltas is re-applied in
/// order, so a later group that set the same control still wins.
/// `None` when `cursor` is not a step of `history`.
pub fn rewrite(history: &[HistoryStep], cursor: usize, new: DevelopSettings, label: &str) -> Option<Rewrite> {
    let base = &history.get(cursor)?.settings;
    let sec = section_of(label, Some(base), &new);
    let group = groups(history).into_iter().find(|g| (g.start..=g.end).contains(&cursor));
    // steps after the cursor that the edit replaces: the rest of its own group, when it edits that group's section
    let resume = match group {
        Some(g) if sec.is_some() && g.section == sec => g.end + 1,
        _ => cursor + 1,
    };
    let mut out: Vec<HistoryStep> = history.get(..=cursor)?.to_vec();
    let at = out.len();
    out.push(HistoryStep { label: label.into(), settings: Arc::new(new) });
    for pair in history.get(resume.saturating_sub(1)..).unwrap_or_default().windows(2) {
        let [before, step] = pair else { continue };
        let prev = out.last().map(|s| s.settings.clone()).unwrap_or_default();
        let settings = match delta(&before.settings, &step.settings) {
            Some(d) => Arc::new(lcc_develop::apply_partial(&prev, &d, 1.0)),
            None => prev,
        };
        out.push(HistoryStep { label: step.label.clone(), settings });
    }
    Some(Rewrite { history: out, at })
}
