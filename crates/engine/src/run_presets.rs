//! Process recipes (Capture One's term): export the same photos with several export presets in one
//! go (`export.runPresets {presetIds}`). Each photo is decoded once and that decode feeds every
//! preset's render of it ([`PreparedExport::share_decodes`]); the whole run is one row in the
//! activity stack, counting photos.
//!
//! Sources: own design after Capture One's process recipes (behaviour only).

use serde_json::{Value, json};

use crate::export::{CANCELLED, Destination, DestinationMode, ExportOptions, PreparedExport, prepare_batch, run_batch_cancellable};

/// The most presets one run takes.
pub const MAX_PRESETS: usize = 32;

/// One preset of a run: its name, options and folder.
struct Recipe {
    name: String,
    opts: ExportOptions,
    to: Destination,
}

/// One photo's exports, one per recipe (in recipe order; a recipe is absent where it failed to prepare).
struct Step {
    photo: lcc_catalog::PhotoId,
    label: String,
    items: Vec<(usize, PreparedExport)>,
}

/// A run set up from the session ([`prepare`]); [`Prepared::run`] does the work without it.
pub struct Prepared {
    recipes: Vec<Recipe>,
    steps: Vec<Step>,
    /// Photos that couldn't be prepared for a recipe: `{photo, preset, error}`.
    failed: Vec<Value>,
}

/// What a run did.
pub struct Report {
    /// Per recipe: `{preset, files: [per-photo results as app.export reports them]}`.
    pub presets: Vec<Value>,
    /// Every per-photo result, all recipes.
    pub files: Vec<Value>,
    pub photos: usize,
    /// Source decodes made (one per photo and source level, shared by the recipes).
    pub decodes: usize,
    pub cancelled: bool,
}

impl Report {
    pub fn json(&self) -> Value {
        let written = self.files.iter().filter(|f| f.get("path").is_some() && f.get("error").is_none()).count();
        json!({"presets": self.presets, "photos": self.photos, "decodes": self.decodes, "written": written, "cancelled": self.cancelled})
    }
}

/// The preset names of `p.presetIds`: a non-empty list of distinct (ignoring case) names of known presets.
fn preset_names(s: &crate::Session, p: &Value) -> Result<Vec<String>, String> {
    let list = p.get("presetIds").and_then(Value::as_array).ok_or("missing `presetIds`: a list of export preset names (see export.presets)")?;
    if list.is_empty() {
        return Err("`presetIds` is empty: name at least one export preset".into());
    }
    if list.len() > MAX_PRESETS {
        return Err(format!("`presetIds` names {} presets; at most {MAX_PRESETS}", list.len()));
    }
    let known = s.all_export_presets();
    let mut out: Vec<String> = Vec::new();
    for v in list {
        let name = v.as_str().map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| format!("`presetIds` holds {v}: each must be a preset name"))?;
        let (preset, _) = known
            .iter()
            .find(|(x, _)| x.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("unknown export preset `{name}` (see export.presets)"))?;
        if out.iter().any(|n| n.eq_ignore_ascii_case(&preset.name)) {
            return Err(format!("`presetIds` names `{name}` twice"));
        }
        out.push(preset.name.clone());
    }
    Ok(out)
}

/// The photos of `p.ids`, else the selection (else the active photo).
fn photo_ids(s: &crate::Session, p: &Value) -> Result<Vec<lcc_catalog::PhotoId>, String> {
    let ids: Vec<lcc_catalog::PhotoId> = match p.get("ids") {
        None | Some(Value::Null) => s.selected(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| v.as_u64().map(lcc_catalog::PhotoId).ok_or_else(|| format!("`ids` must be photo ids, got {v}")))
            .collect::<Result<_, _>>()?,
        Some(v) => return Err(format!("`ids` must be a list of photo ids, got {v}")),
    };
    if ids.is_empty() {
        return Err("no photos to export: select some or pass `ids`".into());
    }
    if let Some(id) = ids.iter().find(|id| s.catalog.photo(**id).is_none()) {
        return Err(format!("no photo with id {}", id.0));
    }
    let mut seen = std::collections::HashSet::new();
    Ok(ids.into_iter().filter(|id| seen.insert(*id)).collect())
}

/// Set up `export.runPresets {presetIds, ids?, dir?}`: every argument is checked and every recipe's
/// folder known before anything is decoded or written.
pub fn prepare(s: &mut crate::Session, p: &Value) -> Result<Prepared, String> {
    let names = preset_names(s, p)?;
    let ids = photo_ids(s, p)?;
    let dir = match p.get("dir") {
        None | Some(Value::Null) => None,
        Some(Value::String(d)) if !d.trim().is_empty() => Some(d.clone()),
        Some(v) => return Err(format!("`dir` must be a folder path, got {v}")),
    };
    let mut recipes = Vec::with_capacity(names.len());
    for name in &names {
        let mut call = json!({"preset": name});
        if let Some(d) = &dir {
            call["dir"] = json!(d);
        }
        let params = s.export_params(&call)?;
        let opts = ExportOptions::from_params(&params).map_err(|e| format!("preset `{name}`: {e}"))?;
        if opts.add_to_photos {
            return Err(format!("preset `{name}` adds to Apple Photos, which a run of several presets doesn't do; export with it on its own"));
        }
        let folder = params.get("dir").and_then(Value::as_str).unwrap_or("").to_string();
        if folder.trim().is_empty() && opts.destination_mode != DestinationMode::SameAsOriginal {
            return Err(format!("preset `{name}` names no folder: pass `dir`"));
        }
        recipes.push(Recipe { name: name.clone(), opts, to: Destination { dir: folder, exact: None } });
    }
    let mut steps: Vec<Step> = ids
        .iter()
        .map(|id| Step { photo: *id, label: s.catalog.photo(*id).map(|ph| ph.file_name.clone()).unwrap_or_default(), items: Vec::new() })
        .collect();
    let mut failed = Vec::new();
    for (ri, r) in recipes.iter().enumerate() {
        // per photo, so `{seq}` numbers as an export with this preset alone would and one photo's
        // problem (a demo photo exported as its original) doesn't stop the others
        match prepare_batch(s, &ids, &r.opts) {
            Ok(items) => {
                for (step, item) in steps.iter_mut().zip(items) {
                    step.items.push((ri, item));
                }
            }
            Err(_) => {
                for (seq, step) in steps.iter_mut().enumerate() {
                    match crate::export::prepare_export(s, step.photo, &r.opts, seq + 1) {
                        Ok(item) => step.items.push((ri, item)),
                        Err(e) => failed.push(json!({"photo": step.photo.0, "preset": r.name, "error": e})),
                    }
                }
            }
        }
    }
    Ok(Prepared { recipes, steps, failed })
}

impl Prepared {
    /// Photos in the run.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Export photo by photo, each decoded once for all recipes; `progress(done, file)` before each
    /// photo. A set `cancel` stops it between steps; the files written so far stay.
    pub fn run(self, cancel: &std::sync::atomic::AtomicBool, progress: &mut dyn FnMut(usize, &str)) -> Report {
        use std::sync::atomic::Ordering;
        let Prepared { recipes, steps, failed } = self;
        let mut per: Vec<Vec<Value>> = vec![Vec::new(); recipes.len()];
        for f in failed {
            if let Some(ri) = recipes.iter().position(|r| f.get("preset").and_then(Value::as_str) == Some(r.name.as_str())) {
                per[ri].push(f);
            }
        }
        let stopped = || cancel.load(Ordering::Relaxed);
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (mut decodes, photos) = (0, steps.len());
        let mut cancelled = false;
        for (done, step) in steps.into_iter().enumerate() {
            progress(done, &step.label);
            if stopped() {
                cancelled = true;
                break;
            }
            let (which, mut items): (Vec<usize>, Vec<PreparedExport>) = step.items.into_iter().unzip();
            match PreparedExport::share_decodes(&mut items, &stopped) {
                Ok(n) => decodes += n,
                Err(_) => {
                    cancelled = true;
                    break;
                }
            }
            for (ri, item) in which.into_iter().zip(items) {
                if stopped() {
                    cancelled = true;
                    break;
                }
                let (Some(r), Some(out)) = (recipes.get(ri), per.get_mut(ri)) else { continue };
                flag.store(false, Ordering::Relaxed);
                let res = run_batch_cancellable(
                    vec![item],
                    &r.opts,
                    &r.to,
                    &mut crate::export::write_file,
                    &|path| std::path::Path::new(path).exists(),
                    false,
                    &mut |_, _| !stopped(),
                    Some(flag.clone()),
                );
                match res {
                    Ok(files) => out.extend(files),
                    Err(e) if e == CANCELLED => cancelled = true,
                    Err(e) => out.push(json!({"photo": step.photo.0, "file": step.label, "error": e})),
                }
            }
            if cancelled {
                break;
            }
        }
        progress(photos, "");
        let files: Vec<Value> = per.iter().flatten().cloned().collect();
        let presets = recipes.iter().zip(per).map(|(r, f)| json!({"preset": r.name, "files": f})).collect();
        Report { presets, files, photos, decodes, cancelled }
    }
}

/// `export.runPresets`: prepare, run with one activity row (✕ stops it), mark the photos exported.
pub fn run_command(s: &mut crate::Session, p: &Value) -> Result<Value, String> {
    let prepared = prepare(s, p)?;
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let row = s.activity.start_batch("export", "Exporting", crate::activity::Cancel::Flag(cancel.clone()));
    let total = prepared.len() as u64;
    row.progress(0, total);
    let report = prepared.run(&cancel, &mut |done, name| {
        if done > 0 {
            row.item_done();
        }
        row.detail(name);
    });
    drop(row);
    crate::export::mark_exported(s, &report.files);
    Ok(report.json())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn session_with_presets(dir: &std::path::Path) -> Session {
        let mut s = Session::with_demo();
        for (name, edge) in [("Small", 64), ("Smaller", 48)] {
            let d = dir.join(name);
            s.execute("export.savePreset", &json!({"name": name, "params": {"format": "jpeg", "longEdge": edge, "dir": d.to_string_lossy()}}))
                .unwrap();
        }
        s
    }

    #[test]
    fn hostile_arguments_are_refused_before_anything_is_written() {
        let dir = lcc_testkit::TempDir::new("lcc-run-presets-bad");
        let mut s = session_with_presets(&dir);
        let id = s.visible_cloned()[0].0;
        for p in [
            json!({}),
            json!({"presetIds": []}),
            json!({"presetIds": "Small"}),
            json!({"presetIds": [1, 2]}),
            json!({"presetIds": [""]}),
            json!({"presetIds": ["No Such Preset"], "ids": [id]}),
            json!({"presetIds": ["Small", "small"], "ids": [id]}),
            json!({"presetIds": vec!["Small"; MAX_PRESETS + 1], "ids": [id]}),
            json!({"presetIds": ["Small"], "ids": [u64::MAX]}),
            json!({"presetIds": ["Small"], "ids": [-1]}),
            json!({"presetIds": ["Small"], "ids": "x"}),
            json!({"presetIds": ["Small"], "ids": []}),
            json!({"presetIds": ["Small"], "ids": [id], "dir": 5}),
        ] {
            assert!(s.execute("export.runPresets", &p).is_err(), "{p}");
        }
        assert!(!dir.join("Small").exists() && !dir.join("Smaller").exists());
        // a preset naming no folder needs `dir`
        s.execute("export.savePreset", &json!({"name": "Nowhere", "params": {"format": "jpeg", "longEdge": 32}})).unwrap();
        let err = s.execute("export.runPresets", &json!({"presetIds": ["Nowhere"], "ids": [id]})).unwrap_err().to_string();
        assert!(err.contains("dir"), "{err}");
    }

    #[test]
    fn several_presets_share_one_decode_per_photo_and_one_activity_row() {
        let dir = lcc_testkit::TempDir::new("lcc-run-presets");
        let mut s = session_with_presets(&dir);
        let ids: Vec<u64> = s.visible_cloned().iter().take(2).map(|p| p.0).collect();
        let r = s.execute("export.runPresets", &json!({"presetIds": ["Small", "Smaller"], "ids": ids})).unwrap();
        assert_eq!(r["photos"], 2);
        assert_eq!(r["written"], 4, "{r}");
        // one decode per photo however many presets read it
        assert_eq!(r["decodes"], 2, "{r}");
        for (name, edge) in [("Small", 64), ("Smaller", 48)] {
            let files: Vec<_> = std::fs::read_dir(dir.join(name)).unwrap().collect();
            assert_eq!(files.len(), 2, "{name}");
            let f = r["presets"].as_array().unwrap().iter().find(|p| p["preset"] == name).unwrap();
            for file in f["files"].as_array().unwrap() {
                assert!(file["width"].as_u64().unwrap().max(file["height"].as_u64().unwrap()) <= edge);
            }
        }
        let rows = s.activity.finished();
        assert_eq!(rows.iter().filter(|b| b.label == "Exporting").count(), 1);
        assert!(s.activity.list().is_empty(), "the row ends with the run");
        // the photos are marked exported
        assert!(ids.iter().all(|id| s.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().exported.is_some()));
    }

    #[test]
    fn a_cancelled_run_stops_between_photos() {
        let dir = lcc_testkit::TempDir::new("lcc-run-presets-cancel");
        let mut s = session_with_presets(&dir);
        let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|p| p.0).collect();
        let prepared = prepare(&mut s, &json!({"presetIds": ["Small", "Smaller"], "ids": ids})).unwrap();
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let report = prepared.run(&cancel, &mut |done, _| {
            if done == 1 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        });
        assert!(report.cancelled);
        assert_eq!(report.json()["written"], 2);
    }
}
