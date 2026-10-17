//! The Edit panel's Plug-in Filters section: the active photo's pixel-filter plug-in stack, one row
//! per entry (enable, name, stage, reorder, remove) with controls generated from the plug-in's
//! manifest parameters. Every edit goes through the `develop.pluginFilter.*` commands; a slider drag
//! is wrapped in `develop.beginInteraction` / `endInteraction`, so it is one History step.

use egui::{Color32, RichText};
use lcc_develop::controls::{ControlSpec, Section, Track};
use lcc_develop::{DevelopSettings, FilterStage, PLUGIN_FILTERS_SECTION, PluginFilter};
use serde_json::{Map, Value, json};

use crate::LccApp;
use crate::i18n::tr;
use crate::widgets::register;

/// An installed filter plug-in as `plugin.list` describes it (missing keys: no controls).
#[derive(Clone, Debug, Default)]
pub(crate) struct FilterPlugin {
    pub id: String,
    pub name: String,
    pub module_hash: String,
    /// The manifest's parameter schema (`{key: {type, label?, …}}`), or `None` when not given.
    pub params: Option<Map<String, Value>>,
    /// Parameter names in the manifest's order (`paramOrder`); keys of `params` it misses follow.
    pub order: Vec<String>,
    pub analyze: bool,
}

impl FilterPlugin {
    /// The parameter schema in the manifest's order.
    pub(crate) fn ordered_params(&self) -> Vec<(&str, &Value)> {
        let Some(schema) = &self.params else { return Vec::new() };
        let mut out: Vec<(&str, &Value)> = self.order.iter().filter_map(|k| schema.get_key_value(k)).map(|(k, v)| (k.as_str(), v)).collect();
        for (k, v) in schema {
            if !out.iter().any(|(o, _)| *o == k) {
                out.push((k.as_str(), v));
            }
        }
        out
    }
}

/// Installed filter plug-ins from a `plugin.list` reply; entries without an id or module hash are skipped.
pub(crate) fn filter_plugins(list: &Value) -> Vec<FilterPlugin> {
    let Some(all) = list.get("plugins").and_then(Value::as_array) else { return Vec::new() };
    all.iter()
        .filter(|p| p.get("kind").and_then(Value::as_str) == Some("filter"))
        .filter_map(|p| {
            let id = p.get("id").and_then(Value::as_str)?.to_string();
            let module_hash = p.get("moduleHash").and_then(Value::as_str)?.to_string();
            Some(FilterPlugin {
                name: p.get("name").and_then(Value::as_str).unwrap_or(&id).to_string(),
                id,
                module_hash,
                params: p.get("params").and_then(Value::as_object).cloned(),
                order: p
                    .get("paramOrder")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default(),
                analyze: p.get("analyze").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// Defaults from a manifest parameter schema (what a new entry starts with).
fn defaults(schema: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (k, s) in schema {
        let v = match s.get("type").and_then(Value::as_str) {
            Some("number") | Some("int") | Some("bool") => s.get("default").cloned(),
            Some("choice") => s
                .get("default")
                .filter(|d| d.is_string())
                .cloned()
                .or_else(|| s.get("options").and_then(Value::as_array).and_then(|o| o.first()).cloned()),
            _ => None,
        };
        if let Some(v) = v.filter(|v| !v.is_null()) {
            out.insert(k.clone(), v);
        }
    }
    out
}

/// What a row asks for this frame.
enum Action {
    Run(&'static str, Value),
    Begin(String),
    End,
}

pub(crate) fn show(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    super::edit::section(app, ui, d, PLUGIN_FILTERS_SECTION, "Plug-in Filters", body);
}

/// Every frame: stores finished measurements and keeps polling while one runs.
pub(crate) fn pump(app: &mut LccApp, ctx: &egui::Context) {
    if app.session.plugin_analyze_pump() {
        ctx.request_repaint();
    }
    if app.session.plugin_analyses_running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

const WARN: Color32 = Color32::from_rgb(0xe0, 0xa0, 0x30);

/// The inset of everything in the section that isn't a slider (sliders inset themselves).
fn inset<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, add).inner
}

/// What a row needs from the session besides the entry.
struct RowState<'a> {
    plugin: Option<&'a FilterPlugin>,
    can_measure: bool,
    measuring: bool,
    measure_error: Option<String>,
    warnings: Vec<&'a str>,
}

fn body(app: &mut LccApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    // `plugin.list` is only asked while the section is open
    let list = app.session.execute("plugin.list", &json!({})).unwrap_or(Value::Null);
    let installed = filter_plugins(&list);
    let can_measure = lcc_engine::find_command("develop.pluginFilter.analyze").is_some();
    let mut actions: Vec<Action> = Vec::new();
    let photo = app.session.active();
    // the loupe's latest render of this photo: its warnings (a filter skipped or failed)
    let warnings: Vec<String> = photo
        .and_then(|p| app.renderer.textures.get(&crate::render::Slot::Main).filter(|t| t.photo == p))
        .map(|t| t.warnings.clone())
        .unwrap_or_default();
    let n = d.plugin_filters.len();
    if n == 0 {
        inset(ui, |ui| ui.label(RichText::new(tr("No plug-in filters on this photo")).weak()));
    }
    let mut claimed = vec![false; warnings.len()];
    for (i, f) in d.plugin_filters.iter().enumerate() {
        let mut mine = Vec::new();
        for (w, c) in warnings.iter().zip(claimed.iter_mut()) {
            if let Some(m) = lcc_pipeline::plugin_filters::warning_message(w, f) {
                mine.push(m);
                *c = true;
            }
        }
        let st = RowState {
            plugin: installed.iter().find(|p| p.id == f.plugin),
            can_measure,
            measuring: photo.is_some_and(|p| app.session.plugin_analyzing(p, f.uid)),
            measure_error: photo.and_then(|p| app.session.plugin_analyze_error(p, f.uid)).map(str::to_string),
            warnings: mine,
        };
        ui.push_id(("pluginFilter", f.uid), |ui| row(ui, f, i, n, &st, &mut actions));
        inset(ui, |ui| ui.separator());
    }
    inset(ui, |ui| {
        // render-level warnings (not about one entry) stay under the stack
        for (w, _) in warnings.iter().zip(&claimed).filter(|(_, c)| !**c) {
            ui.label(RichText::new(format!("⚠ {w}")).color(WARN));
        }
        let r = ui.menu_button(tr("Add Filter…"), |ui| {
            ui.set_min_width(180.0);
            if installed.is_empty() {
                ui.label(RichText::new(tr("No filter plug-ins installed")).weak());
            }
            for p in &installed {
                let r = ui.button(&p.name);
                register(ui.ctx(), format!("pluginFilterAdd:{}", p.id), r.rect);
                if r.clicked() {
                    let params = p.params.as_ref().map(defaults).unwrap_or_default();
                    // no stage: the plug-in's own (its manifest, or the engine's hint for it)
                    actions.push(Action::Run(
                        "develop.pluginFilter.add",
                        json!({"plugin": p.id, "module_hash": p.module_hash, "name": p.name, "params": params}),
                    ));
                    ui.close();
                }
            }
        });
        register(ui.ctx(), "pluginFilterAddMenu", r.response.rect);
    });
    for a in actions {
        // errors surface through the app's command error path; nothing here can corrupt the stack
        let _ = match a {
            Action::Run(id, p) => app.run(id, p),
            Action::Begin(label) => app.run("develop.beginInteraction", json!({"label": label})),
            Action::End => app.run("develop.endInteraction", json!({})),
        };
    }
}

fn row(ui: &mut egui::Ui, f: &PluginFilter, i: usize, n: usize, st: &RowState, out: &mut Vec<Action>) {
    let uid = f.uid;
    let plugin = st.plugin;
    inset(ui, |ui| {
        ui.horizontal(|ui| {
            let mut on = f.enabled;
            let r = ui.checkbox(&mut on, "");
            crate::access::label(&r, "Enabled");
            if r.changed() {
                out.push(Action::Run("develop.pluginFilter.set", json!({"uid": uid, "enabled": on})));
            }
            ui.label(RichText::new(&f.name).strong());
            if plugin.is_none() {
                ui.label(RichText::new(format!("⚠ {}", tr("not installed"))).color(WARN))
                    .on_hover_text(tr("This filter's plug-in is not installed: it is skipped and its settings are kept."));
            }
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Stage")).strong()).on_hover_text(tr(
                "Source: on the decoded raw, before white balance and tone (e.g. negative conversion). Display: after the develop adjustments.",
            ));
            let name = |s: FilterStage| match s {
                FilterStage::Source => tr("Source"),
                FilterStage::Display => tr("Display"),
            };
            egui::ComboBox::from_id_salt("stage").width(76.0).selected_text(name(f.stage)).show_ui(ui, |ui| {
                for (s, key) in [(FilterStage::Source, "source"), (FilterStage::Display, "display")] {
                    if ui.selectable_label(f.stage == s, name(s)).clicked() && f.stage != s {
                        out.push(Action::Run("develop.pluginFilter.set", json!({"uid": uid, "stage": key})));
                    }
                }
            });
            if st.can_measure && plugin.is_some_and(|p| p.analyze) {
                if st.measuring {
                    ui.add(egui::Spinner::new().size(12.0));
                    let r = ui.add_enabled(false, egui::Button::new(tr("Measuring…")));
                    register(ui.ctx(), format!("pluginFilterMeasure:{uid}"), r.rect);
                } else {
                    let r = ui.button(tr("Measure"));
                    register(ui.ctx(), format!("pluginFilterMeasure:{uid}"), r.rect);
                    if r.clicked() {
                        out.push(Action::Run("develop.pluginFilter.analyze", json!({"uid": uid})));
                    }
                }
            }
            if ui.add_enabled(i > 0, egui::Button::new("▲").small()).on_hover_text(tr("Move Up")).clicked() {
                out.push(Action::Run("develop.pluginFilter.move", json!({"uid": uid, "index": i.saturating_sub(1)})));
            }
            if ui.add_enabled(i + 1 < n, egui::Button::new("▼").small()).on_hover_text(tr("Move Down")).clicked() {
                out.push(Action::Run("develop.pluginFilter.move", json!({"uid": uid, "index": i + 1})));
            }
            if ui.small_button("×").on_hover_text(tr("Remove")).clicked() {
                out.push(Action::Run("develop.pluginFilter.remove", json!({"uid": uid})));
            }
        });
        if let Some(e) = &st.measure_error {
            ui.label(RichText::new(format!("⚠ {}: {e}", tr("Measure"))).color(WARN));
        }
        for w in &st.warnings {
            ui.label(RichText::new(format!("⚠ {w}")).color(WARN));
        }
    });
    let Some(p) = plugin.filter(|p| p.params.is_some()) else {
        // no plug-in or no schema: values shown read-only
        inset(ui, |ui| {
            for (k, v) in &f.params {
                ui.label(RichText::new(format!("{}: {v}", lcc_filters_label(k, None))).weak());
            }
        });
        return;
    };
    for (k, s) in p.ordered_params() {
        param(ui, uid, &f.name, k, s, f.params.get(k), out);
    }
}

/// A parameter's label: the manifest's `label` (as `plugin.list` gives it), else the key humanised.
fn lcc_filters_label(key: &str, spec: Option<&Value>) -> String {
    match spec.and_then(|s| s.get("label")).and_then(Value::as_str).filter(|l| !l.trim().is_empty()) {
        Some(l) => l.to_string(),
        None => humanise(key),
    }
}

/// `dynamic_range` / `dynamicRange` → "Dynamic range".
pub(crate) fn humanise(key: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if matches!(c, '_' | '-' | ' ') {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        let acronym = w.chars().count() > 1 && w.chars().all(|c| !c.is_lowercase());
        let w = if acronym { w.clone() } else { w.to_lowercase() };
        if i == 0 {
            let mut cs = w.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        } else {
            out.push(' ');
            out.push_str(&w);
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

/// A `'static` control id for a plug-in parameter slider (the develop slider widget keys its state and
/// its control-channel name on it). Interned once per name, at most [`MAX_IDS`] of them.
fn slider_id(uid: u32, key: &str) -> &'static str {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock, PoisonError};
    const MAX_IDS: usize = 4096;
    static IDS: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let name = format!("pluginFilter.{uid}.{key}");
    let mut ids = IDS.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(id) = ids.get(&name) {
        return id;
    }
    if ids.len() >= MAX_IDS {
        return "pluginFilter.param";
    }
    let id: &'static str = Box::leak(name.clone().into_boxed_str());
    ids.insert(name, id);
    id
}

/// The develop control spec a numeric parameter's slider uses.
fn slider_spec(uid: u32, key: &str, lo: f64, hi: f64, def: f64, int: bool) -> ControlSpec {
    let span = hi - lo;
    let decimals: u8 = if int {
        0
    } else if span <= 10.0 {
        2
    } else if span <= 100.0 {
        1
    } else {
        0
    };
    ControlSpec {
        id: slider_id(uid, key),
        label: "",
        section: Section::Effects,
        min: lo,
        max: hi,
        default: def.clamp(lo, hi),
        step: 10f64.powi(-i32::from(decimals)),
        decimals,
        track: if lo < 0.0 && hi > 0.0 { Track::Centered } else { Track::Plain },
    }
}

fn param(ui: &mut egui::Ui, uid: u32, name: &str, key: &str, s: &Value, cur: Option<&Value>, out: &mut Vec<Action>) {
    let label = lcc_filters_label(key, Some(s));
    let set = |v: Value| Action::Run("develop.pluginFilter.set", json!({"uid": uid, "params": {key: v}}));
    match s.get("type").and_then(Value::as_str) {
        Some(t @ ("number" | "int")) => {
            let int = t == "int";
            let (lo, hi) = match (s.get("min").and_then(Value::as_f64), s.get("max").and_then(Value::as_f64)) {
                (Some(lo), Some(hi)) => (lo, hi),
                // ints default to a wide range, as the manifest does
                _ if int => (-1_000_000.0, 1_000_000.0),
                _ => return,
            };
            if !(lo.is_finite() && hi.is_finite()) || lo == hi {
                return;
            }
            let (lo, hi) = if lo > hi { (hi, lo) } else { (lo, hi) };
            let def = s.get("default").and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(lo);
            let spec = slider_spec(uid, key, lo, hi, def, int);
            let v = cur.and_then(Value::as_f64).unwrap_or(def).clamp(lo, hi);
            let o = crate::widgets::slider(ui, &spec, v, ui.is_enabled(), Some(&label));
            if o.drag_started && !o.reset {
                out.push(Action::Begin(format!("Plug-in: {name} › {label}")));
            }
            if let Some(nv) = o.value.filter(|nv| nv.is_finite()) {
                out.push(set(if int { json!(nv.round() as i64) } else { json!(nv) }));
            }
            if o.drag_stopped && !o.reset {
                out.push(Action::End);
            }
        }
        Some("bool") => inset(ui, |ui| {
            let mut b = cur.and_then(Value::as_bool).or_else(|| s.get("default").and_then(Value::as_bool)).unwrap_or(false);
            let r = ui.checkbox(&mut b, &label);
            if r.changed() {
                out.push(set(json!(b)));
            }
        }),
        Some("choice") => {
            let Some(opts) = s.get("options").and_then(Value::as_array) else { return };
            let now = cur.and_then(Value::as_str).or_else(|| s.get("default").and_then(Value::as_str)).unwrap_or("");
            inset(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(&label);
                    egui::ComboBox::from_id_salt(("choice", key)).selected_text(now).show_ui(ui, |ui| {
                        for o in opts.iter().filter_map(Value::as_str) {
                            if ui.selectable_label(o == now, o).clicked() && o != now {
                                out.push(set(json!(o)));
                            }
                        }
                    });
                });
            });
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_list_parsing_is_defensive() {
        let list = json!({"plugins": [
            {"id": "a.neg", "name": "Negative", "kind": "filter", "moduleHash": "h1", "analyze": true,
             "params": {"gamma": {"type": "number", "min": 1.0, "max": 3.0, "default": 2.2},
                        "mode": {"type": "choice", "options": ["c41", "bw"]},
                        "flip": {"type": "bool"}}},
            {"id": "b.cmd", "name": "Command", "kind": "command"},
            {"id": "c.nohash", "kind": "filter"},
            {"kind": "filter", "moduleHash": "x"},
            "junk",
        ]});
        let f = filter_plugins(&list);
        assert_eq!(f.len(), 1);
        assert!(f[0].analyze);
        let d = defaults(f[0].params.as_ref().unwrap());
        assert_eq!(d.get("gamma"), Some(&json!(2.2)));
        assert_eq!(d.get("mode"), Some(&json!("c41")));
        assert!(d.get("flip").is_none(), "no default given: the plug-in's own applies");
        assert!(filter_plugins(&Value::Null).is_empty());
        // no paramOrder: every key still shows
        assert_eq!(f[0].ordered_params().len(), 3);
        assert!(filter_plugins(&json!({"plugins": 3})).is_empty());
    }

    #[test]
    fn params_follow_the_manifest_order_and_get_labels() {
        let list = json!({"plugins": [{"id": "a", "kind": "filter", "moduleHash": "h",
            "params": {"a": {"type": "bool"}, "dynamic_range": {"type": "bool"}, "temp": {"type": "bool", "label": "Temperature"}},
            "paramOrder": ["temp", "dynamic_range", "gone", "a"]}]});
        let f = filter_plugins(&list);
        let keys: Vec<&str> = f[0].ordered_params().iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, ["temp", "dynamic_range", "a"]);
        let labels: Vec<String> = f[0].ordered_params().iter().map(|(k, s)| lcc_filters_label(k, Some(s))).collect();
        assert_eq!(labels, ["Temperature", "Dynamic range", "A"]);
        assert_eq!(humanise("dynamicRange"), "Dynamic range");
    }
}
