//! Control surfaces in the app: the [`lightcraft_surface`] engine wired to the session, the
//! `surface.*` control-channel methods (pairing, subscribe, input, layout, icons), the pushes to
//! subscribed surfaces and the `surface.*` UI commands. The protocol is documented in
//! `docs/control-surfaces.md`; the editor is Settings ▸ Controls (`panels::controls`); the
//! on-screen readout is `panels::surface_osd`.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use lightcraft_catalog::PhotoId;
use lightcraft_surface as ls;
use ls::layout::Host;
use ls::{Binding, Effect, TargetKind};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::control::{ControlRequest, Outcome};
use crate::state::{BeforeAfter, RightPanel, ViewMode, Zoom};

/// Where a connection's pushed events go, and whether it is still open (the transport clears
/// `open` when the peer goes away).
#[derive(Clone)]
pub struct EventSink {
    pub tx: Sender<Value>,
    pub open: Arc<AtomicBool>,
}

impl EventSink {
    fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }
}

/// Load and save `control-surfaces.json` (the desktop host stores it in the settings folder).
pub trait SurfaceStore {
    /// The saved text, `None` when there is none yet.
    fn load(&self) -> Result<Option<String>, String>;
    fn save(&self, text: &str) -> Result<(), String>;
}

struct Subscriber {
    conn: u64,
    client: String,
    device: &'static ls::DeviceDef,
    sink: EventSink,
    last: Option<Value>,
    /// Values the surface shows outside its layout (actions from its own app, `surface.watch`).
    watch: Vec<String>,
}

/// The most values one surface may watch.
pub const MAX_WATCH: usize = 128;

/// The editor's selection (Settings ▸ Controls).
#[derive(Clone, Debug, Default)]
pub struct EditorState {
    /// The page being edited (`None` = the profile's base bindings).
    pub page: Option<String>,
    /// The control being edited.
    pub control: Option<String>,
    /// The value whose tuning is shown.
    pub tuning_target: String,
    /// Filter text for the value and command lists.
    pub filter: String,
    /// A message from the last import / export.
    pub message: Option<String>,
    /// The JSON of the selected command binding's parameters, as typed.
    pub params_text: String,
    /// Name typed for a new profile or page.
    pub new_name: String,
    /// The surface shown (`keypad`, `dialpad`, `ring`…; empty: the first).
    pub device: String,
    /// The part of the page shown (`pages`, `feel`, `readout`, `setup`; empty: pages).
    pub section: String,
    /// The Keys and Dials window is open (the drawn device and its editor:
    /// `panels::controls::editor_window`).
    pub window: bool,
    /// Touch to find switched off (it is on by default: while Settings ▸ Controls is open, a
    /// control touched on the device is selected in the editor and does nothing to the photo).
    pub touch_find_off: bool,
    /// Touch to find is listening, and when Settings ▸ Controls was last drawn (egui time, s):
    /// it stops by itself once the page is gone.
    pub touch_find: Option<f64>,
}

/// How long after Settings ▸ Controls was last drawn Touch to find stops listening (s).
const TOUCH_FIND_GRACE: f64 = 0.4;

pub struct SurfaceHost {
    pub runtime: ls::Runtime,
    pub pairing: ls::protocol::Pairing,
    /// Connection → the paired client it authenticated as.
    authed: BTreeMap<u64, String>,
    subscribers: Vec<Subscriber>,
    last_photo: Option<PhotoId>,
    /// The photo a gesture's undo step was opened on, and its label.
    gesture: Option<(PhotoId, String)>,
    /// Settings changed and not yet saved, since (egui time, s).
    dirty_since: Option<f64>,
    loaded: bool,
    /// Why the settings file couldn't be read (shown in Settings ▸ Controls; the file is kept).
    pub load_error: Option<String>,
    /// Why saving failed last.
    pub save_error: Option<String>,
    pub editor: EditorState,
    /// The last TactileBridge import, for the report in Settings ▸ Controls.
    pub last_report: Option<ls::tactilebridge::Report>,
    /// Points → physical pixels last frame (zoom percent of a fitted photo).
    pixels_per_point: f32,
    /// egui time of the last tick (s).
    now: f64,
    /// The port the surface listener runs on.
    pub listening: Option<u16>,
    /// Why it couldn't start, and when to try again (egui time, s).
    pub listen_error: Option<String>,
    listen_retry_at: f64,
    /// The surface plugin's install state (Settings ▸ Controls).
    pub plugin: crate::surface_plugin::PluginState,
}

impl Default for SurfaceHost {
    fn default() -> Self {
        SurfaceHost {
            runtime: ls::Runtime::new(ls::SurfaceSettings::default()),
            pairing: ls::protocol::Pairing::new(&seed()),
            authed: BTreeMap::new(),
            subscribers: Vec::new(),
            last_photo: None,
            gesture: None,
            dirty_since: None,
            loaded: false,
            load_error: None,
            save_error: None,
            editor: EditorState { tuning_target: "light.exposure".into(), ..Default::default() },
            last_report: None,
            pixels_per_point: 1.0,
            now: 0.0,
            listening: None,
            listen_error: None,
            listen_retry_at: 0.0,
            plugin: Default::default(),
        }
    }
}

/// Unpredictable bytes for pairing codes and tokens (std's per-process random hash keys, the
/// time and the process id; no extra dependency).
fn seed() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::new();
    for i in 0..8u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    // (the web build has no process id: std panics there)
    #[cfg(not(target_arch = "wasm32"))]
    out.extend_from_slice(&std::process::id().to_le_bytes());
    out
}

impl SurfaceHost {
    pub fn settings(&self) -> &ls::SurfaceSettings {
        self.runtime.settings()
    }

    /// A dial (or other surface control) is adjusting the photo right now: its gesture's undo
    /// step is open. The canvas hides overlays and tool graphics meanwhile.
    pub fn adjusting(&self) -> bool {
        self.gesture.is_some()
    }

    /// The app clock at the last frame (egui time, s).
    pub fn now(&self) -> f64 {
        self.now
    }

    /// Settings were changed (editor, keys): apply and save soon.
    pub fn changed(&mut self) {
        self.runtime.settings_changed();
        self.dirty_since.get_or_insert(self.now);
    }

    /// Surfaces subscribed right now: (client, device name).
    pub fn connected(&self) -> Vec<(String, &'static str)> {
        self.subscribers.iter().filter(|s| s.sink.is_open()).map(|s| (s.client.clone(), s.device.name)).collect()
    }

    /// Forget a paired client (Settings ▸ Controls): its token stops working, its connections
    /// are told and dropped.
    pub fn unpair(&mut self, client: &str) {
        self.runtime.settings_mut().paired.retain(|c| c.client != client);
        let gone: Vec<u64> = self.authed.iter().filter(|(_, c)| *c == client).map(|(k, _)| *k).collect();
        for conn in gone {
            self.authed.remove(&conn);
        }
        for s in self.subscribers.iter().filter(|s| s.client == client) {
            let _ = s.sink.tx.send(json!({"event": "surface.unpaired"}));
        }
        self.subscribers.retain(|s| s.client != client);
        self.changed();
    }
}

// ------------------------------------------------------------------------------------- values

/// Current value of a target (no active photo, no selected mask: none).
pub fn value(app: &LightcraftApp, target: &str) -> Option<f64> {
    let spec = ls::targets::target_spec(target)?;
    match spec.kind {
        TargetKind::Develop => {
            let d = app.session.develop_of(app.session.active()?)?;
            lightcraft_develop::controls::get(&d, target)
        }
        TargetKind::Mask => {
            let field = ls::targets::mask_field(target)?;
            let d = app.session.develop_of(app.session.active()?)?;
            let id = app.session.active_mask?;
            let m = d.masks.iter().find(|m| m.id == id)?;
            serde_json::to_value(m.adjust).ok()?.get(field)?.as_f64()
        }
        TargetKind::Ui => Some(match target {
            "ui.brushSize" => f64::from(app.ui.brush_size) * 400.0,
            "ui.brushFeather" => f64::from(app.ui.brush_feather),
            "ui.brushFlow" => f64::from(app.ui.brush_flow),
            "ui.removeSize" => f64::from(app.ui.remove_size) * 1000.0,
            "ui.removeFeather" => f64::from(app.ui.remove_feather),
            "ui.removeOpacity" => f64::from(app.ui.remove_opacity),
            "ui.zoom" => zoom_percent(app),
            _ => return None,
        }),
    }
}

/// The loupe's zoom in percent (a fitted photo: what fitting gives).
fn zoom_percent(app: &LightcraftApp) -> f64 {
    match app.ui.zoom {
        Zoom::Percent(p) if p.is_finite() => f64::from(p),
        _ => {
            let w = app.session.active().and_then(|id| app.session.catalog.photo(id)).map(|p| p.width.max(1) as f64);
            match (app.image_rect, w) {
                (Some(r), Some(w)) => (f64::from(r.width()) * f64::from(app.surface.pixels_per_point) / w * 100.0).clamp(5.0, 800.0),
                _ => 100.0,
            }
        }
    }
}

fn set_ui_value(app: &mut LightcraftApp, target: &str, v: f64) {
    let Some(spec) = ls::targets::target_spec(target) else { return };
    let v = spec.clamp(v) as f32;
    match target {
        "ui.brushSize" => app.ui.brush_size = (v / 400.0).clamp(0.002, 0.5),
        "ui.brushFeather" => app.ui.brush_feather = v,
        "ui.brushFlow" => app.ui.brush_flow = v,
        "ui.removeSize" => {
            app.ui.remove_size = (v / 1000.0).clamp(0.001, 0.25);
            if app.session.active_spot.is_some() {
                let _ = app.run("spot.update", json!({"size": app.ui.remove_size}));
            }
        }
        "ui.removeFeather" => app.ui.remove_feather = v,
        "ui.removeOpacity" => app.ui.remove_opacity = v,
        "ui.zoom" => {
            let _ = app.run("view.navigate", json!({"zoom": {"percent": v}}));
        }
        _ => {}
    }
}

// ------------------------------------------------------------------------------------- effects

fn label_of(target: &str) -> String {
    ls::targets::target_spec(target).map_or_else(|| target.to_string(), |s| s.label.to_string())
}

/// Is the session's open interaction the one our gesture opened?
fn ours(app: &LightcraftApp) -> bool {
    match (&app.surface.gesture, &app.session.interaction) {
        (Some((photo, label)), Some(i)) => i.photo == *photo && i.label == *label,
        _ => false,
    }
}

fn begin(app: &mut LightcraftApp, target: &str) {
    let Some(spec) = ls::targets::target_spec(target) else { return };
    if spec.kind == TargetKind::Ui {
        return;
    }
    let Some(photo) = app.session.active() else { return };
    let label = label_of(target);
    if app.run("develop.beginInteraction", json!({"label": label})).is_ok() {
        app.surface.gesture = Some((photo, label));
    }
}

fn end(app: &mut LightcraftApp, apply_to_selected: bool) {
    if ours(app) {
        // Apply to Selected: the end of the gesture carries the change to every selected photo,
        // in the same undo step (the session's Auto Sync does exactly that)
        let before = app.session.auto_sync;
        if apply_to_selected {
            app.session.auto_sync = true;
        }
        let _ = app.run("develop.endInteraction", json!({}));
        app.session.auto_sync = before;
    }
    app.surface.gesture = None;
}

fn adjust(app: &mut LightcraftApp, target: &str, delta: f64) {
    let Some(spec) = ls::targets::target_spec(target) else { return };
    if !delta.is_finite() {
        return;
    }
    if spec.kind != TargetKind::Ui && !ours(app) {
        // something else took over the edit (a mouse drag, a photo change): a new undo step
        begin(app, target);
    }
    match spec.kind {
        TargetKind::Develop => {
            let _ = app.run("develop.adjust", json!({"control": target, "delta": delta}));
        }
        TargetKind::Mask => {
            let (Some(field), Some(v)) = (ls::targets::mask_field(target), value(app, target)) else { return };
            let _ = app.run("mask.adjust", json!({"values": {field: spec.clamp(v + delta)}}));
        }
        TargetKind::Ui => {
            let v = value(app, target).unwrap_or(spec.default);
            set_ui_value(app, target, v + delta);
        }
    }
}

fn reset(app: &mut LightcraftApp, target: &str) {
    let Some(spec) = ls::targets::target_spec(target) else { return };
    match spec.kind {
        TargetKind::Develop => {
            let _ = app.run("develop.resetControl", json!({"control": target}));
        }
        TargetKind::Mask => {
            if let Some(field) = ls::targets::mask_field(target) {
                let _ = app.run("mask.adjust", json!({"values": {field: spec.default}}));
            }
        }
        TargetKind::Ui => set_ui_value(app, target, spec.default),
    }
}

/// Run the engine's effects.
pub fn run_effects(app: &mut LightcraftApp, effects: Vec<Effect>) {
    for e in effects {
        match e {
            Effect::Command { command, params } => {
                let params = if params.is_null() { json!({}) } else { params };
                if let Err(err) = app.run(&command, params) {
                    log::info!("surface: {command}: {err}");
                }
            }
            Effect::Begin { target } => begin(app, &target),
            Effect::Adjust { target, delta } => adjust(app, &target, delta),
            Effect::End { apply_to_selected, .. } => end(app, apply_to_selected),
            Effect::Reset { target } => reset(app, &target),
            Effect::Changed { .. } => {}
            Effect::Save => {
                let now = app.surface.now;
                app.surface.dirty_since.get_or_insert(now);
            }
        }
    }
}

// ------------------------------------------------------------------------------------- context

/// The context key of what the app shows.
pub fn context_key(app: &LightcraftApp) -> String {
    // Dialogs get their own page only when the user asked for it; Settings ▸ Controls never: the
    // keys show the page being edited there.
    let controls_open = matches!(&app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "controls" || app.surface.editor.window);
    let dialog = if controls_open || !app.surface.settings().dialog_pages {
        None
    } else {
        app.ui.dialog.as_ref().and_then(|d| serde_json::to_value(d).ok()).and_then(|v| v.get("kind").and_then(Value::as_str).map(str::to_string))
    };
    let library =
        matches!(app.ui.view, ViewMode::PhotoGrid | ViewMode::SquareGrid | ViewMode::Survey | ViewMode::People) || app.session.active().is_none();
    let panel = match app.ui.right {
        RightPanel::Edit | RightPanel::Profiles => Some("edit"),
        RightPanel::Crop => Some("crop"),
        RightPanel::Remove => Some("remove"),
        RightPanel::Masking => Some("masking"),
        RightPanel::RedEye => Some("redeye"),
        _ => None,
    };
    let section = app.ui.open_sections.last().map(String::as_str);
    let tool = match app.ui.tool.as_str() {
        "" => None,
        t => Some(t),
    };
    ls::context::context_key(&ls::context::AppState {
        dialog: dialog.as_deref(),
        library,
        panel,
        section,
        mask_selected: app.session.active_mask.is_some(),
        tool,
    })
}

/// A command's own label from the registry (UI commands, then engine commands), translated.
pub fn registry_label(app: &LightcraftApp, command: &str) -> Option<String> {
    crate::menus::ui_commands()
        .find(|c| c.0 == command)
        .map(|c| c.1.to_string())
        .or_else(|| app.session.commands().into_iter().find(|c| c.id == command).map(|c| c.label.to_string()))
        .map(|l| crate::i18n::tr(&l).to_string())
}

// ------------------------------------------------------------------------------------- layout

struct AppHost<'a>(&'a LightcraftApp);

impl Host for AppHost<'_> {
    fn value(&self, target: &str) -> Option<f64> {
        value(self.0, target)
    }

    fn label(&self, b: &Binding) -> String {
        match b {
            Binding::Command { command, params } => {
                let short = ls::icons::command_label(command, params);
                if short != *command {
                    return crate::i18n::tr(&short).to_string();
                }
                // the registry's own label
                let label = crate::menus::ui_commands()
                    .find(|c| c.0 == command)
                    .map(|c| c.1.to_string())
                    .or_else(|| self.0.session.commands().into_iter().find(|c| c.id == command).map(|c| c.label.to_string()))
                    .unwrap_or_else(|| command.clone());
                crate::i18n::tr(&label).to_string()
            }
            other => crate::i18n::tr(&other.describe()).to_string(),
        }
    }

    fn state(&self, b: &Binding) -> Option<bool> {
        let app = self.0;
        let Binding::Command { command, params } = b else { return None };
        let photo = app.session.active().and_then(|id| app.session.catalog.photo(id));
        Some(match command.as_str() {
            "develop.autoSync" => app.session.auto_sync,
            "view.beforeAfter" => app.ui.before_after != BeforeAfter::Off,
            "view.clipping" => app.ui.show_clipping,
            "view.maskOverlay" => app.ui.mask_overlay,
            "view.filmstrip" => app.ui.filmstrip,
            "view.survey" => app.ui.view == ViewMode::Survey,
            "develop.treatment" => app.session.active().and_then(|id| app.session.develop_of(id)).is_some_and(|d| crate::is_bw(&d)),
            "panel.crop" => app.ui.right == RightPanel::Crop,
            "panel.remove" => app.ui.right == RightPanel::Remove,
            "panel.masking" => app.ui.right == RightPanel::Masking,
            "panel.edit" => app.ui.right == RightPanel::Edit,
            // New Brush and Add / Subtract are one-shot: never lit; the plain brush is a mode
            "tool.brush" if params.get("new").is_some() || params.get("op").is_some() => return None,
            "tool.brush" => app.ui.tool == "brush",
            "tool.linear" => app.ui.tool == "linear",
            "tool.radial" => app.ui.tool == "radial",
            "tool.wbPicker" => app.ui.tool == "wbPicker",
            "photo.rate" => photo.is_some_and(|p| params.get("rating").and_then(Value::as_u64) == Some(u64::from(p.rating))),
            "photo.label" => {
                let want = params.get("label").and_then(Value::as_str).and_then(lightcraft_catalog::ColorLabel::parse);
                photo.is_some_and(|p| p.label.is_some() && p.label == want)
            }
            "photo.pick" => photo.is_some_and(|p| p.flag == lightcraft_catalog::Flag::Pick),
            _ => return None,
        })
    }

    fn tr(&self, english: &str) -> String {
        crate::i18n::tr(english).to_string()
    }
}

/// The layout of a device as the app shows it now.
pub fn layout(app: &LightcraftApp, device: &'static ls::DeviceDef) -> ls::layout::DeviceLayout {
    ls::layout::build(&app.surface.runtime, device, &AppHost(app))
}

// ------------------------------------------------------------------------------------- frames

/// Once per frame: load settings, follow the app's context, flush dial movement, end idle
/// gestures, push layouts to subscribed surfaces, save settings.
pub fn tick(app: &mut LightcraftApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    app.surface.now = now;
    app.surface.pixels_per_point = ctx.pixels_per_point();
    crate::surface_plugin::poll(app);
    if !app.surface.loaded {
        app.surface.loaded = true;
        load(app);
    }
    // Touch to find ends with the page that started it: the console acts on photos again
    let controls_open = matches!(&app.ui.dialog, Some(crate::state::Dialog::Settings { tab }) if tab == "controls" || app.surface.editor.window);
    if app.surface.editor.touch_find.is_some_and(|at| !controls_open || now - at > TOUCH_FIND_GRACE) {
        app.surface.editor.touch_find = None;
        app.surface.runtime.cancel_learn();
    }
    let now_ms = now * 1000.0;
    // the photo changed under a gesture: end it (its ticks belonged to the other photo)
    let photo = app.session.active();
    if photo != app.surface.last_photo {
        app.surface.last_photo = photo;
        let ends: Vec<Effect> = app.surface.runtime.interrupt().into_iter().filter(|e| !matches!(e, Effect::Adjust { .. })).collect();
        run_effects(app, ends);
        app.surface.gesture = None;
    }
    let key = context_key(app);
    app.surface.runtime.set_context(&key);
    let effects = app.surface.runtime.flush(now_ms);
    run_effects(app, effects);
    push(app, now_ms);
    listener(app, now);
    #[cfg(not(target_arch = "wasm32"))]
    app.surface.pairing.expire(now_ms.max(0.0) as u64);
    if let Some(since) = app.surface.dirty_since
        && now - since >= 0.5
    {
        app.surface.dirty_since = None;
        save(app);
    }
    if app.surface.runtime.busy(now_ms) {
        ctx.request_repaint_after(std::time::Duration::from_millis(u64::from(app.surface.settings().transport.flush_ms)));
    }
}

/// Keep the surface listener running on the configured port while surfaces are enabled.
fn listener(app: &mut LightcraftApp, now: f64) {
    let want = app.surface.settings().enabled.then_some(app.surface.settings().port);
    if want == app.surface.listening || now < app.surface.listen_retry_at {
        return;
    }
    let Some(start) = app.services.surface_listener.as_mut() else { return };
    // stop the old one first (a new port, or switched off)
    if app.surface.listening.is_some() {
        let _ = start(None);
        app.surface.listening = None;
    }
    match want {
        Some(port) => match start(Some(port)) {
            Ok(()) => {
                app.surface.listening = Some(port);
                app.surface.listen_error = None;
            }
            Err(e) => {
                log::warn!("control surfaces: {e}");
                app.surface.listen_error = Some(e);
                app.surface.listen_retry_at = now + 2.0;
            }
        },
        None => app.surface.listen_error = None,
    }
}

fn load(app: &mut LightcraftApp) {
    let Some(store) = app.services.surface_store.as_ref() else { return };
    match store.load() {
        Ok(Some(text)) => match ls::SurfaceSettings::from_json(&text) {
            Ok(s) => app.surface.runtime.replace_settings(s),
            Err(e) => {
                log::warn!("{e}");
                app.notices
                    .push(format!("{e}. Control surfaces use their defaults; Settings ▸ Controls saves a new file when you change something."));
                app.surface.load_error = Some(e);
            }
        },
        Ok(None) => {}
        Err(e) => {
            log::warn!("control-surfaces.json: {e}");
            app.surface.load_error = Some(e);
        }
    }
    // the Logi plugin installed: on, and trusted, before the listener starts
    crate::surface_plugin::auto_setup(app);
}

/// Save the settings now (Settings ▸ Controls, keys that change settings).
pub fn save(app: &mut LightcraftApp) {
    let Some(store) = app.services.surface_store.as_ref() else { return };
    let text = app.surface.settings().to_json();
    match store.save(&text) {
        Ok(()) => {
            app.surface.save_error = None;
            app.surface.load_error = None;
        }
        Err(e) => {
            log::warn!("control-surfaces.json: {e}");
            app.surface.save_error = Some(e);
        }
    }
}

/// Send each subscriber its layout when it changed (held back while a value is still catching
/// up with the dial: echo suppression).
fn push(app: &mut LightcraftApp, _now_ms: f64) {
    app.surface.subscribers.retain(|s| s.sink.is_open());
    if app.surface.subscribers.is_empty() {
        return;
    }
    if let Some(t) = app.surface.runtime.turning()
        && app.surface.runtime.holds_feedback(t)
    {
        return;
    }
    let wanted: Vec<(&'static ls::DeviceDef, Vec<String>)> = app.surface.subscribers.iter().map(|s| (s.device, s.watch.clone())).collect();
    let mut layouts: Vec<(usize, Value)> = Vec::new();
    for (i, (d, watch)) in wanted.iter().enumerate() {
        let mut v = serde_json::to_value(layout(app, d)).unwrap_or(Value::Null);
        if !watch.is_empty()
            && let Value::Object(m) = &mut v
        {
            m.insert("values".into(), watched_values(app, watch));
        }
        layouts.push((i, v));
    }
    let mut dead = Vec::new();
    for (i, v) in layouts {
        let Some(s) = app.surface.subscribers.get_mut(i) else { continue };
        if s.last.as_ref() == Some(&v) {
            continue;
        }
        if s.sink.tx.send(json!({"event": "surface.layout", "layout": v})).is_err() {
            dead.push(i);
        }
        s.last = Some(v);
    }
    for i in dead.into_iter().rev() {
        if i < app.surface.subscribers.len() {
            app.surface.subscribers.remove(i);
        }
    }
}

/// `{target: {"value": "+0.35", "position": 0.53}}` for watched values that have one now.
fn watched_values(app: &LightcraftApp, watch: &[String]) -> Value {
    let show = app.surface.settings().look.show_values;
    let mut m = serde_json::Map::new();
    for t in watch {
        let Some(spec) = ls::targets::target_spec(t) else { continue };
        let Some(v) = value(app, t) else { continue };
        let text = if show { json!(spec.format(v)) } else { Value::Null };
        m.insert(t.clone(), json!({"value": text, "position": spec.position(v)}));
    }
    Value::Object(m)
}

/// `surface.catalog`: every action, with labels and categories in the app's language.
pub fn catalog(app: &LightcraftApp) -> Value {
    let host = AppHost(app);
    let all = ls::catalog::entries();
    let mut groups: Vec<&'static str> = Vec::new();
    for e in &all {
        if !groups.contains(&e.group) {
            groups.push(e.group);
        }
    }
    let entries: Vec<Value> = all
        .into_iter()
        .map(|e| {
            json!({
                "key": e.key,
                "group": host.tr(e.group),
                "label": host.label(&e.binding),
                "icon": e.icon,
                "value": e.value,
            })
        })
        .collect();
    json!({"groups": groups.iter().map(|g| host.tr(g)).collect::<Vec<_>>(), "entries": entries})
}

// ------------------------------------------------------------------------------------- protocol

fn ok(v: Value) -> Outcome {
    Outcome::Done(json!({"ok": true, "result": v}))
}
fn err(e: impl std::fmt::Display) -> Outcome {
    Outcome::Done(json!({"ok": false, "error": e.to_string()}))
}

/// The most events one `surface.input` may carry.
pub const MAX_EVENTS: usize = 256;

/// One `surface.input` event: a control of the device, or an action from the surface's own app.
enum Ev {
    Input(ls::Input),
    Act(Binding, Option<i32>),
}

fn run_event(app: &mut LightcraftApp, e: Ev, now_ms: f64) -> usize {
    let effects = match e {
        Ev::Input(i) => app.surface.runtime.input(i, now_ms),
        Ev::Act(b, ticks) => app.surface.runtime.act(&b, ticks, now_ms),
    };
    let n = effects.len();
    run_effects(app, effects);
    n
}

fn parse_events(p: &Value) -> Result<Vec<Ev>, String> {
    let list = p.get("events").and_then(Value::as_array).ok_or("surface.input: give `events: [...]`")?;
    if list.len() > MAX_EVENTS {
        return Err(format!("surface.input: at most {MAX_EVENTS} events at once"));
    }
    let mut out = Vec::new();
    for e in list {
        if let Some(key) = e.get("action").and_then(Value::as_str) {
            let entry = ls::catalog::find(key).ok_or_else(|| format!("surface.input: no action `{}`", ls::protocol::clean(key)))?;
            if let Some(t) = e.get("ticks") {
                let t = t.as_f64().filter(|t| t.is_finite()).ok_or("surface.input: `ticks` must be a number")?;
                out.push(Ev::Act(entry.binding, Some(t.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32)));
            } else if let Some(down) = e.get("press").and_then(Value::as_bool) {
                out.push(if down { Ev::Act(entry.binding, None) } else { Ev::Input(ls::Input::Release { control: "action".into() }) });
            } else {
                return Err("surface.input: an event needs `ticks` or `press`".into());
            }
            continue;
        }
        let control = e
            .get("control")
            .and_then(Value::as_str)
            .map(ls::protocol::clean)
            .filter(|c| !c.is_empty())
            .ok_or("surface.input: each event needs a `control` or an `action`")?;
        if let Some(t) = e.get("ticks") {
            let t = t.as_f64().filter(|t| t.is_finite()).ok_or("surface.input: `ticks` must be a number")?;
            out.push(Ev::Input(ls::Input::Turn { control, ticks: t.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32 }));
        } else if let Some(down) = e.get("press").and_then(Value::as_bool) {
            let page = e.get("page").and_then(Value::as_u64).and_then(|n| usize::try_from(n).ok()).filter(|n| (1..=64).contains(n));
            out.push(Ev::Input(if down { ls::Input::Press { control, page } } else { ls::Input::Release { control } }));
        } else {
            return Err("surface.input: an event needs `ticks` or `press`".into());
        }
    }
    Ok(out)
}

fn hello(app: &mut LightcraftApp, req: &ControlRequest) -> Outcome {
    let p = &req.params;
    let protocol = p.get("protocol").and_then(Value::as_u64).unwrap_or(u64::from(ls::protocol::PROTOCOL));
    if protocol > u64::from(ls::protocol::PROTOCOL) {
        return err(format!("surface.hello: this LightCraft speaks surface protocol {}; update LightCraft", ls::protocol::PROTOCOL));
    }
    let paired = match p.get("token").and_then(Value::as_str) {
        Some(t) => match ls::protocol::verify(app.surface.settings(), t).map(|c| c.client.clone()) {
            Some(client) => {
                app.surface.authed.insert(req.conn, client);
                true
            }
            None => false,
        },
        None => app.surface.authed.contains_key(&req.conn) || req.conn == 0,
    };
    ok(json!({
        "protocol": ls::protocol::PROTOCOL,
        "app": "LightCraft",
        "version": env!("CARGO_PKG_VERSION"),
        "paired": paired,
        "enabled": app.surface.settings().enabled,
        "devices": ls::devices::devices().iter().map(|d| json!({"id": d.id, "name": d.name, "controls": d.controls.iter().map(|c| json!({"id": c.id, "kind": c.kind, "label": c.label})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
    }))
}

fn pair(app: &mut LightcraftApp, req: &ControlRequest) -> Outcome {
    let p = &req.params;
    let now = (app.surface.now * 1000.0).max(0.0) as u64;
    if let Some(id) = p.get("request").and_then(Value::as_str) {
        return match app.surface.pairing.poll(id, now) {
            Some(ls::protocol::PairState::Approved { token }) => {
                if let Some(c) = ls::protocol::verify(app.surface.settings(), &token).map(|c| c.client.clone()) {
                    app.surface.authed.insert(req.conn, c);
                }
                ok(json!({"status": "approved", "token": token}))
            }
            Some(state) => ok(serde_json::to_value(state).unwrap_or_default()),
            None => err("surface.pair: no such request (it may have expired); ask again"),
        };
    }
    let client = p.get("client").and_then(Value::as_str).unwrap_or("");
    let name = p.get("name").and_then(Value::as_str).unwrap_or(client);
    match app.surface.pairing.ask(client, name, now) {
        Ok(r) => {
            let text = crate::i18n::tr_format!("{name} asks to control LightCraft: see Settings, Controls", name = r.name.clone());
            app.toast_for(&app.tasks.repaint.clone().unwrap_or_default(), text, 8.0);
            ok(json!({"status": "pending", "request": r.request, "code": r.code}))
        }
        Err(e) => err(e),
    }
}

/// Handle `surface.*` control methods; `None` for other methods.
pub fn handle(app: &mut LightcraftApp, req: &ControlRequest) -> Option<Outcome> {
    let m = req.method.as_str();
    if !m.starts_with("surface.") {
        return None;
    }
    if !app.surface.loaded {
        app.surface.loaded = true;
        load(app);
    }
    let p = &req.params;
    Some(match m {
        "surface.hello" => hello(app, req),
        "surface.pair" => pair(app, req),
        "surface.catalog" => ok(catalog(app)),
        "surface.icons" => {
            let names: Vec<String> = match p.get("names").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).take(512).map(str::to_string).collect(),
                None => ls::icons::names().map(str::to_string).collect(),
            };
            let map: serde_json::Map<String, Value> = names.into_iter().filter_map(|n| ls::icons::svg(&n).map(|s| (n, json!(s)))).collect();
            ok(Value::Object(map))
        }
        _ => {
            if !app.surface.settings().enabled {
                return Some(err("control surfaces are switched off (Settings ▸ Controls)"));
            }
            let client = if req.conn == 0 { Some("local".to_string()) } else { app.surface.authed.get(&req.conn).cloned() };
            let Some(client) = client else {
                return Some(err(
                    "this surface isn't paired: call surface.pair, approve it in Settings ▸ Controls, then surface.hello with its token",
                ));
            };
            match m {
                "surface.subscribe" => {
                    let device = p.get("device").and_then(Value::as_str).unwrap_or(ls::devices::MX_CREATIVE_CONSOLE.id);
                    let Some(device) = ls::devices::find_device(device) else {
                        return Some(err(format!("surface.subscribe: unknown device `{device}`")));
                    };
                    let Some(sink) = req.sink.clone() else { return Some(err("surface.subscribe: this connection can't receive events")) };
                    app.surface.subscribers.retain(|s| s.conn != req.conn);
                    let l = serde_json::to_value(layout(app, device)).unwrap_or_default();
                    app.surface.subscribers.push(Subscriber { conn: req.conn, client, device, sink, last: Some(l.clone()), watch: Vec::new() });
                    ok(json!({"layout": l}))
                }
                "surface.watch" => {
                    let targets: Vec<String> = p
                        .get("targets")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .filter(|t| ls::targets::target_spec(t).is_some())
                                .take(MAX_WATCH)
                                .map(str::to_string)
                                .collect()
                        })
                        .unwrap_or_default();
                    match app.surface.subscribers.iter_mut().find(|s| s.conn == req.conn) {
                        Some(s) => {
                            s.watch = targets;
                            // the next frame pushes the values
                            s.last = None;
                            ok(Value::Null)
                        }
                        None => err("surface.watch: subscribe first"),
                    }
                }
                "surface.unsubscribe" => {
                    app.surface.subscribers.retain(|s| s.conn != req.conn);
                    ok(Value::Null)
                }
                "surface.layout" => {
                    let device = p.get("device").and_then(Value::as_str).unwrap_or(ls::devices::MX_CREATIVE_CONSOLE.id);
                    match ls::devices::find_device(device) {
                        Some(d) => ok(serde_json::to_value(layout(app, d)).unwrap_or_default()),
                        None => err(format!("surface.layout: unknown device `{device}`")),
                    }
                }
                "surface.input" => match parse_events(p) {
                    Ok(events) => {
                        let now_ms = app.surface.now * 1000.0;
                        let mut n = 0;
                        for e in events {
                            n += run_event(app, e, now_ms);
                        }
                        if let Some(ctx) = app.tasks.repaint.clone() {
                            ctx.request_repaint();
                        }
                        ok(json!({"effects": n}))
                    }
                    Err(e) => err(e),
                },
                other => err(format!("unknown method `{other}`")),
            }
        }
    })
}

// ------------------------------------------------------------------------------------- commands

/// `surface.*` UI commands (`engine.execute`); `None` for other ids.
pub fn command(app: &mut LightcraftApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if !app.surface.loaded {
        app.surface.loaded = true;
        load(app);
    }
    if let Some(r) = crate::surface_plugin::command(app, id) {
        return Some(r);
    }
    let now_ms = app.surface.now * 1000.0;
    let r = match id {
        "surface.maskPrevious" | "surface.maskNext" => step_mask(app, if id == "surface.maskNext" { 1 } else { -1 }),
        "surface.dialogConfirm" => crate::control::confirm_dialog(app),
        "surface.dialogCancel" => {
            app.ui.dialog = None;
            Ok(Value::Null)
        }
        "surface.pageNext" | "surface.pagePrevious" => {
            let group = match p.get("group").and_then(Value::as_str) {
                Some("keys") => Some(ls::PageGroup::Keys),
                Some("dials") => Some(ls::PageGroup::Dials),
                _ => None,
            };
            let b = if id == "surface.pageNext" { Binding::PageNext { group } } else { Binding::PagePrevious { group } };
            act(app, b, now_ms)
        }
        "surface.page" => match p.get("page").and_then(Value::as_str) {
            Some(page) => act(app, Binding::Page { page: page.into() }, now_ms),
            None => Err("surface.page: give `page`".into()),
        },
        "surface.profile" => match p.get("profile").and_then(Value::as_str) {
            Some(name) if app.surface.settings().profiles.iter().any(|q| q.name == name) => {
                act(app, Binding::Profile { profile: name.into() }, now_ms)
            }
            Some(name) => Err(format!("surface.profile: no profile `{name}`")),
            None => act(app, Binding::ProfileNext, now_ms),
        },
        "surface.applyToSelected" | "surface.followContext" => {
            let (cur, b) = if id == "surface.applyToSelected" {
                (app.surface.settings().apply_to_selected, Binding::ApplyToSelected)
            } else {
                (app.surface.settings().follow_context, Binding::FollowContext)
            };
            match p.get("on").and_then(Value::as_bool) {
                Some(on) if on == cur => Ok(json!({"on": on})),
                _ => act(app, b, now_ms).map(|_| {
                    let on = if id == "surface.applyToSelected" {
                        app.surface.settings().apply_to_selected
                    } else {
                        app.surface.settings().follow_context
                    };
                    json!({"on": on})
                }),
            }
        }
        "surface.state" => Ok(state(app)),
        "surface.import" => import(app, p),
        "surface.export" => export(app, p),
        "surface.forget" => match p.get("client").and_then(Value::as_str) {
            // what Forget does in Settings > Controls: the client's token stops working
            Some(c) if app.surface.settings().paired.iter().any(|x| x.client == c) => {
                let c = c.to_string();
                app.surface.unpair(&c);
                Ok(Value::Null)
            }
            Some(c) => Err(format!("surface.forget: no paired surface `{}`", ls::protocol::clean(c))),
            None => Err("surface.forget: give `client`".into()),
        },
        "surface.settings" => match p.get("settings") {
            // replace the whole settings file content (agents, tests); keeps pairings
            Some(v) => match ls::SurfaceSettings::from_json(&v.to_string()) {
                Ok(mut s) => {
                    s.paired = app.surface.settings().paired.clone();
                    app.surface.runtime.replace_settings(s);
                    app.surface.dirty_since.get_or_insert(app.surface.now);
                    Ok(Value::Null)
                }
                Err(e) => Err(e),
            },
            None => Ok(serde_json::to_value(app.surface.settings()).unwrap_or_default()),
        },
        "surface.input" => match parse_events(p) {
            // the same input a surface sends, from a command (agents, tests, the editor's preview)
            Ok(events) => {
                for e in events {
                    run_event(app, e, now_ms);
                }
                Ok(Value::Null)
            }
            Err(e) => Err(e),
        },
        _ => return None,
    };
    Some(r)
}

fn act(app: &mut LightcraftApp, b: Binding, now_ms: f64) -> Result<Value, String> {
    let effects = app.surface.runtime.perform("command", &b, now_ms);
    run_effects(app, effects);
    Ok(state(app))
}

fn step_mask(app: &mut LightcraftApp, step: i64) -> Result<Value, String> {
    let id = app.session.active().ok_or("no active photo")?;
    let d = app.session.develop_of(id).ok_or("no active photo")?;
    let ids: Vec<u32> = d.masks.iter().map(|m| m.id).collect();
    if ids.is_empty() {
        return Err("this photo has no masks".into());
    }
    let n = ids.len() as i64;
    let i = app.session.active_mask.and_then(|m| ids.iter().position(|x| *x == m)).map_or(if step > 0 { -1 } else { n }, |i| i as i64);
    let j = (i + step).rem_euclid(n);
    let next = ids.get(usize::try_from(j).unwrap_or(0)).copied().ok_or("no mask")?;
    app.run("mask.select", json!({"id": next}))?;
    Ok(json!({"id": next}))
}

/// `surface.state`: profile, context, pages, focus, toggles, connected surfaces, waiting pairing
/// requests.
pub fn state(app: &LightcraftApp) -> Value {
    let r = &app.surface.runtime;
    json!({
        "enabled": r.settings().enabled,
        "profile": r.settings().profile,
        "profiles": r.settings().profiles.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        "context": r.context(),
        "keyPage": r.pages().get(&ls::PageGroup::Keys),
        "dialPage": r.pages().get(&ls::PageGroup::Dials),
        "focus": r.focus(),
        "turning": r.turning(),
        "applyToSelected": r.settings().apply_to_selected,
        "followContext": r.settings().follow_context,
        "connected": app.surface.connected().into_iter().map(|(c, d)| json!({"client": c, "device": d})).collect::<Vec<_>>(),
        "paired": r.settings().paired.iter().map(|c| json!({"client": c.client, "name": c.name})).collect::<Vec<_>>(),
        "pairing": app.surface.pairing.waiting().map(|q| json!({"request": q.request, "client": q.client, "name": q.name, "code": q.code})).collect::<Vec<_>>(),
        "osd": r.shown(app.surface.now * 1000.0).map(|(s, _)| match s {
            ls::runtime::Shown::Value { target } => json!({"value": target, "text": value(app, target).and_then(|v| ls::targets::target_spec(target).map(|s| s.format(v)))}),
            ls::runtime::Shown::Command { label, .. } => json!({"command": label}),
            ls::runtime::Shown::Notice { text } => json!({"notice": text}),
        }),
        "loadError": app.surface.load_error,
        "saveError": app.surface.save_error,
    })
}

/// The largest file `surface.import` reads.
const MAX_IMPORT: u64 = 16 * 1024 * 1024;

fn read_file(path: &str) -> Result<Vec<u8>, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let meta = std::fs::metadata(path).map_err(|e| format!("{path}: {e}"))?;
        if meta.len() > MAX_IMPORT {
            return Err(format!("{path} is too large"));
        }
        std::fs::read(path).map_err(|e| format!("{path}: {e}"))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = (path, MAX_IMPORT);
        Err("files can't be read here".into())
    }
}

/// TactileBridge's settings file on this computer, if there is one.
pub fn tactilebridge_settings_path() -> Option<std::path::PathBuf> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let base = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)?;
        let p = base.join("TactileBridge").join("settings.json");
        p.is_file().then_some(p)
    }
    #[cfg(target_arch = "wasm32")]
    {
        None
    }
}

/// Loupedeck's profiles for the Loupedeck+ made for the photo editor whose actions LightCraft
/// maps (Loupedeck keeps them under `@_lightroom`), as its software keeps them on this computer:
/// (the profile's name, its `ProfileInfo.json`), newest first. Read only.
pub fn loupedeck_profiles() -> Vec<(String, std::path::PathBuf)> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let Some(base) = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from) else { return Vec::new() };
        let mut out = Vec::new();
        for device in ["Loupedeck15", "Loupedeck10"] {
            let Ok(apps) = std::fs::read_dir(base.join("Loupedeck").join("Applications").join(device)) else { continue };
            for app in apps.flatten().take(200) {
                if !app.file_name().to_string_lossy().to_ascii_lowercase().contains("lightroom") {
                    continue;
                }
                let Ok(profiles) = std::fs::read_dir(app.path().join("Profiles")) else { continue };
                for prof in profiles.flatten().take(200) {
                    let file = prof.path().join("ProfileInfo.json");
                    let Ok(meta) = std::fs::metadata(&file) else { continue };
                    if meta.len() > MAX_IMPORT {
                        continue;
                    }
                    let Ok(text) = std::fs::read_to_string(&file) else { continue };
                    let Ok(v) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else { continue };
                    let name = v.get("displayName").and_then(Value::as_str).unwrap_or("Loupedeck").to_string();
                    let changed = v.get("lastModifiedTimeUtc").and_then(Value::as_str).unwrap_or_default().to_string();
                    out.push((changed, name, file));
                    if out.len() >= 50 {
                        break;
                    }
                }
            }
        }
        // the one changed last (the user's own) first
        out.sort_by(|a, b| b.0.cmp(&a.0));
        out.into_iter().map(|(_, n, f)| (n, f)).collect()
    }
    #[cfg(target_arch = "wasm32")]
    {
        Vec::new()
    }
}

/// Is this file a Loupedeck+ profile (its `ProfileInfo.json`, or an `.lp5` holding one)?
fn is_loupedeck(bytes: &[u8]) -> bool {
    let json = if bytes.starts_with(b"PK") { ls::zip::read_member(bytes, "ProfileInfo.json").unwrap_or_default() } else { bytes.to_vec() };
    let text = String::from_utf8_lossy(json.get(..json.len().min(8192)).unwrap_or_default());
    text.contains("\"Loupedeck15\"") || text.contains("\"Loupedeck10\"")
}

/// A Loupedeck+ profile's layout into every LightCraft profile (the console has no pages: its
/// controls hold on every page and in every profile), replacing an earlier import.
fn import_loupedeck(app: &mut LightcraftApp, s: &mut ls::SurfaceSettings, bytes: &[u8]) -> Result<Value, String> {
    let presets: Vec<(String, String)> = app.session.presets.iter().map(|p| (p.name.to_lowercase(), p.id.clone())).collect();
    let find = |name: &str| {
        let n = name.trim().to_lowercase();
        presets.iter().find(|(p, _)| *p == n).map(|(_, id)| id.clone())
    };
    let imported = ls::loupedeck::import(bytes, &find)?;
    for p in &mut s.profiles {
        p.base.retain(|c, _| !c.starts_with("ld."));
        p.base.extend(imported.bindings.clone());
    }
    app.surface.last_report = Some(imported.report.clone());
    Ok(json!({"profile": s.profile, "device": ls::devices::LOUPEDECK_PLUS.id, "loupedeck": imported.name, "report": imported.report}))
}

/// `surface.import {path}`: a TactileBridge `settings.json`, a Logi profile (`.lp5`), a
/// Loupedeck+ profile (its `ProfileInfo.json` or an exported `.lp5`) or a LightCraft layout file
/// (`.lcsurface`, a profile as JSON).
fn import(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let path = p.get("path").and_then(Value::as_str).ok_or("surface.import: give `path`")?;
    let bytes = read_file(path)?;
    let lower = path.to_ascii_lowercase();
    let mut s = app.surface.settings().clone();
    let result = if is_loupedeck(&bytes) {
        import_loupedeck(app, &mut s, &bytes)?
    } else if lower.ends_with(".lp5") {
        let (mut profile, report) = ls::tactilebridge::import_lp5(&bytes)?;
        profile.name = unique_profile_name(&s, &profile.name);
        let name = profile.name.clone();
        s.profiles.push(profile);
        s.profile = name.clone();
        app.surface.last_report = Some(report.clone());
        json!({"profile": name, "report": report})
    } else if lower.ends_with(".lcsurface") {
        let text = String::from_utf8_lossy(&bytes);
        let mut profile: ls::Profile = serde_json::from_str(&text).map_err(|e| format!("{path}: not a LightCraft layout file ({e})"))?;
        profile.name = unique_profile_name(&s, if profile.name.trim().is_empty() { "Imported" } else { &profile.name });
        let name = profile.name.clone();
        s.profiles.push(profile);
        s.profile = name.clone();
        json!({"profile": name})
    } else {
        let text = String::from_utf8_lossy(&bytes);
        let report = ls::tactilebridge::import_settings(&mut s, &text)?;
        app.surface.last_report = Some(report.clone());
        json!({"profile": s.profile, "report": report})
    };
    s.paired = app.surface.settings().paired.clone();
    app.surface.runtime.replace_settings(s);
    app.surface.dirty_since.get_or_insert(app.surface.now);
    Ok(result)
}

fn unique_profile_name(s: &ls::SurfaceSettings, base: &str) -> String {
    let mut name = base.trim().to_string();
    let mut n = 2;
    while s.profiles.iter().any(|p| p.name == name) {
        name = format!("{} {n}", base.trim());
        n += 1;
    }
    name
}

/// `surface.export {path, profile?}`: write a profile as a LightCraft layout file.
fn export(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    let path = p.get("path").and_then(Value::as_str).ok_or("surface.export: give `path`")?;
    let name = p.get("profile").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| app.surface.settings().profile.clone());
    let profile = app.surface.settings().profiles.iter().find(|q| q.name == name).ok_or_else(|| format!("surface.export: no profile `{name}`"))?;
    let text = serde_json::to_string_pretty(profile).map_err(|e| e.to_string())?;
    match app.services.write.as_mut() {
        Some(w) => w(path, text.as_bytes())?,
        None => return Err("files can't be written here".into()),
    }
    Ok(json!({"path": path, "profile": name}))
}
