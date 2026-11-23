//! Getting Started (fork-owned): the guide a first start walks through once its library is open.
//!
//! The welcome dialog (`catalog_ui`) creates or opens the library; this guide follows it, one page
//! at a time: the AI features (each a download that costs memory, and time on a computer without a
//! graphics card), the interface (layout, appearance, text size, language, grading mode), a few key
//! settings, a short tour of the modules, and a last page that says what will be downloaded.
//!
//! Nothing here is a setting of its own. The interface page runs the same commands as the menus
//! (`view.modernLayout`, `view.compactLayout`, `view.appearance`, `view.theme`, `view.gradingMode`)
//! and writes the same settings as Settings ▸ Interface, so a change shows at once; **Undo these
//! changes** puts back how it looked when the guide opened. The key settings page writes the
//! existing settings (`catalog.promptAtStartup`, `update.prefs`, `library.xmpPreferences`, Reduce
//! motion). The AI choices are only remembered until **Finish**, which sends each ticked feature's
//! own download command (`denoise.models.download`, `ai.models.download`, `faces.models.download`
//! then `faces.enable`, `ml.models.download` then `ml.enable`, `lens.db.download`); they download
//! in the background through the activity stack, so Finish never waits. An unticked feature is never
//! downloaded, and **Skip** (or Esc) closes the guide having downloaded nothing.
//!
//! Which features are offered comes from the engine's own model lists (a build without a feature
//! lists no row for it); which are ticked at first depends on the computer ([`preticked`]): nothing
//! under 8 GB of memory, the light ones from 8 GB, the heavier masks only from 16 GB with a real
//! graphics card. Faces and search are never ticked for the user: they scan the whole library.
//!
//! `AppSettings::onboarding_done` is set by Finish or Skip, so the guide follows a welcome dialog
//! only once; Help ▸ Getting Started… (`app.gettingStarted`) opens it again at any time. The tour's
//! pictures are drawn in code from the app's own icons (original work).

use egui::{RichText, Ui};
use serde_json::{Value, json};

use crate::LccApp;
use crate::i18n::tr;
use crate::menus::UiCommand;
use crate::widgets::register;

/// The guide's command.
pub const COMMANDS: &[UiCommand] = &[("app.gettingStarted", "Getting Started…", None, "Help")];

/// The AI page.
pub const STEP_AI: usize = 0;
/// The interface page.
pub const STEP_INTERFACE: usize = 1;
/// The key settings page.
pub const STEP_SETTINGS: usize = 2;
/// The first tour page.
pub const STEP_TOUR: usize = 3;
/// The tour pages: (title, text, help topic, icon).
const TOUR: &[(&str, &str, &str, crate::icons::Icon)] = &[
    (
        "Import",
        "Bring photos in from a memory card, a phone or a folder. Add them where they are, or copy them to a folder of your choice: the originals are never changed. Use File ▸ Import Photos, or drop files on the window.",
        "tourImport",
        crate::icons::Icon::Folder,
    ),
    (
        "Library",
        "Browse your photos in a grid. Rate them with stars, flag the keepers and reject the rest, then sort them into collections and keywords. Filters find any photo by date, camera, rating or text.",
        "tourLibrary",
        crate::icons::Icon::GridPhoto,
    ),
    (
        "Develop",
        "Edit a photo without ever changing its file: light, colour, detail, crop, masks and presets. Every step is kept in the history, so you can go back at any time.",
        "tourDevelop",
        crate::icons::Icon::Sliders,
    ),
    (
        "AI tools",
        "Masks that find the subject, the sky or an object you click, AI Denoise, face recognition and search by description. They run on this computer once their models are downloaded.",
        "tourAi",
        crate::icons::Icon::Subject,
    ),
    (
        "Map, Book, Slideshow, Print and Web",
        "Place photos on a map, lay out a photo book, play a slideshow, print with your own layouts, or make a web gallery.",
        "tourOutput",
        crate::icons::Icon::Book,
    ),
    (
        "Export and sync",
        "Export JPEG, TIFF and more at any size with saved presets, publish to online services, and keep a library in step with your other devices.",
        "tourExport",
        crate::icons::Icon::Share,
    ),
];
/// The last page.
pub const STEP_FINISH: usize = STEP_TOUR + TOUR.len();

/// One AI (or download) feature the AI page offers.
#[derive(Clone, Debug, PartialEq)]
pub struct AiRow {
    /// `denoise`, `superres`, `objects`, `subject`, `scene`, `faces`, `search` or `lens`.
    pub key: &'static str,
    /// The download's size (bytes), when it is known.
    pub size: Option<u64>,
    /// The model's licence and where to read it.
    pub licence: String,
    pub licence_url: String,
    /// Already on this computer: shown ticked, nothing to download.
    pub installed: bool,
    /// Ticked.
    pub on: bool,
    /// What Finish runs for it, in order.
    pub commands: Vec<(String, Value)>,
}

/// How the app looked when the guide opened (Undo these changes).
#[derive(Clone, Debug, PartialEq)]
struct Look {
    modern: bool,
    compact: bool,
    appearance: crate::state::AppearanceMode,
    dark: crate::state::DarkTheme,
    light: crate::state::LightTheme,
    font: crate::theme::UiFontSize,
    grading: crate::state::GradingMode,
    language: crate::i18n::Locale,
}

impl Look {
    fn of(app: &LccApp) -> Self {
        let s = &app.ui.settings;
        Look {
            modern: app.ui.modern.enabled,
            compact: s.compact_layout,
            appearance: s.appearance_mode,
            dark: s.dark_theme,
            light: s.light_theme,
            font: s.ui_font_size,
            grading: s.grading_mode,
            language: app.ui.language,
        }
    }

    fn restore(&self, app: &mut LccApp) {
        let _ = app.run("view.compactLayout", json!({"on": self.compact}));
        let _ = app.run("view.modernLayout", json!({"on": self.modern}));
        let s = &mut app.ui.settings;
        s.compact_layout = self.compact;
        s.appearance_mode = self.appearance;
        s.dark_theme = self.dark;
        s.light_theme = self.light;
        s.ui_font_size = self.font;
        s.grading_mode = self.grading;
        app.ui.language = self.language;
        crate::i18n::set_language(self.language);
    }
}

/// The guide, while it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct Wizard {
    pub step: usize,
    pub rows: Vec<AiRow>,
    /// This computer's memory (bytes) and graphics card, as the AI page says them.
    pub ram: Option<u64>,
    pub gpu: Option<String>,
    /// The graphics card is a software one (or there is none): AI is slow here.
    pub slow_gpu: bool,
    look: Look,
}

/// The guide's state on [`LccApp`].
#[derive(Default)]
pub struct Onboarding {
    pub wizard: Option<Wizard>,
    /// A welcome dialog was shown: the guide follows once its library is open (and only if the
    /// guide was never finished or skipped).
    pub after_welcome: bool,
}

/// Whether a feature is ticked at first on a computer with `ram_gb` of memory (`None`: unknown) and,
/// with `real_gpu`, a hardware graphics card. Nothing under 8 GB (or unknown); Denoise, Super
/// Resolution, Object masks and lens profiles from 8 GB; Subject and scene masks from 16 GB with a
/// real graphics card; face recognition and AI search never (they scan the whole library).
pub fn preticked(key: &str, ram_gb: Option<f64>, real_gpu: bool) -> bool {
    let Some(gb) = ram_gb.filter(|g| g.is_finite()) else { return false };
    // (a "8 GB" computer reports a little less)
    match key {
        "denoise" | "superres" | "objects" | "lens" => gb >= 7.5,
        "subject" | "scene" => gb >= 15.5 && real_gpu,
        _ => false,
    }
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn models(v: &Value) -> Vec<Value> {
    v.get("models").and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The features this build offers, from the engine's model lists, ticked for this computer.
fn rows(app: &mut LccApp, ram_gb: Option<f64>, real_gpu: bool) -> Vec<AiRow> {
    let mut out: Vec<AiRow> = Vec::new();
    let mut push = |key: &'static str, size: Option<u64>, licence: &str, url: &str, installed: bool, commands: Vec<(String, Value)>| {
        if out.iter().any(|r| r.key == key) {
            return;
        }
        let on = installed || preticked(key, ram_gb, real_gpu);
        out.push(AiRow { key, size, licence: licence.to_string(), licence_url: url.to_string(), installed, on, commands });
    };
    if let Ok(list) = app.session.execute("denoise.models.list", &json!({})) {
        for m in models(&list) {
            let key = match str_of(&m, "domain") {
                "bayerToRgb" => "denoise",
                "rgbToRgb4x" => "superres",
                _ => continue,
            };
            let installed = m.get("installed").and_then(Value::as_bool).unwrap_or(false);
            if str_of(&m, "downloadHost").is_empty() && !installed {
                continue;
            }
            let id = str_of(&m, "id").to_string();
            let lic = m.get("licence").cloned().unwrap_or_default();
            let cmd = vec![("denoise.models.download".to_string(), json!({"id": id, "acknowledged": true}))];
            push(key, m.get("sizeBytes").and_then(Value::as_u64), str_of(&lic, "name"), str_of(&lic, "url"), installed, cmd);
        }
    }
    if let Ok(st) = app.session.execute("ai.models.status", &json!({}))
        && st.get("available").and_then(Value::as_bool) == Some(true)
    {
        for m in models(&st) {
            let key = match str_of(&m, "id") {
                "sam2" => "objects",
                "subject" => "subject",
                "scene" => "scene",
                _ => continue,
            };
            let id = str_of(&m, "id").to_string();
            let installed = m.get("installed").and_then(Value::as_bool).unwrap_or(false);
            let cmd = vec![("ai.models.download".to_string(), json!({"id": id, "acknowledged": true}))];
            push(key, m.get("sizeBytes").and_then(Value::as_u64), str_of(&m, "license"), str_of(&m, "licenseUrl"), installed, cmd);
        }
    }
    if let Ok(list) = app.session.execute("faces.models.list", &json!({}))
        && list.get("runtime").and_then(Value::as_bool) == Some(true)
    {
        let embedders: Vec<Value> = models(&list)
            .into_iter()
            .filter(|m| str_of(m, "role") == "embedder" && m.get("retired").and_then(Value::as_bool) != Some(true))
            .collect();
        let installed = embedders.iter().any(|m| m.get("installed").and_then(Value::as_bool) == Some(true));
        if let Some(m) = embedders.iter().find(|m| !str_of(m, "downloadHost").is_empty()) {
            let lic = m.get("licence").cloned().unwrap_or_default();
            let cmd = vec![
                ("faces.models.download".to_string(), json!({"id": str_of(m, "id"), "acknowledged": true})),
                ("faces.enable".to_string(), json!({"enabled": true})),
            ];
            push("faces", m.get("sizeBytes").and_then(Value::as_u64), str_of(&lic, "name"), str_of(&lic, "url"), installed, cmd);
        }
    }
    if let Ok(list) = app.session.execute("ml.models.list", &json!({}))
        && list.get("runtime").and_then(Value::as_bool) == Some(true)
        // the known (downloadable) models come first in the list
        && let Some(m) = models(&list).into_iter().find(|m| str_of(m, "role") == "embedder")
    {
        let installed = m.get("installed").and_then(Value::as_bool).unwrap_or(false);
        let lic = m.get("licence").cloned().unwrap_or_default();
        let cmd = vec![
            ("ml.models.download".to_string(), json!({"id": str_of(&m, "id"), "acceptLicence": true, "acknowledged": true})),
            ("ml.enable".to_string(), json!({"enabled": true})),
        ];
        push("search", m.get("sizeBytes").and_then(Value::as_u64), str_of(&lic, "spdx"), str_of(&lic, "url"), installed, cmd);
    }
    if let Ok(st) = app.session.execute("lens.db.status", &json!({}))
        && st.get("canDownload").and_then(Value::as_bool) == Some(true)
    {
        let installed = st.get("downloaded").and_then(Value::as_bool).unwrap_or(false);
        let cmd = vec![("lens.db.download".to_string(), json!({}))];
        push("lens", None, "CC BY-SA 3.0", "https://creativecommons.org/licenses/by-sa/3.0/", installed, cmd);
    }
    out
}

/// The graphics card's name, when one is ready, and whether it is a real one (not a software adapter).
fn graphics() -> (Option<String>, bool) {
    let on = lcc_engine::gpu::ready() && lcc_engine::gpu::available();
    let name = if on { lcc_engine::gpu::adapter_name() } else { None };
    (name, on && !lcc_engine::gpu::software_adapter())
}

/// Opens the guide at `step` (clamped to its pages).
pub fn open(app: &mut LccApp, step: usize) {
    let ram = lcc_engine::memory::installed_ram().map(|b| b as u64);
    let ram_gb = ram.map(|b| b as f64 / f64::from(1u32 << 30));
    let (gpu, real_gpu) = graphics();
    let rows = rows(app, ram_gb, real_gpu);
    let look = Look::of(app);
    app.onboarding.wizard = Some(Wizard { step: step.min(STEP_FINISH), rows, ram, gpu, slow_gpu: !real_gpu, look });
}

/// What Finish runs: the ticked features' download commands (installed ones need none).
pub fn finish_commands(w: &Wizard) -> Vec<(String, Value)> {
    w.rows.iter().filter(|r| r.on && !r.installed).flat_map(|r| r.commands.iter().cloned()).collect()
}

/// Closes the guide; `finish`: start the ticked downloads first (`ran`: the commands sent, `errors`: those
/// that refused). Either way it counts as seen.
pub fn close(app: &mut LccApp, finish: bool) -> Result<Value, String> {
    let Some(w) = app.onboarding.wizard.take() else { return Ok(json!({"open": false})) };
    app.ui.settings.onboarding_done = true;
    let mut ran = Vec::new();
    let mut errors = Vec::new();
    if finish {
        for (cmd, p) in finish_commands(&w) {
            if let Err(e) = app.run(&cmd, p) {
                errors.push(e);
            }
            ran.push(cmd);
        }
    }
    if let Some(e) = errors.first() {
        let ctx = app.tasks.repaint.clone().unwrap_or_default();
        app.toast_error(&ctx, e.clone());
    }
    Ok(json!({"open": false, "ran": ran, "errors": errors}))
}

/// `app.gettingStarted {step?, action?: next|back|skip|finish, tick?: {key: bool}}`: opens the
/// guide (at the AI page, or `step`), or drives the open one → `{open, step, steps, rows}`.
pub fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    if id != "app.gettingStarted" {
        return None;
    }
    let action = p.get("action").and_then(Value::as_str);
    if app.onboarding.wizard.is_none() || p.get("step").is_some() && action.is_none() {
        let step = p.get("step").and_then(Value::as_u64).map_or(STEP_AI, |s| usize::try_from(s).unwrap_or(STEP_FINISH));
        open(app, step);
    }
    if let Some(ticks) = p.get("tick").and_then(Value::as_object)
        && let Some(w) = app.onboarding.wizard.as_mut()
    {
        for r in w.rows.iter_mut().filter(|r| !r.installed) {
            if let Some(on) = ticks.get(r.key).and_then(Value::as_bool) {
                r.on = on;
            }
        }
    }
    match action {
        None => {}
        Some("next") => step_by(app, 1),
        Some("back") => step_by(app, -1),
        Some("skip") => return Some(close(app, false)),
        Some("finish") => return Some(close(app, true)),
        Some(other) => return Some(Err(format!("app.gettingStarted: unknown action `{other}` (next|back|skip|finish)"))),
    }
    let Some(w) = app.onboarding.wizard.as_ref() else { return Some(Ok(json!({"open": false}))) };
    let rows: Vec<Value> = w.rows.iter().map(|r| json!({"key": r.key, "on": r.on, "installed": r.installed, "sizeBytes": r.size})).collect();
    Some(Ok(json!({"open": true, "step": w.step, "steps": STEP_FINISH + 1, "rows": rows})))
}

fn step_by(app: &mut LccApp, by: isize) {
    if let Some(w) = app.onboarding.wizard.as_mut() {
        w.step = w.step.saturating_add_signed(by).min(STEP_FINISH);
    }
}

/// Opens the guide after a welcome dialog, once its library is open (never twice).
fn follow_welcome(app: &mut LccApp) {
    #[cfg(not(target_arch = "wasm32"))]
    let welcoming = matches!(app.catalog_ui.dialog, Some(crate::catalog_ui::CatalogDialog::Welcome { .. }));
    #[cfg(target_arch = "wasm32")]
    let welcoming = false;
    if !app.onboarding.after_welcome || welcoming || app.session.library.is_none() {
        return;
    }
    app.onboarding.after_welcome = false;
    if !app.ui.settings.onboarding_done && app.onboarding.wizard.is_none() {
        open(app, STEP_AI);
    }
}

fn title_of(step: usize) -> &'static str {
    match step {
        STEP_AI => "Choose AI features",
        STEP_INTERFACE => "Interface",
        STEP_SETTINGS => "Key settings",
        STEP_FINISH => "You're all set",
        s => TOUR.get(s.saturating_sub(STEP_TOUR)).map_or("Getting Started", |t| t.0),
    }
}

fn row_text(key: &str) -> (&'static str, &'static str) {
    match key {
        "denoise" => (
            "AI Denoise",
            "Cleans up noise in high-ISO raw photos. Uses 1–2 GB of memory while it works: seconds per photo with a graphics card, minutes without.",
        ),
        "superres" => ("AI Super Resolution", "Enlarges photos four times. The memory it needs grows with the size of the result."),
        "objects" => ("Object masks", "Click a thing in a photo to mask it. Uses about 1 GB of memory while it works."),
        "subject" => ("Subject and background masks", "Finds the main subject in one click. Uses about 1 GB of memory while it works."),
        "scene" => ("Sky, water and plant masks", "Masks the sky, water and vegetation more precisely. Uses under 1 GB of memory."),
        "faces" => (
            "Face recognition",
            "Finds and groups the people in your photos. It scans the whole library in the background, which keeps the computer busy for a while.",
        ),
        "search" => ("AI search", "Find photos by describing them. It indexes the whole library in the background and uses about 1 GB of memory."),
        "lens" => ("Lens profiles", "Corrects distortion and vignetting for thousands of lenses. A small download that doesn't slow anything down."),
        _ => ("", ""),
    }
}

fn mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1e6)
}

/// Draws the guide, when it is open (once a frame).
pub fn show(app: &mut LccApp, ctx: &egui::Context) {
    follow_welcome(app);
    let Some(mut w) = app.onboarding.wizard.take() else { return };
    // the graphics card may come up after the guide opened (the ticks stay as they were)
    if w.gpu.is_none() {
        let (gpu, real) = graphics();
        w.gpu = gpu;
        w.slow_gpu = !real;
    }
    // (read before drawing: a list Esc closes this frame is already shut afterwards)
    let popup_open = egui::Popup::is_any_open(ctx);
    let t = crate::theme::Tokens::get(ctx);
    let window_id = egui::Id::new("getting-started");
    let width = (ctx.content_rect().width() - 2.0 * crate::dialog_frame::SCREEN_MARGIN).clamp(240.0, 560.0);
    let mut nav: Option<&str> = None;
    let mut undo = false;
    let mut topic: Option<&'static str> = None;
    let shown = egui::Window::new(tr("Getting Started"))
        .id(window_id)
        .title_bar(false)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .frame(crate::dialog_frame::card_frame(ctx))
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .fixed_size([width, 0.0])
        .show(ctx, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 8.0;
            crate::dialog_frame::card_header(ui, tr(title_of(w.step)), None);
            ui.label(
                RichText::new(crate::i18n::tr_format!("Step {n} of {total}", n = w.step + 1, total = STEP_FINISH + 1)).color(t.text_dim).small(),
            );
            let mut layout = crate::dialog_frame::ModalLayout::begin(ui, window_id);
            layout.body(ui, |ui| {
                ui.set_width(width);
                match w.step {
                    STEP_AI => ai_page(ui, &t, &mut w),
                    STEP_INTERFACE => undo = interface_page(app, ui),
                    STEP_SETTINGS => settings_page(app, ui, &t),
                    STEP_FINISH => finish_page(ui, &t, &w),
                    s => topic = tour_page(ui, &t, s),
                }
            });
            layout.footer(ui, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    if w.step > STEP_AI {
                        let r = ui.button(tr("Back"));
                        register(ui.ctx(), "button:wizardBack", r.rect);
                        if r.clicked() {
                            nav = Some("back");
                        }
                    }
                    if w.step < STEP_FINISH {
                        let r = ui.button(tr("Skip"));
                        register(ui.ctx(), "button:wizardSkip", r.rect);
                        if r.clicked() {
                            nav = Some("skip");
                        }
                    }
                    let (label, id, act) =
                        if w.step == STEP_FINISH { ("Finish", "button:wizardFinish", "finish") } else { ("Next", "button:wizardNext", "next") };
                    let r = ui.button(RichText::new(tr(label)).strong());
                    register(ui.ctx(), id, r.rect);
                    if r.clicked() {
                        nav = Some(act);
                    }
                });
            });
        });
    crate::dialog_frame::ModalLayout::finish(ctx, window_id, shown.as_ref().map(|s| s.response.rect));
    if let Some(s) = shown.as_ref() {
        ctx.memory_mut(|m| m.set_modal_layer(s.response.layer_id));
    }
    if undo {
        w.look.restore(app);
    }
    app.onboarding.wizard = Some(w);
    // Esc skips the guide, unless it is closing a list (the language picker) first
    if !popup_open && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        nav = Some("skip");
    }
    if let Some(t) = topic
        && let Err(e) = app.run("app.openHelp", json!({"topic": t}))
    {
        app.toast_error(ctx, e);
    }
    if let Some(act) = nav
        && let Some(Err(e)) = run(app, "app.gettingStarted", &json!({"action": act}))
    {
        app.toast_error(ctx, e);
    }
}

fn dim(t: &crate::theme::Tokens, s: &str) -> RichText {
    RichText::new(tr(s)).color(t.text_dim)
}

fn ai_page(ui: &mut Ui, t: &crate::theme::Tokens, w: &mut Wizard) {
    ui.label(dim(
        t,
        "AI features run on this computer. Each needs a one-time download, and memory while it works; without a graphics card they are slow. Tick what you want now: the downloads start when you finish, and Settings ▸ AI Models changes this later.",
    ));
    let ram = w.ram.map_or_else(|| tr("unknown").to_string(), |b| format!("{:.0} GB", b as f64 / f64::from(1u32 << 30)));
    let gpu = match (&w.gpu, w.slow_gpu) {
        (Some(name), false) => name.clone(),
        _ => tr("none: AI runs slowly").to_string(),
    };
    ui.label(RichText::new(crate::i18n::tr_format!("This computer: {ram} of memory, graphics card: {gpu}", ram = ram, gpu = gpu)).strong());
    if w.rows.is_empty() {
        ui.label(dim(t, "This version has no AI features to download."));
    }
    for r in &mut w.rows {
        let (title, text) = row_text(r.key);
        let mut label = tr(title).to_string();
        if r.installed {
            label = format!("{label} ({})", tr("installed"));
        } else if let Some(size) = r.size {
            label = format!("{label} ({})", mb(size));
        }
        let resp = ui.add_enabled(!r.installed, egui::Checkbox::new(&mut r.on, RichText::new(label).strong()));
        register(ui.ctx(), format!("check:wizardAi.{}", r.key), resp.rect);
        ui.indent(r.key, |ui| {
            ui.label(dim(t, text));
            if !r.licence.is_empty() {
                let lic = crate::i18n::tr_format!("Licence: {name}", name = r.licence.as_str());
                if r.licence_url.starts_with("https://") {
                    ui.hyperlink_to(RichText::new(lic).small(), r.licence_url.as_str());
                } else {
                    ui.label(RichText::new(lic).small().color(t.text_dim));
                }
            }
        });
    }
    ui.label(dim(
        t,
        "Generative Fill uses an online service, set up in Settings ▸ Generative Fill. Rendering speed and memory use are in Settings ▸ Performance.",
    ));
}

/// The interface page; true when Undo these changes was clicked.
fn interface_page(app: &mut LccApp, ui: &mut Ui) -> bool {
    use crate::state::AppearanceMode;
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.label(dim(&t, "Choose how the app looks. Each change shows at once; Settings ▸ Interface has more."));
    ui.label(RichText::new(tr("Layout")).strong());
    let now = if app.ui.modern.enabled {
        "modern"
    } else if app.ui.settings.compact_layout {
        "compact"
    } else {
        "classic"
    };
    for (k, title, text) in [
        ("classic", "Classic", "Panels on both sides of the photo, like a desktop photo editor."),
        ("modern", "Modern", "The photo first, the tools in a dock: for touch screens and small windows."),
        ("compact", "Compact", "Classic, drawn denser to show more at once."),
    ] {
        let r = ui.radio(now == k, RichText::new(tr(title)).strong());
        register(ui.ctx(), format!("radio:wizardLayout.{k}"), r.rect);
        ui.indent(k, |ui| ui.label(dim(&t, text)));
        if r.clicked() && now != k {
            let _ = match k {
                "modern" => app.run("view.modernLayout", json!({"on": true})),
                "compact" => app.run("view.compactLayout", json!({"on": true})),
                _ => app.run("view.modernLayout", json!({"on": false})).and_then(|_| app.run("view.compactLayout", json!({"on": false}))),
            };
        }
    }
    ui.label(RichText::new(tr("Appearance")).strong());
    ui.horizontal_wrapped(|ui| {
        for (mode, label, m) in [
            ("auto", "Sync with System", AppearanceMode::Auto),
            ("light", "Light Mode", AppearanceMode::Light),
            ("dark", "Dark Mode", AppearanceMode::Dark),
        ] {
            let r = ui.radio(app.ui.settings.appearance_mode == m, tr(label));
            register(ui.ctx(), format!("radio:wizardAppearance.{mode}"), r.rect);
            if r.clicked() {
                let _ = app.run("view.appearance", json!({"mode": mode}));
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        for k in crate::theme::ThemeKind::ALL {
            let s = &app.ui.settings;
            let on = if k.is_dark() { s.dark_theme.kind() == k } else { s.light_theme.kind() == k };
            let r = ui.selectable_label(on, tr(k.label()));
            register(ui.ctx(), format!("button:wizardTheme.{}", k.id()), r.rect);
            if r.clicked() {
                let _ = app.run("view.theme", json!({"theme": k.id()}));
            }
        }
    });
    ui.label(RichText::new(tr("Text size")).strong());
    ui.horizontal_wrapped(|ui| {
        for f in crate::theme::UiFontSize::ALL {
            let r = ui.radio(app.ui.settings.ui_font_size == f, tr(f.label()));
            register(ui.ctx(), format!("radio:wizardFont.{}", f.id()), r.rect);
            if r.clicked() {
                app.ui.settings.ui_font_size = f;
            }
        }
    });
    ui.label(RichText::new(tr("Language")).strong());
    let current = crate::panels::settings::language_label(ui.ctx(), app.ui.language);
    let combo = egui::ComboBox::from_id_salt("wizardLanguage").selected_text(current).width(220.0).height(320.0).show_ui(ui, |ui| {
        for (i, language) in crate::i18n::Locale::for_picker().into_iter().enumerate() {
            let label = crate::panels::settings::language_label(ui.ctx(), language);
            let r = ui.selectable_value(&mut app.ui.language, language, label);
            register(ui.ctx(), format!("button:wizardLanguage-{i}"), r.rect);
        }
    });
    register(ui.ctx(), "combo:wizardLanguage", combo.response.rect);
    crate::i18n::set_language(app.ui.language);
    ui.label(RichText::new(tr("Grading Mode")).strong());
    ui.horizontal_wrapped(|ui| {
        for (m, label) in [(crate::state::GradingMode::Witchcraft, "Witchcraft"), (crate::state::GradingMode::Eboda, "Eboda")] {
            let r = ui.radio(app.ui.settings.grading_mode == m, tr(label));
            register(ui.ctx(), format!("radio:wizardGrading.{}", m.id()), r.rect);
            if r.clicked() {
                let _ = app.run("view.gradingMode", json!({"mode": m.id()}));
            }
        }
    });
    ui.label(dim(
        &t,
        "Witchcraft: Lift, Gamma and Gain wheels and HSL curves. Eboda: one Color Grading panel with shadow, midtone and highlight wheels.",
    ));
    let r = ui.button(tr("Undo these changes"));
    register(ui.ctx(), "button:wizardUndoLook", r.rect);
    r.clicked()
}

fn settings_page(app: &mut LccApp, ui: &mut Ui, t: &crate::theme::Tokens) {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(dir) = app.session.catalog_dir() {
        ui.label(RichText::new(tr("Catalog")).strong());
        ui.label(RichText::new(dir.display().to_string()).color(t.text_label));
    }
    let mut ask = app.ui.settings.at_launch == crate::state::LaunchLibrary::Ask;
    let r = ui.checkbox(&mut ask, tr("Ask which catalog to open each time the app starts"));
    register(ui.ctx(), "check:wizardAskAtLaunch", r.rect);
    if r.changed() {
        let _ = app.run("catalog.promptAtStartup", json!({"on": ask}));
    }
    let prefs = app.session.execute("update.prefs", &json!({})).unwrap_or_default();
    if prefs.get("checkAtStartup").is_some() {
        let mut updates = prefs.get("checkAtStartup").and_then(Value::as_bool).unwrap_or(false);
        let r = ui.checkbox(&mut updates, tr("Check for updates when the app starts"));
        register(ui.ctx(), "check:wizardUpdates", r.rect);
        if r.changed()
            && let Err(e) = app.session.execute("update.prefs", &json!({"checkAtStartup": updates}))
        {
            log::warn!("update.prefs: {e}");
        }
    }
    let mut xmp = app.session.xmp.auto_write;
    let r = ui.checkbox(&mut xmp, tr("Automatically write changes into XMP sidecars"));
    register(ui.ctx(), "check:wizardXmp", r.rect);
    if r.changed() {
        let _ = app.run("library.xmpPreferences", json!({"autoWrite": xmp}));
    }
    ui.indent("xmp", |ui| ui.label(dim(t, "Other apps then see your ratings, keywords and edits. It writes a small file next to each photo.")));
    let r = ui.checkbox(&mut app.ui.settings.reduce_motion, tr("Reduce motion"));
    register(ui.ctx(), "check:wizardReduceMotion", r.rect);
    ui.label(dim(t, "Raw defaults, import options and the memory budget are in Settings, under Import and Performance."));
}

fn tour_page(ui: &mut Ui, t: &crate::theme::Tokens, step: usize) -> Option<&'static str> {
    let (_, text, topic, icon) = TOUR.get(step.saturating_sub(STEP_TOUR))?;
    // a code-drawn picture: the module's icon on a tinted card
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, t.radius(10.0), t.accent.gamma_multiply(0.18));
    let side = 56.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(side, side));
    crate::icons::paint(p, icon_rect, *icon, t.accent);
    for (i, dx) in [-2.6_f32, 2.6].iter().enumerate() {
        let c = rect.center() + egui::vec2(dx * side * 0.55, 0.0);
        let r = egui::Rect::from_center_size(c, egui::vec2(side * 0.7, side * 0.5));
        let tone = if i == 0 { t.text_dim } else { t.text_label };
        p.rect_stroke(r, t.radius(4.0), egui::Stroke::new(1.5, tone.gamma_multiply(0.6)), egui::StrokeKind::Inside);
    }
    ui.label(tr(text));
    let r = ui.button(tr("Read more in the manual"));
    register(ui.ctx(), "button:wizardManual", r.rect);
    r.clicked().then_some(*topic)
}

fn finish_page(ui: &mut Ui, t: &crate::theme::Tokens, w: &Wizard) {
    let chosen: Vec<&AiRow> = w.rows.iter().filter(|r| r.on && !r.installed).collect();
    if chosen.is_empty() {
        ui.label(tr("Nothing will be downloaded."));
    } else {
        ui.label(tr("These downloads start when you click Finish. They run in the background, and the activity list shows how far they are:"));
        for r in chosen {
            let (title, _) = row_text(r.key);
            let size = r.size.map(|s| format!(" ({})", mb(s))).unwrap_or_default();
            ui.label(RichText::new(format!("• {}{size}", tr(title))).color(t.text));
        }
    }
    ui.label(dim(t, "To start, import some photos with File ▸ Import Photos. You can open this guide again from Help ▸ Getting Started…"));
}
