//! The active photo's pixel-filter plug-in stack (`develop.pluginFilter.*`): add, remove, change,
//! reorder and list entries. Each edit is one History step; Auto Sync carries it to the selection
//! like any develop edit, and Copy / Paste / Sync Settings and presets carry the stack as the
//! "Plug-in Filters" group. Rendering the stack is the pipeline's business, not this module's.

use lcc_develop::{DevelopSettings, FilterStage, MAX_PLUGIN_FILTERS, PluginFilter};
use serde_json::{Map, Value, json};

use super::{CommandSpec, always, bad, cmd, has_active};
use crate::catalog::PhotoId;
use crate::{Result, Session};

/// Longest plug-in id, module hash, display name or parameter key accepted.
const MAX_TEXT: usize = 256;
/// Most parameters one entry holds.
const MAX_PARAMS: usize = 64;

fn text(p: &Value, key: &str, c: &str) -> Result<String> {
    let t = p.get(key).and_then(Value::as_str).ok_or_else(|| bad(c, format!("missing string `{key}`")))?;
    if t.is_empty() || t.len() > MAX_TEXT {
        return Err(bad(c, format!("`{key}` must be 1..{MAX_TEXT} bytes")));
    }
    Ok(t.to_string())
}

fn uid(p: &Value, c: &str) -> Result<u32> {
    let v = p.get("uid").and_then(Value::as_u64).ok_or_else(|| bad(c, "missing integer `uid`"))?;
    u32::try_from(v).map_err(|_| bad(c, "no such plug-in filter"))
}

fn stage(p: &Value, c: &str) -> Result<Option<FilterStage>> {
    match p.get("stage") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|_| bad(c, "`stage` must be \"source\" or \"display\"")),
    }
}

/// `params` as a checked object (numbers finite, values scalar), or `None` when absent.
fn params(p: &Value, c: &str) -> Result<Option<Map<String, Value>>> {
    let m = match p.get("params") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Object(m)) => m,
        Some(_) => return Err(bad(c, "`params` must be an object")),
    };
    if m.len() > MAX_PARAMS {
        return Err(bad(c, format!("at most {MAX_PARAMS} parameters")));
    }
    for (k, v) in m {
        if k.is_empty() || k.len() > MAX_TEXT {
            return Err(bad(c, "parameter names must be 1..256 bytes"));
        }
        let ok = match v {
            Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
            Value::Bool(_) => true,
            Value::String(s) => s.len() <= MAX_TEXT,
            _ => false,
        };
        if !ok {
            return Err(bad(c, format!("parameter `{k}` must be a number, boolean or short string")));
        }
    }
    Ok(Some(m.clone()))
}

/// A stack entry as a sidecar stores it (see `lcc_meta::plugin_filters_ns`).
pub(crate) fn to_xmp(f: &PluginFilter) -> lcc_meta::XmpPluginFilter {
    lcc_meta::XmpPluginFilter {
        plugin: f.plugin.clone(),
        module_hash: f.module_hash.clone(),
        name: f.name.clone(),
        enabled: if f.enabled { "True" } else { "False" }.into(),
        stage: match f.stage {
            FilterStage::Source => "source",
            FilterStage::Display => "display",
        }
        .into(),
        params: serde_json::to_string(&f.params).unwrap_or_default(),
        analysis: f.analysis.as_ref().and_then(|a| serde_json::to_string(a).ok()).unwrap_or_default(),
    }
}

/// A sidecar's stack entry checked like `develop.pluginFilter.add` checks its arguments, or `None` when any part
/// is malformed (the entry is skipped). `uid` is the caller's: sidecars don't store uids.
pub(crate) fn from_xmp(e: &lcc_meta::XmpPluginFilter, uid: u32) -> Option<PluginFilter> {
    let c = "sidecar";
    let params_v: Value = if e.params.is_empty() { Value::Null } else { serde_json::from_str(&e.params).ok()? };
    let p = json!({ "plugin": e.plugin, "module_hash": e.module_hash, "name": e.name, "params": params_v, "stage": (!e.stage.is_empty()).then_some(&e.stage) });
    let enabled = match e.enabled.as_str() {
        "" | "True" | "true" => true,
        "False" | "false" => false,
        _ => return None,
    };
    let analysis = if e.analysis.is_empty() {
        None
    } else {
        if e.analysis.len() > MAX_ANALYSIS {
            return None;
        }
        Some(serde_json::from_str::<Value>(&e.analysis).ok()?)
    };
    Some(PluginFilter {
        uid,
        plugin: text(&p, "plugin", c).ok()?,
        module_hash: text(&p, "module_hash", c).ok()?,
        name: text(&p, "name", c).ok()?,
        params: params(&p, c).ok()?.unwrap_or_default(),
        enabled,
        stage: stage(&p, c).ok()?.unwrap_or_default(),
        analysis,
    })
}

/// A sidecar's whole stack: malformed entries skipped, at most [`MAX_PLUGIN_FILTERS`], uids 1, 2, …
pub(crate) fn stack_from_xmp(entries: &[lcc_meta::XmpPluginFilter]) -> Vec<PluginFilter> {
    let mut out: Vec<PluginFilter> = Vec::new();
    for e in entries {
        if out.len() >= MAX_PLUGIN_FILTERS {
            break;
        }
        let uid = u32::try_from(out.len()).unwrap_or(u32::MAX).saturating_add(1);
        if let Some(f) = from_xmp(e, uid) {
            out.push(f);
        }
    }
    out
}

fn active(s: &Session, c: &str) -> Result<(PhotoId, DevelopSettings)> {
    let id = s.active().ok_or_else(|| bad(c, "no active photo"))?;
    let d = s.develop_of(id).ok_or_else(|| bad(c, "no active photo"))?;
    Ok((id, (*d).clone()))
}

fn position(d: &DevelopSettings, uid: u32, c: &str) -> Result<usize> {
    d.plugin_filters.iter().position(|f| f.uid == uid).ok_or_else(|| bad(c, "no such plug-in filter"))
}

fn label(name: &str) -> String {
    format!("Plug-in: {name}")
}

fn add(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.add";
    let plugin = text(p, "plugin", c)?;
    let (mut params, mut stage_given) = (params(p, c)?.unwrap_or_default(), stage(p, c)?);
    // an installed plug-in: checked against its manifest, defaults filled in, its build recorded
    let (module_hash, name) = match crate::filter_plugins::registry().get(&plugin) {
        Some(f) => {
            // no stage given: the one the plug-in asks for
            stage_given = stage_given.or(Some(crate::filter_plugins::preferred_stage(&f)));
            params = f.manifest().resolve_params(&Value::Object(params)).map_err(|e| bad(c, e.to_string()))?;
            if let Some(h) = p.get("module_hash").and_then(Value::as_str)
                && h != f.module_hash()
            {
                return Err(bad(c, "`module_hash` is not the installed build of this plug-in"));
            }
            let name = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty() && n.len() <= MAX_TEXT).unwrap_or(&f.manifest().name);
            (f.module_hash().to_string(), name.to_string())
        }
        None => (text(p, "module_hash", c)?, text(p, "name", c)?),
    };
    let stage = stage_given.unwrap_or_default();
    let (id, mut d) = active(s, c)?;
    if d.plugin_filters.len() >= MAX_PLUGIN_FILTERS {
        return Err(bad(c, format!("at most {MAX_PLUGIN_FILTERS} plug-in filters")));
    }
    let uid = match d.plugin_filters.iter().map(|f| f.uid).max() {
        Some(m) => m.checked_add(1).ok_or_else(|| bad(c, "no free uid"))?,
        None => 1,
    };
    let l = label(&name);
    d.plugin_filters.push(PluginFilter { uid, plugin, module_hash, name, params, enabled: true, stage, analysis: None });
    s.set_develop(id, d, &l)?;
    Ok(json!({ "uid": uid }))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.remove";
    let u = uid(p, c)?;
    let (id, mut d) = active(s, c)?;
    let i = position(&d, u, c)?;
    let f = d.plugin_filters.remove(i);
    s.set_develop(id, d, &format!("Remove Plug-in: {}", f.name))?;
    Ok(Value::Null)
}

fn set(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.set";
    let u = uid(p, c)?;
    let patch = params(p, c)?;
    let st = stage(p, c)?;
    let enabled = match p.get("enabled") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_bool().ok_or_else(|| bad(c, "`enabled` must be a boolean"))?),
    };
    let (id, mut d) = active(s, c)?;
    let i = position(&d, u, c)?;
    let f = d.plugin_filters.get_mut(i).ok_or_else(|| bad(c, "no such plug-in filter"))?;
    let mut l = label(&f.name);
    if let Some(m) = patch {
        if let [(k, _)] = m.iter().collect::<Vec<_>>().as_slice() {
            l = format!("{l} › {k}");
        }
        f.params.extend(m);
        if f.params.len() > MAX_PARAMS {
            return Err(bad(c, format!("at most {MAX_PARAMS} parameters")));
        }
    }
    if let Some(e) = enabled {
        f.enabled = e;
    }
    if let Some(st) = st {
        f.stage = st;
    }
    s.set_develop(id, d, &l)?;
    Ok(Value::Null)
}

fn move_to(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.move";
    let u = uid(p, c)?;
    let to = p.get("index").and_then(Value::as_u64).ok_or_else(|| bad(c, "missing integer `index`"))?;
    let (id, mut d) = active(s, c)?;
    let i = position(&d, u, c)?;
    let to = usize::try_from(to).ok().filter(|t| *t < d.plugin_filters.len()).ok_or_else(|| bad(c, "`index` out of range"))?;
    let f = d.plugin_filters.remove(i);
    let l = format!("Move Plug-in: {}", f.name);
    d.plugin_filters.insert(to, f);
    s.set_develop(id, d, &l)?;
    Ok(Value::Null)
}

/// Largest analysis JSON accepted (serialized bytes).
const MAX_ANALYSIS: usize = 64 * 1024;

fn set_analysis(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.setAnalysis";
    let u = uid(p, c)?;
    let a = match p.get("analysis") {
        None => return Err(bad(c, "missing `analysis`")),
        Some(Value::Null) => None,
        Some(v) => {
            if serde_json::to_vec(v).map_or(true, |b| b.len() > MAX_ANALYSIS) {
                return Err(bad(c, format!("`analysis` must be at most {MAX_ANALYSIS} bytes of JSON")));
            }
            Some(v.clone())
        }
    };
    let (id, mut d) = active(s, c)?;
    let i = position(&d, u, c)?;
    let f = d.plugin_filters.get_mut(i).ok_or_else(|| bad(c, "no such plug-in filter"))?;
    f.analysis = a;
    let l = format!("{} › Measure", label(&f.name));
    s.set_develop(id, d, &l)?;
    Ok(Value::Null)
}

/// Long edge of the whole frame `pc_analyze` measures.
const ANALYZE_EDGE: usize = 1024;

/// What a measurement thread sends back: the analysis, or why there is none.
type Measured = std::result::Result<Value, String>;

/// One measurement running in the background.
pub(crate) struct Analysis {
    photo: PhotoId,
    uid: u32,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    rx: std::sync::mpsc::Receiver<Measured>,
    /// The activity row, kept while the job runs.
    _task: crate::activity::TaskGuard,
}

/// Measurements running in the background, and the latest failure of each entry.
#[derive(Default)]
pub(crate) struct Analyses {
    running: Vec<Analysis>,
    failed: Vec<(PhotoId, u32, String)>,
}

/// Measures `src` with `filter` for `entry` (its old analysis left out): the analysis (`Null` when the
/// plug-in has none to give).
fn measure(filter: &lcc_filters::Filter, entry: &PluginFilter, src: &lcc_raster::Rgb32f, cancel: &std::sync::atomic::AtomicBool) -> Measured {
    let (data, w, h) = lcc_pipeline::plugin_filters::analysis_input(src, ANALYZE_EDGE);
    let (w, h) = (u32::try_from(w).map_err(|_| "image too large")?, u32::try_from(h).map_err(|_| "image too large")?);
    let mut entry = entry.clone();
    entry.analysis = None;
    let params = lcc_pipeline::plugin_filters::filter_params(&entry, "linear");
    Ok(filter.analyze_rgb(&data, w, h, &params, cancel).map_err(|e| e.to_string())?.unwrap_or(Value::Null))
}

/// Stores `analysis` on entry `uid` of `photo` (one History step); false when the entry is gone.
fn store_analysis(s: &mut Session, photo: PhotoId, uid: u32, analysis: Value) -> Result<bool> {
    let Some(d) = s.develop_of(photo) else { return Ok(false) };
    let mut d = (*d).clone();
    let Some(f) = d.plugin_filters.iter_mut().find(|f| f.uid == uid) else { return Ok(false) };
    f.analysis = (!analysis.is_null()).then_some(analysis);
    let l = format!("{} › Measure", label(&f.name));
    s.set_develop(photo, d, &l)?;
    Ok(true)
}

fn analyze(s: &mut Session, p: &Value) -> Result<Value> {
    let c = "develop.pluginFilter.analyze";
    let u = uid(p, c)?;
    let (id, d) = active(s, c)?;
    let f = d.plugin_filters.get(position(&d, u, c)?).cloned().ok_or_else(|| bad(c, "no such plug-in filter"))?;
    let filter = crate::filter_plugins::registry().get(&f.plugin).ok_or_else(|| bad(c, format!("plug-in {:?} is not installed", f.plugin)))?;
    if !filter.has_analyze() {
        return Err(bad(c, format!("unsupported: plug-in {:?} has no whole-image measurement (pc_analyze)", f.plugin)));
    }
    if s.filter_analyses.running.iter().any(|a| a.photo == id && a.uid == u) {
        return Ok(json!({ "started": false, "running": true }));
    }
    s.filter_analyses.failed.retain(|(ph, x, _)| !(*ph == id && *x == u));
    let wait = p.get("wait").and_then(Value::as_bool).unwrap_or(false);
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let task = s.activity.start("pluginAnalyze", &format!("Measuring {}", f.name), crate::activity::Cancel::Flag(cancel.clone()));
    let photo = s.catalog.photo(id).cloned().ok_or_else(|| bad(c, "no active photo"))?;
    let source = s.media.source_ref(&photo, crate::media::SourceLevel::Preview);
    let job = {
        let (filter, cancel) = (filter.clone(), cancel.clone());
        move || -> Measured {
            let src = source.load_source()?;
            measure(&filter, &f, &src.image, &cancel)
        }
    };
    if wait {
        let analysis = job().map_err(|e| bad(c, e))?;
        drop(task);
        store_analysis(s, id, u, analysis.clone())?;
        return Ok(json!({ "analysis": analysis }));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new().name("plugin-analyze".into()).spawn(move || {
        let _ = tx.send(job());
    });
    if let Err(e) = spawned {
        return Err(bad(c, format!("can't start the measurement: {e}")));
    }
    s.filter_analyses.running.push(Analysis { photo: id, uid: u, cancel, rx, _task: task });
    Ok(json!({ "started": true }))
}

impl Session {
    /// Takes in finished plug-in filter measurements (each stored as one History step) and drops
    /// cancelled ones (nothing is stored). Cheap; the app calls it every frame. True when any landed.
    pub fn plugin_analyze_pump(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut landed = Vec::new();
        let jobs = std::mem::take(&mut self.filter_analyses.running);
        for a in jobs {
            if a.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                continue;
            }
            match a.rx.try_recv() {
                Err(TryRecvError::Empty) => self.filter_analyses.running.push(a),
                Err(TryRecvError::Disconnected) => self.filter_analyses.failed.push((a.photo, a.uid, "the measurement stopped".into())),
                Ok(Err(e)) => self.filter_analyses.failed.push((a.photo, a.uid, e)),
                Ok(Ok(v)) => landed.push((a.photo, a.uid, v)),
            }
        }
        self.filter_analyses.failed.truncate(64);
        let any = !landed.is_empty();
        for (photo, uid, v) in landed {
            if let Err(e) = store_analysis(self, photo, uid, v) {
                self.filter_analyses.failed.push((photo, uid, e.to_string()));
            }
        }
        any
    }

    /// Whether any measurement is running (the app keeps polling while one is).
    pub fn plugin_analyses_running(&self) -> bool {
        !self.filter_analyses.running.is_empty()
    }

    /// Whether entry `uid` of `photo` is being measured.
    pub fn plugin_analyzing(&self, photo: PhotoId, uid: u32) -> bool {
        self.filter_analyses.running.iter().any(|a| a.photo == photo && a.uid == uid)
    }

    /// Why the last measurement of entry `uid` of `photo` failed, if it did.
    pub fn plugin_analyze_error(&self, photo: PhotoId, uid: u32) -> Option<&str> {
        self.filter_analyses.failed.iter().rev().find(|(p, u, _)| *p == photo && *u == uid).map(|(_, _, e)| e.as_str())
    }
}

fn pump(s: &mut Session, _p: &Value) -> Result<Value> {
    s.plugin_analyze_pump();
    let running: Vec<Value> = s.filter_analyses.running.iter().map(|a| json!({"photo": a.photo.0, "uid": a.uid})).collect();
    let failed: Vec<Value> = s.filter_analyses.failed.iter().map(|(p, u, e)| json!({"photo": p.0, "uid": u, "error": e})).collect();
    Ok(json!({ "running": running, "failed": failed }))
}

fn list(s: &mut Session, _p: &Value) -> Result<Value> {
    let (_, d) = active(s, "develop.pluginFilter.list")?;
    Ok(json!({
        "enabled": d.section_enabled(lcc_develop::PLUGIN_FILTERS_SECTION),
        "filters": serde_json::to_value(&d.plugin_filters).unwrap_or(Value::Null),
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        cmd!(
            "develop.pluginFilter.add",
            "Add Plug-in Filter",
            [],
            None,
            "{plugin, module_hash, name, params?: {key: number|bool|string}, stage?: source|display} — appends a pixel-filter plug-in to the active photo; returns {uid}",
            has_active,
            add
        ),
        cmd!("develop.pluginFilter.remove", "Remove Plug-in Filter", [], None, "{uid}", has_active, remove),
        cmd!(
            "develop.pluginFilter.set",
            "Change Plug-in Filter",
            [],
            None,
            "{uid, params?: {key: value} (merged), enabled?: bool, stage?: source|display}",
            has_active,
            set
        ),
        cmd!("develop.pluginFilter.move", "Move Plug-in Filter", [], None, "{uid, index} — new position in the stack", has_active, move_to),
        cmd!(
            "develop.pluginFilter.setAnalysis",
            "Measure Plug-in Filter",
            [],
            None,
            "{uid, analysis: JSON (null clears)} — stores the filter's whole-image measurement",
            has_active,
            set_analysis
        ),
        cmd!(
            "develop.pluginFilter.analyze",
            "Measure Plug-in Filter on Photo",
            [],
            None,
            "{uid, wait?: false} — runs the plug-in's whole-image measurement (pc_analyze) on the photo in the background (an activity row; `develop.pluginFilter.pump` stores it as one History step when done) → {started}; `wait: true` measures before returning → {analysis}. An `unsupported` error when the plug-in has none",
            has_active,
            analyze
        ),
        cmd!(
            query "develop.pluginFilter.pump",
            "Plug-in Filter Measurements",
            [],
            None,
            "{} → {running: [{photo, uid}], failed: [{photo, uid, error}]} — stores finished measurements (each one History step); called every frame by the app",
            always,
            pump
        ),
        cmd!(query "develop.pluginFilter.list", "List Plug-in Filters", [], None, "{} — returns {enabled, filters: [...]}", has_active, list),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(s: &Session) -> Vec<PluginFilter> {
        s.develop_of(s.active().unwrap()).unwrap().plugin_filters.clone()
    }

    fn add_one(s: &mut Session, name: &str) -> u64 {
        let r =
            s.execute("develop.pluginFilter.add", &json!({"plugin": "t.f", "module_hash": "ab", "name": name, "params": {"amount": 1.0}})).unwrap();
        r["uid"].as_u64().unwrap()
    }

    #[test]
    fn edits_are_history_steps_and_undo_restores() {
        let mut s = Session::with_demo();
        let u0 = s.undo.len();
        let a = add_one(&mut s, "Neg");
        let b = add_one(&mut s, "Glow");
        assert_ne!(a, b);
        assert_eq!(s.undo.len(), u0 + 2);
        s.execute("develop.pluginFilter.set", &json!({"uid": a, "params": {"gamma": 2.2}, "stage": "source"})).unwrap();
        let f = &stack(&s)[0];
        assert_eq!((f.params.len(), f.stage), (2, FilterStage::Source));
        s.execute("develop.pluginFilter.setAnalysis", &json!({"uid": a, "analysis": {"base": [0.8, 0.5, 0.3]}})).unwrap();
        s.execute("develop.pluginFilter.set", &json!({"uid": a, "enabled": false})).unwrap();
        assert!(stack(&s)[0].analysis.is_some(), "set keeps the analysis");
        s.execute("develop.pluginFilter.move", &json!({"uid": a, "index": 1})).unwrap();
        assert_eq!(stack(&s)[1].uid as u64, a);
        s.execute("develop.pluginFilter.remove", &json!({"uid": b})).unwrap();
        assert_eq!(stack(&s).len(), 1);
        assert_eq!(s.undo.len(), u0 + 7);
        let listed = s.execute("develop.pluginFilter.list", &json!({})).unwrap();
        assert_eq!(listed["filters"].as_array().unwrap().len(), 1);
        for _ in 0..7 {
            s.execute("edit.undo", &json!({})).unwrap();
        }
        assert!(stack(&s).is_empty());
    }

    #[test]
    fn hostile_args_error_without_panic() {
        let mut s = Session::with_demo();
        let a = add_one(&mut s, "Neg");
        let bad_calls = [
            ("develop.pluginFilter.add", json!({})),
            ("develop.pluginFilter.add", json!({"plugin": 1, "module_hash": "x", "name": "n"})),
            ("develop.pluginFilter.add", json!({"plugin": "p", "module_hash": "x", "name": "n", "params": [1]})),
            ("develop.pluginFilter.add", json!({"plugin": "p", "module_hash": "x", "name": "n", "params": {"a": {"b": 1}}})),
            ("develop.pluginFilter.add", json!({"plugin": "p", "module_hash": "x", "name": "n", "stage": "middle"})),
            ("develop.pluginFilter.add", json!({"plugin": "x".repeat(10_000), "module_hash": "x", "name": "n"})),
            ("develop.pluginFilter.remove", json!({"uid": 999})),
            ("develop.pluginFilter.remove", json!({"uid": u64::MAX})),
            ("develop.pluginFilter.remove", json!({"uid": "1"})),
            ("develop.pluginFilter.set", json!({"uid": a, "enabled": "yes"})),
            ("develop.pluginFilter.set", json!({"uid": 12345, "enabled": false})),
            ("develop.pluginFilter.move", json!({"uid": a, "index": u64::MAX})),
            ("develop.pluginFilter.move", json!({"uid": a, "index": -1})),
            ("develop.pluginFilter.move", json!({"uid": a, "index": 1})),
            ("develop.pluginFilter.setAnalysis", json!({"uid": a})),
            ("develop.pluginFilter.setAnalysis", json!({"uid": 77, "analysis": 1})),
            ("develop.pluginFilter.setAnalysis", json!({"uid": a, "analysis": "x".repeat(100_000)})),
        ];
        for (id, p) in bad_calls {
            assert!(s.execute(id, &p).is_err(), "{id} {p}");
        }
        assert_eq!(stack(&s).len(), 1);
        for _ in 0..MAX_PLUGIN_FILTERS - 1 {
            add_one(&mut s, "More");
        }
        assert!(s.execute("develop.pluginFilter.add", &json!({"plugin": "p", "module_hash": "x", "name": "n"})).is_err());
    }
}
