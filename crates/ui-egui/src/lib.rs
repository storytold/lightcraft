//! LightCraft's egui frontend: a Lightroom-style UI over `lightcraft-engine`.
//!
//! The UI is thin: every action goes through [`LightcraftApp::run`], which handles UI commands
//! (views, panels, zoom — see [`menus::ui_commands`]) and forwards everything else to the engine.
//! The same entry point serves menus, shortcuts, buttons and the control channel ([`control`]).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod album_picker;
pub mod control;
pub mod credits;
pub mod date_picker;
pub mod export_task;
pub mod headless;
pub mod i18n;
pub mod icons;
pub mod import;
pub mod lightroom_import;
pub mod links;
pub mod menu_level;
pub mod menubar;
pub mod menus;
pub mod merge;
mod model_setup;
pub mod panels;
pub mod pick;
pub mod region;
pub mod render;
pub mod shortcuts;
pub mod softpaint;
pub mod state;
pub mod sync;
pub mod tasks;
pub mod text_field;
pub mod theme;
pub mod titlebar;
pub mod widgets;

#[cfg(test)]
mod tests_album_picker;
#[cfg(test)]
mod tests_crop_rotate;
#[cfg(test)]
mod tests_curve;
#[cfg(test)]
mod tests_date_picker;
#[cfg(test)]
mod tests_filmstrip;
#[cfg(test)]
mod tests_grid;
#[cfg(test)]
mod tests_keymap;
#[cfg(test)]
mod tests_keyword_files;
#[cfg(test)]
mod tests_keyword_list;
#[cfg(test)]
mod tests_keyword_set;
#[cfg(test)]
mod tests_keywording;
#[cfg(test)]
mod tests_labels;
#[cfg(test)]
mod tests_library_problem;
#[cfg(test)]
mod tests_masking;
#[cfg(test)]
mod tests_masking_layout;
#[cfg(test)]
mod tests_menubar;
#[cfg(test)]
mod tests_offline;
#[cfg(test)]
mod tests_panels;
#[cfg(test)]
mod tests_preview_limit;
#[cfg(test)]
mod tests_quit_unsaved;
#[cfg(test)]
mod tests_scroll;
#[cfg(test)]
mod tests_switch_library;
#[cfg(test)]
mod tests_sync;
#[cfg(test)]
mod tests_titlebar;
#[cfg(test)]
mod tests_unsaved;
#[cfg(test)]
mod tests_zoom;
#[cfg(test)]
mod tests_zoom_keys;

use std::sync::mpsc::{Receiver, Sender};

use lightcraft_engine::Session;
use serde_json::Value;

pub use control::{ControlRequest, ControlResponse};
pub use state::UiState;

const TOAST_SECONDS: f64 = 1.4;

pub type PickFiles = Box<dyn FnMut() -> Vec<String>>;
/// A save dialog: suggested file name → chosen path (`None` = cancelled).
pub type SaveFile = Box<dyn FnMut(&str) -> Option<String>>;
pub type WriteFn = Box<dyn FnMut(&str, &[u8]) -> Result<(), String>>;
/// A writer other threads can use (background export).
pub type SharedWrite = std::sync::Arc<dyn Fn(&str, &[u8]) -> Result<(), String> + Send + Sync>;
pub type PngEncode = Box<dyn Fn(&lightcraft_raster::Rgba8) -> Vec<u8>>;
/// A folder chooser (`None` = cancelled).
pub type PickFolder = Box<dyn FnMut() -> Option<String>>;
/// Reveal a file in the system file manager (Finder / Explorer / the folder on Linux).
pub type RevealFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Open a URL in the user's browser.
pub type OpenUrlFn = Box<dyn FnMut(&str) -> Result<(), String>>;
/// Open a file in an application (`app` = "" for the system's default one).
pub type OpenWithFn = Box<dyn FnMut(&str, &str) -> Result<(), String>>;
/// Something the host does with the session (web: back up or restore the library in browser
/// storage); the work may finish asynchronously.
pub type HostAction = Box<dyn FnMut(&mut Session) -> Result<Value, String>>;

/// Platform services injected by the host app (desktop or web).
#[derive(Default)]
pub struct Services {
    /// Native file dialogs for commands, shown off the UI thread and answered later
    /// (`pick`; #191). With it, the synchronous pickers below serve only the buttons inside
    /// dialogs; without it (the web, tests) they answer the commands too.
    pub picker: Option<pick::Picker>,
    /// Show an open dialog for photos; returns paths.
    pub pick_files: Option<PickFiles>,
    /// Local ONNX model; a manifest must accompany it.
    pub pick_denoise_model: Option<PickFiles>,
    /// Open dialog for a face model (`.onnx`; Settings ▸ Faces ▸ Add a model file…).
    pub pick_model_file: Option<PickFiles>,
    /// Open dialog for preset files (`.lcpreset`, `.xmp`, `.lrtemplate`, `.zip`, `.dng`, Luminar `.lmp` / `.mplumpack`).
    pub pick_preset_files: Option<PickFiles>,
    /// Open dialog for a GPS track log (`.gpx`; Photo ▸ Auto-Tag from Tracklog…).
    pub pick_tracklog: Option<PickFiles>,
    /// Open dialog for a monitor ICC profile (`.icc` / `.icm`; Settings ▸ Display; desktop only).
    pub pick_display_profile: Option<PickFiles>,
    /// Save dialog for an exported `.lcpreset` file.
    pub save_preset_file: Option<SaveFile>,
    /// Open dialog for a keyword list file (`.txt`, Photo Supreme's `.utf8`; File ▸ Import Keywords…).
    pub pick_keyword_list: Option<PickFiles>,
    /// Save dialog for an exported keyword list (File ▸ Export Keywords…).
    pub save_keyword_list: Option<SaveFile>,
    /// Open dialog for point-curve preset files (`.lccurve`).
    pub pick_curve_preset_files: Option<PickFiles>,
    /// Save dialog for an exported `.lccurve` file.
    pub save_curve_preset_file: Option<SaveFile>,
    pub write: Option<WriteFn>,
    /// Thread-safe writer: with it, UI-started exports run in the background (desktop only).
    pub write_shared: Option<SharedWrite>,
    /// PNG encoder (the host links an image encoder; the UI crate stays codec-free).
    pub png: Option<PngEncode>,
    /// Show a file in the system file manager (desktop only).
    pub reveal: Option<RevealFn>,
    /// The host's current log file (`<settings>/logs/lightcraft.log`), which Help ▸ Open Log
    /// Folder reveals (#260). None where no log is kept: the web, `--memory`, `LIGHTCRAFT_NO_PREFS`.
    pub log_file: Option<String>,
    /// Choose a folder (Settings → General → Open Library…; desktop only).
    pub pick_folder: Option<PickFolder>,
    /// Open a Lightroom Classic `.lrcat` catalog for read-only import.
    pub pick_lightroom_catalog: Option<PickFolder>,
    /// Open a web link in the browser (Help menu, About, Discord button).
    pub open_url: Option<OpenUrlFn>,
    /// Open a file in an external editor (Edit in External Editor; desktop only).
    pub open_with: Option<OpenWithFn>,
    /// File ▸ Back Up Library…: save the whole library (catalog and originals) as one file the
    /// user keeps (web only: there the library lives in browser storage, which the browser may
    /// clear; on the desktop it is a folder backed up like any other).
    pub backup_library: Option<HostAction>,
    /// File ▸ Restore Library from Backup… (web only; keeps the current library).
    pub restore_library: Option<HostAction>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Perf {
    /// Layout of the last frame ([`LightcraftApp::ui`], including commands run from widgets).
    pub frame_ms: f64,
    /// Per-frame logic before layout ([`LightcraftApp::logic`]: control channel, shortcuts,
    /// render polling, pending catalog persistence).
    pub logic_ms: f64,
    /// The whole update of the last frame: logic + layout.
    pub update_ms: f64,
    /// The slowest whole update since start.
    pub max_update_ms: f64,
    pub fps: f64,
}

pub struct LightcraftApp {
    pub(crate) model_setup: model_setup::Pending,
    /// Per-catalog-revision caches of library-wide results the panels show every frame
    /// (expensive on big libraries).
    pub caches: Caches,
    pub session: Session,
    pub ui: UiState,
    pub services: Services,
    pub renderer: render::Renderer,
    pub perf: Perf,
    /// macOS: the host draws the traffic lights over our top bar.
    pub integrated_titlebar: bool,
    /// The host installed a native menu bar (no in-window menus then).
    pub native_menu: bool,
    /// Shortcuts the native menu bar currently handles (`Cmd+Z`, `G`…): the egui shortcut handler
    /// leaves them alone so nothing fires twice.
    pub native_shortcuts: std::collections::HashSet<String>,
    /// The keyboard shortcuts editor is waiting for a key press for this command: no shortcut
    /// fires (the native menu bar drops its accelerators too) until it gets one or is cancelled.
    pub recording_shortcut: Option<String>,
    /// The host is [`headless::Headless`] (it answers viewport screenshot commands itself).
    pub headless_host: bool,
    /// Warnings to show one at a time (damaged settings files…, issue #103).
    pub notices: Vec<String>,
    /// Quitting was stopped because changes couldn't be saved: the prompt's text.
    pub quit_prompt: Option<String>,
    /// Quit Anyway was chosen: the window may close with unsaved changes.
    pub quit_confirmed: bool,
    control_rx: Option<Receiver<ControlRequest>>,
    pending_screenshots: Vec<PendingShot>,
    screenshot_token: u64,
    /// Offscreen context for headless screenshots of the windowed app.
    shadow: Option<headless::HeadlessView>,
    /// Synthetic input events (from the control channel) injected one step per frame.
    pub synthetic: Vec<egui::Event>,
    /// The text the UI last put on the clipboard (`ui.inspect` → `copied`).
    pub copied: Option<String>,
    /// Native file dialogs up for commands (`pick`): each command runs again when its closes.
    pub(crate) pending_picks: Vec<pick::Pending>,
    /// Modifiers announced for synthetic input (held from a button down to its release).
    synthetic_mods: egui::Modifiers,
    /// Clear `synthetic_mods` on the next frame.
    synthetic_mods_release: bool,
    styled: bool,
    /// The language the installed fonts were built for: the CJK fallback order follows the UI
    /// language's script, so switching language reinstalls them.
    font_language: i18n::Locale,
    /// Browser font loaded separately from WASM so the module stays below its host's size limit.
    chinese_font: Option<std::sync::Arc<egui::FontData>>,
    fonts_ready: bool,
    last_time: f64,
    /// Rect of the photo canvas and the displayed image (screen points) from the last frame.
    pub canvas_rect: Option<egui::Rect>,
    pub image_rect: Option<egui::Rect>,
    /// Scroll offsets (points) of the photo grid (vertical) and the filmstrip (horizontal) as
    /// drawn last (`ui.inspect` → `scroll`).
    pub grid_scroll: Option<f32>,
    pub film_scroll: Option<f32>,
    /// Widget registry from the last frame (automation ids → rects).
    pub widgets: Vec<(String, egui::Rect)>,
    /// In-progress on-canvas gesture (brush stroke points, gradient drag…).
    pub gesture: Option<panels::detail::Gesture>,
    /// What the loupe drew last frame: photo and source ("render", "cached", "embedded", "small",
    /// "thumb", "none").
    pub loupe_shown: Option<(lightcraft_catalog::PhotoId, &'static str)>,
    /// The window render the loupe asked for last, by job key (see [`region`]); kept for the few
    /// windows whose textures can be on screen, and for the inspector.
    pub region_view: Option<region::RegionView>,
    /// The same for the Before side of a Before/After view.
    pub region_before_view: Option<region::RegionView>,
    /// The loupe's render sizes while a pinch or two-finger scroll runs.
    pub(crate) size_hold: region::SizeHold,
    /// (photo, look, window frame size) a window was refused for: it reads more than one render holds.
    pub(crate) window_refused: Option<(lightcraft_catalog::PhotoId, u64, usize)>,
    pub(crate) region_tiles: std::collections::HashMap<(bool, u64), region::RegionView>,
    /// Photo Merge dialog previews and background merges.
    pub merge: merge::MergeState,
    /// An import in progress (the import review dialog's batches).
    pub import: Option<import::ImportTask>,
    /// A folder scan in progress (feeds the import review).
    pub scan: Option<import::ScanTask>,
    /// A Synchronize Folder scan in progress (feeds its dialog).
    pub sync: Option<sync::SyncTask>,
    /// `session.folder_changes` came from the Synchronize Folder dialog (and goes with it).
    pub sync_owns_changes: bool,
    /// A Synchronize Folder at work in the background.
    pub sync_run: Option<sync::SyncRun>,
    /// A Lightroom catalog inspect/import in progress.
    pub lightroom: Option<lightroom_import::LightroomTask>,
    /// Last terminal Lightroom result, exposed by the command's status/wait response.
    pub lightroom_last: Option<Value>,
    /// A background export in progress.
    pub export: Option<export_task::ExportTask>,
    /// Background file-system work of other commands (Find Missing Photos, auto import…).
    pub tasks: tasks::Tasks,
    /// The files of the last finished background export (`ui.inspect` → `export.last`).
    pub last_export_result: Option<Value>,
    /// The look the loupe shows while the pointer rests on a preset or profile (set by the
    /// panels each frame; nothing is committed, no history entry).
    pub hover_preview: Option<HoverPreview>,
    /// The window is in full screen (as last reported by the host, or as last requested).
    pub window_is_fullscreen: bool,
    /// The GPU preference last applied (`app.gpu`), to apply Settings changes once.
    gpu_applied: Option<bool>,
    /// The memory budget setting last applied (MB, 0 = automatic).
    memory_applied: Option<u32>,
    /// The display profile setting last applied (`app.displayProfile`).
    pub(crate) display_applied: Option<String>,
    /// Why the display profile setting could not be applied (shown in Settings ▸ Display).
    pub display_error: Option<String>,
    /// The library failed to open at launch: the blocking window, then the temporary-session
    /// banner (issue #100). Cleared once a library opens.
    pub library_problem: Option<panels::library_problem::LibraryProblem>,
}

impl LightcraftApp {
    pub fn new(mut session: Session, services: Services) -> Self {
        // AI mask requests run on the model's worker; frames apply their results (never wait)
        session.segmenter.background = true;
        Self {
            session,
            ui: UiState::default(),
            services,
            renderer: render::Renderer::default(),
            perf: Perf::default(),
            caches: Caches::default(),
            integrated_titlebar: false,
            native_menu: false,
            native_shortcuts: Default::default(),
            recording_shortcut: None,
            headless_host: false,
            notices: vec![],
            quit_prompt: None,
            quit_confirmed: false,
            control_rx: None,
            pending_screenshots: vec![],
            screenshot_token: 0,
            shadow: None,
            synthetic: vec![],
            copied: None,
            pending_picks: vec![],
            synthetic_mods: egui::Modifiers::NONE,
            synthetic_mods_release: false,
            styled: false,
            font_language: i18n::Locale::En,
            chinese_font: None,
            fonts_ready: false,
            last_time: 0.0,
            canvas_rect: None,
            image_rect: None,
            grid_scroll: None,
            film_scroll: None,
            widgets: vec![],
            gesture: None,
            loupe_shown: None,
            region_view: None,
            region_before_view: None,
            size_hold: Default::default(),
            window_refused: None,
            region_tiles: Default::default(),
            merge: merge::MergeState::default(),
            import: None,
            scan: None,
            sync: None,
            sync_owns_changes: false,
            sync_run: None,
            lightroom: None,
            lightroom_last: None,
            export: None,
            tasks: Default::default(),
            last_export_result: None,
            hover_preview: None,
            window_is_fullscreen: false,
            gpu_applied: None,
            memory_applied: None,
            display_applied: None,
            display_error: None,
            library_problem: None,
            model_setup: Default::default(),
        }
    }

    /// Supply the browser's separately downloaded Simplified Chinese font before the first frame.
    pub fn with_chinese_font(mut self, bytes: Vec<u8>) -> Self {
        if !bytes.is_empty() {
            self.chinese_font = Some(std::sync::Arc::new(egui::FontData::from_owned(bytes)));
        }
        self
    }

    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        // keep CPU copies of photo textures so `ui.screenshot {"headless": true}` can draw them
        self.renderer.keep_pixels = true;
        self
    }

    /// Run a UI or engine command by id. The single entry point for every frontend path.
    pub fn run(&mut self, id: &str, params: Value) -> Result<Value, String> {
        if let Some(result) = model_setup::intercept(self, id, &params) {
            return result;
        }
        if let Some(r) = menus::run_ui_command(self, id, &params) {
            return r;
        }
        let r = self.session.execute(id, &params).map_err(|e| e.to_string());
        if r.is_ok() && id == "mask.adjust" {
            // Judge local adjustments on the photo, without the selection overlay obscuring them.
            // Keep it hidden after release; O / the overlay eye can show it again.
            self.ui.mask_overlay = false;
        }
        if r.is_ok() && id == "photo.label" {
            let label = params.get("label").and_then(Value::as_str).and_then(lightcraft_catalog::ColorLabel::parse);
            let text = match label {
                Some(label) => {
                    let name =
                        self.session.catalog.custom_label_name(label).map(str::to_owned).unwrap_or_else(|| i18n::tr(&format!("{label:?}")).into());
                    i18n::tr_format!("{name} Label", name = name)
                }
                None => i18n::tr("Color label cleared").into(),
            };
            // Native menu clicks can arrive before logic() updates last_time after an idle gap.
            let now = self.tasks.repaint.as_ref().map(|ctx| ctx.input(|i| i.time)).unwrap_or(self.last_time);
            self.ui.toast = Some((text, now + TOAST_SECONDS, label));
        }
        match &r {
            Err(e) => {
                log::warn!("{id}: {e}");
                self.ui.status = e.clone();
                // no detector yet: offer it (its terms first) instead of only saying so
                if id == "faces.detect" && e.contains("Settings > Faces") {
                    panels::faces::offer_detector(self);
                }
            }
            Ok(v) if id == "faces.detect" => self.ui.status = detect_summary(v),
            Ok(_) => {}
        }
        r
    }

    /// Show a transient toast at the bottom of the canvas (like the reference app's HUD).
    /// Advance a running slideshow (wrapping around at the end).
    fn slideshow_tick(&mut self, ctx: &egui::Context) {
        let Some((interval, due, paused)) = self.ui.slideshow else { return };
        if !self.ui.fullscreen {
            self.ui.slideshow = None;
            return;
        }
        if paused {
            return;
        }
        let now = ctx.input(|i| i.time);
        if now >= due {
            let vis = self.session.visible_cloned();
            if let Some(cur) = self.session.active()
                && !vis.is_empty()
            {
                let i = vis.iter().position(|x| *x == cur).map_or(0, |i| (i + 1) % vis.len());
                let _ = self.run("library.select", serde_json::json!({"ids": [vis[i].0]}));
            }
            self.ui.slideshow = Some((interval, now + interval, false));
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64((due - now).clamp(0.05, interval)));
    }

    /// Announce the start and end of a Build Previews run.
    fn preview_build_status(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;
        let Some(b) = self.session.preview_build.clone() else { return };
        let key = std::sync::Arc::as_ptr(&b) as usize;
        if b.finished.load(Ordering::Relaxed) {
            if self.ui.preview_build_seen != Some((key, true)) {
                self.ui.preview_build_seen = Some((key, true));
                let (done, failed) = (b.done.load(Ordering::Relaxed), b.failed.load(Ordering::Relaxed));
                let plural = if done == 1 { "" } else { "s" };
                let mut msg = match (b.error(), b.what) {
                    (Some(e), what) => format!("{}: {e}", if what.is_empty() { "previews" } else { what }),
                    (None, "") => crate::i18n::tr_format!("Previews ready for {done} photo{plural}", done = done, plural = plural),
                    (None, what) => {
                        crate::i18n::tr_format!("Done ({what}): {done} photo{plural}", done = done, plural = plural, what = crate::i18n::tr(what))
                    }
                };
                if failed > 0 {
                    msg.push_str(&crate::i18n::tr_format!(" · {failed} couldn't be rendered", failed = failed));
                }
                self.toast(ctx, msg);
            }
        } else {
            if self.ui.preview_build_seen != Some((key, false)) {
                self.ui.preview_build_seen = Some((key, false));
                let msg = match b.what {
                    "" => crate::i18n::tr_format!("Building previews for {} photos…", b.total),
                    what => crate::i18n::tr_format!("Working on {what} for {} photos…", b.total, what = crate::i18n::tr(what)),
                };
                self.toast(ctx, msg);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// Announce when saving the library starts failing (changes then live only in memory and are
    /// retried) and when it works again; the top bar's cloud icon shows the state meanwhile.
    fn save_status(&mut self, ctx: &egui::Context) {
        let unsaved = self.session.unsaved().map(|(n, e)| (n, e.to_string()));
        match (unsaved, self.ui.unsaved_seen) {
            (Some((n, e)), false) => {
                self.ui.unsaved_seen = true;
                let t = ctx.input(|i| i.time);
                self.ui.toast = Some((
                    crate::i18n::tr_format!(
                        "{n} change{} saved in memory but not written to disk: {e} — LightCraft will retry",
                        if n == 1 { "" } else { "s" },
                        n = n,
                        e = e
                    ),
                    t + 6.0,
                    None,
                ));
            }
            (None, true) => {
                self.ui.unsaved_seen = false;
                self.toast(ctx, "Library saved");
            }
            (Some(_), true) => ctx.request_repaint_after(std::time::Duration::from_secs(2)), // retry
            (None, false) => {}
        }
    }

    pub fn toast(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        self.toast_for(ctx, text, TOAST_SECONDS);
    }

    /// A toast that stays `secs` seconds (messages that say where to look or what to do next).
    pub fn toast_for(&mut self, ctx: &egui::Context, text: impl Into<String>, secs: f64) {
        let t = ctx.input(|i| i.time);
        self.ui.toast = Some((text.into(), t + secs, None));
    }

    /// AI masks: apply finished background requests (clicks, descriptions, detail passes) and
    /// show their errors; start a zoomed-in detail pass once the clicking stops
    /// (`ui.detail_due`); watch the model download. Never waits for the model.
    fn ai_mask_detail(&mut self, ctx: &egui::Context) {
        let polled = self.session.segment_poll();
        let now = ctx.input(|i| i.time);
        if polled.changed {
            ctx.request_repaint();
        }
        if let Some(mask) = polled.refine {
            self.ui.detail_due = Some((now + 0.3, mask));
        }
        if let Some(e) = polled.messages.into_iter().last() {
            self.ai_error(ctx, e, None);
        }
        let seg = &self.session.segmenter;
        if seg.busy() || seg.pending_clicks().is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        let download = seg.download_status();
        if download.running {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // a download finishing while its dialog is closed: say so once
        if self.ui.sam_downloading && !download.running {
            self.ui.sam_downloading = false;
            let open = matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. }));
            match (&download.error, open) {
                (_, true) => {}
                (Some(e), false) if e.contains("cancelled") => {
                    self.toast(ctx, crate::i18n::tr("SAM 3 download stopped: it resumes where it left off next time."))
                }
                (Some(e), false) => self.toast_error(ctx, crate::i18n::tr_format!("The SAM 3 download failed: {e}", e = e)),
                (None, false) if download.finished => {
                    self.toast_error(ctx, crate::i18n::tr("The SAM 3 model is installed: Object and Describe masks are ready."))
                }
                (None, false) => {}
            }
        }
        if let Some((due, mask)) = self.ui.detail_due {
            if now >= due && !self.session.segmenter.busy() && !self.session.segmenter.detail_busy() {
                self.ui.detail_due = None;
                if let Err(e) = self.run("mask.refineDetail", serde_json::json!({"id": mask})) {
                    log::warn!("detail pass: {e}");
                }
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(150));
            }
        }
        if self.session.segmenter.detail_busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// An AI mask error: when the model isn't installed, the dialog that offers to download it
    /// (`then`: the AI mask to start afterwards); otherwise a toast.
    pub fn ai_error(&mut self, ctx: &egui::Context, e: impl Into<String>, then: Option<(&str, &str)>) {
        let e = e.into();
        if lightcraft_engine::segment::Segmenter::AVAILABLE && e.starts_with(lightcraft_engine::segment::NOT_INSTALLED) {
            self.offer_sam_download(then);
        } else {
            self.toast_error(ctx, e);
        }
    }

    /// Open the dialog offering the SAM 3 download (never downloads by itself).
    pub fn offer_sam_download(&mut self, then: Option<(&str, &str)>) {
        if self.ui.dialog.is_none() || matches!(self.ui.dialog, Some(state::Dialog::SamModel { .. })) {
            self.ui.dialog = Some(state::Dialog::SamModel { then: then.map(|(k, o)| (k.to_string(), o.to_string())), error: None });
        }
    }

    /// A toast for an error the user has to read and act on (stays 6 s).
    pub fn toast_error(&mut self, ctx: &egui::Context, text: impl Into<String>) {
        let t = ctx.input(|i| i.time);
        self.ui.toast = Some((text.into(), t + 6.0, None));
    }

    fn drain_control(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            let reply = req.reply.clone();
            match control::handle(self, ctx, &req) {
                control::Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                control::Outcome::Screenshot { path, headless } => {
                    self.screenshot_token += 1;
                    let now = now_ms();
                    self.pending_screenshots.push(PendingShot {
                        token: self.screenshot_token,
                        path,
                        reply,
                        headless: headless && !self.headless_host,
                        not_before: now + 120.0,
                        deadline: now + SCREENSHOT_SETTLE_MS,
                        frames: 0,
                        sent_at: None,
                    });
                }
            }
        }
        self.control_rx = Some(rx);
    }

    /// Advance pending screenshots: wait (≥ 3 frames, ≥ 120 ms) until no renders are in flight
    /// (or a timeout), then capture — via the host's compositor, or headlessly
    /// ([`Self::headless_screenshot`]) when asked or when the compositor delivers nothing.
    fn issue_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let now = now_ms();
        let busy = self.renderer.in_flight() > 0 || self.merge.busy();
        let mut shots = std::mem::take(&mut self.pending_screenshots);
        let mut shadow_ticked = false;
        shots.retain_mut(|s| {
            if let Some(sent) = s.sent_at {
                if now - sent < SCREENSHOT_FALLBACK_MS || self.headless_host {
                    return true;
                }
                // The compositor delivered nothing (display asleep, window occluded): go headless.
                log::warn!("ui.screenshot: no frame from the compositor after {SCREENSHOT_FALLBACK_MS} ms; rendering headlessly");
                s.sent_at = None;
                s.headless = true;
                s.deadline = now + SCREENSHOT_SETTLE_MS;
            }
            s.frames += 1;
            let ready = now >= s.not_before && s.frames >= 3 && (!busy || now > s.deadline);
            if s.headless {
                // The shadow frame requests the renders the UI needs, even while the window shows nothing.
                let img = if ready || !shadow_ticked { self.headless_screenshot(ctx, ready) } else { None };
                shadow_ticked = true;
                if let Some(img) = img {
                    let _ = s.reply.send(control::save_screenshot(self, &img, s.path.as_deref()));
                    return false;
                }
                true
            } else {
                if ready {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::new(s.token)));
                    s.sent_at = Some(now);
                }
                true
            }
        });
        shots.append(&mut self.pending_screenshots);
        self.pending_screenshots = shots;
        if !self.pending_screenshots.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
    }

    /// Draw the UI into an offscreen context at the window's size; with `capture`, rasterize it
    /// on the CPU (photo textures come from the renderer's CPU copies).
    pub fn headless_screenshot(&mut self, main: &egui::Context, capture: bool) -> Option<egui::ColorImage> {
        let mut view = self.shadow.take().unwrap_or_default();
        let size = main.input(|i| i.content_rect()).size();
        let size = if size.x >= 1.0 && size.y >= 1.0 { size } else { egui::vec2(1600.0, 1000.0) };
        // Feed native viewport points; the shadow context applies the copied UI zoom.
        let zoom = main.zoom_factor();
        let size = size * zoom;
        let ppp = main.pixels_per_point() / zoom;
        let time = main.input(|i| i.time);
        if view.frames() == 0 {
            // warm-up pass: activates our fonts (pending font definitions live in `Memory`, which
            // is replaced below)
            view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |_| {});
        }
        // same scroll offsets, open sections, style… as the window
        let memory = main.memory(|m| m.clone());
        view.ctx.memory_mut(|m| *m = memory);
        view.run(headless::HeadlessView::raw_input(size, ppp, time, vec![]), |ui| self.ui(ui));
        let img = capture.then(|| {
            let mut tex = self.renderer.cpu_textures();
            if let (Some((t, ..)), Some(px)) = (&self.merge.preview, &self.merge.preview_pixels) {
                tex.insert(t.id(), crate::softpaint::CpuTexture::linear(px.clone()));
            }
            view.paint(&tex)
        });
        self.shadow = Some(view);
        img
    }

    fn collect_screenshots(&mut self, ctx: &egui::Context) {
        if self.pending_screenshots.is_empty() {
            return;
        }
        let events: Vec<_> = ctx.input(|i| {
            i.raw
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Screenshot { user_data, image, .. } => {
                        let token = user_data.data.as_ref().and_then(|d| d.downcast_ref::<u64>()).copied()?;
                        Some((token, image.clone()))
                    }
                    _ => None,
                })
                .collect()
        });
        for (token, image) in events {
            if let Some(i) = self.pending_screenshots.iter().position(|s| s.token == token) {
                let s = self.pending_screenshots.remove(i);
                let _ = s.reply.send(control::save_screenshot(self, &image, s.path.as_deref()));
            }
        }
    }

    /// The process is about to end (settings and library are saved by then): make sure nothing is
    /// inside the GPU driver when it does (issue #620: a render running on a worker thread while
    /// the process exits crashes in the driver). No GPU work starts from here on; the render
    /// workers get up to `timeout` to finish their jobs, and what is left of it goes to GPU work
    /// on other threads (export, denoise, the device warm-up). `false`: the deadline passed first
    /// (the host exits anyway). The app renders nothing afterwards.
    pub fn shutdown(&mut self, timeout: std::time::Duration) -> bool {
        lightcraft_engine::gpu::begin_shutdown();
        #[cfg(not(target_arch = "wasm32"))]
        let t0 = std::time::Instant::now();
        let stopped = self.renderer.shutdown(timeout);
        #[cfg(not(target_arch = "wasm32"))]
        let timeout = timeout.saturating_sub(t0.elapsed());
        let idle = lightcraft_engine::gpu::wait_idle(timeout);
        stopped && idle
    }

    /// Per-frame logic before layout (control channel, renders, shortcuts, drops).
    pub fn logic(&mut self, ctx: &egui::Context) {
        i18n::set_language(self.ui.language);
        let t0 = now_ms();
        panels::library_problem::logic(self);
        self.logic_inner(ctx);
        self.perf.logic_ms = now_ms() - t0;
    }

    fn logic_inner(&mut self, ctx: &egui::Context) {
        if !self.styled {
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            theme::apply(ctx);
            // File → Add from Device lists cards scanned in the background: show hot-plugs
            let repaint = ctx.clone();
            lightcraft_engine::devices::on_change(move || repaint.request_repaint());
            // "is the original there?" (grid, Info panel, Missing Photos) answers from a cache a
            // worker fills: a sleeping NAS or a dropped share never stalls a frame
            let repaint = ctx.clone();
            self.session.media.availability.run_in_background(std::sync::Arc::new(move || repaint.request_repaint()));
            self.styled = true;
            self.font_language = self.ui.language;
        } else if self.font_language != self.ui.language {
            // Shared Han characters take the active language's forms (Japanese faces for 日本語,
            // the Simplified Chinese face for 简体中文): rebuild the fallback order.
            theme::install_fonts_with_chinese(ctx, self.chinese_font.as_ref());
            self.font_language = self.ui.language;
        } else {
            self.fonts_ready = true;
        }
        panels::notices::logic(self);
        // closing the window (or Quit) with changes only in memory: retry, else ask first
        if ctx.input(|i| i.viewport().close_requested()) && !panels::notices::may_close(self) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let now = ctx.input(|i| i.time);
        let dt = now - self.last_time;
        if dt > 0.0 {
            self.perf.fps = self.perf.fps * 0.9 + (1.0 / dt).min(240.0) * 0.1;
        }
        self.last_time = now;
        self.drain_control(ctx);
        self.apply_settings(ctx);
        if !self.synthetic.is_empty() {
            ctx.request_repaint();
        }
        self.renderer.poll(ctx, &mut self.session);
        merge::poll(self, ctx);
        import::poll_scan(self, ctx);
        sync::poll(self, ctx);
        sync::poll_run(self, ctx);
        import::tick(self, ctx);
        lightroom_import::tick(self, ctx);
        tasks::poll(self, ctx);
        self.preview_build_status(ctx);
        self.save_status(ctx);
        self.slideshow_tick(ctx);
        panels::faces::pump(self, ctx);
        // back from an external editor: pick up the files it saved
        let focused = ctx.input(|i| i.focused);
        if focused && !self.ui.was_focused && !self.ui.external_edits.is_empty() {
            let ids = self.ui.external_edits.clone();
            if let Ok(r) = self.session.execute("photo.reload", &serde_json::json!({"ids": ids}))
                && r["reloaded"].as_array().is_some_and(|a| !a.is_empty())
            {
                self.toast(ctx, "Updated the edits saved in the external editor");
            }
        }
        self.ui.was_focused = focused;
        // auto import: list the watched folder every few seconds (on a worker thread: it may be on
        // a network share), then import what's new like any import (also on a worker thread)
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(folder) = self.session.import_defaults.auto_folder.clone() {
            const LABEL: &str = "Auto Import";
            let now = ctx.input(|i| i.time);
            if now - self.ui.auto_import_at >= 3.0
                && self.import.is_none()
                && self.sync_run.is_none()
                && self.lightroom.is_none()
                && !self.tasks.is_running(LABEL)
            {
                self.ui.auto_import_at = now;
                let work = move || lightcraft_engine::cmd::library::list_auto_import_folder(&folder);
                let done = |app: &mut LightcraftApp, _ctx: &egui::Context, listing: Result<Vec<(String, u64)>, String>| {
                    let listing = match listing {
                        Ok(l) => l,
                        Err(e) => return log::debug!("auto import: {e}"),
                    };
                    let p = serde_json::json!({"listing": listing, "start": false});
                    let Ok(r) = app.session.execute("library.autoImportScan", &p) else { return };
                    let Some(mut params) = r.get("import").cloned().filter(|_| app.import.is_none()) else { return };
                    let paths: Vec<String> =
                        params["paths"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
                    if let Some(o) = params.as_object_mut() {
                        o.remove("paths");
                    }
                    let undo0 = app.session.undo.len();
                    app.import = Some(import::ImportTask::new(paths, params, undo0, false).auto());
                };
                if let Err(e) = tasks::spawn(self, LABEL, work, done) {
                    log::warn!("{e}");
                }
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(3));
        }
        self.session.persist_if_dirty();
        panels::denoise::pump(self, ctx);
        model_setup::pump(self, ctx);
        panels::faces::pump(self, ctx);
        self.collect_screenshots(ctx);
        self.issue_screenshots(ctx);
        if self.fonts_ready {
            shortcuts::handle(self, ctx);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let dropped: Vec<String> =
                ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().to_string()).filter(|p| !p.is_empty()).collect());
            // preset files import as presets, everything else as photos
            // a dropped face model opens its licence dialog; presets and photos as before
            let (models, dropped): (Vec<String>, Vec<String>) = dropped.into_iter().partition(|p| is_model_file(p));
            if let Some(model) = models.first()
                && let Err(e) = panels::faces::open_dialog(self, model)
            {
                self.toast(ctx, e);
            }
            let (presets, photos): (Vec<String>, Vec<String>) = dropped.into_iter().partition(|p| is_preset_file(p));
            if !presets.is_empty() {
                let _ = self.run("file.importPresets", serde_json::json!({"paths": presets}));
            }
            // read and added on a worker thread (dropped folders can be large, or on a slow drive)
            if !photos.is_empty() {
                if self.lightroom.is_some() {
                    self.toast(ctx, "Finish Lightroom catalog import before adding photos");
                } else if let Err(e) = import::start_paths(self, photos) {
                    self.toast(ctx, e);
                }
            }
        }
    }

    /// Apply settings that act outside the UI state: GPU rendering and window full screen.
    fn apply_settings(&mut self, ctx: &egui::Context) {
        if self.gpu_applied != Some(self.ui.settings.gpu) {
            self.gpu_applied = Some(self.ui.settings.gpu);
            let _ = self.session.execute("app.gpu", &serde_json::json!({"enabled": self.ui.settings.gpu}));
            // GPU device + kernels off the UI thread, once the window is up and only when GPU
            // rendering is on: a broken driver must not keep the window from appearing (issue #136)
            if self.ui.settings.gpu {
                lightcraft_engine::gpu::warm_up();
            }
        }
        let mb = self.ui.settings.memory_mb;
        // automatic at startup: leave the engine's default alone
        if self.memory_applied != Some(mb) && (mb > 0 || self.memory_applied.is_some()) {
            let mb = if mb == 0 { (lightcraft_engine::memory::default_budget() >> 20) as u32 } else { mb };
            let _ = self.session.execute("app.memoryBudget", &serde_json::json!({"mb": mb}));
        }
        self.memory_applied = Some(mb);
        // the monitor profile (Settings ▸ Display): previews follow it from the next frame
        if self.display_applied.as_ref() != Some(&self.ui.settings.display_profile) {
            let path = self.ui.settings.display_profile.clone();
            match lightcraft_engine::display::Display::load_opt(&path) {
                Ok(d) => {
                    self.renderer.set_display(d);
                    self.display_error = None;
                }
                Err(msg) => {
                    // a profile that can't be used: previews are sRGB, the setting is kept
                    self.renderer.set_display(None);
                    log::warn!("display profile: {msg}");
                    self.toast_error(ctx, format!("{}: {msg}", crate::i18n::tr("Display profile not used")));
                    self.display_error = Some(msg);
                }
            }
            self.display_applied = Some(path);
        }
        if let Some(fs) = ctx.input(|i| i.viewport().fullscreen) {
            self.window_is_fullscreen = fs;
        }
        if let Some(on) = self.ui.window_fullscreen.take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
            self.window_is_fullscreen = on;
        }
    }

    /// Inject synthetic events (pointer events one per frame; key sequences up to the release).
    pub fn raw_input_hook(&mut self, raw: &mut egui::RawInput) {
        // the frame after a synthetic release / key: modifiers back up
        if std::mem::take(&mut self.synthetic_mods_release) && self.synthetic_mods != egui::Modifiers::NONE {
            self.synthetic_mods = egui::Modifiers::NONE;
            raw.events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        }
        if self.synthetic.is_empty() {
            return;
        }
        let n = match self.synthetic[0] {
            egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } | egui::Event::MouseWheel { .. } => 1,
            _ => self.synthetic.iter().position(|e| matches!(e, egui::Event::Key { pressed: false, .. })).map_or(self.synthetic.len(), |i| i + 1),
        };
        if let Some(egui::Event::PointerMoved(p) | egui::Event::PointerButton { pos: p, .. }) = self.synthetic.first() {
            raw.events.push(egui::Event::PointerMoved(*p));
        }
        // egui's `input.modifiers` follow `ModifiersChanged` events, not the modifiers carried by
        // pointer/key events: announce the injected events' modifiers (held from button down to
        // the frame after up), so ⌥-drag, ⇧-click … work through the control channel
        let carried = self.synthetic[..n].iter().find_map(|e| match e {
            egui::Event::PointerButton { pressed, modifiers, .. } => Some((*modifiers, !pressed)),
            egui::Event::Key { modifiers, .. } => Some((*modifiers, true)),
            _ => None,
        });
        if let Some((m, ends)) = carried {
            if m != self.synthetic_mods {
                self.synthetic_mods = m;
                raw.events.push(egui::Event::ModifiersChanged(m));
            }
            self.synthetic_mods_release = ends;
        }
        raw.events.extend(self.synthetic.drain(..n));
    }

    /// Frame timings once layout is done (`t0`: when layout started).
    fn end_frame(&mut self, ctx: &egui::Context, t0: f64) {
        ctx.output(|o| {
            for c in &o.commands {
                if let egui::OutputCommand::CopyText(text) = c {
                    self.copied = Some(text.clone());
                }
            }
        });
        self.perf.frame_ms = now_ms() - t0;
        self.perf.update_ms = self.perf.logic_ms + self.perf.frame_ms;
        self.perf.max_update_ms = self.perf.max_update_ms.max(self.perf.update_ms);
    }

    /// Lay out the whole window.
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        i18n::set_language(self.ui.language);
        let ctx = ui.ctx().clone();
        if !self.fonts_ready {
            ctx.request_repaint();
            return;
        }
        let t0 = now_ms();
        if std::mem::take(&mut self.ui.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // panels set it again this frame while the pointer rests on a preset or profile
        self.hover_preview = None;
        self.ai_mask_detail(&ctx);
        if self.ui.fullscreen {
            // full-screen preview: the photo alone on black
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(egui::Color32::BLACK)).show(ui, |ui| panels::detail::show(self, ui));
            panels::second::show(self, &ctx);
            panels::notices::show(self, &ctx);
            panels::dialogs::show(self, &ctx);
            panels::library_problem::show(self, &ctx);
            panels::toast(self, &ctx);
            self.widgets = widgets::take_registry(&ctx);
            self.end_frame(&ctx, t0);
            return;
        }
        // Order matters: earlier panels take the full edge (top bar spans the window; the tool strip,
        // right panels and left panel run to the bottom; the bottom bar sits between them).
        panels::topbar::show(self, ui);
        panels::library_problem::banner(self, ui);
        panels::strip::show(self, ui);
        if self.ui.right != state::RightPanel::None {
            panels::right::show(self, ui);
        }
        if self.ui.presets {
            panels::presets::show(self, ui);
        }
        if self.ui.left_panel {
            panels::left::show(self, ui);
        }
        panels::bottombar::show(self, ui);
        let t = theme::Tokens::get(&ctx);
        let bg = if matches!(self.ui.view, state::ViewMode::Detail | state::ViewMode::Compare | state::ViewMode::Survey | state::ViewMode::Reference)
        {
            t.canvas
        } else {
            t.grid_bg
        };
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(bg)).show(ui, |ui| match self.ui.view {
            state::ViewMode::PhotoGrid | state::ViewMode::SquareGrid => panels::grid::show(self, ui),
            state::ViewMode::Detail => panels::detail::show(self, ui),
            state::ViewMode::Compare => panels::compare::show_compare(self, ui),
            state::ViewMode::Survey => panels::compare::show_survey(self, ui),
            state::ViewMode::Reference => panels::compare::show_reference(self, ui),
            state::ViewMode::People => panels::people::show(self, ui),
        });
        panels::second::show(self, &ctx);
        panels::notices::show(self, &ctx);
        panels::dialogs::show(self, &ctx);
        panels::library_problem::show(self, &ctx);
        import::progress(self, &ctx);
        sync::progress_window(self, &ctx);
        import::scan_progress(self, &ctx);
        lightroom_import::progress(self, &ctx);
        export_task::poll(self, &ctx);
        pick::poll(self, &ctx);
        panels::grid::drag_feedback(self, &ctx);
        panels::left::album_drag_feedback(self, &ctx);
        panels::keyword_list::drag_feedback(self, &ctx);
        panels::toast(self, &ctx);
        self.widgets = widgets::take_registry(&ctx);
        self.end_frame(&ctx, t0);
    }
}

/// How long a screenshot waits for in-flight renders.
const SCREENSHOT_SETTLE_MS: f64 = 3000.0;
/// How long a windowed screenshot waits for the compositor before falling back to headless.
const SCREENSHOT_FALLBACK_MS: f64 = 2000.0;

/// A `ui.screenshot` request in progress.
struct PendingShot {
    token: u64,
    path: Option<String>,
    reply: Sender<ControlResponse>,
    headless: bool,
    not_before: f64,
    deadline: f64,
    frames: u32,
    /// When the viewport screenshot command went out.
    sent_at: Option<f64>,
}

/// Wall-clock milliseconds since the Unix epoch (`web-time` maps to `Date.now()` on the web).
pub fn now_ms() -> f64 {
    use web_time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64() * 1000.0).unwrap_or(0.0)
}

/// Whether the settings render in black & white.
/// A temporary look for the loupe (hovering a preset or profile).
#[derive(Clone, Debug, PartialEq)]
pub struct HoverPreview {
    /// What is previewed (e.g. "Preset: Warm Glow").
    pub label: String,
    /// The photo's settings with the look applied.
    pub settings: lightcraft_develop::DevelopSettings,
}

pub fn is_bw(d: &lightcraft_develop::DevelopSettings) -> bool {
    d.treatment == lightcraft_develop::Treatment::Bw || d.profile.id == "lc.mono" || d.profile.id.starts_with("lc.bw.")
}

/// "Found 3 faces in 2 photos" for a `faces.detect` result.
fn detect_summary(v: &Value) -> String {
    let photos = v["photos"].as_array().map(Vec::as_slice).unwrap_or_default();
    let counts: Vec<usize> = photos.iter().map(|p| p["faces"].as_array().map_or(0, Vec::len)).collect();
    let (faces, with) = (counts.iter().sum::<usize>(), counts.iter().filter(|n| **n > 0).count());
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (faces, photos.len()) {
        (0, 1) => "No faces found".into(),
        (0, n) => format!("No faces found in {}", plural(n, "photo")),
        (f, 1) => format!("Found {}", plural(f, "face")),
        (f, _) => format!("Found {} in {}", plural(f, "face"), plural(with, "photo")),
    }
}

/// A dropped face model file (`.onnx`).
pub fn is_model_file(path: &str) -> bool {
    std::path::Path::new(path).extension().is_some_and(|e| e.eq_ignore_ascii_case("onnx"))
}

/// Files dropped on the window that are presets rather than photos.
pub fn is_preset_file(path: &str) -> bool {
    let ext = std::path::Path::new(path).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    ["lcpreset", "lrtemplate", "xmp", "zip", "cube", "lmp", "mplumpack"].contains(&ext.as_str())
}

#[cfg(test)]
mod drop_tests {
    #[test]
    fn detection_results_are_summarised_in_words() {
        let r =
            |counts: &[usize]| serde_json::json!({"photos": counts.iter().map(|n| serde_json::json!({"faces": vec![0; *n]})).collect::<Vec<_>>()});
        assert_eq!(super::detect_summary(&r(&[0])), "No faces found");
        assert_eq!(super::detect_summary(&r(&[1])), "Found 1 face");
        assert_eq!(super::detect_summary(&r(&[3])), "Found 3 faces");
        assert_eq!(super::detect_summary(&r(&[0, 0])), "No faces found in 2 photos");
        assert_eq!(super::detect_summary(&r(&[2, 0, 1])), "Found 3 faces in 2 photos");
        assert_eq!(super::detect_summary(&serde_json::json!({})), "No faces found in 0 photos");
    }

    #[test]
    fn dropped_face_models_are_told_apart() {
        for p in ["/a/model.onnx", "/a/dir/M.ONNX"] {
            assert!(super::is_model_file(p), "{p}");
        }
        for p in ["/a/model.onnx.jpg", "/a/onnx", "/a/b.png"] {
            assert!(!super::is_model_file(p), "{p}");
        }
    }

    #[test]
    fn dropped_presets_are_told_apart_from_photos() {
        for p in ["/a/Look.lrtemplate", "/a/b.XMP", "/a/pack.zip", "/a/x.lcpreset", "/a/Magic Hour.mplumpack", "/a/Pop.lmp", "/a/Bundle.LMP"] {
            assert!(super::is_preset_file(p), "{p}");
        }
        for p in ["/a/IMG_1.CR2", "/a/b.dng", "/a/c.jpg", "/a/folder"] {
            assert!(!super::is_preset_file(p), "{p}");
        }
    }
}

/// Results recomputed only when the catalog (or their inputs) change.
#[derive(Default)]
pub struct Caches {
    keyword_tree: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::KeywordNode>>)>,
    /// The Keywording box's read-only names (with containing keywords, or Will Export), by library
    /// revision, selection and view.
    keyword_names: Option<(u64, u64, std::sync::Arc<Vec<panels::keywording::Chip>>)>,
    /// The Keywording box's chips, by library revision and selection.
    keyword_chips: Option<(u64, u64, std::sync::Arc<Vec<panels::keywording::Chip>>)>,
    /// The Keyword List's tick boxes, by library revision and selection.
    keyword_ticks: Option<(u64, u64, std::sync::Arc<panels::keyword_list::Ticks>)>,
    folder_tree: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::FolderNode>>)>,
    people: Option<(u64, lightcraft_catalog::Filter, std::sync::Arc<Vec<lightcraft_catalog::Person>>)>,
    suggestions: Option<(u64, std::sync::Arc<Vec<String>>)>,
    counts: Option<(u64, LibraryCounts)>,
    date_groups: Option<(u64, std::sync::Arc<Vec<lightcraft_catalog::DateGroup>>)>,
    filter_values: Option<(u64, std::sync::Arc<FilterValues>)>,
    album_counts: Option<(u64, std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>>)>,
    /// How often the album counts were recomputed (tests check that unchanged frames don't).
    pub album_count_scans: usize,
    /// What the open smart-album rule dialog shows besides the rules ([`Caches::rules_view`]).
    rules_view: Option<(u64, std::sync::Arc<RulesView>)>,
    pub rules_view_scans: usize,
    /// The problems of every saved smart album that has some ([`Caches::smart_album_problems`]).
    smart_problems: Option<(u64, std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, Vec<lightcraft_catalog::rules::Problem>>>)>,
    pub smart_problem_scans: usize,
    /// AI denoise: what the pump last saw, the model list and the downloads being watched.
    pub denoise: panels::denoise::Ui,
    /// Bumped when a face model is installed, removed or chosen, so Settings re-reads the list at once.
    pub faces_epoch: u64,
    /// Face model downloads the user started, followed until installed (see `panels::faces::pump`).
    pub faces_dl_watch: Vec<String>,
    /// Whether the background face indexer is running, how many faces it has embedded, how many photos are left, and
    /// when to ask it again.
    pub faces_active: bool,
    pub faces_indexed: u64,
    pub faces_pending: u64,
    pub faces_next_pump: f64,
    /// When the user last dragged, typed or scrolled, and when they last moved the pointer (egui time).
    pub last_input: f64,
    /// The pace the face scan was last given, and whether the window was in front then (for `ui.inspect`).
    pub faces_pace: &'static str,
    pub faces_in_front: bool,
    pub last_move: f64,
    /// Name suggestions for the photo in the loupe: (photo, catalog revision, faces indexed, suggestions by region).
    pub face_hints: Option<(u64, u64, u64, std::sync::Arc<panels::faces::Hints>)>,
    /// The open person page: (name, catalog revision, faces indexed, when it was asked for, the page).
    pub person_page: Option<(String, u64, u64, f64, std::sync::Arc<panels::person::PersonPage>)>,
    /// The unnamed faces: (catalog revision, faces indexed, when it was asked for, the list).
    pub unnamed: Option<(u64, u64, f64, std::sync::Arc<panels::unnamed::Unnamed>)>,
    /// The most photos the face scan has had left at once since it last finished (the progress bar's whole).
    pub faces_peak: u64,
    person_names: Option<(u64, std::sync::Arc<Vec<String>>)>,
    /// The grid's date runs, layout and indexes (by the visible list's generation).
    pub grid: panels::grid::GridCache,
    /// What the grid did on its frames (benchmarks and tests check unchanged frames stay cheap).
    pub grid_stats: panels::grid::GridStats,
}

/// The left panel's counts.
#[derive(Clone, Copy, Debug, Default)]
pub struct LibraryCounts {
    pub total: usize,
    pub picks: usize,
    pub deleted: usize,
}

/// The values the filter bar's pickers offer.
#[derive(Clone, Debug, Default)]
pub struct FilterValues {
    pub cameras: Vec<String>,
    pub lenses: Vec<String>,
    pub keywords: Vec<String>,
}

/// What the smart-album rule dialog shows besides the rules themselves: their problems (each row
/// marks its own), how many photos they match and the albums an Album rule picks from.
pub struct RulesView {
    pub problems: Vec<lightcraft_catalog::rules::Problem>,
    pub count: usize,
    pub albums: Vec<album_picker::AlbumEntry>,
}

pub(crate) fn key_of(parts: impl std::hash::Hash) -> u64 {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(parts)
}

impl Caches {
    /// The folders the library's photos were imported from.
    pub fn folder_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::FolderNode>> {
        match &self.folder_tree {
            Some((r, t)) if *r == cat.revision => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.folder_tree());
                self.folder_tree = Some((cat.revision, t.clone()));
                t
            }
        }
    }
    /// The library's keyword tree.
    pub fn keyword_tree(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::KeywordNode>> {
        match &self.keyword_tree {
            Some((r, t)) if *r == cat.revision => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.keyword_tree());
                self.keyword_tree = Some((cat.revision, t.clone()));
                t
            }
        }
    }
    /// The Keywording box's read-only names for the selection: what exported files carry
    /// (`export`), or the keywords with those containing them.
    pub(crate) fn keyword_names(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
        export: bool,
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of((selection, export));
        match &self.keyword_names {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(if export {
                    panels::keywording::will_export(cat, selection)
                } else {
                    panels::keywording::with_containing(cat, selection)
                });
                self.keyword_names = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// The selection's keywords (the Keywording box's chips).
    pub(crate) fn keyword_chips(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<Vec<panels::keywording::Chip>> {
        let key = key_of(selection);
        match &self.keyword_chips {
            Some((r, k, c)) if *r == cat.revision && *k == key => c.clone(),
            _ => {
                let c = std::sync::Arc::new(panels::keywording::chips(cat, selection));
                self.keyword_chips = Some((cat.revision, key, c.clone()));
                c
            }
        }
    }
    /// How many selected photos have each keyword (the Keyword List's tick boxes).
    pub(crate) fn keyword_ticks(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        selection: &[lightcraft_catalog::PhotoId],
    ) -> std::sync::Arc<panels::keyword_list::Ticks> {
        let key = key_of(selection);
        match &self.keyword_ticks {
            Some((r, k, t)) if *r == cat.revision && *k == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(panels::keyword_list::Ticks::of(cat, selection));
                self.keyword_ticks = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// The people named on faces among the photos the filter lets through (its own `person` aside),
    /// with photo counts.
    pub fn people(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        filter: &lightcraft_catalog::Filter,
    ) -> std::sync::Arc<Vec<lightcraft_catalog::Person>> {
        let key = lightcraft_catalog::Filter { person: None, ..filter.clone() };
        match &self.people {
            Some((r, f, t)) if *r == cat.revision && *f == key => t.clone(),
            _ => {
                let t = std::sync::Arc::new(cat.people_in(&key));
                self.people = Some((cat.revision, key, t.clone()));
                t
            }
        }
    }
    /// All Photos / Picks / Recently Deleted counts.
    pub fn counts(&mut self, cat: &lightcraft_catalog::Catalog) -> LibraryCounts {
        if let Some((r, c)) = self.counts
            && r == cat.revision
        {
            return c;
        }
        let mut c = LibraryCounts::default();
        for p in cat.photos() {
            if p.in_library() {
                c.total += 1;
                if p.flag == lightcraft_catalog::Flag::Pick {
                    c.picks += 1;
                }
            } else if p.deleted && !p.local {
                c.deleted += 1;
            }
        }
        self.counts = Some((cat.revision, c));
        c
    }
    /// Everyone named on a face in the library (for completing a name as it is typed).
    pub fn person_names(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<String>> {
        match &self.person_names {
            Some((r, v)) if *r == cat.revision => v.clone(),
            _ => {
                let v = std::sync::Arc::new(cat.people().into_iter().map(|p| p.name).collect::<Vec<_>>());
                self.person_names = Some((cat.revision, v.clone()));
                v
            }
        }
    }
    /// The By Date tree.
    pub fn date_groups(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<Vec<lightcraft_catalog::DateGroup>> {
        match &self.date_groups {
            Some((r, g)) if *r == cat.revision => g.clone(),
            _ => {
                let g = std::sync::Arc::new(cat.date_groups());
                self.date_groups = Some((cat.revision, g.clone()));
                g
            }
        }
    }
    /// Cameras, lenses and keywords in the library (filter bar pickers).
    pub fn filter_values(&mut self, cat: &lightcraft_catalog::Catalog) -> std::sync::Arc<FilterValues> {
        match &self.filter_values {
            Some((r, v)) if *r == cat.revision => v.clone(),
            _ => {
                let distinct = |mut v: Vec<String>| {
                    v.retain(|s| !s.trim().is_empty());
                    v.sort_by_key(|s| s.to_lowercase());
                    v.dedup();
                    v
                };
                let lib = || cat.photos().filter(|p| p.in_library());
                let v = std::sync::Arc::new(FilterValues {
                    cameras: distinct(lib().map(|p| p.meta.camera.clone()).collect()),
                    lenses: distinct(lib().map(|p| p.meta.lens.clone()).collect()),
                    keywords: cat.keywords().into_iter().map(|(k, _)| k).collect(),
                });
                self.filter_values = Some((cat.revision, v.clone()));
                v
            }
        }
    }
    /// Keyword suggestions for a photo with `current` keywords and the typed `prefix`.
    pub fn suggestions(&mut self, cat: &lightcraft_catalog::Catalog, current: &[String], prefix: &str, n: usize) -> std::sync::Arc<Vec<String>> {
        let k = key_of((cat.revision, current, prefix, n));
        match &self.suggestions {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                let v = std::sync::Arc::new(cat.keyword_suggestions(current, prefix, n));
                self.suggestions = Some((k, v.clone()));
                v
            }
        }
    }
    /// Every album's photo count for the sidebar (a smart album evaluates its rules, which
    /// scans the catalog). Recomputed when the catalog changes (photos, metadata, album rules
    /// all bump its revision) and — only while some smart album has an "in the last…" rule —
    /// when `now` (the session clock, ISO) enters a new minute, so such counts follow the clock
    /// within a minute without rescanning every frame.
    /// [`RulesView`] for `rules` edited in smart album `editing` (`None`: a new album), within
    /// `folder` when it was made from a folder view: worked out again only when the catalog, the
    /// rules, the language or (for "in the last…" rules) the minute changes, not every frame.
    pub fn rules_view(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        rules: &lightcraft_catalog::RuleSet,
        editing: Option<u64>,
        folder: Option<String>,
        now: &str,
    ) -> std::sync::Arc<RulesView> {
        let relative = rules.depends_on_now();
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let k = key_of((cat.revision, serde_json::to_string(rules).unwrap_or_default(), editing, &folder, minute, i18n::language().code()));
        if let Some((key, v)) = &self.rules_view
            && *key == k
        {
            return v.clone();
        }
        if relative {
            lightcraft_catalog::rules::set_now(Some(now.to_string()));
        }
        let problems = rules.check_for(cat, editing.map(lightcraft_catalog::AlbumId));
        let filter = lightcraft_catalog::Filter { rule_set: Some(rules.clone()), library_folder: folder, ..Default::default() };
        let count = if problems.is_empty() { cat.query(&filter, &Default::default()).len() } else { 0 };
        let v = std::sync::Arc::new(RulesView { problems, count, albums: panels::dialogs::album_entries(cat, editing) });
        self.rules_view_scans += 1;
        self.rules_view = Some((k, v.clone()));
        v
    }

    /// The problems of every saved smart album that has some (`Catalog::smart_album_problems`),
    /// for the sidebar's ⚠ marks: worked out again only when the catalog changes.
    pub fn smart_album_problems(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, Vec<lightcraft_catalog::rules::Problem>>> {
        if let Some((rev, v)) = &self.smart_problems
            && *rev == cat.revision
        {
            return v.clone();
        }
        let v: std::sync::Arc<std::collections::HashMap<_, _>> = std::sync::Arc::new(
            cat.albums().filter(|a| a.is_smart()).map(|a| (a.id, cat.smart_album_problems(a.id))).filter(|(_, p)| !p.is_empty()).collect(),
        );
        self.smart_problem_scans += 1;
        self.smart_problems = Some((cat.revision, v.clone()));
        v
    }

    pub fn album_counts(
        &mut self,
        cat: &lightcraft_catalog::Catalog,
        now: &str,
    ) -> std::sync::Arc<std::collections::HashMap<lightcraft_catalog::AlbumId, usize>> {
        let relative = cat.albums().any(|a| a.smart.as_ref().is_some_and(|f| f.depends_on_now()));
        let minute = if relative { now.get(..16).unwrap_or(now) } else { "" };
        let k = key_of((cat.revision, minute));
        match &self.album_counts {
            Some((h, v)) if *h == k => v.clone(),
            _ => {
                if relative {
                    // "in the last N days" counts back from the session clock, as in the grid
                    lightcraft_catalog::rules::set_now(Some(now.to_string()));
                }
                let v: std::sync::Arc<std::collections::HashMap<_, _>> =
                    std::sync::Arc::new(cat.albums().map(|a| (a.id, cat.album_count(a.id))).collect());
                self.album_count_scans += 1;
                self.album_counts = Some((k, v.clone()));
                v
            }
        }
    }
}

#[cfg(test)]
mod cache_tests {
    use lightcraft_catalog::{Album, AlbumId, Catalog, Filter, Op, Photo, PhotoId, RuleSet, Source};

    fn photo(id: u64, captured: &str, rating: u8) -> Op {
        let mut p = Photo::new(PhotoId(id), Source::Demo { scene: 0 }, &format!("p{id}.jpg"), "JPEG", 60, 40, "2026-01-01T00:00:00");
        p.captured = Some(captured.into());
        p.rating = rating;
        Op::AddPhoto { photo: Box::new(p) }
    }

    fn smart(id: u64, rules: serde_json::Value) -> Op {
        let mut a = Album::new(AlbumId(id), format!("smart {id}"));
        let rules: RuleSet = serde_json::from_value(rules).unwrap();
        a.smart = Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() }));
        Op::AddAlbum { album: a }
    }

    /// Smart-album counts: unchanged frames reuse them; metadata and rule changes and (for
    /// "in the last…" rules) the clock crossing into a new minute recompute them.
    #[test]
    fn smart_album_counts_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for (i, d) in ["2026-09-30T11:59:30", "2026-09-29T08:00:00", "2026-01-01T08:00:00"].iter().enumerate() {
            cat.apply(photo(i as u64 + 1, d, if i == 0 { 5 } else { 1 })).unwrap();
        }
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        cat.apply(smart(11, serde_json::json!({"rules": [{"field": "captureDate", "op": "inLast", "value": {"n": 1, "unit": "hours"}}]}))).unwrap();
        let now = "2026-09-30T12:00:00";
        let n = c.album_counts(&cat, now);
        assert_eq!((n[&AlbumId(10)], n[&AlbumId(11)]), (1, 1));
        assert_eq!(c.album_count_scans, 1);
        for _ in 0..10 {
            c.album_counts(&cat, "2026-09-30T12:00:40");
        }
        assert_eq!(c.album_count_scans, 1, "unchanged frames in the same minute don't rescan");
        // metadata change
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 2);
        // rule change
        let rs: RuleSet = serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 5}]})).unwrap();
        cat.apply(Op::SetAlbumRules { id: AlbumId(10), rules: Box::new(Filter { rule_set: Some(rs), ..Default::default() }) }).unwrap();
        assert_eq!(c.album_counts(&cat, now)[&AlbumId(10)], 1);
        // an hour later the 11:59:30 photo has left "in the last hour", with no catalog change
        assert_eq!(c.album_counts(&cat, "2026-09-30T13:00:10")[&AlbumId(11)], 0);
        assert_eq!(c.album_count_scans, 4);
        lightcraft_catalog::rules::set_now(None);
    }

    /// The rule dialog's problems, live count and album list are worked out once per change to the
    /// catalog or the rules, not every frame; so are the sidebar's smart-album problems.
    #[test]
    fn rule_dialog_and_problems_are_cached_until_something_changes() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        for i in 0..3u64 {
            cat.apply(photo(i + 1, "2026-09-30T11:00:00", if i == 0 { 5 } else { 1 })).unwrap();
        }
        let rules =
            |v: u64| -> RuleSet { serde_json::from_value(serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": v}]})).unwrap() };
        let now = "2026-09-30T12:00:00";
        let view = c.rules_view(&cat, &rules(3), None, None, now);
        assert_eq!((view.count, view.problems.len()), (1, 0));
        for _ in 0..10 {
            c.rules_view(&cat, &rules(3), None, None, now);
        }
        assert_eq!(c.rules_view_scans, 1, "unchanged frames reuse it");
        assert_eq!(c.rules_view(&cat, &rules(1), None, None, now).count, 3, "a rule change");
        assert_eq!(c.rules_view(&cat, &rules(9), None, None, now).problems.len(), 1);
        cat.apply(Op::SetRating { id: PhotoId(2), rating: 4 }).unwrap();
        assert_eq!(c.rules_view(&cat, &rules(3), None, None, now).count, 2, "a catalog change");
        assert_eq!(c.rules_view_scans, 4);
        // the sidebar's problems
        cat.apply(Op::AddAlbum { album: lightcraft_catalog::Album::new(AlbumId(20), "Trip") }).unwrap();
        cat.apply(smart(21, serde_json::json!({"rules": [{"field": "album", "op": "is", "value": 20}]}))).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_none_or(Vec::is_empty));
        for _ in 0..10 {
            c.smart_album_problems(&cat);
        }
        assert_eq!(c.smart_problem_scans, 1);
        cat.apply(Op::RemoveAlbum { id: AlbumId(20) }).unwrap();
        assert!(c.smart_album_problems(&cat).get(&AlbumId(21)).is_some_and(|p| !p.is_empty()), "its album is gone");
        assert_eq!(c.smart_problem_scans, 2);
    }

    /// Without "in the last…" rules the clock never causes a rescan.
    #[test]
    fn absolute_rules_ignore_the_clock() {
        let mut cat = Catalog::default();
        let mut c = super::Caches::default();
        cat.apply(photo(1, "2026-09-30T11:59:30", 5)).unwrap();
        cat.apply(smart(10, serde_json::json!({"rules": [{"field": "rating", "op": "gte", "value": 3}]}))).unwrap();
        c.album_counts(&cat, "2026-09-30T12:00:00");
        c.album_counts(&cat, "2027-01-01T00:00:00");
        assert_eq!(c.album_count_scans, 1);
    }
}

#[cfg(test)]
mod tests_model_setup;
