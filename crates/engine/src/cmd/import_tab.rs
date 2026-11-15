//! The Import tab's commands (OneSlips, LR-IMP-WINDOW): what an import of the browsed folder would
//! take (its picks, the selection or all, counted), import presets (named sets of import
//! settings, kept with the library) and finishing an import unattended (`library.finishImport`).
//!
//! Browsed (Local) photos are only looked at until they are imported: nothing here writes to,
//! moves or deletes their files. `import.run` hands them to `library.import`, which promotes their
//! records (or, copied or moved, lets each record follow its file) with their culling.
//!
//! Ported by hand from OneSlips 12d31e4fc, 5c4f63bae, 8259b2e6a.

use lcc_catalog::{Flag, PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{CommandSpec, always, bad, cmd, str_param};
use crate::{LibrarySource, Result, Session};

/// A named set of import settings: the `library.import` parameters it sets (mode, destination,
/// organize, rename, presets, keywords, metadataPreset…) plus `previews`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImportPreset {
    pub name: String,
    pub settings: Value,
}

/// Which browsed photos an import takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Take {
    Picks,
    Selected,
    All,
}

impl Take {
    pub fn parse(s: &str) -> Option<Take> {
        match s {
            "picks" => Some(Take::Picks),
            "selected" | "selection" => Some(Take::Selected),
            "all" => Some(Take::All),
            _ => None,
        }
    }
}

/// The browsed photos not in the library yet, among those the folder view shows (in its order).
fn browsed_in_view(s: &mut Session) -> Vec<PhotoId> {
    if s.source != LibrarySource::Folder {
        return Vec::new();
    }
    let ids = s.visible_cloned();
    ids.into_iter().filter(|id| s.catalog.photo(*id).is_some_and(|p| p.local && !p.deleted)).collect()
}

impl Session {
    /// The browsed photos an import would take: (picks, selected, all).
    pub fn import_targets(&mut self) -> (Vec<PhotoId>, Vec<PhotoId>, Vec<PhotoId>) {
        let all = browsed_in_view(self);
        let picks = all.iter().copied().filter(|id| self.catalog.photo(*id).is_some_and(|p| p.flag == Flag::Pick)).collect();
        let selected = all.iter().copied().filter(|id| self.selection.ids.contains(id)).collect();
        (picks, selected, all)
    }

    /// The files of `take` ([`Session::import_targets`]).
    pub fn import_paths(&mut self, take: Take) -> Vec<String> {
        let (picks, selected, all) = self.import_targets();
        let ids = match take {
            Take::Picks => picks,
            Take::Selected => selected,
            Take::All => all,
        };
        ids.iter()
            .filter_map(|id| match &self.catalog.photo(*id)?.source {
                Source::File { path } => Some(path.clone()),
                Source::Demo { .. } => None,
            })
            .collect()
    }

    /// Get just-imported photos ready unattended (FINISH-PER-PHOTO, PERF-PREVIEW-ONCE), from the
    /// photo on screen outward: the AI masks their settings bring (an import preset's People,
    /// Object or Describe masks) queued on the AI worker, their Smooth Skin's analysis, then their
    /// previews, built once each. Each stage runs in the background with its own activity row
    /// (Computing AI masks, Finding skin, the previews). `previews` is `none`, `standard` or `full`.
    pub fn finish_import(&mut self, ids: &[u64], previews: &str) -> Result<Value> {
        const C: &str = "library.finishImport";
        let size = match previews {
            "none" => None,
            "standard" => Some("standard"),
            "full" | "1:1" => Some("full"),
            other => return Err(bad(C, format!("unknown previews `{other}` (none|standard|full)"))),
        };
        let ordered = self.screen_outward_order(ids);
        if ordered.is_empty() {
            return Ok(json!({"running": false, "total": 0}));
        }
        let (masks, skin, notes) = self.finish_ai_stages(&ordered);
        let mut out = match size {
            Some(size) => self.execute("library.buildPreviews", &json!({"size": size, "ids": ordered}))?,
            None => json!({"running": false, "total": 0}),
        };
        // kept in the library until done, so a restart takes it up again ([`crate::finishing`]).
        // A build asked for while another runs waits for its turn; the one tracked here is the
        // finish's own, not the running one it queued behind.
        if size.is_some() || masks > 0 || skin > 0 {
            let build = size.filter(|_| out.get("queued") != Some(&serde_json::json!(true))).and(self.preview_build.clone());
            self.finishing_started(crate::finishing::FinishPlan { ids: ordered.clone(), previews: previews.to_string() }, build);
        }
        out["aiMasks"] = json!(masks);
        out["skin"] = json!(skin);
        if !notes.is_empty() {
            out["notes"] = json!(notes);
        }
        Ok(out)
    }

    /// The AI stages of [`Session::finish_import`] for `ordered`: → (AI mask selections queued,
    /// photos whose Smooth Skin is being analysed, one note per distinct reason a stage could not
    /// run, such as a model that is not installed). Nothing here fails the finish.
    fn finish_ai_stages(&mut self, ordered: &[u64]) -> (usize, usize, Vec<String>) {
        let (mut masks, mut skin, mut notes) = (0usize, 0usize, Vec::<String>::new());
        let mut note = |e: String| {
            if !notes.contains(&e) && notes.len() < 8 {
                notes.push(e);
            }
        };
        if !crate::segment::Segmenter::AVAILABLE {
            return (0, 0, notes);
        }
        // (work the user didn't ask for right now: the user's own AI masks go first, `Segmenter::low_priority`)
        let was_low = std::mem::replace(&mut self.segmenter.low_priority, true);
        for id in ordered.iter().copied().map(PhotoId) {
            // (in the app the AI worker computes them; headless, the next edit or Update AI Mask does)
            if self.segmenter.background {
                match self.segment_missing_later(id) {
                    Ok(n) => masks += n,
                    Err(e) => note(e),
                }
            }
            match self.skin_ensure(id, false) {
                Ok(true) => skin += 1,
                Ok(false) => {}
                Err(e) => note(e.to_string()),
            }
        }
        self.segmenter.low_priority = was_low;
        (masks, skin, notes)
    }

    /// `ids` (those still in the catalog, once each) ordered from the photo on screen outward: the
    /// one shown first, then its neighbours in the view on both sides, then the rest in order.
    pub(crate) fn screen_outward_order(&mut self, ids: &[u64]) -> Vec<u64> {
        let mut seen = std::collections::HashSet::new();
        let ids: Vec<u64> = ids.iter().copied().filter(|i| self.catalog.photo(PhotoId(*i)).is_some() && seen.insert(*i)).collect();
        let Some(active) = self.active() else { return ids };
        let view = self.visible_cloned();
        let Some(at) = view.iter().position(|id| *id == active) else { return ids };
        let place = |i: u64| view.iter().position(|id| id.0 == i).map(|p| p.abs_diff(at));
        let mut keyed: Vec<(usize, usize, u64)> = ids.iter().enumerate().map(|(n, i)| (place(*i).unwrap_or(usize::MAX), n, *i)).collect();
        keyed.sort();
        keyed.into_iter().map(|(_, _, i)| i).collect()
    }
}

fn take_param(p: &Value, c: &str) -> Result<Take> {
    let w = str_param(p, "what").unwrap_or("all");
    Take::parse(w).ok_or_else(|| bad(c, format!("unknown what `{w}` (picks|selected|all)")))
}

fn targets(s: &mut Session, p: &Value) -> Result<Value> {
    let (picks, selected, all) = s.import_targets();
    let mut out = json!({"picks": picks.len(), "selected": selected.len(), "all": all.len()});
    if p.get("what").is_some() {
        out["paths"] = json!(s.import_paths(take_param(p, "import.targets")?));
    }
    Ok(out)
}

/// `library.import` for the browsed folder: the photos `what` names, with the given settings.
fn import_browsed(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "import.run";
    let take = take_param(p, C)?;
    if s.source != LibrarySource::Folder {
        return Err(bad(C, "browse a folder first (library.browse)"));
    }
    let paths = s.import_paths(take);
    if paths.is_empty() {
        return Err(bad(
            C,
            match take {
                Take::Picks => "no picks to import: flag photos as picks (P) first",
                Take::Selected => "no browsed photos selected",
                Take::All => "nothing to import: every photo here is in the catalog already",
            },
        ));
    }
    let previews = str_param(p, "previews").unwrap_or("none").to_string();
    let mut params = p.clone();
    if let Some(o) = params.as_object_mut() {
        o.remove("what");
        o.remove("previews");
        // (browsing stays browsing: an import from the Import tab is for real)
        o.remove("local");
    }
    params["paths"] = json!(paths);
    let mut r = s.execute("library.import", &params)?;
    let ids: Vec<u64> = ["imported", "restored"].iter().flat_map(|k| r[*k].as_array().into_iter().flatten().filter_map(Value::as_u64)).collect();
    r["finishing"] = s.finish_import(&ids, &previews)?;
    Ok(r)
}

fn save_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "import.savePreset";
    let name = str_param(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad(C, "give the preset a name"))?.to_string();
    let settings = p.get("settings").cloned().filter(Value::is_object).ok_or_else(|| bad(C, "settings must be an object"))?;
    match s.import_presets.iter_mut().find(|x| x.name.eq_ignore_ascii_case(&name)) {
        Some(x) => x.settings = settings,
        None => s.import_presets.push(ImportPreset { name: name.clone(), settings }),
    }
    s.save_prefs()?;
    Ok(json!({"name": name, "presets": s.import_presets.len()}))
}

fn delete_preset(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "import.deletePreset";
    let name = str_param(p, "name").ok_or_else(|| bad(C, "missing name"))?;
    let n = s.import_presets.len();
    s.import_presets.retain(|x| !x.name.eq_ignore_ascii_case(name));
    if s.import_presets.len() == n {
        return Err(bad(C, format!("no import preset `{name}`")));
    }
    s.save_prefs()?;
    Ok(json!({"deleted": name}))
}

fn finish(s: &mut Session, p: &Value) -> Result<Value> {
    let previews = str_param(p, "previews").unwrap_or("standard").to_string();
    let ids: Vec<u64> = if p.get("ids").is_some() || p.get("id").is_some() {
        s.targets(p)?.into_iter().map(|i| i.0).collect()
    } else {
        s.selection.ids.iter().map(|i| i.0).collect()
    };
    s.finish_import(&ids, &previews)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            query "import.targets",
            "Import Targets",
            [],
            None,
            "{what?: picks|selected|all}: the browsed (not yet imported) photos of the folder view an import would take, counted: {picks, selected, all, paths? (of `what`)}",
            always,
            targets
        ),
        cmd!(
            "import.run",
            "Import Browsed Photos",
            [],
            None,
            "{what?: picks|selected|all (default all), previews?: none|standard|full, …library.import settings (mode, destination, organize, rename, preset, presets, keywords, metadataPreset, album…)}: import the browsed folder's picks, selection or all its photos not in the library yet (their culling and edits go along; copied or moved, each record follows its file), then build their previews in the background (library.finishImport) → the library.import report + {finishing}",
            always,
            import_browsed
        ),
        cmd!(query "import.presets", "Import Presets", [], None, "{} → [{name, settings}]", always, |s, _| Ok(json!(s.import_presets))),
        cmd!(
            "import.savePreset",
            "Save Import Preset",
            [],
            None,
            "{name, settings: {mode, destination, organize, rename, presets, keywords, metadataPreset, previews…}}: keep the Import tab's settings under a name (replaces one of that name; saved with the library)",
            always,
            save_preset
        ),
        cmd!("import.deletePreset", "Delete Import Preset", [], None, "{name}", always, delete_preset),
        cmd!(
            "library.finishImport",
            "Finish Import",
            [],
            None,
            "{ids? (default: the selection), previews?: none|standard (default)|full}: get photos ready unattended, from the photo on screen outward, each stage in the background with its own activity row: the AI masks their settings bring (on the AI worker), their Smooth Skin analysis, then their previews built once each (library.buildPreviews) → {total, done, failed, running, aiMasks, skin, notes?}",
            always,
            finish
        ),
    ]
}
