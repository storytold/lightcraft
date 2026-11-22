//! Photo ▸ Develop Settings ▸ Create Camera Profile from Color Chart… (LR-PROF-CREATE, LR-PROF-DUAL): the chart photo,
//! developed and oriented as the app shows it, with the patch grid found on it (corners can be
//! dragged, with a magnifier; the view zooms and pans), the light, what to match and the name, an
//! optional second chart photo under another light (picked from the library) for a dual-illuminant
//! profile, and after building, each light's per-patch ΔE report. Decoding, finding and fitting run on worker threads
//! with a row in the activity stack ([`crate::tasks::spawn`]); the profile lands among the camera
//! profiles through [`lcc_engine::chart_profiling::install`].

use std::collections::HashMap;
use std::sync::Arc;

use lcc_catalog::PhotoId;
use lcc_engine::chart_profiling::{self as cp, Prepared, Request, camprof};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;
use crate::state::Dialog;

/// Activity-stack labels (translated by the stack).
const PREPARING: &str = "Reading the color chart photo";
const BUILDING: &str = "Creating camera profile";

/// The dialog's choices (`Dialog::ChartProfile`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChartProfileDialog {
    /// Profile name: the camera and the light until edited.
    pub name: String,
    /// The name was typed: it is no longer kept up to date with the camera and light.
    pub name_edited: bool,
    /// Each photo's light: `D55` (Daylight / Flash) or `A` (Tungsten).
    pub lights: [String; 2],
    pub natural: bool,
    /// Even out the light on the chart (falloff across the card, glare on the patches).
    pub even_light: bool,
    /// Each photo's chart corners (normalised to its frame) and quarter turns, once known.
    pub corners: [Option<[[f64; 2]; 4]>; 2],
    pub turns: [u8; 2],
    /// Each photo's chart, by key (found by its colours; the ColorChecker 24 until then).
    pub charts: [String; 2],
    /// Which photo the picture shows (0 or 1).
    pub slot: usize,
    /// Which light's report is shown (index into the report's calibrations).
    pub report_tab: usize,
}

impl ChartProfileDialog {
    /// `slot`'s chart: its key looked up, the ColorChecker 24 when unknown.
    pub fn chart(&self, slot: usize) -> &'static camprof::reference::Chart {
        self.charts.get(slot).and_then(|k| camprof::reference::by_key(k)).unwrap_or_else(camprof::chart)
    }

    /// Where `slot`'s chart was found (normalized corners, quarter turns, chart key); an unknown slot is ignored.
    fn set_located(&mut self, slot: usize, corners: [[f64; 2]; 4], turns: u8, chart: &str) {
        if let (Some(c), Some(t), Some(k)) = (self.corners.get_mut(slot), self.turns.get_mut(slot), self.charts.get_mut(slot)) {
            *c = Some(corners);
            *t = turns;
            *k = chart.to_string();
        }
    }

    /// `slot`'s corners, `None` for an unknown slot too.
    fn corners_at(&self, slot: usize) -> Option<[[f64; 2]; 4]> {
        self.corners.get(slot).copied().flatten()
    }

    /// `slot`'s quarter turns, 0 for an unknown slot.
    fn turns_at(&self, slot: usize) -> u8 {
        self.turns.get(slot).copied().unwrap_or(0)
    }
}

impl Default for ChartProfileDialog {
    fn default() -> Self {
        ChartProfileDialog {
            name: String::new(),
            name_edited: false,
            lights: ["D55".into(), "A".into()],
            natural: true,
            even_light: true,
            corners: [None, None],
            turns: [0, 0],
            charts: [camprof::chart().key.to_string(), camprof::chart().key.to_string()],
            slot: 0,
            report_tab: 0,
        }
    }
}

/// The dialog's id in what the dialogs remember (`dialog.remember`).
const REMEMBER: &str = "cameraProfile";

/// A fresh dialog with the lights and look chosen last time (client note 13); the defaults when
/// nothing (or nothing readable) is remembered.
fn remembered(app: &LccApp) -> ChartProfileDialog {
    let mut opts = ChartProfileDialog::default();
    let Some(r) = app.session.dialogs.other.get(REMEMBER) else { return opts };
    if let Some(lights) = r.get("lights").and_then(Value::as_array) {
        for (slot, l) in opts.lights.iter_mut().zip(lights) {
            if let Some(l) = l.as_str().filter(|l| matches!(*l, "D55" | "A")) {
                *slot = l.to_string();
            }
        }
    }
    if let Some(natural) = r.get("natural").and_then(Value::as_bool) {
        opts.natural = natural;
    }
    opts
}

/// One chart photo in the dialog.
pub struct Shot {
    pub photo: Option<PhotoId>,
    pub path: String,
    pub prepared: Option<Arc<Prepared>>,
    pub texture: Option<egui::TextureHandle>,
    pub error: Option<String>,
}

/// The dialog's photos and results (kept on the app; the choices live in the dialog).
#[derive(Default)]
pub struct ChartProfileState {
    pub shots: [Option<Shot>; 2],
    /// The last profile built here: `profile.createFromChart`'s answer.
    pub report: Option<Value>,
    pub error: Option<String>,
    /// The corner being dragged.
    drag: Option<usize>,
    /// Each photo's zoom and pan.
    pub views: [View; 2],
    /// The second-chart picker, while open.
    pub picker: Option<Picker>,
    /// What the picker found in catalog photos (`None`: no chart).
    pub scans: HashMap<PhotoId, Option<cp::ChartScan>>,
    /// A profile of the name asked for is there: the question shown.
    pub clash: Option<Clash>,
    /// [`fingerprint`] of the last profile built.
    pub built: Option<String>,
    /// The profile menus also list camera profiles made for other bodies of the model.
    pub other_bodies: bool,
    /// "Adopt for This Body" also makes the profile the body's default.
    pub adopt_as_default: bool,
    /// Photos whose body serial was read from their file (imported before serials were kept):
    /// `true` while the read is under way.
    pub serial_reads: HashMap<PhotoId, bool>,
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.to_string())
}

/// Decode a photo for `slot` on a worker thread; the dialog shows it when it is ready.
fn load(app: &mut LccApp, slot: usize, photo: Option<PhotoId>, path: String) -> Result<Value, String> {
    let Some(entry) = app.chart_profile.shots.get_mut(slot) else { return Err("no such chart slot".into()) };
    *entry = Some(Shot { photo, path: path.clone(), prepared: None, texture: None, error: None });
    app.chart_profile.report = None;
    if let Some(view) = app.chart_profile.views.get_mut(slot) {
        *view = View::default();
    }
    let (p, shown) = (path.clone(), file_name(&path));
    crate::tasks::spawn(
        app,
        PREPARING,
        Some("cameraProfile"),
        move || cp::prepare_path(&p),
        move |app, ctx, result| {
            let Some(Some(shot)) = app.chart_profile.shots.get_mut(slot) else { return };
            if shot.path != path {
                return; // replaced meanwhile
            }
            match result {
                Ok(prep) => {
                    let img = &prep.display;
                    let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
                    shot.texture = Some(ctx.load_texture(format!("chart-profile-{slot}"), color, egui::TextureOptions::LINEAR));
                    let located = prep.located.map(|l| (cp::normalized_corners(&prep.frame, &l.corners), l.turns, l.chart));
                    shot.prepared = Some(Arc::new(prep));
                    if let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog
                        && let Some((corners, turns, chart)) = located
                    {
                        opts.set_located(slot, corners, turns, &chart.key);
                    }
                    // the second chart is under the other light: tungsten beside daylight, else daylight
                    if slot == 1
                        && let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog
                    {
                        opts.lights[1] = if opts.lights[0] == "D55" { "A".into() } else { "D55".into() };
                    }
                }
                Err(e) => shot.error = Some(e),
            }
        },
    )?;
    Ok(json!({"slot": slot, "path": shown}))
}

/// Open the dialog on the active photo (a raw photo of a ColorChecker); with exactly two photos
/// selected, the other becomes the second chart.
pub fn open(app: &mut LccApp) -> Result<Value, String> {
    let mut ids = app.session.targets(&json!({})).map_err(|e| e.to_string())?;
    if let Some(active) = app.session.active()
        && let Some(i) = ids.iter().position(|x| *x == active)
    {
        ids.swap(0, i);
    }
    let path_of = |app: &LccApp, id: PhotoId| match app.session.catalog.photo(id).map(|p| &p.source) {
        Some(lcc_catalog::Source::File { path }) => Some(path.clone()),
        _ => None,
    };
    let first = ids.first().copied().ok_or_else(|| crate::i18n::tr("Select a raw photo of a ColorChecker first").to_string())?;
    let path = path_of(app, first).ok_or_else(|| crate::i18n::tr("The photo has no file to profile from").to_string())?;
    app.chart_profile = ChartProfileState::default();
    app.ui.dialog = Some(Dialog::ChartProfile { opts: remembered(app) });
    load(app, 0, Some(first), path)?;
    // exactly two photos selected: the other is the second chart (another light)
    if ids.len() == 2
        && let Some(&second) = ids.get(1)
        && let Some(p) = path_of(app, second)
    {
        load(app, 1, Some(second), p)?;
    }
    Ok(json!({"photos": ids.len().min(2)}))
}

/// `cameraProfile.second {id? | path?}`: the second chart photo, under another light: a library
/// photo, or a file used for the profile only (not imported). Without either, the picker opens.
pub fn choose_second(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    if !matches!(app.ui.dialog, Some(Dialog::ChartProfile { .. })) {
        return Err("the camera profile dialog isn't open".into());
    }
    let (photo, path) = if let Some(id) = p.get("id").and_then(Value::as_u64) {
        let ph = app.session.catalog.photo(PhotoId(id)).ok_or_else(|| format!("no photo {id}"))?;
        match &ph.source {
            lcc_catalog::Source::File { path } => (Some(PhotoId(id)), path.clone()),
            _ => return Err(crate::i18n::tr("The photo has no file to profile from").to_string()),
        }
    } else if let Some(path) = p.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        (None, path.to_string())
    } else {
        return pick_second(app, &json!({}));
    };
    if let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog {
        opts.corners[1] = None;
        opts.slot = 1;
    }
    app.chart_profile.picker = None;
    load(app, 1, photo, path)
}

/// Raw files the second-chart picker offers.
const RAW_EXTENSIONS: &[&str] = &[
    "dng", "arw", "cr2", "cr3", "nef", "nrw", "raf", "rw2", "orf", "pef", "srw", "x3f", "3fr", "iiq", "erf", "kdc", "mos", "mef", "raw", "rwl",
    "srf", "sr2",
];

/// `cameraProfile.removeSecond`: back to a single-light profile.
pub fn remove_second(app: &mut LccApp) -> Result<Value, String> {
    app.chart_profile.shots[1] = None;
    app.chart_profile.report = None;
    if let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog {
        opts.corners[1] = None;
        opts.slot = 0;
        opts.report_tab = 0;
    }
    Ok(Value::Null)
}

/// `cameraProfile.find {slot?}`: look for the chart again; `cameraProfile.turn {slot?}`: patch A01
/// a corner further round; `cameraProfile.align {slot?}`: move the corners onto the chart.
pub fn adjust(app: &mut LccApp, command: &str, p: &Value) -> Result<Value, String> {
    let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog else { return Err("the camera profile dialog isn't open".into()) };
    let slot = p.get("slot").and_then(Value::as_u64).map_or(opts.slot, |s| s.min(1) as usize);
    adjust_in(&mut app.chart_profile, opts, command, slot)
}

fn adjust_in(state: &mut ChartProfileState, opts: &mut ChartProfileDialog, command: &str, slot: usize) -> Result<Value, String> {
    let prep = state.shots.get(slot).and_then(|s| s.as_ref()).and_then(|s| s.prepared.clone()).ok_or("the chart photo isn't read yet")?;
    match command {
        "cameraProfile.turn" => {
            let t = opts.turns.get_mut(slot).ok_or("no such chart slot")?;
            *t = (*t + 1) % 4;
        }
        "cameraProfile.find" => {
            let found =
                camprof::find_chart(&prep.frame).ok_or_else(|| crate::i18n::tr("No color chart found: drag the corners onto it").to_string())?;
            opts.set_located(slot, cp::normalized_corners(&prep.frame, &found.corners), found.turns, &found.chart.key);
        }
        _ => {
            let corners = opts.corners_at(slot).and_then(|c| cp::corners_from_normalized(&prep.frame, c)).ok_or("no corners to align")?;
            let refined = camprof::refine(&prep.frame, opts.chart(slot), &corners, opts.turns_at(slot))
                .ok_or_else(|| crate::i18n::tr("The corners are too far from the chart to align: drag them closer").to_string())?;
            let c = opts.corners.get_mut(slot).ok_or("no such chart slot")?;
            *c = Some(cp::normalized_corners(&prep.frame, &refined.corners));
        }
    }
    state.report = None;
    Ok(json!({"slot": slot, "corners": opts.corners_at(slot), "turns": opts.turns_at(slot), "chart": opts.charts.get(slot)}))
}

/// A profile of the name asked for is there: what the dialog asks before saving.
#[derive(Clone, Debug, PartialEq)]
pub struct Clash {
    pub id: String,
    pub name: String,
    pub used_by: usize,
    /// The name "Keep both" saves under (editable).
    pub keep_name: String,
}

/// Everything that decides what Create Profile makes: while it is unchanged after a build, there
/// is nothing to build again.
pub fn fingerprint(app: &LccApp, opts: &ChartProfileDialog) -> String {
    let paths: Vec<Option<String>> = app.chart_profile.shots.iter().map(|s| s.as_ref().map(|s| s.path.clone())).collect();
    json!({"paths": paths, "corners": opts.corners, "turns": opts.turns, "lights": opts.lights, "natural": opts.natural, "name": opts.name.trim()})
        .to_string()
}

/// The profile Create Profile would make was just made (nothing changed since).
pub fn is_built(app: &LccApp, opts: &ChartProfileDialog) -> bool {
    app.chart_profile.built.as_deref() == Some(fingerprint(app, opts).as_str())
}

/// The dialog's Create Profile: when a profile of that name is there for the camera, ask first
/// ([`Clash`]); else fit on a worker thread, then file the profile and show the report.
pub fn start_build(app: &mut LccApp, opts: &ChartProfileDialog) -> Result<Value, String> {
    if is_built(app, opts) {
        return Ok(json!({"built": true}));
    }
    if let Some(e) = same_light(app, opts) {
        return Err(e);
    }
    // the lights and the look are asked the same way next time, in every library (client note 13)
    if let Err(e) = app.run("dialog.remember", json!({"dialog": REMEMBER, "choices": {"lights": opts.lights, "natural": opts.natural}})) {
        log::warn!("camera profile: remembering the choices: {e}");
    }
    let camera = app.chart_profile.shots[0].as_ref().and_then(|s| s.prepared.as_ref()).map(|p| p.camera_model.clone()).unwrap_or_default();
    if let Some(id) = cp::existing(&opts.name, &camera) {
        let clash =
            Clash { used_by: cp::used_by(&app.session, &id), id, name: opts.name.trim().to_string(), keep_name: cp::free_name(&opts.name, &camera) };
        app.chart_profile.clash = Some(clash.clone());
        return Ok(json!({"exists": {"id": clash.id, "name": clash.name, "usedBy": clash.used_by}}));
    }
    build_now(app, opts, cp::OnExists::Fail, None)
}

/// `cameraProfile.resolve {choice: replace | keepBoth | cancel, name?}`: answer the name clash.
pub fn resolve_clash(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let Some(Dialog::ChartProfile { opts }) = app.ui.dialog.clone() else { return Err("the camera profile dialog isn't open".into()) };
    let Some(clash) = app.chart_profile.clash.take() else { return Err("nothing to decide".into()) };
    use cp::OnExists;
    match p.get("choice").and_then(Value::as_str).unwrap_or("cancel") {
        "replace" => build_now(app, &opts, OnExists::Replace, None),
        "keepBoth" => {
            let name = p.get("name").and_then(Value::as_str).map(str::to_string).unwrap_or(clash.keep_name);
            if let Some(Dialog::ChartProfile { opts }) = &mut app.ui.dialog {
                opts.name = name.clone();
                opts.name_edited = true;
            }
            let mut renamed = opts.clone();
            renamed.name = name;
            renamed.name_edited = true;
            start_build(app, &renamed)
        }
        _ => Ok(Value::Null),
    }
}

fn build_now(app: &mut LccApp, opts: &ChartProfileDialog, on_exists: cp::OnExists, _: Option<()>) -> Result<Value, String> {
    if app.tasks.is_running(BUILDING) {
        return Err(crate::i18n::tr("A camera profile is being built").to_string());
    }
    let shot = |i: usize| app.chart_profile.shots.get(i).and_then(|s| s.as_ref());
    let first = shot(0).and_then(|s| s.prepared.clone()).ok_or_else(|| crate::i18n::tr("The chart photo isn't read yet").to_string())?;
    let second =
        shot(1).map(|s| s.prepared.clone().ok_or_else(|| crate::i18n::tr("The second chart photo isn't read yet").to_string())).transpose()?;
    let request = |i: usize| Request {
        corners: opts.corners_at(i),
        turns: Some(opts.turns_at(i)),
        refine: false,
        illuminant: opts.lights.get(i).cloned().unwrap_or_default(),
        chart: Some(opts.chart(i).key.to_string()),
    };
    let (req_a, req_b) = (request(0), request(1));
    let options = camprof::BuildOptions {
        illuminant: String::new(),
        natural: opts.natural,
        correction_table: true,
        camera_model: String::new(),
        name: opts.name.clone(),
        even_light: opts.even_light,
    };
    let apply: Vec<PhotoId> = (0..2).filter_map(|i| shot(i).and_then(|s| s.photo)).collect();
    // the chart photos' bodies start with the new profile
    let bodies: Vec<String> = {
        let mut b: Vec<String> = Vec::new();
        for k in apply.iter().filter_map(|id| app.session.catalog.photo(*id).and_then(|p| lcc_engine::profile_bodies::key_of(p))) {
            if !b.contains(&k) {
                b.push(k);
            }
        }
        b
    };
    let fp = fingerprint(app, opts);
    app.chart_profile.error = None;
    crate::tasks::spawn(
        app,
        BUILDING,
        Some("cameraProfile"),
        move || {
            let out = cp::build((&first, &req_a), second.as_deref().map(|p| (p, &req_b)), &options)?;
            let preps: Vec<&Prepared> = std::iter::once(first.as_ref()).chain(second.as_deref()).collect();
            let report = cp::report_json(&preps, &out);
            Ok::<_, String>((out.dcp, report))
        },
        move |app, ctx, result| match result.and_then(|(dcp, mut report)| {
            let library =
                lcc_engine::camera_profiles::dcp_library_dir().ok_or("no camera-profiles folder (no configuration folder on this system)")?;
            let installed = cp::install(&mut app.session, &library, &dcp, &bodies, on_exists, &apply).map_err(|e| e.to_string())?;
            if let (Some(o), Some(i)) = (report.as_object_mut(), installed.as_object()) {
                o.extend(i.clone());
            }
            Ok(report)
        }) {
            Ok(report) => {
                let name = report.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let msg = if report.get("replaced").and_then(Value::as_bool) == Some(true) {
                    crate::i18n::tr_format!("Camera profile “{name}” replaced", name = name)
                } else {
                    crate::i18n::tr_format!("Camera profile “{name}” created", name = name)
                };
                app.toast(ctx, msg);
                app.chart_profile.report = Some(report);
                app.chart_profile.built = Some(fp);
            }
            Err(e) => app.chart_profile.error = Some(e),
        },
    )?;
    Ok(json!({"started": true}))
}

/// Whether Create Profile can run now.
pub fn can_build(app: &LccApp, opts: &ChartProfileDialog) -> bool {
    let ready =
        |i: usize| app.chart_profile.shots.get(i).and_then(|s| s.as_ref()).is_some_and(|s| s.prepared.is_some()) && opts.corners_at(i).is_some();
    !app.tasks.is_running(BUILDING)
        && same_light(app, opts).is_none()
        && app.chart_profile.clash.is_none()
        && !is_built(app, opts)
        && !opts.name.trim().is_empty()
        && ready(0)
        && (app.chart_profile.shots[1].is_none() || ready(1))
}

/// The name clash, in the dialog: Replace, Keep Both (under an editable name) or Cancel.
fn clash_panel(app: &mut LccApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let Some(mut clash) = app.chart_profile.clash.clone() else { return };
    let mut choice = None;
    egui::Frame::group(ui.style()).fill(t.inset).show(ui, |ui| {
        ui.set_width(250.0);
        ui.label(egui::RichText::new(crate::i18n::tr_format!("A profile “{name}” is already there.", name = clash.name)).color(t.text));
        if clash.used_by > 0 {
            ui.label(egui::RichText::new(crate::i18n::tr_format!("Used by {n} photos; they will update.", n = clash.used_by)).color(t.text_dim));
        }
        let r = ui.button(crate::i18n::tr("Replace"));
        crate::widgets::register(ui.ctx(), "button:chartProfileReplace", r.rect);
        if r.clicked() {
            choice = Some("replace");
        }
        crate::text_field::TextField::singleline("chartProfileKeepName", &mut clash.keep_name).label(crate::i18n::tr("Name")).width(240.0).show(ui);
        ui.horizontal(|ui| {
            let r = ui.button(crate::i18n::tr("Keep Both"));
            crate::widgets::register(ui.ctx(), "button:chartProfileKeepBoth", r.rect);
            if r.clicked() {
                choice = Some("keepBoth");
            }
            let r = ui.button(crate::i18n::tr("Cancel"));
            crate::widgets::register(ui.ctx(), "button:chartProfileClashCancel", r.rect);
            if r.clicked() {
                choice = Some("cancel");
            }
        });
    });
    let name = clash.keep_name.clone();
    app.chart_profile.clash = Some(clash);
    if let Some(c) = choice {
        app.chart_profile.error = resolve_clash(app, &json!({"choice": c, "name": name})).err();
    }
}

/// A Lab (D50) colour as display sRGB.
fn swatch(lab: &Value) -> egui::Color32 {
    let v = |i: usize| lab.get(i).and_then(Value::as_f64).unwrap_or(0.0);
    let xyz = camprof::color::lab_to_xyz(camprof::Vec3::new(v(0), v(1), v(2)), camprof::color::D50);
    let d65 = camprof::color::white_map(camprof::color::Cat::Bradford, camprof::color::D50, camprof::color::D65) * xyz;
    let rgb = lcc_color::SRGB.from_xyz().apply([d65[0], d65[1], d65[2]]);
    let e = |c: f64| lcc_color::transfer::encode_srgb8(c.clamp(0.0, 1.0) as f32);
    egui::Color32::from_rgb(e(rgb[0]), e(rgb[1]), e(rgb[2]))
}

/// The two lights the dialog offers (CLI and MCP take every standard illuminant): DNG's D55 for
/// daylight and flash, Standard light A for tungsten; both have a definite colour temperature, which
/// a dual-illuminant profile needs.
const LIGHTS: &[(&str, &str)] = &[("D55", "Daylight / Flash"), ("A", "Tungsten")];

/// A light as the dialog names it: Daylight / Flash, Tungsten, else its standard name.
pub fn light_label(v: &str) -> String {
    crate::i18n::tr(LIGHTS.iter().find(|(k, _)| *k == v).map_or(v, |(_, l)| l)).to_string()
}

/// A profile's illuminant in a report: Daylight, Tungsten, else its standard name.
fn light_word(v: &str) -> String {
    crate::i18n::tr(&cp::light_word(v)).to_string()
}

/// What `slot`'s chart's greys measure (information only), with its corners as they are now.
pub fn estimate(app: &LccApp, opts: &ChartProfileDialog, slot: usize) -> Option<cp::LightEstimate> {
    let prep = app.chart_profile.shots.get(slot)?.as_ref()?.prepared.clone()?;
    let c = opts.corners.get(slot).copied().flatten().and_then(|c| cp::corners_from_normalized(&prep.frame, c))?;
    cp::estimate_light(
        &prep,
        &camprof::Located { corners: c, turns: opts.turns.get(slot).copied().unwrap_or(0), score: None, chart: opts.chart(slot) },
    )
}

/// The light `slot` will be profiled for.
fn resolved_light(_: &LccApp, opts: &ChartProfileDialog, slot: usize) -> Option<String> {
    opts.lights.get(slot).cloned()
}

/// The light menu: Daylight / Flash or Tungsten.
fn light_combo(ui: &mut egui::Ui, id: &str, value: &mut String) -> bool {
    let mut changed = false;
    if !LIGHTS.iter().any(|(k, _)| k == value) {
        *value = cp::DEFAULT_LIGHT.into();
        changed = true;
    }
    let r = egui::ComboBox::from_id_salt(id).selected_text(light_label(value)).width(220.0).show_ui(ui, |ui| {
        for (k, _) in LIGHTS {
            if ui.selectable_label(value == k, light_label(k)).clicked() && value != k {
                *value = k.to_string();
                changed = true;
            }
        }
    });
    crate::widgets::register(ui.ctx(), format!("combo:{id}"), r.response.rect);
    changed
}

/// The chart view's zoom and pan: `zoom` 1 = the whole photo fits, `center` the point of the photo
/// (0–1 across and down) at the middle of the view.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub zoom: f32,
    pub center: [f32; 2],
}

impl Default for View {
    fn default() -> Self {
        View { zoom: 1.0, center: [0.5, 0.5] }
    }
}

/// How far the chart view zooms in past fitting the photo.
pub const MAX_ZOOM: f32 = 32.0;

impl View {
    fn fit_scale(area: egui::Rect, tex: [usize; 2]) -> f32 {
        (area.width() / tex[0].max(1) as f32).min(area.height() / tex[1].max(1) as f32)
    }

    /// Where the photo lies on screen.
    pub fn image_rect(&self, area: egui::Rect, tex: [usize; 2]) -> egui::Rect {
        let s = Self::fit_scale(area, tex) * self.zoom;
        let size = egui::vec2(tex[0] as f32 * s, tex[1] as f32 * s);
        egui::Rect::from_min_size(area.center() - egui::vec2(self.center[0] * size.x, self.center[1] * size.y), size)
    }

    /// Keep the photo covering the view where it can (centred where it is smaller).
    fn clamp(&mut self, area: egui::Rect, tex: [usize; 2]) {
        let size = self.image_rect(area, tex).size();
        for (c, (view, img)) in self.center.iter_mut().zip([(area.width(), size.x), (area.height(), size.y)]) {
            *c = if img <= view || !c.is_finite() {
                0.5
            } else {
                let half = view / 2.0 / img;
                c.clamp(half, 1.0 - half)
            };
        }
    }

    /// Zoom by `factor`, keeping the photo point under `anchor` where it is.
    pub fn zoom_about(&mut self, area: egui::Rect, tex: [usize; 2], factor: f32, anchor: egui::Pos2) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let before = self.image_rect(area, tex);
        let p = (anchor - before.min) / before.size();
        let lowest = self.actual_zoom(area, tex, 1.0).min(1.0);
        self.zoom = (self.zoom * factor).clamp(lowest, MAX_ZOOM);
        let size = self.image_rect(area, tex).size();
        let off = (anchor - area.center()) / size;
        self.center = [p.x - off.x, p.y - off.y];
        self.clamp(area, tex);
    }

    /// Move the photo by `delta` screen points.
    pub fn pan(&mut self, area: egui::Rect, tex: [usize; 2], delta: egui::Vec2) {
        let size = self.image_rect(area, tex).size();
        self.center = [self.center[0] - delta.x / size.x.max(1.0), self.center[1] - delta.y / size.y.max(1.0)];
        self.clamp(area, tex);
    }

    /// The zoom at which one pixel of the photo is one pixel of the screen.
    pub fn actual_zoom(&self, area: egui::Rect, tex: [usize; 2], pixels_per_point: f32) -> f32 {
        1.0 / (Self::fit_scale(area, tex) * pixels_per_point.max(0.1)).max(1e-6)
    }

    /// 100 %, about the view's middle.
    pub fn set_actual(&mut self, area: egui::Rect, tex: [usize; 2], pixels_per_point: f32) {
        self.zoom = self.actual_zoom(area, tex, pixels_per_point).min(MAX_ZOOM);
        self.clamp(area, tex);
    }
}

/// The size of the chart view.
const VIEW_SIZE: egui::Vec2 = egui::vec2(640.0, 430.0);

/// `cameraProfile.zoom {mode: fit | actual | in | out, slot?}`: the chart view's zoom (in / out
/// about the middle).
pub fn zoom(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let Some(Dialog::ChartProfile { opts }) = &app.ui.dialog else { return Err("the camera profile dialog isn't open".into()) };
    let slot = p.get("slot").and_then(Value::as_u64).map_or(opts.slot, |s| s.min(1) as usize);
    let tex = app
        .chart_profile
        .shots
        .get(slot)
        .and_then(|s| s.as_ref())
        .and_then(|s| s.texture.as_ref())
        .map(|t| t.size())
        .ok_or("the chart photo isn't read yet")?;
    let area = egui::Rect::from_min_size(egui::Pos2::ZERO, VIEW_SIZE);
    let ppp = app.tasks.repaint.as_ref().map_or(1.0, |c| c.pixels_per_point());
    let view = app.chart_profile.views.get_mut(slot).ok_or("no such chart slot")?;
    match p.get("mode").and_then(Value::as_str).unwrap_or("fit") {
        "fit" => *view = View::default(),
        "actual" => view.set_actual(area, tex, ppp),
        "in" => view.zoom_about(area, tex, 2.0, area.center()),
        "out" => view.zoom_about(area, tex, 0.5, area.center()),
        other => return Err(format!("unknown zoom `{other}` (fit, actual, in or out)")),
    }
    Ok(json!({"zoom": view.zoom, "center": view.center}))
}

/// The picture: the developed photo with the patch grid and draggable corners; wheel zooms, right
/// / middle (or Space +) drag pans, double-click fits; a magnifier shows the corner being placed.
fn picture(app: &mut LccApp, ui: &mut egui::Ui, opts: &mut ChartProfileDialog) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (area, response) = ui.allocate_exact_size(VIEW_SIZE, egui::Sense::click_and_drag());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 4.0, t.canvas);
    crate::widgets::register(ui.ctx(), "chartProfile.picture", area);
    let slot = opts.slot.min(1);
    let Some(shot) = app.chart_profile.shots.get(slot).and_then(|s| s.as_ref()) else { return };
    let (Some(tex), Some(prep)) = (shot.texture.clone(), shot.prepared.clone()) else {
        let text = shot.error.clone().unwrap_or_else(|| crate::i18n::tr_format!("Reading {name}…", name = file_name(&shot.path)));
        painter.text(area.center(), egui::Align2::CENTER_CENTER, text, t.font(13.0), t.text);
        return;
    };
    let size = tex.size();
    // zoom and pan
    let Some(view) = app.chart_profile.views.get_mut(slot) else { return };
    if response.hovered() {
        let (pinch, smooth, at) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta.y, i.pointer.hover_pos()));
        let speed = ui.ctx().options(|o| o.input_options.scroll_zoom_speed);
        let factor = pinch * (speed * smooth).exp();
        if factor != 1.0
            && let Some(at) = at
        {
            view.zoom_about(area, size, factor, at);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
    if response.double_clicked() {
        *view = View::default();
    }
    let space = ui.input(|i| i.key_down(egui::Key::Space));
    let panning = response.dragged_by(egui::PointerButton::Secondary)
        || response.dragged_by(egui::PointerButton::Middle)
        || (space && response.dragged_by(egui::PointerButton::Primary) && app.chart_profile.drag.is_none());
    if panning {
        view.pan(area, size, response.drag_delta());
    }
    let img = view.image_rect(area, size);
    painter.image(tex.id(), img, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    // frame (normalised) ↔ screen, through the photo's orientation
    let to_screen = |p: [f64; 2]| {
        let d = prep.to_display(p);
        egui::pos2(img.min.x + d[0] as f32 * img.width(), img.min.y + d[1] as f32 * img.height())
    };
    let from_screen = |q: egui::Pos2| {
        prep.from_display([f64::from((q.x - img.min.x) / img.width().max(1.0)), f64::from((q.y - img.min.y) / img.height().max(1.0))])
    };
    // dragging a corner (left button, without Space)
    if response.drag_started_by(egui::PointerButton::Primary)
        && !space
        && let (Some(at), Some(c)) = (response.interact_pointer_pos(), opts.corners_at(slot))
    {
        app.chart_profile.drag = c
            .iter()
            .enumerate()
            .map(|(i, p)| (i, to_screen(*p).distance(at)))
            .filter(|(_, d)| *d < 16.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i);
    }
    if let (Some(i), Some(at)) = (app.chart_profile.drag, response.interact_pointer_pos())
        && response.dragged_by(egui::PointerButton::Primary)
        && let Some(Some(c)) = opts.corners.get_mut(slot)
        && let Some(corner) = c.get_mut(i)
    {
        *corner = from_screen(at).map(|v| v.clamp(0.0, 1.0));
        app.chart_profile.report = None;
    }
    if response.drag_stopped() {
        app.chart_profile.drag = None;
    }
    let Some(corners) = opts.corners_at(slot) else {
        painter.text(
            area.center_bottom() - egui::vec2(0.0, 14.0),
            egui::Align2::CENTER_CENTER,
            crate::i18n::tr("No color chart found: use Find Chart, or place the corners"),
            t.font(12.0),
            t.text,
        );
        return;
    };
    // the patch grid: the squares sampled, A01 marked
    let Some(px) = cp::corners_from_normalized(&prep.frame, corners) else { return };
    let chart = opts.chart(slot);
    let turns = opts.turns_at(slot) % 4;
    let layout = if turns % 2 == 1 { camprof::reference::Layout { columns: chart.layout.rows, rows: chart.layout.columns } } else { chart.layout };
    let (fw, fh) = (prep.frame.width.max(1) as f64, prep.frame.height.max(1) as f64);
    let extent = px.patch_extent(layout, camprof::SAMPLE_FRACTION);
    let green = egui::Color32::from_rgb(80, 220, 120);
    for i in 0..layout.patches() {
        let c = px.patch_centre(layout, i);
        let a = to_screen([(c[0] - extent[0]) / fw, (c[1] - extent[1]) / fh]);
        let b = to_screen([(c[0] + extent[0]) / fw, (c[1] + extent[1]) / fh]);
        painter.rect_stroke(egui::Rect::from_two_pos(a, b), 0.0, egui::Stroke::new(1.5, green), egui::StrokeKind::Middle);
        let (fr, fc) = (i / layout.columns.max(1), i % layout.columns.max(1));
        if camprof::reference::chart_cell(turns, fr, fc, chart.layout.rows, chart.layout.columns) == (0, 0) {
            painter.text(to_screen([c[0] / fw, c[1] / fh]), egui::Align2::CENTER_CENTER, "A01", t.semibold(11.0), egui::Color32::WHITE);
        }
    }
    let ring = corners.map(to_screen);
    for (i, (&p, &next)) in ring.iter().zip(ring.iter().cycle().skip(1)).enumerate() {
        painter.line_segment([p, next], egui::Stroke::new(1.0, green.gamma_multiply(0.6)));
        painter.circle(p, 6.0, egui::Color32::from_black_alpha(140), egui::Stroke::new(2.0, egui::Color32::WHITE));
        crate::widgets::register(ui.ctx(), format!("chartProfile.corner{i}"), egui::Rect::from_center_size(p, egui::vec2(12.0, 12.0)));
    }
    // the magnifier: the corner being dragged, else one the pointer rests on
    let hovered = ui.input(|i| i.pointer.hover_pos()).and_then(|at| ring.iter().position(|p| p.distance(at) < 10.0));
    if let Some(&at) = app.chart_profile.drag.or(hovered).and_then(|i| ring.get(i)) {
        magnifier(&painter, area, img, &tex, at, &t);
    }
}

/// A 4× close-up of the photo around `at`, in the view's corner away from it.
fn magnifier(painter: &egui::Painter, area: egui::Rect, img: egui::Rect, tex: &egui::TextureHandle, at: egui::Pos2, t: &crate::theme::Tokens) {
    const SIDE: f32 = 150.0;
    const POWER: f32 = 4.0;
    let left = at.x > area.center().x;
    let top = at.y > area.center().y;
    let min = egui::pos2(if left { area.min.x + 8.0 } else { area.max.x - SIDE - 8.0 }, if top { area.min.y + 8.0 } else { area.max.y - SIDE - 8.0 });
    let rect = egui::Rect::from_min_size(min, egui::vec2(SIDE, SIDE));
    let centre = (at - img.min) / img.size();
    let half = egui::vec2(SIDE / 2.0 / (img.width() * POWER).max(1.0), SIDE / 2.0 / (img.height() * POWER).max(1.0));
    let uv = egui::Rect::from_min_max(egui::pos2(centre.x - half.x, centre.y - half.y), egui::pos2(centre.x + half.x, centre.y + half.y));
    painter.rect_filled(rect, 3.0, t.canvas);
    painter.image(tex.id(), rect, uv, egui::Color32::WHITE);
    let c = rect.center();
    let cross = egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 220, 120));
    painter.line_segment([egui::pos2(rect.min.x, c.y), egui::pos2(rect.max.x, c.y)], cross);
    painter.line_segment([egui::pos2(c.x, rect.min.y), egui::pos2(c.x, rect.max.y)], cross);
    painter.rect_stroke(rect, 3.0, egui::Stroke::new(1.5, egui::Color32::WHITE), egui::StrokeKind::Inside);
    crate::widgets::register(painter.ctx(), "chartProfile.magnifier", rect);
}

/// One light's per-patch report: the chart (reference above, profile below) and each patch's ΔE.
fn report_grid(ui: &mut egui::Ui, cal: &Value) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let patches = cal["patches"].as_array().cloned().unwrap_or_default();
    let cell = egui::vec2(40.0, 52.0);
    egui::Grid::new("chartProfileReport").spacing(egui::vec2(3.0, 3.0)).show(ui, |ui| {
        for (i, p) in patches.iter().enumerate() {
            let (rect, resp) = ui.allocate_exact_size(cell, egui::Sense::hover());
            let sw = egui::Rect::from_min_size(rect.min, egui::vec2(cell.x, 32.0));
            let (top, bottom) = (
                egui::Rect::from_min_max(sw.min, egui::pos2(sw.max.x, sw.center().y)),
                egui::Rect::from_min_max(egui::pos2(sw.min.x, sw.center().y), sw.max),
            );
            ui.painter().rect_filled(top, 0.0, swatch(&p["referenceLab"]));
            ui.painter().rect_filled(bottom, 0.0, swatch(&p["renderedLab"]));
            let de = p["deltaE"].as_f64().unwrap_or(f64::NAN);
            let colour = if de < 1.0 {
                t.text_dim
            } else if de < 2.5 {
                t.text
            } else {
                t.caution
            };
            ui.painter().text(egui::pos2(rect.center().x, rect.max.y - 9.0), egui::Align2::CENTER_CENTER, format!("{de:.2}"), t.font(10.5), colour);
            resp.on_hover_text(format!("{} {}: ΔE {de:.2}", p["id"].as_str().unwrap_or(""), p["name"].as_str().unwrap_or("")));
            if (i + 1) % 6 == 0 {
                ui.end_row();
            }
        }
    });
}

/// The in-dialog chooser of the second chart photo: catalog photos of the same camera (or all), the
/// ones with a chart (found in the background) first.
#[derive(Default)]
pub struct Picker {
    pub all_cameras: bool,
    pub search: String,
}

/// Activity label of the background chart search.
const SCANNING: &str = "Looking for color charts";
/// At most this many photos are looked through for charts.
const SCAN_LIMIT: usize = 300;

/// A catalog photo the picker offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub id: PhotoId,
    pub path: String,
    pub file_name: String,
    pub captured: Option<String>,
    pub camera: String,
    /// `None`: not looked at yet; `Some(None)`: no chart.
    pub scan: Option<Option<cp::ChartScan>>,
}

/// The photos the second-chart picker offers, best first: raw files (not the first chart), of the
/// first chart's camera unless `all_cameras`, matching `search`; those with a chart first, among
/// them those under another light than `first_light` (an illuminant name) first, then the ones not
/// looked at yet, then those without a chart.
pub fn candidates(app: &LccApp, all_cameras: bool, search: &str, first_light: Option<&str>) -> Vec<Candidate> {
    let first = app.chart_profile.shots[0].as_ref();
    let first_id = first.and_then(|s| s.photo);
    let camera = first_id.and_then(|id| app.session.catalog.photo(id)).map(|p| p.meta.camera.clone()).unwrap_or_default();
    let needle = search.trim().to_lowercase();
    let mut out: Vec<Candidate> = app
        .session
        .catalog
        .photos()
        .filter(|p| p.kind == lcc_catalog::MediaKind::Raw && Some(p.id) != first_id)
        .filter(|p| all_cameras || camera.is_empty() || p.meta.camera == camera)
        .filter(|p| needle.is_empty() || p.file_name.to_lowercase().contains(&needle))
        .filter_map(|p| match &p.source {
            lcc_catalog::Source::File { path } => Some(Candidate {
                id: p.id,
                path: path.clone(),
                file_name: p.file_name.clone(),
                captured: p.captured.clone(),
                camera: p.meta.camera.clone(),
                scan: app.chart_profile.scans.get(&p.id).copied(),
            }),
            _ => None,
        })
        .collect();
    let rank = |c: &Candidate| match c.scan {
        Some(Some(s)) if s.light.map(|l| l.illuminant) != first_light || first_light.is_none() => 0,
        Some(Some(_)) => 1,
        None => 2,
        Some(None) => 3,
    };
    out.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| b.captured.cmp(&a.captured)).then_with(|| a.file_name.cmp(&b.file_name)));
    out
}

/// Look for charts in the candidates not looked at yet, on a worker thread (one activity row).
fn scan_candidates(app: &mut LccApp, list: &[Candidate]) {
    if app.tasks.is_running(SCANNING) {
        return;
    }
    let todo: Vec<(PhotoId, String)> = list.iter().filter(|c| c.scan.is_none()).take(SCAN_LIMIT).map(|c| (c.id, c.path.clone())).collect();
    if todo.is_empty() {
        return;
    }
    let _ = crate::tasks::spawn(
        app,
        SCANNING,
        Some("cameraProfile"),
        move || todo.into_iter().map(|(id, path)| (id, cp::scan_path(&path))).collect::<Vec<_>>(),
        |app, _, found| {
            for (id, scan) in found {
                app.chart_profile.scans.insert(id, scan);
            }
        },
    );
}

/// `cameraProfile.pickSecond`: open the picker (`{close: true}` closes it).
pub fn pick_second(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    if !matches!(app.ui.dialog, Some(Dialog::ChartProfile { .. })) {
        return Err("the camera profile dialog isn't open".into());
    }
    if p.get("close").and_then(Value::as_bool) == Some(true) {
        app.chart_profile.picker = None;
        return Ok(Value::Null);
    }
    app.chart_profile.picker = Some(Picker { all_cameras: p.get("allCameras").and_then(Value::as_bool).unwrap_or(false), search: String::new() });
    let list = candidates(app, app.chart_profile.picker.as_ref().is_some_and(|p| p.all_cameras), "", None);
    scan_candidates(app, &list);
    Ok(json!({"candidates": list.len()}))
}

/// The picker, in place of the chart view.
fn picker(app: &mut LccApp, ui: &mut egui::Ui, opts: &mut ChartProfileDialog) {
    let t = crate::theme::Tokens::get(ui.ctx());
    // (as wide as the chart view it stands in for: the dialog keeps its size)
    ui.set_min_width(VIEW_SIZE.x);
    ui.set_max_width(VIEW_SIZE.x);
    let Some(state) = app.chart_profile.picker.as_mut() else { return };
    let (mut all, mut search) = (state.all_cameras, std::mem::take(&mut state.search));
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(crate::i18n::tr("Second chart: a photo of the same chart under another light")).color(t.text_label));
    });
    ui.horizontal(|ui| {
        crate::text_field::TextField::singleline("chartProfilePickerSearch", &mut search)
            .hint(crate::i18n::tr("File name"))
            .label(crate::i18n::tr("Search"))
            .width(200.0)
            .show(ui);
        let r = ui.checkbox(&mut all, crate::i18n::tr("Show all cameras"));
        crate::widgets::register(ui.ctx(), "check:chartProfileAllCameras", r.rect);
        let r = ui
            .button(crate::i18n::tr("Browse for File…"))
            .on_hover_text(crate::i18n::tr("A chart photo that isn't in the catalog: it is used for the profile only and is not imported."));
        crate::widgets::register(ui.ctx(), "button:chartProfileBrowse", r.rect);
        if r.clicked() {
            app.chart_profile.error = browse_second(app).err();
        }
        let r = ui.button(crate::i18n::tr("Cancel"));
        crate::widgets::register(ui.ctx(), "button:chartProfilePickerCancel", r.rect);
        if r.clicked() {
            app.chart_profile.picker = None;
        }
    });
    if all {
        ui.label(egui::RichText::new(crate::i18n::tr("A camera profile is for one camera: use a photo from the same camera body.")).color(t.caution));
    }
    let Some(state) = app.chart_profile.picker.as_mut() else { return };
    let rescan = state.all_cameras != all;
    state.all_cameras = all;
    state.search = search.clone();
    // (photos whose greys measure another light than the first chart's first)
    let first_light = estimate(app, opts, 0).map(|e| e.illuminant.to_string());
    let list = candidates(app, all, &search, first_light.as_deref());
    if rescan {
        scan_candidates(app, &list);
    }
    if app.tasks.is_running(SCANNING) {
        ui.label(egui::RichText::new(crate::i18n::tr("Looking for color charts…")).color(t.text_dim));
    }
    let mut chosen = None;
    egui::ScrollArea::vertical().id_salt("chartProfilePicker").max_height(VIEW_SIZE.y - 60.0).max_width(VIEW_SIZE.x).auto_shrink([true, false]).show(
        ui,
        |ui| {
            if list.is_empty() {
                ui.label(egui::RichText::new(crate::i18n::tr("No other raw photos from this camera in the catalog.")).color(t.text_dim));
            }
            egui::Grid::new("chartProfilePickerGrid").spacing(egui::vec2(8.0, 8.0)).show(ui, |ui| {
                for (i, c) in list.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(150.0, 150.0), egui::Sense::click());
                    crate::widgets::register(ui.ctx(), format!("chartProfilePick:{}", c.id.0), rect);
                    crate::panels::grid::request_thumb(app, c.id, 256, 20);
                    let thumb = egui::Rect::from_min_size(rect.min, egui::vec2(150.0, 96.0));
                    ui.painter().rect_filled(thumb, 3.0, t.canvas);
                    if let Some(tex) = app.renderer.thumb(c.id) {
                        let [w, h] = tex.size;
                        let s = (thumb.width() / w.max(1) as f32).min(thumb.height() / h.max(1) as f32);
                        let r = egui::Rect::from_center_size(thumb.center(), egui::vec2(w as f32 * s, h as f32 * s));
                        ui.painter().image(
                            tex.tex.id(),
                            r,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    }
                    if resp.hovered() {
                        ui.painter().rect_stroke(thumb, 3.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
                    }
                    let what = match c.scan {
                        Some(Some(s)) => match s.light {
                            Some(l) => crate::i18n::tr_format!("Chart · greys about {cct:.0} K", cct = (l.cct / 100.0).round() * 100.0),
                            None => crate::i18n::tr("Chart · light unknown").to_string(),
                        },
                        Some(None) => crate::i18n::tr("No chart found").to_string(),
                        None => String::new(),
                    };
                    let date = c.captured.as_deref().map(|d| d.get(..10).unwrap_or(d)).unwrap_or("");
                    let mut y = thumb.max.y + 4.0;
                    for (line, colour) in [(c.file_name.as_str(), t.text), (date, t.text_dim), (what.as_str(), t.text_dim)] {
                        ui.painter().text(egui::pos2(rect.min.x, y), egui::Align2::LEFT_TOP, line, t.font(11.0), colour);
                        y += 15.0;
                    }
                    if all && !c.camera.is_empty() {
                        resp.clone().on_hover_text(c.camera.as_str());
                    }
                    if resp.clicked() {
                        chosen = Some(c.clone());
                    }
                    if (i + 1) % 4 == 0 {
                        ui.end_row();
                    }
                }
            });
        },
    );
    if let Some(c) = chosen {
        opts.corners[1] = None;
        opts.slot = 1;
        app.chart_profile.picker = None;
        app.chart_profile.error = load(app, 1, Some(c.id), c.path).err();
    }
}

/// Browse for a second chart photo that isn't in the library (not imported).
fn browse_second(app: &mut LccApp) -> Result<Value, String> {
    let request = crate::pick::PickRequest::file(crate::i18n::tr("Second Color Chart Photo"), crate::i18n::tr("Raw photos"), RAW_EXTENSIONS);
    match crate::pick::ask(app, "cameraProfile.second", &json!({}), "path", request, |svc| svc.pick_files.as_mut().map(|f| f())) {
        crate::pick::Picked::Now(paths) => match paths.first() {
            Some(path) => choose_second(app, &json!({"path": path})),
            None => Ok(Value::Null),
        },
        _ => Ok(Value::Null),
    }
}

/// The profile name the dialog suggests ([`cp::default_name`]): the camera and body, the light(s),
/// the mode and the first chart's date.
fn suggested_name(app: &LccApp, opts: &ChartProfileDialog) -> Option<String> {
    let first = app.chart_profile.shots[0].as_ref()?.prepared.clone()?;
    let mut lights = vec![resolved_light(app, opts, 0)?];
    if app.chart_profile.shots[1].is_some() {
        lights.push(resolved_light(app, opts, 1)?);
    }
    let lights: Vec<&str> = lights.iter().map(String::as_str).collect();
    Some(cp::default_name(&first, &lights, opts.natural))
}

/// One photo's light: the menu, and a note when the camera doesn't say.
fn light_row(app: &mut LccApp, ui: &mut egui::Ui, opts: &mut ChartProfileDialog, slot: usize) {
    let t = crate::theme::Tokens::get(ui.ctx());
    if !app.chart_profile.shots.get(slot).and_then(Option::as_ref).is_some_and(|s| s.prepared.is_some()) {
        return;
    }
    let Some(light) = opts.lights.get_mut(slot) else { return };
    if light_combo(ui, &format!("chartProfileLight{slot}"), light) {
        app.chart_profile.report = None;
    }
    // what the greys measure, for information (never chosen from: a camera's colour isn't a light meter)
    if let Some(e) = estimate(app, opts, slot) {
        ui.label(
            egui::RichText::new(crate::i18n::tr_format!("The chart's greys measure about {cct:.0} K", cct = (e.cct / 100.0).round() * 100.0))
                .color(t.text_dim),
        );
    }
}

/// Both charts set to the same light: the build would be refused.
pub fn same_light(app: &LccApp, opts: &ChartProfileDialog) -> Option<String> {
    (app.chart_profile.shots[1].is_some() && opts.lights[0] == opts.lights[1]).then(|| {
        let other = LIGHTS.iter().find(|(k, _)| *k != opts.lights[0]).map_or("", |(k, _)| k);
        crate::i18n::tr_format!(
            "Both charts are set to {light}; set the second to {other}.",
            light = light_label(&opts.lights[0]),
            other = light_label(other)
        )
    })
}

/// The dialog body.
pub fn body(app: &mut LccApp, ui: &mut egui::Ui, opts: &mut ChartProfileDialog) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let has_second = app.chart_profile.shots[1].is_some();
    if !opts.name_edited
        && let Some(name) = suggested_name(app, opts)
    {
        opts.name = name;
    }
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            if app.chart_profile.picker.is_some() {
                picker(app, ui, opts);
                return;
            }
            ui.horizontal(|ui| {
                if has_second {
                    for (i, label) in [(0, "First Chart"), (1, "Second Chart")] {
                        if crate::widgets::text_button(ui, &format!("chartProfileSlot-{i}"), label, opts.slot == i).clicked() {
                            opts.slot = i;
                        }
                    }
                }
                ui.label(egui::RichText::new(crate::i18n::tr("Wheel: zoom · right-drag or Space-drag: move · double-click: fit")).color(t.text_dim));
            });
            opts.slot = if has_second { opts.slot.min(1) } else { 0 };
            picture(app, ui, opts);
            let slot = opts.slot;
            if let Some(shot) = app.chart_profile.shots.get(slot).and_then(Option::as_ref) {
                let mut caption = file_name(&shot.path);
                if let Some(prep) = &shot.prepared {
                    caption = format!("{caption} · {}", prep.camera_model);
                }
                ui.label(egui::RichText::new(caption).color(t.text_dim));
            }
            ui.horizontal(|ui| {
                for (id, label, command) in [
                    ("chartProfileFind", "Find Chart", "cameraProfile.find"),
                    ("chartProfileAlign", "Align to Chart", "cameraProfile.align"),
                    ("chartProfileTurn", "Turn A01", "cameraProfile.turn"),
                ] {
                    let r = ui.button(crate::i18n::tr(label));
                    crate::widgets::register(ui.ctx(), format!("button:{id}"), r.rect);
                    if r.clicked() {
                        app.chart_profile.error = adjust_in(&mut app.chart_profile, opts, command, slot).err();
                    }
                }
                ui.separator();
                for (id, label, mode) in [("chartProfileFit", "Fit", "fit"), ("chartProfile100", "100%", "actual")] {
                    let r = ui.button(crate::i18n::tr(label));
                    crate::widgets::register(ui.ctx(), format!("button:{id}"), r.rect);
                    if r.clicked()
                        && let Some(tex) = app.chart_profile.shots.get(slot).and_then(Option::as_ref).and_then(|s| s.texture.as_ref()).map(|t| t.size())
                        && let Some(view) = app.chart_profile.views.get_mut(slot)
                    {
                        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, VIEW_SIZE);
                        if mode == "fit" {
                            *view = View::default();
                        } else {
                            view.set_actual(area, tex, ui.ctx().pixels_per_point());
                        }
                    }
                }
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.set_width(260.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Profile name")).color(t.text_label));
            let before = opts.name.clone();
            crate::text_field::TextField::singleline("chartProfileName", &mut opts.name).label(crate::i18n::tr("Profile name")).width(250.0).show(ui);
            if opts.name != before {
                opts.name_edited = true;
            }
            ui.label(egui::RichText::new(crate::i18n::tr(if has_second { "Light of the first chart" } else { "Light" })).color(t.text_label));
            light_row(app, ui, opts, 0);
            ui.label(egui::RichText::new(crate::i18n::tr("Rendering")).color(t.text_label));
            for (natural, label, id) in [(true, "Photographic", "chartProfileRendering-photographic"), (false, "Reproduction", "chartProfileRendering-reproduction")] {
                let r = ui.radio(opts.natural == natural, crate::i18n::tr(label));
                crate::widgets::register(ui.ctx(), format!("radio:{id}"), r.rect);
                if r.clicked() && opts.natural != natural {
                    opts.natural = natural;
                    app.chart_profile.report = None;
                }
            }
            ui.label(
                egui::RichText::new(crate::i18n::tr(if opts.natural {
                    "Colors match the chart; each keeps the lightness the camera recorded. For people and everyday photos."
                } else {
                    "Everything matches the chart exactly, including how light or dark each color is. For artwork, products and color-critical copy work."
                }))
                .color(t.text_dim),
            );
            let r = ui
                .checkbox(&mut opts.even_light, crate::i18n::tr("Even out the light on the chart"))
                .on_hover_text(crate::i18n::tr(
                    "Corrects light that falls off across the chart and glare on its patches. Kept only when it makes the grey patches more even.",
                ));
            crate::widgets::register(ui.ctx(), "check:chartProfileEvenLight", r.rect);
            if r.changed() {
                app.chart_profile.report = None;
            }
            ui.separator();
            ui.label(egui::RichText::new(crate::i18n::tr("Second chart (other light)")).color(t.text_label));
            match app.chart_profile.shots[1].as_ref().map(|s| file_name(&s.path)) {
                None => {
                    ui.label(
                        egui::RichText::new(crate::i18n::tr(
                            "Optional: a chart photo under another light, such as tungsten, makes a dual-illuminant profile.",
                        ))
                        .color(t.text_dim),
                    );
                    let r = ui.button(crate::i18n::tr("Add Second Chart…"));
                    crate::widgets::register(ui.ctx(), "button:chartProfileSecond", r.rect);
                    if r.clicked() {
                        app.chart_profile.error = pick_second(app, &json!({})).err();
                    }
                }
                Some(name) => {
                    ui.label(egui::RichText::new(name).color(t.text));
                    light_row(app, ui, opts, 1);
                    if let Some(e) = same_light(app, opts) {
                        ui.label(egui::RichText::new(e).color(t.caution));
                    }
                    ui.horizontal(|ui| {
                        let r = ui.button(crate::i18n::tr("Change…"));
                        crate::widgets::register(ui.ctx(), "button:chartProfileChangeSecond", r.rect);
                        if r.clicked() {
                            app.chart_profile.error = pick_second(app, &json!({})).err();
                        }
                        let r = ui.button(crate::i18n::tr("Remove Second Chart"));
                        crate::widgets::register(ui.ctx(), "button:chartProfileRemoveSecond", r.rect);
                        if r.clicked() {
                            app.chart_profile.shots[1] = None;
                            app.chart_profile.report = None;
                            opts.corners[1] = None;
                            opts.slot = 0;
                            opts.report_tab = 0;
                        }
                    });
                }
            }
            ui.separator();
            if let (Some(a), Some(b)) = (
                app.chart_profile.shots[0].as_ref().and_then(|s| s.prepared.clone()),
                app.chart_profile.shots[1].as_ref().and_then(|s| s.prepared.clone()),
            ) && let Some((x, y)) = cp::bodies_differ(&a, &b)
            {
                ui.label(egui::RichText::new(crate::i18n::tr_format!("These charts come from two different camera bodies ({x}, {y})", x = x, y = y)).color(t.caution));
            }
            clash_panel(app, ui);
            if app.tasks.is_running(BUILDING) {
                ui.label(egui::RichText::new(crate::i18n::tr("Building the profile…")).color(t.text_dim));
            }
            if let Some(e) = &app.chart_profile.error {
                ui.label(egui::RichText::new(e.as_str()).color(t.caution));
            }
            if let Some(report) = app.chart_profile.report.clone() {
                ui.label(
                    egui::RichText::new(crate::i18n::tr_format!("“{name}” added to Camera Profiles", name = report.get("name").and_then(Value::as_str).unwrap_or("")))
                        .color(t.text),
                );
                let cals = report.get("calibrations").and_then(Value::as_array).cloned().unwrap_or_default();
                if cals.len() > 1 {
                    ui.horizontal(|ui| {
                        for (i, c) in cals.iter().enumerate() {
                            let label = light_word(c["illuminant"].as_str().unwrap_or(""));
                            if crate::widgets::text_button(ui, &format!("labelSet-chartProfileReport-{i}"), &label, opts.report_tab == i).clicked() {
                                opts.report_tab = i;
                            }
                        }
                    });
                }
                let tab = opts.report_tab.min(cals.len().saturating_sub(1));
                if let Some(cal) = cals.get(tab) {
                    ui.label(
                        egui::RichText::new(crate::i18n::tr_format!(
                            "{light}: mean ΔE {mean:.2}, largest {max:.2} ({n} patches)",
                            light = light_word(cal["illuminant"].as_str().unwrap_or("")),
                            mean = cal["meanDeltaE"].as_f64().unwrap_or(0.0),
                            max = cal["maxDeltaE"].as_f64().unwrap_or(0.0),
                            n = cal["patchesUsed"].as_u64().unwrap_or(0)
                        ))
                        .color(t.text),
                    );
                    report_grid(ui, cal);
                    ui.label(egui::RichText::new(crate::i18n::tr("Above: the chart. Below: the profile. ΔE is CIEDE2000.")).color(t.text_dim));
                }
            }
        });
    });
}

#[cfg(test)]
mod view_tests {
    use super::*;

    const AREA: egui::Rect = egui::Rect { min: egui::pos2(0.0, 0.0), max: egui::pos2(640.0, 430.0) };
    const TEX: [usize; 2] = [3000, 2000];

    #[test]
    fn zooming_keeps_the_point_under_the_pointer() {
        let mut v = View::default();
        let fit = v.image_rect(AREA, TEX);
        assert!((fit.width() - 640.0).abs() < 0.01 || (fit.height() - 430.0).abs() < 0.01, "fits: {fit:?}");
        let at = egui::pos2(500.0, 120.0);
        let p = (at - fit.min) / fit.size();
        v.zoom_about(AREA, TEX, 3.0, at);
        assert!((v.zoom - 3.0).abs() < 1e-4);
        let r = v.image_rect(AREA, TEX);
        let q = (at - r.min) / r.size();
        assert!((p - q).length() < 1e-3, "{p:?} vs {q:?}");
        // never past fitting, never past MAX_ZOOM, never NaN
        v.zoom_about(AREA, TEX, 1e-6, at);
        assert!(v.zoom <= 1.0 + 1e-4);
        v.zoom_about(AREA, TEX, 1e9, at);
        assert!((v.zoom - MAX_ZOOM).abs() < 1e-3);
        v.zoom_about(AREA, TEX, f32::NAN, at);
        assert!(v.zoom.is_finite() && v.center.iter().all(|c| c.is_finite()));
    }

    #[test]
    fn panning_stays_on_the_photo_and_100_percent_is_one_pixel_per_pixel() {
        let mut v = View { zoom: 4.0, center: [0.5, 0.5] };
        v.pan(AREA, TEX, egui::vec2(-1e6, -1e6));
        let r = v.image_rect(AREA, TEX);
        assert!((r.max.x - AREA.max.x).abs() < 0.5 && (r.max.y - AREA.max.y).abs() < 0.5, "pinned to the photo's edge: {r:?}");
        v.set_actual(AREA, TEX, 2.0);
        let r = v.image_rect(AREA, TEX);
        assert!((r.width() * 2.0 - TEX[0] as f32).abs() < 0.5, "{r:?}");
        // a photo smaller than the view at fit is centred
        let mut small = View::default();
        small.pan(AREA, TEX, egui::vec2(50.0, 50.0));
        assert_eq!(small.center, [0.5, 0.5]);
    }
}

#[cfg(test)]
mod remember_tests {
    use super::*;

    /// Client note 13: the camera profile creator opens with the lights and look chosen last time;
    /// anything unreadable is the defaults.
    #[test]
    fn the_dialog_opens_with_the_last_choices() {
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        assert_eq!(remembered(&app), ChartProfileDialog::default());
        app.run("dialog.remember", json!({"dialog": REMEMBER, "choices": {"lights": ["A", "D55"], "natural": false}})).unwrap();
        let o = remembered(&app);
        assert_eq!((o.lights.clone(), o.natural), (["A".to_string(), "D55".to_string()], false));
        app.run("dialog.remember", json!({"dialog": REMEMBER, "choices": {"lights": ["X", 3], "natural": "yes"}})).unwrap();
        assert_eq!(remembered(&app), ChartProfileDialog::default());
    }
}
