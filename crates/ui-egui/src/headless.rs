//! Headless UI rendering: run the app's UI in an offscreen [`egui::Context`] and rasterize it on
//! the CPU ([`crate::softpaint`]) — no window, no GPU, no compositor.
//!
//! Two users:
//! - [`Headless`]: a complete windowless app session (`<cli> snapshot`, tests). It plays
//!   the role eframe plays for the desktop app: builds [`egui::RawInput`], runs `logic` + `ui`,
//!   keeps a CPU mirror of the textures, executes viewport commands (`Screenshot` is answered with
//!   a CPU-rendered frame, `InnerSize` resizes, `Close` quits). Control-protocol requests go
//!   through the very same handler as the desktop app's control server.
//! - [`HeadlessView`] inside the desktop app: `ui.screenshot {"headless": true}` (and the
//!   automatic fallback when the compositor delivers no frame, e.g. while the display sleeps)
//!   draws the app's UI into a shadow context from `logic`, which keeps ticking when the window
//!   is occluded.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, RawInput, TextureId, ViewportCommand, ViewportId};
use serde_json::{Value, json};

use crate::softpaint::{self, CpuTexture, Layered, TextureStore};
use crate::{ControlRequest, LccApp};

/// Background behind the panels (eframe's default clear colour, made opaque).
const CLEAR: Color32 = Color32::from_rgb(12, 12, 12);
/// Simulated frame interval (deterministic animation time).
const FRAME_DT: f64 = 1.0 / 60.0;

/// Validate the scaled screenshot dimensions before layout or pixel allocation.
pub fn viewport_pixels(size: [f32; 2], pixels_per_point: f32) -> Result<[usize; 2], String> {
    if size.iter().any(|edge| *edge < 1.0) {
        return Err("viewport dimensions must be at least one logical point".into());
    }
    native_viewport_pixels(size, pixels_per_point)
}

fn native_viewport_pixels(size: [f32; 2], pixels_per_point: f32) -> Result<[usize; 2], String> {
    let [width, height] = size;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 || !pixels_per_point.is_finite() || pixels_per_point <= 0.0 {
        return Err("viewport dimensions and scale must be finite and positive".into());
    }
    let axis = |value: f32| {
        // Match egui/scissor rounding and the existing screenshot dimension contract.
        let pixels = (value * pixels_per_point).round();
        if !(1.0..=softpaint::MAX_IMAGE_EDGE as f32).contains(&pixels) {
            return Err(format!("scaled viewport edges must be between 1 and {} pixels", softpaint::MAX_IMAGE_EDGE));
        }
        Ok(pixels as usize)
    };
    let (w, h) = (axis(width)?, axis(height)?);
    if w.checked_mul(h).is_none_or(|pixels| pixels > softpaint::MAX_IMAGE_PIXELS) {
        return Err(format!("scaled viewport exceeds {} pixels", softpaint::MAX_IMAGE_PIXELS));
    }
    Ok([w, h])
}

/// Convert an effective-point resize to the native viewport before validating pixel bounds.
/// Sub-point native edges after zooming out are valid when they round to at least one pixel.
pub fn resized_viewport(size: [f32; 2], native_pixels_per_point: f32, zoom: f32) -> Result<egui::Vec2, String> {
    if size.iter().any(|edge| !edge.is_finite() || *edge < 1.0) || !zoom.is_finite() || zoom <= 0.0 {
        return Err("resize dimensions must be at least one finite logical point with a positive zoom".into());
    }
    let native_size = egui::Vec2::from(size) * zoom;
    native_viewport_pixels([native_size.x, native_size.y], native_pixels_per_point)?;
    Ok(native_size)
}

fn bounded_viewport(size: egui::Vec2, scale: f32) -> (egui::Vec2, f32) {
    if native_viewport_pixels([size.x, size.y], scale).is_ok() { (size, scale) } else { (egui::vec2(1600.0, 1000.0), 1.0) }
}

/// An offscreen egui context with our fonts and theme, and a CPU mirror of its textures.
pub struct HeadlessView {
    pub ctx: egui::Context,
    pub textures: TextureStore,
    pub(crate) shapes: Vec<egui::epaint::ClippedShape>,
    pixels_per_point: f32,
    size: [usize; 2],
    frames: u64,
    /// The pretend host's clipboard: what the UI last copied, and what a paste request pastes.
    pub clipboard: String,
    /// Events the host owes the next frame (a paste request's paste, as a desktop host sends it).
    owed: Vec<egui::Event>,
    /// The last AccessKit tree update (when `ctx.enable_accesskit()` is on): what a screen reader sees.
    pub access: Option<egui::accesskit::TreeUpdate>,
}

impl Default for HeadlessView {
    fn default() -> Self {
        Self::new()
    }
}

impl HeadlessView {
    pub fn new() -> Self {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        HeadlessView {
            ctx,
            textures: TextureStore::default(),
            shapes: vec![],
            pixels_per_point: 1.0,
            size: [1600, 1000],
            frames: 0,
            clipboard: String::new(),
            owed: vec![],
            access: None,
        }
    }

    /// Input for one frame of a `size` (points) viewport at `pixels_per_point`.
    pub fn raw_input(size: egui::Vec2, pixels_per_point: f32, time: f64, events: Vec<egui::Event>) -> RawInput {
        let (size, pixels_per_point) = bounded_viewport(size, pixels_per_point);
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut raw = RawInput {
            screen_rect: Some(rect),
            time: Some(time),
            predicted_dt: FRAME_DT as f32,
            focused: true,
            events,
            max_texture_side: Some(16384),
            ..Default::default()
        };
        let info = raw.viewports.entry(ViewportId::ROOT).or_default();
        info.native_pixels_per_point = Some(pixels_per_point);
        info.inner_rect = Some(rect);
        info.outer_rect = Some(rect);
        info.focused = Some(true);
        raw
    }

    /// Run one frame; keeps the shapes for [`Self::paint`]. Returns the root viewport's commands.
    pub fn run(&mut self, mut raw: RawInput, run_ui: impl FnMut(&mut egui::Ui)) -> Vec<ViewportCommand> {
        if let Some(rect) = raw.screen_rect {
            let native_scale = raw.viewports.get(&ViewportId::ROOT).and_then(|info| info.native_pixels_per_point).unwrap_or(1.0);
            self.size = native_viewport_pixels([rect.width(), rect.height()], native_scale).unwrap_or([0, 0]);
            // The host viewport is measured before UI zoom. Egui lays out in zoomed points;
            // retain the host's physical size while allowing text and controls to grow.
            let zoom = self.ctx.zoom_factor();
            if zoom.is_finite() && zoom > 0.0 {
                raw.screen_rect = Some(rect / zoom);
                if let Some(info) = raw.viewports.get_mut(&ViewportId::ROOT) {
                    info.inner_rect = info.inner_rect.map(|rect| rect / zoom);
                    info.outer_rect = info.outer_rect.map(|rect| rect / zoom);
                }
            }
        }
        raw.events.splice(0..0, std::mem::take(&mut self.owed));
        let mut out = self.ctx.run_ui(raw, run_ui);
        self.frames += 1;
        self.textures.apply(std::mem::take(&mut out.textures_delta));
        self.shapes = std::mem::take(&mut out.shapes);
        self.pixels_per_point = out.pixels_per_point;
        if let Some(update) = out.platform_output.accesskit_update.take() {
            self.access = Some(update);
        }
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = c {
                self.clipboard.clone_from(text);
            }
        }
        let commands = out.viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default();
        // what a desktop host does with a cut, copy or paste request (egui-winit): the event, next frame
        for c in &commands {
            match c {
                ViewportCommand::RequestCut => self.owed.push(egui::Event::Cut),
                ViewportCommand::RequestCopy => self.owed.push(egui::Event::Copy),
                ViewportCommand::RequestPaste if !self.clipboard.is_empty() => self.owed.push(egui::Event::Paste(self.clipboard.clone())),
                _ => {}
            }
        }
        commands
    }

    /// Frames run so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Every piece of text the last frame painted (labels, tooltips, values), in paint order.
    /// Every shape of the last frame, with `Shape::Vec` flattened.
    pub fn painted_shapes(&self) -> Vec<egui::epaint::Shape> {
        fn flat(shape: &egui::epaint::Shape, out: &mut Vec<egui::epaint::Shape>) {
            match shape {
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| flat(shape, out)),
                other => out.push(other.clone()),
            }
        }
        let mut out = Vec::new();
        self.shapes.iter().for_each(|s| flat(&s.shape, &mut out));
        out
    }

    pub fn painted_text(&self) -> Vec<String> {
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(shape) => out.push(shape.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for shape in &self.shapes {
            texts(&shape.shape, &mut out);
        }
        out
    }

    /// Size in pixels of the last frame.
    pub fn size_px(&self) -> [usize; 2] {
        self.size
    }

    /// Rasterize the last frame. `extra` textures (by id) take precedence over the context's own.
    pub fn paint(&self, extra: &HashMap<TextureId, CpuTexture>) -> ColorImage {
        if viewport_pixels([self.size[0] as f32, self.size[1] as f32], 1.0).is_err()
            || !self.pixels_per_point.is_finite()
            || self.pixels_per_point <= 0.0
        {
            return ColorImage::new([0, 0], Vec::new());
        }
        let prims = self.ctx.tessellate(self.shapes.clone(), self.pixels_per_point);
        softpaint::paint(&prims, &Layered { over: extra, base: &self.textures }, self.size_px(), self.pixels_per_point, CLEAR)
    }
}

/// A windowless app session: the app's `logic` + `ui` driven frame by frame into a
/// [`HeadlessView`], with control-protocol requests answered by the shared handler.
pub struct Headless {
    pub app: LccApp,
    pub view: HeadlessView,
    /// The largest texture the pretend GPU takes (what a WebGL device may report: 2048).
    pub max_texture_side: usize,
    /// The limit egui was last told. Like a native host (egui-winit) it is reported once, not in
    /// every frame's input: code that reads the raw input sees `None` from then on (issue #652).
    reported_texture_side: Option<usize>,
    /// Logical size (points) and scale.
    pub size: egui::Vec2,
    pub pixels_per_point: f32,
    pub(crate) time: f64,
    frames: u64,
    pub(crate) events: Vec<egui::Event>,
    control: Sender<ControlRequest>,
    quit: bool,
    /// The simulated window is zoomed (`Maximized(true)` seen, reported back to the app like a real host).
    pub window_maximized: bool,
    /// Window-management and pointer commands the app sent (`StartDrag`, `Maximized`,
    /// `CursorPosition`, …), oldest first.
    pub window_commands: Vec<ViewportCommand>,
    /// The pointer cursor the last frame asked for (what a real host would show).
    pub last_cursor: egui::CursorIcon,
    /// Files the pretend host drops on the window in the next frame (as a desktop host reports a drag-and-drop).
    pub dropped: Vec<std::path::PathBuf>,
}

/// A file the pretend host drops on the window ([`Headless::dropped`]).
#[derive(Debug)]
struct HostFile(std::path::PathBuf);

impl egui::DroppedFile for HostFile {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }

    #[cfg(target_arch = "wasm32")]
    fn bytes_async(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, String>> + '_>> {
        Box::pin(async { Err("no file system on the web".to_owned()) })
    }
}

/// A headless run (`lcc-cli snapshot`, tests) is a one-shot process: however it ends, it
/// first waits for an Apple Photos import its script left running (`export.addToPhotos {wait:
/// false}`), bounded, so it never abandons osascript or the import's result.
impl Drop for Headless {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        self.app.session.wait_for_photos_imports();
    }
}

impl Headless {
    /// Wrap `app` (its control channel is replaced by the driver's).
    /// Invalid dimensions fall back to 1600×1000 at scale 1; external callers can use
    /// [`viewport_pixels`] to reject them before constructing the session.
    pub fn new(app: LccApp, size: [f32; 2], pixels_per_point: f32) -> Self {
        let (size, pixels_per_point) = if viewport_pixels(size, pixels_per_point).is_ok() {
            (egui::Vec2::from(size), pixels_per_point)
        } else {
            (egui::vec2(1600.0, 1000.0), 1.0)
        };
        // every unit test is its own process under nextest: generate each demo photo once per build
        #[cfg(test)]
        lcc_engine::scene_disk::use_build_dir();
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = app.with_control(rx);
        app.headless_host = true;
        // a headless run never opens the file manager or starts other programs
        app.session.desktop = std::sync::Arc::new(lcc_engine::desktop::Recorder::default());
        Headless {
            app,
            view: HeadlessView::new(),
            max_texture_side: 16384,
            reported_texture_side: None,
            size,
            pixels_per_point,
            time: 0.0,
            frames: 0,
            events: vec![],
            dropped: Vec::new(),
            control: tx,
            quit: false,
            window_maximized: false,
            window_commands: vec![],
            last_cursor: egui::CursorIcon::Default,
        }
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// The simulated clock (seconds) the next frame runs at.
    pub fn time(&self) -> f64 {
        self.time
    }

    /// Every piece of text the last frame painted ([`HeadlessView::painted_text`]).
    pub fn painted_text(&self) -> Vec<String> {
        self.view.painted_text()
    }

    /// The channel requests reach the app on (a transport in a test feeds it, like the desktop
    /// app's control server).
    pub fn control_sender(&self) -> Sender<ControlRequest> {
        self.control.clone()
    }

    /// `app.quit` was requested.
    pub fn quit_requested(&self) -> bool {
        self.quit
    }

    /// Run one frame.
    pub fn step(&mut self) {
        (self.size, self.pixels_per_point) = bounded_viewport(self.size, self.pixels_per_point);
        let mut raw = HeadlessView::raw_input(self.size, self.pixels_per_point, self.time, std::mem::take(&mut self.events));
        raw.viewports.entry(ViewportId::ROOT).or_default().maximized = Some(self.window_maximized);
        raw.max_texture_side = (self.reported_texture_side != Some(self.max_texture_side)).then_some(self.max_texture_side);
        self.reported_texture_side = Some(self.max_texture_side);
        raw.dropped_files =
            self.dropped.drain(..).map(|path| std::sync::Arc::new(HostFile(path)) as std::sync::Arc<dyn egui::DroppedFile + Send + Sync>).collect();
        self.app.raw_input_hook(&mut raw);
        let app = &mut self.app;
        let mut cursor = egui::CursorIcon::Default;
        let commands = self.view.run(raw, |ui| {
            app.logic(ui.ctx());
            app.ui(ui);
            cursor = ui.ctx().output(|output| output.cursor_icon);
        });
        self.last_cursor = cursor;
        self.time += FRAME_DT;
        self.frames += 1;
        for c in commands {
            match c {
                ViewportCommand::Screenshot(user_data) => {
                    let image = Arc::new(self.paint());
                    self.events.push(egui::Event::Screenshot { viewport_id: ViewportId::ROOT, user_data, image });
                }
                ViewportCommand::InnerSize(size) => {
                    // Viewport commands use current egui points, which include UI zoom.
                    if let Ok(native_size) = resized_viewport([size.x, size.y], self.pixels_per_point, self.view.ctx.zoom_factor()) {
                        self.size = native_size;
                    }
                }
                ViewportCommand::Close => self.quit = true,
                ViewportCommand::Maximized(on) => {
                    self.window_maximized = on;
                    self.window_commands.push(c);
                }
                // recorded, not carried out: the simulated pointer moves only when a test moves it
                ViewportCommand::StartDrag | ViewportCommand::CursorPosition(_) | ViewportCommand::CursorGrab(_) => self.window_commands.push(c),
                _ => {}
            }
        }
    }

    /// Is anything still in progress (renders, file-system checks the sidebar waits for, the albums' counts,
    /// queued input)?
    pub fn busy(&self) -> bool {
        self.app.renderer.in_flight() > 0
            || crate::panels::left::fs_cached_running(&self.view.ctx) > 0
            || self.app.caches.album_recount_running()
            || self.app.merge.busy()
            || self.app.scan.is_some()
            || self.app.sync.is_some()
            || self.app.sync_run.is_some()
            || self.app.folder_poll.task.is_some()
            || self.app.import.is_some()
            || self.app.export.is_some()
            || self.app.session.denoise_busy()
            || self.app.session.segmenter.busy()
            || self.app.session.segmenter.analyzing()
            || self.app.session.genfill.running()
            || !self.app.tasks.is_empty()
            || !self.app.synthetic.is_empty()
            || !self.events.is_empty()
    }

    /// Clicks widget `id` (as `ui.widgets` names it) as the user would: input the app doesn't mark
    /// as an agent's, so it reaches what only the user may do (allowing a plug-in's permission).
    /// The control channel has no such path. False when the widget isn't on screen.
    pub fn click_as_user(&mut self, id: &str) -> bool {
        let Some(at) = self.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| r.center()) else { return false };
        let (button, modifiers) = (egui::PointerButton::Primary, egui::Modifiers::NONE);
        self.app.user_events.extend([
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton { pos: at, button, pressed: true, modifiers },
            egui::Event::PointerButton { pos: at, button, pressed: false, modifiers },
        ]);
        while !self.app.user_events.is_empty() {
            self.step();
        }
        self.settle(Duration::from_secs(5));
        true
    }

    /// Run frames until nothing is pending (renders finished, input consumed) for a few frames in
    /// a row, or `timeout` passes. Returns whether it settled.
    pub fn settle(&mut self, timeout: Duration) -> bool {
        let t0 = Instant::now();
        let mut quiet = 0;
        loop {
            self.step();
            if self.busy() {
                quiet = 0;
                std::thread::sleep(Duration::from_millis(1));
            } else {
                quiet += 1;
            }
            // a few quiet frames let layout and short UI animations finish
            if quiet >= 12 {
                return true;
            }
            if t0.elapsed() > timeout {
                // (a test that then fails says why: what was still rendering)
                log::warn!("settle timed out after {timeout:?}; pending renders: {:?}", self.app.renderer.pending_slots());
                return false;
            }
        }
    }

    /// Run frames until `done` holds or `timeout` passes (a render that finishes on a busy machine
    /// after a quiet spell, where [`Self::settle`] alone would stop too early). Returns `done`.
    pub fn step_until(&mut self, timeout: Duration, done: impl Fn(&Self) -> bool) -> bool {
        let t0 = Instant::now();
        while !done(self) {
            if t0.elapsed() > timeout {
                return false;
            }
            self.step();
            std::thread::sleep(Duration::from_millis(1));
        }
        true
    }

    /// Rasterize the last frame (with the photo textures).
    pub fn paint(&self) -> ColorImage {
        self.view.paint(&HashMap::new())
    }

    /// Settle, then render the current UI.
    pub fn snapshot(&mut self, timeout: Duration) -> ColorImage {
        self.settle(timeout);
        self.paint()
    }

    /// Tests that browse a folder in the temp directory expect it to be a Local location of its
    /// own. Where the temp directory lies inside the home folder (Windows), the sidebar lists it
    /// inside Home's tree instead, so hide Home for the test; elsewhere this does nothing.
    #[cfg(test)]
    pub(crate) fn hide_home_above(&mut self, path: &std::path::Path) {
        let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
        if !home.is_empty() && lcc_catalog::query::folder_within(&path.to_string_lossy(), &home) {
            let r = self.request("engine.execute", json!({"command": "local.hide", "params": {"path": home}}), Duration::from_secs(10));
            assert_eq!(r["ok"], true, "{r}");
        }
    }

    /// Send a control-protocol request (see [`crate::control`]) and run frames until it is
    /// answered, then until its input has been consumed. Returns `{"ok": …, "result"|"error": …}`.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Value {
        let (req, rx) = ControlRequest::new(method, params);
        if self.control.send(req).is_err() {
            return json!({"ok": false, "error": "control channel closed"});
        }
        let t0 = Instant::now();
        let reply = loop {
            self.step();
            if let Ok(v) = rx.try_recv() {
                break v;
            }
            if t0.elapsed() > timeout {
                return json!({"ok": false, "error": "timeout"});
            }
            if self.busy() {
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        // let injected input (clicks, keys, drags) play out before the next request
        let t1 = Instant::now();
        while (!self.app.synthetic.is_empty() || !self.events.is_empty()) && t1.elapsed() < timeout {
            self.step();
        }
        self.step();
        reply
    }
}

/// For tests, a render offload: renders wait (stay queued) until `open` is set, then run; those
/// whose slot `fails` picks come back failed, as an undecodable raw's would. Install it with
/// `h.app.renderer.set_offload(Box::new(RenderGate::new(open.clone())))`.
#[cfg(test)]
pub(crate) struct RenderGate {
    open: std::rc::Rc<std::cell::Cell<bool>>,
    fails: fn(crate::render::Slot) -> bool,
    jobs: Vec<(crate::render::Slot, lcc_engine::media::RenderJob)>,
}

#[cfg(test)]
impl RenderGate {
    pub(crate) fn new(open: std::rc::Rc<std::cell::Cell<bool>>) -> Self {
        Self::failing(open, |_| false)
    }
    pub(crate) fn failing(open: std::rc::Rc<std::cell::Cell<bool>>, fails: fn(crate::render::Slot) -> bool) -> Self {
        Self { open, fails, jobs: Vec::new() }
    }
}

#[cfg(test)]
impl crate::render::RenderOffload for RenderGate {
    fn try_start(&mut self, slot: crate::render::Slot, job: lcc_engine::media::RenderJob) -> Option<lcc_engine::media::RenderJob> {
        if !self.open.get() {
            return Some(job);
        }
        self.jobs.push((slot, job));
        None
    }
    fn finished(&mut self) -> Vec<(crate::render::Slot, lcc_engine::media::RenderResult, f64)> {
        let fails = self.fails;
        self.jobs
            .drain(..)
            .map(|(slot, job)| {
                let mut r = job.run();
                if fails(slot) {
                    r.rendered = Err("no decoder for this raw".into());
                }
                (slot, r, 0.0)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    /// A raw photo is offered the AI Denoise switch with no set-up and one click turns it on with the built-in
    /// denoiser; Settings still offers installing a model, and a photo that is not raw is not offered it.
    #[test]
    fn detail_offers_ai_denoise_for_raw_photos_without_any_set_up() {
        use crate::state::Dialog;
        use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        let dir = lcc_testkit::TempDir::new("lc-ui-denoise");
        // tall enough that the Detail panel's setup button is on screen above the Sync bar, whatever the panel order
        let mut h = demo([1400.0, 1600.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.set_denoise_models_dir(Some(dir.join("denoise-models")));
        // a raw photo (its file need not exist: nothing here reads it)
        let id = PhotoId(h.app.session.catalog.photos().map(|p| p.id.0).max().unwrap_or(0) + 1);
        let mut raw = Photo::new(
            id,
            Source::File { path: dir.join("IMG_1.dng").to_string_lossy().into_owned() },
            "IMG_1.dng",
            "DNG",
            6000,
            4000,
            "2026-10-01T00:00:00",
        );
        raw.kind = MediaKind::Raw;
        h.app.session.commit("setup", Op::AddPhoto { photo: Box::new(raw) }).unwrap();
        let widgets = |h: &mut Headless| h.request("ui.widgets", json!({}), t).to_string();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.app.ui.open_sections = vec!["detail".to_string()];
        h.settle(SETTLE);
        h.step();
        h.step();
        let w = widgets(&mut h);
        assert!(w.contains("\"denoise:enabled\"") && !w.contains("\"denoise:setup\""), "a raw photo is offered the switch, with no set-up: {w}");
        // One click turns it on with the built-in denoiser: nothing to install first.
        let r = h.request("ui.clickWidget", json!({"id": "denoise:enabled"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.dialog, None);
        assert!(h.app.session.develop_of(id).is_some_and(|d| d.enhance.denoise_enabled()));
        assert_eq!(h.app.session.execute("denoise.status", &json!({})).unwrap()["model"]["id"], "builtin");
        // Settings still offers installing a model of one's own.
        h.app.ui.dialog = Some(Dialog::Settings { tab: "ai".into() });
        h.settle(SETTLE);
        let listed = h.app.session.execute("denoise.models.list", &json!({})).unwrap();
        assert_eq!(listed["models"].as_array().unwrap().len(), lcc_denoise::known::all().len(), "{listed}");
        crate::settings_screen::reveal(&mut h, "denoise:installFile");
        assert!(widgets(&mut h).contains("\"denoise:installFile\""));
        h.app.ui.dialog = None;
        // a photo that is not raw: no offer
        h.request(
            "engine.execute",
            json!({"command": "library.select", "params": {"ids": [h.app.session.catalog.photos().find(|p| p.id != id).unwrap().id.0]}}),
            t,
        );
        h.settle(SETTLE);
        h.step();
        h.step();
        assert_eq!(h.app.session.denoise_photo_state(h.app.session.active().unwrap()), lcc_engine::denoise::PhotoState::NotApplicable);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A photo AI Denoise can't use says why under its greyed-out switch, instead of looking broken: a raw that opens
    /// as its embedded preview (a Nikon High Efficiency NEF) names the decoder's reason, a JPEG says raws only, and a
    /// raw denoise does apply to says neither.
    #[test]
    fn detail_says_why_ai_denoise_does_not_apply() {
        use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        let mut h = demo([1400.0, 1600.0]);
        let t = Duration::from_secs(10);
        let next = h.app.session.catalog.photos().map(|p| p.id.0).max().unwrap_or(0);
        let add = |h: &mut Headless, n: u64, name: &str, format: &str, kind: MediaKind, preview_only: Option<&str>| {
            let id = PhotoId(next + n);
            let mut p = Photo::new(id, Source::File { path: format!("/denoise-why/{name}") }, name, format, 6000, 4000, "2026-10-01T00:00:00");
            p.kind = kind;
            p.preview_only = preview_only.map(str::to_string);
            h.app.session.commit("setup", Op::AddPhoto { photo: Box::new(p) }).unwrap();
            id
        };
        let he = add(&mut h, 1, "DSC_0001.NEF", "NEF", MediaKind::Raw, Some("Nikon High Efficiency NEF (JPEG XS) is not decoded yet"));
        let jpeg = add(&mut h, 2, "IMG_0002.jpg", "JPEG", MediaKind::Image, None);
        let raw = add(&mut h, 3, "DSC_0003.NEF", "NEF", MediaKind::Raw, None);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.app.ui.open_sections = vec!["detail".to_string()];
        let shown = |h: &mut Headless, id: PhotoId| {
            h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
            h.settle(SETTLE);
            h.step();
            h.step();
            h.view.painted_text().join("\n")
        };
        const PREVIEW: &str = "AI Denoise needs the raw sensor data, and this file opens as its embedded preview.";
        // a finished picture (JPEG, PNG, TIFF) is denoised by a model made for it: without one installed it says so
        const NEEDS_MODEL: &str = "AI Denoise needs a model for finished pictures: install one in Settings ▸ AI Denoise.";
        const RAW_ONLY: &str = "AI Denoise works on raw files only.";
        let text = shown(&mut h, he);
        assert!(text.contains(PREVIEW), "a preview-only raw says why: {text}");
        assert!(text.contains("Nikon High Efficiency NEF (JPEG XS) is not decoded yet."), "with the decoder's reason: {text}");
        let text = shown(&mut h, jpeg);
        assert!(text.contains(NEEDS_MODEL) && !text.contains(PREVIEW), "a JPEG says it needs a finished-picture model: {text}");
        let text = shown(&mut h, raw);
        assert!(
            !text.contains(RAW_ONLY) && !text.contains(NEEDS_MODEL) && !text.contains(PREVIEW),
            "a raw denoise applies to explains nothing: {text}"
        );
    }

    /// Install from file… with a model the engine rejects says why in the AI Denoise section (#1381), instead of
    /// leaving the button looking dead.
    #[test]
    fn a_rejected_model_file_says_why_in_settings() {
        use crate::state::Dialog;
        let dir = lcc_testkit::TempDir::new("lc-ui-denoise-rejected");
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.set_denoise_models_dir(Some(dir.join("denoise-models")));
        // without the runtime the button is disabled (the workspace build turns it on through the desktop app)
        if h.app.session.execute("denoise.models.list", &json!({})).unwrap()["runtime"] != true {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // another app's model folder: the .onnx with that app's model.json, and no denoise-model.json
        let onnx = dir.join("rawdenoise.onnx");
        std::fs::write(&onnx, b"not a model").unwrap();
        std::fs::write(dir.join("model.json"), br#"{"name": "rawdenoise", "scale": 2}"#).unwrap();
        let picked = onnx.to_string_lossy().into_owned();
        h.app.services.pick_denoise_model = Some(Box::new(move || vec![picked.clone()]));
        h.app.ui.dialog = Some(Dialog::Settings { tab: "ai".into() });
        h.settle(SETTLE);
        crate::settings_screen::reveal(&mut h, "denoise:installFile");
        let r = h.request("ui.clickWidget", json!({"id": "denoise:installFile"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
        let painted = h.painted_text();
        assert!(
            painted
                .iter()
                .any(|s| s.starts_with("No denoise-model.json beside this .onnx file. The model.json there") && s.ends_with("(see docs/denoise.md)")),
            "{painted:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// The pretend host has a clipboard, as a desktop does: text the UI copies lands in it, and a
    /// cut, copy or paste request (from a context menu) comes back as that event on the next frame.
    #[test]
    fn the_headless_host_has_a_clipboard() {
        let mut view = HeadlessView::new();
        let raw = || HeadlessView::raw_input(egui::vec2(200.0, 100.0), 1.0, 0.0, vec![]);
        view.run(raw(), |ui| ui.ctx().copy_text("Lisbon".into()));
        assert_eq!(view.clipboard, "Lisbon");
        for (command, event) in [
            (ViewportCommand::RequestPaste, egui::Event::Paste("Lisbon".into())),
            (ViewportCommand::RequestCopy, egui::Event::Copy),
            (ViewportCommand::RequestCut, egui::Event::Cut),
        ] {
            view.run(raw(), |ui| ui.ctx().send_viewport_cmd(command.clone()));
            let mut seen = vec![];
            view.run(raw(), |ui| seen = ui.input(|i| i.events.clone()));
            assert!(seen.contains(&event), "{command:?}: {seen:?}");
        }
        // an empty clipboard pastes nothing, as a desktop host does
        view.clipboard.clear();
        view.run(raw(), |ui| ui.ctx().send_viewport_cmd(ViewportCommand::RequestPaste));
        let mut seen = vec![];
        view.run(raw(), |ui| seen = ui.input(|i| i.events.clone()));
        assert!(!seen.iter().any(|e| matches!(e, egui::Event::Paste(_))), "{seen:?}");
    }

    /// A coordinate or delta too large for an `f32` became an infinite or NaN pointer position or
    /// scroll delta (egui panics on NaN positions in debug builds). Such a gesture is refused and
    /// queues nothing; ordinary values work, and an over-large `count` / `steps` is refused too
    /// (`control::bounded_arg`: an error, never a silent clamp or a billion queued events).
    #[test]
    fn gestures_refuse_numbers_that_are_not_screen_positions() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [480.0, 320.0], 1.0);
        let t = Duration::from_secs(5);
        h.step();
        let refused = [
            ("ui.click", json!({"x": 1e308})),
            ("ui.click", json!({"y": -1e308})),
            ("ui.move", json!({"x": 1e300, "y": 1.0})),
            ("ui.drag", json!({"x": 1.0, "toY": 1e308})),
            ("ui.dragWidget", json!({"id": "any", "dx": -1e308})),
            ("ui.scroll", json!({"dy": 1e308})),
            ("ui.pointer", json!({"events": [{"kind": "down", "x": 0.5}, {"kind": "move", "x": 1e308}]})),
        ];
        for (method, p) in refused {
            let r = h.request(method, p.clone(), t);
            assert_eq!(r["ok"], false, "{method} {p}: {r}");
            assert!(r["error"].as_str().is_some_and(|e| e.starts_with(method) && e.contains("between")), "{r}");
        }
        let r = h.request("ui.click", json!({"x": 10.0, "y": 5.0, "count": 2}), t);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.drag", json!({"x": 1.0, "y": 1.0, "toX": 999_999.0, "toY": 1.0, "steps": 20}), t);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.click", json!({"x": 10.0, "y": 5.0, "count": u64::MAX}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["error"].as_str().is_some_and(|e| e.contains("`count`")), "{r}");
        let r = h.request("ui.drag", json!({"x": 1.0, "y": 1.0, "toX": 999_999.0, "toY": 1.0, "steps": 1e12}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["error"].as_str().is_some_and(|e| e.contains("`steps`")), "{r}");
        h.step();
    }

    #[test]
    fn screenshot_dimensions_are_checked_before_layout() {
        assert_eq!(viewport_pixels([1600.0, 1000.0], 2.0).unwrap(), [3200, 2000]);
        assert_eq!(viewport_pixels([345.0, 200.0], 0.9).unwrap(), [311, 180]);
        assert_eq!(viewport_pixels([8000.0, 8000.0], 1.0).unwrap(), [8000, 8000]);
        for (size, scale) in [
            ([f32::NAN, 10.0], 1.0),
            ([10.0, f32::INFINITY], 1.0),
            ([10.0, 10.0], f32::INFINITY),
            ([10.0, 10.0], f32::NAN),
            ([10.0, 10.0], 0.0),
            ([0.0, 10.0], 1.0),
            ([10.0, 10.0], 0.01),
            ([9000.0, 9000.0], 1.0),
            ([9000.0, 100.0], 2.0),
        ] {
            assert!(viewport_pixels(size, scale).is_err(), "{size:?} at {scale}");
        }
        let raw = HeadlessView::raw_input(egui::vec2(f32::INFINITY, 10.0), 1.0, 0.0, vec![]);
        assert_eq!(raw.screen_rect.unwrap().size(), egui::vec2(1600.0, 1000.0));
    }

    #[test]
    fn ui_zoom_preserves_host_pixels_and_scales_layout() {
        for (size, scale, expected) in
            [(egui::vec2(400.0, 240.0), 1.0, [400, 240]), (egui::vec2(345.0, 200.0), 0.9, [311, 180]), (egui::vec2(400.0, 240.0), 2.0, [800, 480])]
        {
            let mut view = HeadlessView::new();
            for frame in 0..9 {
                // the interface scale changes between frames (Cmd+Plus / Cmd+Minus are the
                // photo's zoom, issue #566: egui's own keyboard zoom is off)
                match frame {
                    1 => view.ctx.set_zoom_factor(1.1),
                    5 => view.ctx.set_zoom_factor(1.0),
                    _ => {}
                }
                let raw = HeadlessView::raw_input(size, scale, frame as f64 / 60.0, vec![]);
                view.run(raw, |ui| {
                    ui.label("UI zoom changes layout, keeping the host viewport size");
                });
                assert_eq!(view.size_px(), expected, "frame {frame} at scale {scale}");
                assert_eq!(view.paint(&HashMap::new()).size, expected);
                if frame == 4 {
                    assert!(view.ctx.zoom_factor() > 1.0);
                    assert!(view.ctx.content_rect().width() < size.x);
                }
                if frame == 8 {
                    assert!(view.ctx.zoom_factor() < 1.01);
                }
            }
        }
    }

    #[test]
    fn shadow_screenshot_keeps_the_zoomed_hosts_size_and_scale() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [480.0, 320.0], 2.0);
        let t = Duration::from_secs(5);
        h.view.ctx.set_zoom_factor(1.1);
        assert_eq!(h.request("ui.inspect", json!({}), t)["ok"], true);
        assert!(h.view.ctx.zoom_factor() > 1.0);
        let main = h.view.ctx.clone();
        let screenshot = h.app.headless_screenshot(&main, true).unwrap();
        assert_eq!(screenshot.size, [960, 640]);
        let shadow = h.app.shadow.as_ref().unwrap();
        assert!((shadow.ctx.pixels_per_point() - main.pixels_per_point()).abs() < 0.001);
        assert!((shadow.ctx.content_rect().width() - main.content_rect().width()).abs() < 0.1);
    }

    #[test]
    fn ui_set_thumb_size_zero_is_clamped_and_grid_draws_finite_tiles() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [480.0, 320.0], 1.0);
        let t = Duration::from_secs(5);
        for size in [0.0, -5.0, 1e9] {
            let r = h.request("ui.set", json!({"thumbSize": size, "view": "photoGrid"}), t);
            assert_eq!(r["ok"], true, "{r}");
            let got = h.app.ui.thumb_size;
            assert!(got.is_finite() && (crate::state::THUMB_SIZE_MIN..=crate::state::THUMB_SIZE_MAX).contains(&got), "thumbSize {size} -> {got}");
            assert_eq!(r["result"]["thumbSize"].as_f64().map(|v| v as f32), Some(got));
            h.step();
            for w in h.request("ui.widgets", json!({}), t)["result"].as_array().into_iter().flatten() {
                // `rect_json` is `[x, y, width, height]`; a NaN serialises as null
                let rect = w["rect"].as_array().expect("widget rect is an array");
                assert!(rect.len() == 4 && rect.iter().all(|v| v.as_f64().is_some_and(f64::is_finite)), "non-finite widget rect {w}");
            }
        }
    }

    #[test]
    fn control_resize_uses_effective_points_after_ui_zoom() {
        for scale in [0.9, 1.0, 2.0] {
            let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
            app.ui.settings.gpu = false;
            let mut h = Headless::new(app, [480.0, 320.0], scale);
            let t = Duration::from_secs(5);
            assert_eq!(h.request("ui.zoomFactor", json!({"factor": 1.1}), t)["ok"], true);
            h.step();
            assert!(h.view.ctx.zoom_factor() > 1.0);
            let native_size = resized_viewport([320.0, 240.0], scale, h.view.ctx.zoom_factor()).unwrap();
            let expected = native_viewport_pixels([native_size.x, native_size.y], scale).unwrap();
            assert_eq!(h.request("ui.resize", json!({"width": 320, "height": 240}), t)["ok"], true);
            assert_eq!(h.view.size_px(), expected);
            assert!((h.view.ctx.content_rect().width() - 320.0).abs() < 0.1);
            let main = h.view.ctx.clone();
            assert_eq!(h.app.headless_screenshot(&main, true).unwrap().size, expected);
        }
    }

    #[test]
    fn control_resize_keeps_a_minimum_width_after_zooming_out() {
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [480.0, 320.0], 1.0);
        h.view.ctx.set_zoom_factor(0.5);
        let t = Duration::from_secs(5);
        assert_eq!(h.request("ui.resize", json!({"width": 1, "height": 120}), t)["ok"], true);
        assert_eq!(h.size, egui::vec2(0.5, 60.0));
        assert_eq!(h.view.size_px(), [1, 60]);
        assert!((h.view.ctx.content_rect().width() - 1.0).abs() < 0.01);
        let main = h.view.ctx.clone();
        assert_eq!(h.app.headless_screenshot(&main, true).unwrap().size, [1, 60]);
        // a fractional zoom-out rounds in native points
        let mut app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        app.ui.settings.gpu = false;
        let mut h = Headless::new(app, [480.0, 320.0], 0.9);
        let t = Duration::from_secs(5);
        h.view.ctx.set_zoom_factor(0.9);
        h.step();
        assert!((h.view.ctx.zoom_factor() - 0.9).abs() < 0.001);
        assert_eq!(h.request("ui.resize", json!({"width": 150, "height": 150}), t)["ok"], true);
        assert_eq!(h.size, egui::vec2(135.0, 135.0));
        assert_eq!(h.view.size_px(), [122, 122]);
        assert!((h.view.ctx.content_rect().width() - 150.0).abs() < 0.01);
        let main = h.view.ctx.clone();
        assert_eq!(h.app.headless_screenshot(&main, true).unwrap().size, [122, 122]);
    }

    #[test]
    fn invalid_control_resize_preserves_the_headless_viewport() {
        let app = LccApp::new(lcc_engine::Session::new(), crate::Services::default());
        let mut h = Headless::new(app, [480.0, 320.0], 2.0);
        h.app.ui.settings.gpu = false;
        h.step();
        let size = h.size;
        let rejected = h.request("ui.resize", json!({"width": 9000, "height": 8000}), Duration::from_secs(5));
        assert_eq!(rejected["ok"], false, "{rejected}");
        assert_eq!(h.size, size);
        for bad in [json!({"width": "320", "height": 240}), json!({"width": 320, "height": null}), json!({"width": true})] {
            let rejected = h.request("ui.resize", bad, Duration::from_secs(5));
            assert_eq!(rejected["ok"], false, "{rejected}");
            assert_eq!(h.size, size);
        }
        let accepted = h.request("ui.resize", json!({"width": 320, "height": 240}), Duration::from_secs(5));
        assert_eq!(accepted["ok"], true, "{accepted}");
        assert_eq!(h.size, egui::vec2(320.0, 240.0));
        assert_eq!(h.view.size_px(), [640, 480]);
    }

    /// Generous: renders are slow when the machine is loaded (parallel builds), and a timed-out
    /// settle would show a half-rendered UI.
    const SETTLE: Duration = Duration::from_secs(120);

    fn demo(size: [f32; 2]) -> Headless {
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        Headless::new(app, size, 1.0)
    }

    /// Upstream #1639: plain filmstrip clicks in Survey keep the surveyed group (a new photo joins, a
    /// surveyed one only becomes active); ⌘-click still removes and Shift-click still extends.
    #[test]
    fn survey_filmstrip_clicks_keep_the_group() {
        let mut h = demo([1000.0, 700.0]);
        h.settle(SETTLE);
        let t = Duration::from_secs(10);
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().map(|id| id.0).collect();
        assert!(ids.len() >= 5);
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0], ids[1]]}}), t);
        h.request("ui.set", json!({"view": "survey"}), t);
        h.settle(SETTLE);
        for id in &ids[2..4] {
            h.request("ui.clickWidget", json!({"id": format!("film:{id}")}), t);
            h.settle(SETTLE);
        }
        let state = h.request("ui.inspect", json!({}), t)["result"].clone();
        assert_eq!(state["selection"], json!(ids[..4]));
        h.request("ui.clickWidget", json!({"id": format!("film:{}", ids[1])}), t);
        h.settle(SETTLE);
        let state = h.request("ui.inspect", json!({}), t)["result"].clone();
        assert_eq!(state["selection"], json!(ids[..4]));
        assert_eq!(state["active"], ids[1]);
        // Upstream #3716: a right-click on a surveyed photo makes it the active one (the selection stays), and P
        // flags as Pick with the Crop panel open, where X is the crop aspect swap.
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids[..3]}}), t);
        h.settle(SETTLE);
        h.request("ui.clickWidget", json!({"id": format!("film:{}", ids[2]), "button": "right"}), t);
        h.settle(SETTLE);
        let state = h.request("ui.inspect", json!({}), t)["result"].clone();
        assert_eq!(state["selection"], json!(ids[..3]));
        assert_eq!(state["active"], ids[2]);
        h.app.ui.right = crate::state::RightPanel::Crop;
        h.request("ui.key", json!({"key": "p"}), t);
        h.settle(SETTLE);
        let flag = h.app.session.catalog.photo(lcc_catalog::PhotoId(ids[2])).map(|p| p.flag);
        assert_eq!(flag, Some(lcc_catalog::Flag::Pick));
    }

    /// Agents cut, copy and paste in a text field as the system does for ⌘X / ⌘C / ⌘V
    /// (`ui.clipboard`), and `ui.inspect` → `copied` says what the UI last copied.
    #[test]
    fn agents_cut_copy_and_paste_in_text_fields() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        let ask = |h: &mut Headless, method: &str, params: serde_json::Value| {
            let r = h.request(method, params.clone(), t);
            assert_eq!(r["ok"], true, "{method} {params}: {r}");
            h.step();
            h.step();
            r
        };
        h.settle(SETTLE);
        // with no text field focused `ui.clipboard` is refused, rather than doing what ⌘V does there on one
        // platform (paste edit settings) and nothing on another
        let undo = h.app.session.undo.len();
        for action in ["copy", "paste"] {
            let r = h.request("ui.clipboard", json!({"action": action, "text": "travel"}), t);
            assert_eq!(r["ok"], false, "{action}: {r}");
            assert!(r["error"].as_str().is_some_and(|e| e.contains("text field")), "{r}");
        }
        h.settle(SETTLE);
        assert_eq!(h.app.session.undo.len(), undo, "nothing was pasted");
        ask(&mut h, "ui.clickWidget", json!({"id": "field:search"}));
        ask(&mut h, "ui.text", json!({"text": "wedding"}));
        ask(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
        ask(&mut h, "ui.clipboard", json!({"action": "copy"}));
        assert_eq!(ask(&mut h, "ui.inspect", json!({}))["result"]["copied"], "wedding");
        ask(&mut h, "ui.clipboard", json!({"action": "cut"}));
        assert_eq!(h.app.ui.search, "");
        ask(&mut h, "ui.clipboard", json!({"action": "paste", "text": "travel"}));
        assert_eq!(h.app.ui.search, "travel");
        let r = h.request("ui.clipboard", json!({"action": "shred"}), t);
        assert_eq!(r["ok"], false, "an unknown action is refused: {r}");
    }

    /// Fork #46: the filmstrip's photo menu has Add to / Remove from Quick Collection (B), which
    /// toggles the photo in it.
    #[test]
    fn context_menu_toggles_the_quick_collection() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.settle(SETTLE);
        let id = h.app.session.selection.active.expect("an active photo");
        let in_quick = |h: &Headless| {
            h.app.session.catalog.quick_collection().and_then(|q| h.app.session.catalog.album(q)).is_some_and(|a| a.photos.contains(&id))
        };
        for want in [true, false] {
            h.request("ui.clickWidget", json!({"id": format!("film:{}", id.0), "button": "right"}), t);
            h.settle(SETTLE);
            let r = h.request("ui.clickWidget", json!({"id": "button:contextQuickCollection"}), t);
            assert_eq!(r["ok"], true, "{r}");
            h.settle(SETTLE);
            assert_eq!(in_quick(&h), want);
        }
        // #1239: deleting asks first by default, and the context menu's Delete Photo asks too
        assert!(h.app.ui.settings.confirm_delete, "on by default");
        let cell = format!("film:{}", id.0);
        h.request("ui.clickWidget", json!({"id": cell, "button": "right"}), t);
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "button:contextDeletePhoto"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ConfirmDelete { count: 1, disk_files: 0 }));
        assert!(!h.app.session.catalog.photo(id).unwrap().deleted, "not deleted before the answer");
        h.request("ui.dialog.confirm", json!({}), t);
        h.settle(SETTLE);
        assert!(h.app.session.catalog.photo(id).unwrap().deleted, "deleted after OK");
    }

    /// #1877: a right-click while another photo's context menu is open closes that menu and
    /// opens this photo's, which stays open (it flashed and closed again).
    #[test]
    fn right_clicking_elsewhere_moves_the_context_menu() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        let thumbs: Vec<String> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("thumb:")).map(|(w, _)| w.clone()).collect();
        let (Some(a), Some(b)) = (thumbs.first().cloned(), thumbs.get(1).cloned()) else { panic!("two grid photos: {thumbs:?}") };
        let popup = |w: &str| {
            let id: u64 = w.trim_start_matches("thumb:").parse().expect("a photo id");
            egui::Id::new(("cell", id)).with("popup")
        };
        assert!(!egui::Popup::is_any_open(&h.view.ctx));
        let r = h.request("ui.clickWidget", json!({"id": a, "button": "right"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(egui::Popup::is_id_open(&h.view.ctx, popup(&a)), "the first photo's menu is open");
        // A real click: the press and the release reach the UI on different frames, with the
        // pointer resting on the photo in between (`ui.clickWidget` sends both on one frame).
        let rect = h.app.widgets.iter().find(|(w, _)| *w == b).map(|(_, r)| *r).expect("the second photo on screen");
        let at = egui::pos2(rect.left() + rect.width() * 0.9, rect.center().y);
        let button = egui::PointerButton::Secondary;
        let modifiers = egui::Modifiers::NONE;
        h.app.synthetic.push(egui::Event::PointerMoved(at));
        h.step();
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers });
        h.step();
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers });
        // one frame at a time: the menu flashed for a frame or two before it closed
        for frame in 0..6 {
            h.step();
            assert!(!egui::Popup::is_id_open(&h.view.ctx, popup(&a)), "frame {frame}: the first photo's menu closed");
            if frame > 0 {
                assert!(egui::Popup::is_id_open(&h.view.ctx, popup(&b)), "frame {frame}: the second photo's menu is open");
            }
        }
        h.settle(SETTLE);
        assert!(egui::Popup::is_id_open(&h.view.ctx, popup(&b)), "the second photo's menu stays open");
        // The same photo again, elsewhere on it (the menu that is open belongs to this very
        // widget): the menu moves to the pointer and stays.
        let before = h.view.ctx.read_response(popup(&b)).map(|r| r.rect).expect("the menu's area");
        let at = egui::pos2(rect.left() + rect.width() * 0.2, rect.center().y);
        h.app.synthetic.push(egui::Event::PointerMoved(at));
        h.step();
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers });
        h.step();
        h.step();
        h.app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers });
        for frame in 0..6 {
            h.step();
            if frame > 0 {
                assert!(egui::Popup::is_id_open(&h.view.ctx, popup(&b)), "frame {frame}: the menu is open after a right-click on the same photo");
            }
        }
        h.settle(SETTLE);
        assert!(egui::Popup::is_id_open(&h.view.ctx, popup(&b)), "the menu stays open");
        let after = h.view.ctx.read_response(popup(&b)).map(|r| r.rect).expect("the menu's area");
        assert!((after.left() - at.x).abs() < 4.0, "the menu moved to the pointer: {after:?} vs {at:?} (was {before:?})");
    }

    /// #1171: Up / Down in the grid go to the cell above / below; in the filmstrip, one photo.
    #[test]
    fn up_and_down_move_through_the_grid_rows() {
        // (tall enough that the second row is on screen with the filmstrip under the grid: no scroll between them)
        let mut h = demo([1200.0, 960.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "grid"}), t);
        h.settle(SETTLE);
        let first = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first.0]}}), t);
        h.settle(SETTLE);
        let rect = |h: &Headless| {
            let a = h.app.session.selection.active.unwrap();
            h.app.widgets.iter().find(|(w, _)| *w == format!("thumb:{}", a.0)).map(|(_, r)| *r).expect("grid cell")
        };
        let top = rect(&h);
        h.request("ui.key", json!({"key": "down"}), t);
        h.settle(SETTLE);
        let below = rect(&h);
        let pos = h.app.session.visible_position(h.app.session.selection.active.unwrap()).unwrap();
        assert!(below.top() > top.bottom(), "the next row, at {pos}: {top:?} -> {below:?}");
        assert!(below.left() < top.right() && below.right() > top.left(), "overlapping horizontally");
        h.request("ui.key", json!({"key": "up"}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.session.selection.active, Some(first), "back up");
        h.request("ui.key", json!({"key": "up"}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.session.selection.active, Some(first), "the top row stays put");
        // the filmstrip (one row): Down is the next photo
        h.request("ui.set", json!({"view": "detail"}), t);
        h.settle(SETTLE);
        h.request("ui.key", json!({"key": "down"}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_position(h.app.session.selection.active.unwrap()), Some(1));
    }

    /// Issue #1648: clearing the filters from the loupe's pill empties the search field too, so
    /// the field never shows a search that is no longer applied.
    #[test]
    fn clearing_the_loupe_filters_empties_the_search_field() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        for (method, params) in [("ui.clickWidget", json!({"id": "field:search"})), ("ui.text", json!({"text": "a"}))] {
            let r = h.request(method, params, t);
            assert_eq!(r["ok"], true, "{r}");
            h.settle(SETTLE);
        }
        assert_eq!(h.app.session.filter.text, "a", "filtered while typing");
        assert!(!h.app.session.visible_cloned().is_empty(), "the search finds photos, so the loupe has one to show");
        h.request("engine.execute", json!({"command": "view.detail"}), t);
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "loupe:filters"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!((h.app.ui.search.as_str(), h.app.session.filter.text.as_str()), ("", ""));
    }

    /// Esc in the search field gives back the search from before the edit, and the filter with it.
    #[test]
    fn escape_in_the_search_field_gives_back_the_search() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        for (method, params) in [("ui.clickWidget", json!({"id": "field:search"})), ("ui.text", json!({"text": "travel"}))] {
            let r = h.request(method, params, t);
            assert_eq!(r["ok"], true, "{r}");
            h.settle(SETTLE);
        }
        assert_eq!(h.app.session.filter.text, "travel", "filtered while typing");
        let r = h.request("ui.key", json!({"key": "Escape"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!((h.app.ui.search.as_str(), h.app.session.filter.text.as_str()), ("", ""));
        // the search field is a shared text field: right-click ▸ Paste puts the clipboard's text in and filters by it
        h.view.clipboard = "wedding".into();
        for (id, button) in [("field:search", "right"), ("field:search:paste", "left")] {
            let r = h.request("ui.clickWidget", json!({"id": id, "button": button}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
            h.settle(SETTLE);
        }
        assert_eq!(h.app.ui.search, "wedding");
        assert_eq!(h.app.session.filter.text, "wedding", "and the library is filtered by it");
    }

    #[test]
    fn demo_grid_snapshot_has_ui_pixels() {
        let t0 = Instant::now();
        let mut h = demo([1200.0, 760.0]);
        // settle() can see a quiet spell before the first thumbnails land on a loaded machine
        h.step_until(SETTLE, |h| h.app.renderer.thumb_textures() > 0);
        let img = h.snapshot(SETTLE);
        eprintln!("headless snapshot: {:?} in {:?} ({} frames)", img.size, t0.elapsed(), h.frames());
        assert_eq!(img.size, [1200, 760]);
        // not blank: many distinct colours
        let mut colours: Vec<u32> = img.pixels.iter().map(|c| u32::from_le_bytes(c.to_array())).collect();
        colours.sort_unstable();
        colours.dedup();
        assert!(colours.len() > 200, "only {} colours", colours.len());
        // text in the top bar: bright pixels on the dark chrome
        let top_h = crate::theme::Tokens::default().top_bar_h as usize;
        let bright = img.pixels[..top_h * 1200].iter().filter(|c| c.r() > 150 && c.g() > 150 && c.b() > 150).count();
        assert!(bright > 30, "no text in the top bar ({bright} bright px)");
        // thumbnails arrived (photo textures were drawn)
        assert!(h.app.renderer.thumb_textures() > 0);
    }

    /// Keyboard culling: Compare (rating keys hit the candidate, arrows move it, auto-advance),
    /// Survey (keys hit the active photo only), and auto-advance in Detail.
    #[test]
    fn compare_survey_and_auto_advance_by_keyboard() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let rating = |h: &Headless, id: u64| h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().rating;
        let flag = |h: &Headless, id: u64| h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().flag;
        let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0], vis[1]]}}), t);
        let r = h.request("engine.execute", json!({"command": "view.compare"}), t);
        assert_eq!(r["result"], json!({"select": vis[0], "candidate": vis[1]}), "{r}");
        assert_eq!(h.app.ui.view, crate::state::ViewMode::Compare);
        // rating applies to the candidate (active) only
        let before = rating(&h, vis[0]);
        h.request("ui.key", json!({"key": "2"}), t);
        assert_eq!((rating(&h, vis[0]), rating(&h, vis[1])), (before, 2));
        // arrows move the candidate; the select stays
        h.request("ui.key", json!({"key": "right"}), t);
        assert_eq!(h.app.ui.compare, Some((vis[0], vis[2])));
        h.request("ui.key", json!({"key": "left"}), t);
        assert_eq!(h.app.ui.compare, Some((vis[0], vis[1])));
        // Shift+X: reject and advance to the next candidate
        h.request("ui.key", json!({"key": "x", "shift": true}), t);
        assert_eq!(flag(&h, vis[1]), lcc_catalog::Flag::Reject);
        assert_eq!(h.app.ui.compare, Some((vis[0], vis[2])));
        h.request("engine.execute", json!({"command": "compare.swap"}), t);
        assert_eq!(h.app.ui.compare, Some((vis[2], vis[0])));
        h.request("engine.execute", json!({"command": "compare.makeSelect"}), t);
        assert_eq!(h.app.ui.compare.map(|c| c.0), Some(vis[0]));
        // Survey: the selection tiled; keys hit the active photo, auto-advance walks the survey
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[3], vis[4], vis[5]], "active": vis[3]}}), t);
        let r = h.request("engine.execute", json!({"command": "view.survey"}), t);
        assert_eq!(r["result"]["photos"], 3);
        h.request("engine.execute", json!({"command": "view.autoAdvance"}), t);
        assert!(h.app.ui.auto_advance);
        h.request("ui.key", json!({"key": "5"}), t);
        h.request("ui.key", json!({"key": "p"}), t);
        assert_eq!((rating(&h, vis[3]), flag(&h, vis[4])), (5, lcc_catalog::Flag::Pick));
        assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(vis[5]));
        assert_eq!(h.app.session.selection.ids.len(), 3, "the survey keeps its selection");
        let img = h.snapshot(SETTLE);
        assert_eq!(img.size, [1000, 700]);
        // Detail: auto-advance moves to the next photo
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[6]]}}), t);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("ui.key", json!({"key": "u"}), t);
        assert_eq!(h.app.session.selection.active.map(|p| p.0), Some(vis[7]));
        // Escape leaves the culling views for Detail
        h.request("ui.set", json!({"view": "survey"}), t);
        h.request("ui.key", json!({"key": "escape"}), t);
        assert_eq!(h.app.ui.view, crate::state::ViewMode::Detail);
        // let in-flight renders finish: worker threads must not outlive the test process' TLS
        h.settle(SETTLE);
    }

    /// Face boxes are off by default in the loupe; they show when switched on, and a choice saved
    /// since is kept (an older file's `faceBoxes: true` was never the user's choice).
    #[test]
    fn face_boxes_are_off_by_default_and_show_when_switched_on() {
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        assert!(!h.app.ui.face_boxes, "off by default");
        let id = h.app.session.active().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: None,
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        let boxes = |h: &mut Headless| {
            h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), t);
            h.step();
            h.step();
            h.app.widgets.iter().any(|(w, _)| w.starts_with("regionLabel:"))
        };
        assert!(!boxes(&mut h), "no face box over the photo by default");
        let r = h.request("engine.execute", json!({"command": "view.faceBoxes", "params": {}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(boxes(&mut h), "View ▸ Face Boxes switches them on");
        let saved = serde_json::to_value(&h.app.ui).unwrap();
        assert_eq!(saved["faceBoxesOn"], true);
        let back: crate::state::UiState = serde_json::from_value(saved).unwrap();
        assert!(back.face_boxes);
        let old: crate::state::UiState = serde_json::from_value(json!({"faceBoxes": true})).unwrap();
        assert!(!old.face_boxes, "an older build's default is not the user's choice");
        // Naming a face in the loupe: point at an unnamed face, click its "Add name" label, type, Enter.
        use crate::state::NameEdit;
        h.settle(SETTLE);
        // pointing at the box shows the invitation; clicking it opens the name box
        h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), t);
        h.step();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "regionLabel:0"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(matches!(&h.app.ui.name_edit, Some(NameEdit { index: 0, .. })), "{:?}", h.app.ui.name_edit);
        // Escape closes it without naming anything
        let r = h.request("ui.key", json!({"key": "escape"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert!(h.app.ui.name_edit.is_none(), "{:?}", h.app.ui.name_edit);
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].name, None);
        // open it again, type a name, Enter: the face is named, in one undo step
        h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), t);
        h.step();
        h.step();
        h.request("ui.clickWidget", json!({"id": "regionLabel:0"}), t);
        h.step();
        h.step();
        let undo = h.app.session.undo.len();
        h.request("ui.text", json!({"text": "Ann"}), t);
        h.step();
        h.request("ui.key", json!({"key": "enter"}), t);
        h.step();
        h.step();
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].name.as_deref(), Some("Ann"));
        assert_eq!(h.app.session.undo.len(), undo + 1);
        assert!(h.app.ui.name_edit.is_none());
    }

    /// Photo > People acts on the chosen face (the box the pointer rested on in the loupe): disabled without one,
    /// Edit Name opens its name box, Move to Unnamed takes the name off, Remove Face Region deletes the box.
    #[test]
    fn people_menu_items_act_on_the_chosen_face() {
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.active().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: Some("Ann".into()),
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.request("engine.execute", json!({"command": "view.faceBoxes", "params": {"show": true}}), t);
        h.settle(SETTLE);
        for c in ["face.moveToUnnamed", "face.removeRegion", "face.editName", "face.findSimilar"] {
            assert!(!crate::menus::ui_enabled(&h.app, c), "{c} is off with no face chosen");
            let r = h.request("engine.execute", json!({"command": c}), t);
            assert_eq!(r["ok"], false, "{c}: {r}");
        }
        h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), t);
        h.step();
        h.step();
        h.step();
        assert_eq!(crate::face_menu::chosen_face(&h.app), Some((id.0, 0)));
        for c in ["face.moveToUnnamed", "face.removeRegion", "face.editName"] {
            assert!(crate::menus::ui_enabled(&h.app, c), "{c}");
        }
        let r = h.request("engine.execute", json!({"command": "face.editName"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(matches!(&h.app.ui.name_edit, Some(e) if e.index == 0 && e.text == "Ann"));
        h.app.ui.name_edit = None;
        // Find Similar Faces opens the named person's page
        assert!(crate::menus::ui_enabled(&h.app, "face.findSimilar"));
        let r = h.request("engine.execute", json!({"command": "face.findSimilar"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.person_page.as_deref(), Some("Ann"));
        h.app.ui.person_page = None;
        h.app.ui.view = crate::state::ViewMode::Detail;
        // Shift+O does the same while a face is chosen (it cycles the crop overlay otherwise)
        h.request("ui.key", json!({"key": "O", "shift": true}), t);
        h.step();
        assert!(matches!(&h.app.ui.name_edit, Some(e) if e.index == 0), "Shift+O opens the name box");
        h.app.ui.name_edit = None;
        let r = h.request("engine.execute", json!({"command": "face.moveToUnnamed"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().meta.regions[0].name, None);
        assert!(!crate::menus::ui_enabled(&h.app, "face.moveToUnnamed"), "nothing left to take off");
        let r = h.request("engine.execute", json!({"command": "face.removeRegion"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.session.catalog.photo(id).unwrap().meta.regions.is_empty());
        assert!(!crate::menus::ui_enabled(&h.app, "face.removeRegion"));
    }

    /// Photo > People > Draw Face Regions: while the mode is on, a drag over the loupe adds an unnamed face region
    /// (one undo step) and opens its name box; Escape leaves the mode.
    #[test]
    fn draw_face_regions_mode_adds_a_region_by_dragging() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.active().unwrap();
        let before = h.app.session.catalog.photo(id).unwrap().meta.regions.len();
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert_eq!(crate::menubar::checked(&h.app, "face.drawRegions"), Some(false));
        let r = h.request("engine.execute", json!({"command": "face.drawRegions"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(crate::menubar::checked(&h.app, "face.drawRegions"), Some(true));
        h.step();
        let undo = h.app.session.undo.len();
        let c = h.app.canvas_rect.expect("the loupe is on screen");
        let at = |fx: f32, fy: f32| (c.left() + c.width() * fx, c.top() + c.height() * fy);
        let ((x, y), (tx, ty)) = (at(0.45, 0.4), at(0.55, 0.6));
        let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": tx, "toY": ty, "steps": 8}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let regions = h.app.session.catalog.photo(id).unwrap().meta.regions.clone();
        assert_eq!(regions.len(), before + 1, "the drag drew a region");
        let drawn = regions.last().unwrap();
        assert_eq!(drawn.kind, lcc_meta::RegionKind::Face);
        assert!(drawn.name.is_none() && drawn.rect.x1 > drawn.rect.x0 && drawn.rect.y1 > drawn.rect.y0);
        assert_eq!(h.app.session.undo.len(), undo + 1, "one undo step");
        assert!(matches!(&h.app.ui.name_edit, Some(e) if e.index == before), "its name box opened: {:?}", h.app.ui.name_edit);
        // Escape closes the name box first, then leaves the mode
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        h.step();
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        h.step();
        assert_eq!(crate::menubar::checked(&h.app, "face.drawRegions"), Some(false), "Escape leaves the mode");
    }

    /// Adding a face model: the dialog shows the file's terms, the model is installed only once they are
    /// accepted, and a file the app cannot use only says why.
    #[test]
    fn adding_a_face_model_shows_its_terms_and_installs_only_once_accepted() {
        use crate::state::Dialog;
        let dir = lcc_testkit::TempDir::new("lc-ui-facemodel");
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        let model = dir.join("Mine.onnx");
        std::fs::write(&model, lcc_faces::synthetic::tiny_embedder_model(512)).unwrap();
        let open = |h: &mut Headless, path: &std::path::Path| {
            h.request("engine.execute", json!({"command": "dialog.faceModel", "params": {"path": path.to_string_lossy()}}), t)
        };

        // the dialog opens for the chosen file, with nothing accepted and nothing installed
        assert_eq!(open(&mut h, &model)["ok"], true);
        h.settle(SETTLE);
        h.step();
        let Some(dlg) = h.app.ui.dialog.clone() else { panic!("no dialog") };
        let Dialog::FaceModel { info, accepted, .. } = &dlg else { panic!("{dlg:?}") };
        assert!(!accepted);
        assert_eq!(info["kind"], "draft");
        assert!(!dir.join("models").exists());
        // OK does nothing yet
        assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err());
        assert!(!dir.join("models").exists());
        // ticking the box and confirming installs it
        let r = h.request("ui.clickWidget", json!({"id": "check:faceModel.accept"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(matches!(&h.app.ui.dialog, Some(Dialog::FaceModel { accepted: true, .. })));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let listed = h.app.run("faces.models.list", json!({})).unwrap();
        assert!(listed["models"].as_array().unwrap().iter().any(|m| m["installed"] == true && m["known"] == false), "{listed}");
        // installed, in use and recognition on: back in Settings ▸ Faces, where the scan can be watched
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "ai".into() }));
        assert_eq!(listed["enabled"], true);
        h.app.ui.dialog = None;

        // a file that is not a usable model says why and offers no install
        let junk = dir.join("junk.onnx");
        std::fs::write(&junk, b"not a model").unwrap();
        assert_eq!(open(&mut h, &junk)["ok"], true);
        h.step();
        let Some(Dialog::FaceModel { info, .. }) = h.app.ui.dialog.clone() else { panic!("no dialog") };
        assert_eq!(info["kind"], "unsupported");
        // a path that does not exist is an error, not a dialog
        h.app.ui.dialog = None;
        assert_eq!(open(&mut h, &dir.join("nope.onnx"))["ok"], false);
        assert!(h.app.ui.dialog.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Photo > Detect Faces without the face model offers its download (the AI Models dialog: size and licence first,
    /// nothing fetched before the person asks), and Settings > Faces has a Download button for it.
    #[test]
    fn detect_faces_without_the_face_model_offers_its_download_and_fetches_nothing() {
        use crate::state::Dialog;
        let dir = lcc_testkit::TempDir::new("lc-ui-detector");
        let mut h = demo([1400.0, 900.0]);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        h.app.session.segmenter.models_root = Some(dir.join("models"));

        let r = h.app.run("faces.detect", json!({}));
        assert!(r.is_err(), "no face model yet");
        if !lcc_engine::segment::Segmenter::AVAILABLE {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert!(matches!(&h.app.ui.dialog, Some(Dialog::Settings { tab }) if tab == "ai"), "the download is offered: {:?}", h.app.ui.dialog);
        assert!(!dir.join("models").join("mediapipe-face-landmarker").exists(), "nothing is fetched by offering it");

        // Settings > Faces shows the face model with a Download button
        h.app.ui.dialog = Some(Dialog::Settings { tab: "ai".into() });
        h.step();
        // (the first request switches the widget list on, the next frame fills it)
        h.request("ui.widgets", json!({}), Duration::from_secs(10));
        h.step();
        let w = h.request("ui.widgets", json!({"filter": "faces:"}), Duration::from_secs(10));
        assert!(w.to_string().contains("faces:downloadFaceModel"), "{w}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pressing Download in Settings shows the model's terms first and fetches nothing until they are accepted; a build
    /// that cannot run recognition models offers no download. The licence dialog replaces Settings, which it was opened from.
    #[test]
    fn download_shows_the_terms_first_and_fetches_nothing_until_accepted() {
        use crate::state::Dialog;
        let dir = lcc_testkit::TempDir::new("lc-ui-facedl");
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "ai"}}), t);
        h.settle(SETTLE);
        h.step();
        let runtime = h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] == true;
        if runtime {
            crate::settings_screen::reveal(&mut h, "faces:download:sface-2021dec");
        }
        let r = h.request("ui.clickWidget", json!({"id": "faces:download:sface-2021dec"}), t);
        if !runtime {
            assert_eq!(r["ok"], false, "a build that cannot run the model offers no download: {r}");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        let Some(dlg) = h.app.ui.dialog.clone() else { panic!("no dialog") };
        let Dialog::FaceModel { path, info, accepted } = &dlg else { panic!("the terms did not replace Settings: {dlg:?}") };
        assert_eq!((info["download"].as_str(), *accepted, path.as_str()), (Some("sface-2021dec"), false, ""));
        assert_eq!(info["model"]["licence"]["commercial"], "unknown");
        // OK does nothing until the terms are accepted, and nothing has been fetched
        assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err());
        assert_eq!(h.app.session.execute("faces.models.downloads", &json!({})).unwrap()["downloads"], json!([]));
        // cancelling leaves it that way
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        assert!(h.app.ui.dialog.is_none());
        assert_eq!(h.app.session.execute("faces.models.downloads", &json!({})).unwrap()["downloads"], json!([]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Until face recognition is set up, the People view and the loupe's name box say so and offer the next step: with no
    /// model, a button that opens Settings ▸ Faces; with a model installed but recognition off, one that switches it on.
    #[test]
    fn people_and_the_name_box_offer_to_set_face_recognition_up() {
        use crate::state::{Dialog, ViewMode};
        use lcc_catalog::Op;
        let dir = lcc_testkit::TempDir::new("lc-ui-facesetup");
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        let runtime = h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] == true;
        let id = h.app.session.active().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: None,
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        let offers = |h: &mut Headless| h.request("ui.widgets", json!({}), t).to_string().contains("\"faces:setup\"");

        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.settle(SETTLE);
        h.step();
        assert_eq!(h.app.ui.view, ViewMode::People);
        if !runtime {
            assert!(!offers(&mut h), "a build that cannot run recognition offers nothing");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        // no model: the offer is there, and one click lands in Settings ▸ Faces
        assert!(offers(&mut h));
        let r = h.request("ui.clickWidget", json!({"id": "faces:setup"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "faces".into() }));
        h.app.ui.dialog = None;

        // the loupe's name box offers it too, and its button leaves for Settings with the box closed
        // (face boxes are off by default)
        h.request("engine.execute", json!({"command": "view.faceBoxes", "params": {"show": true}}), t);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), t);
        h.step();
        h.step();
        h.request("ui.clickWidget", json!({"id": "regionLabel:0"}), t);
        h.step();
        h.step();
        assert!(h.app.ui.name_edit.is_some() && offers(&mut h), "the name box offers the setup");
        let r = h.request("ui.clickWidget", json!({"id": "faces:setup"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert!(h.app.ui.name_edit.is_none());
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "faces".into() }));
        h.app.ui.dialog = None;

        // a model installed but recognition off: "Turn on" does it on the spot
        let model = dir.join("Mine.onnx");
        std::fs::write(&model, lcc_faces::synthetic::tiny_embedder_model(512)).unwrap();
        let installed = h.app.run("faces.models.install", json!({"path": model.to_string_lossy(), "acknowledged": true, "activate": false})).unwrap();
        h.app.run("faces.models.select", json!({"id": installed["installed"]["id"]})).unwrap();
        h.app.caches.faces_epoch += 1;
        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.settle(SETTLE);
        h.step();
        assert!(offers(&mut h));
        assert_eq!(h.app.session.execute("faces.models.list", &json!({})).unwrap()["enabled"], false);
        let r = h.request("ui.clickWidget", json!({"id": "faces:setup"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.session.execute("faces.models.list", &json!({})).unwrap()["enabled"], true);
        assert_eq!(h.app.ui.dialog, None, "nothing to go to Settings for");
        h.step();
        assert!(!offers(&mut h), "once it is on the offer is gone");
        h.settle(SETTLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A click on a person's card opens their page, which shows only cropped faces (one per face, not per photo); a click
    /// on one opens its photo; Back, the People button and Escape return to everyone.
    #[test]
    fn a_persons_page_shows_their_faces_and_back_returns_to_everyone() {
        use crate::state::ViewMode;
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(3).collect();
        let face = |x: f64, name: &str| lcc_meta::Region {
            rect: lcc_geom::Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: Some(name.to_string()),
            description: None,
        };
        // Jane Doe twice in the first photo and once in the second; John Roe in the third
        for (id, regions) in [
            (ids[0], vec![face(0.1, "Jane Doe"), face(0.5, "jane doe")]),
            (ids[1], vec![face(0.3, "Jane Doe")]),
            (ids[2], vec![face(0.3, "John Roe")]),
        ] {
            let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
            meta.regions = regions;
            h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        }
        let open_people = |h: &mut Headless| {
            h.request("engine.execute", json!({"command": "view.people"}), t);
            h.settle(SETTLE);
            h.step();
        };
        open_people(&mut h);
        assert_eq!((h.app.ui.view, h.app.ui.person_page.clone()), (ViewMode::People, None));
        // the card opens the page: a face tile for each of the three faces, not a tile per photo
        let r = h.request("ui.clickWidget", json!({"id": "person:Jane Doe"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!((h.app.ui.view, h.app.ui.person_page.as_deref()), (ViewMode::People, Some("Jane Doe")));
        let widgets = h.request("ui.widgets", json!({}), t).to_string();
        for (id, index) in [(ids[0], 0), (ids[0], 1), (ids[1], 0)] {
            assert!(widgets.contains(&format!("\"person-face:{}:{index}\"", id.0)), "a tile for each face: {widgets}");
        }
        assert!(!widgets.contains(&format!("\"person-face:{}:0\"", ids[2].0)), "someone else's face is not on the page");
        // a face opens its photo in the detail view
        h.request("ui.clickWidget", json!({"id": format!("person-face:{}:0", ids[1].0)}), t);
        h.step();
        h.step();
        assert_eq!((h.app.ui.view, h.app.session.active()), (ViewMode::Detail, Some(ids[1])));
        // Escape from that photo goes back to the page, not to the grid
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        assert_eq!((h.app.ui.view, h.app.ui.person_page.as_deref()), (ViewMode::People, Some("Jane Doe")));
        // the People button from a photo shows everyone, with the person last left one click away next to the title
        h.request("ui.clickWidget", json!({"id": format!("person-face:{}:0", ids[1].0)}), t);
        h.step();
        h.step();
        assert_eq!(h.app.ui.view, ViewMode::Detail);
        open_people(&mut h);
        assert_eq!((h.app.ui.view, h.app.ui.person_page.clone()), (ViewMode::People, None));
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "people:last"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!((h.app.ui.view, h.app.ui.person_page.as_deref()), (ViewMode::People, Some("Jane Doe")));
        // a photo reached any other way goes back to the grid on Escape
        h.request("engine.execute", json!({"command": "view.detail"}), t);
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        assert_eq!(h.app.ui.view, ViewMode::PhotoGrid);
        h.request("engine.execute", json!({"command": "view.person", "params": {"name": "Jane Doe"}}), t);
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "person:back"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.person_page, None);
        h.request("engine.execute", json!({"command": "view.person", "params": {"name": "Jane Doe"}}), t);
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        assert_eq!((h.app.ui.view, h.app.ui.person_page.clone()), (ViewMode::People, None), "Escape goes back to everyone");
        h.request("engine.execute", json!({"command": "view.person", "params": {"name": "John Roe"}}), t);
        open_people(&mut h);
        assert_eq!(h.app.ui.person_page, None, "the People button shows everyone");
        // a missing or empty name is refused
        assert_eq!(h.request("engine.execute", json!({"command": "view.person", "params": {}}), t)["ok"], false);
        assert_eq!(h.request("engine.execute", json!({"command": "view.person", "params": {"name": "  "}}), t)["ok"], false);
    }

    /// A person's More grid confirms a score-bounded batch through either ↑ or the focused tile's A key. Both
    /// routes use one undo step and leave hidden, lower-score and already named faces alone.
    #[test]
    fn person_more_confirms_all_above_with_one_undo_step() {
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(7).collect();
        assert_eq!(ids.len(), 7);
        let region = |name: Option<&str>| lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.25, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lcc_meta::RegionKind::Face,
            name: name.map(str::to_string),
            description: None,
        };
        for (id, name) in ids.iter().zip([Some("Jane Doe"), None, None, None, None, None, Some("John Roe")]) {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = vec![region(name)];
            h.app.session.commit("setup", Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
        }
        h.request("engine.execute", json!({"command": "view.person", "params": {"name": "Jane Doe"}}), t);
        h.settle(SETTLE);
        let page = crate::panels::person::PersonPage {
            name: "Jane Doe".into(),
            total: 1,
            confirmed: vec![crate::panels::person::Face { photo: ids[0].0, index: 0, rect: region(None).rect, score: 0.0 }],
            more: ids[1..6]
                .iter()
                .zip([0.95, 0.80, 0.80, 0.80, 0.60])
                .map(|(id, score)| crate::panels::person::Face { photo: id.0, index: 0, rect: region(None).rect, score })
                .collect(),
            ready: true,
            pending: 0,
        };
        let set_page = |h: &mut Headless| {
            h.app.caches.person_page =
                Some(("Jane Doe".into(), h.app.session.catalog.revision, h.app.caches.faces_indexed, 0.0, Arc::new(page.clone())));
            h.step();
        };
        h.app.ui.dismissed_faces.insert((ids[4].0, 0));
        set_page(&mut h);
        let bulk_id = format!("more-face-above:{}:0", ids[2].0);
        assert!(h.request("ui.widgets", json!({"filter": "more-face-above:"}), t).to_string().contains(&bulk_id));
        let undo = h.app.session.undo.len();
        let r = h.request("ui.clickWidget", json!({"id": bulk_id}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        let names = |h: &Headless| ids.iter().map(|id| h.app.session.catalog.photo(*id).unwrap().meta.regions[0].name.clone()).collect::<Vec<_>>();
        assert_eq!(
            names(&h),
            vec![
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                None,
                None,
                Some("John Roe".into())
            ]
        );
        assert_eq!(h.app.session.undo.len(), undo + 1);
        h.request("engine.execute", json!({"command": "edit.undo"}), t);
        h.step();
        assert_eq!(names(&h), vec![Some("Jane Doe".into()), None, None, None, None, None, Some("John Roe".into())]);
        // With no target A cannot name a face, even on this page.
        h.request("ui.move", json!({"x": 1, "y": 1}), t);
        h.view.ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(("more-face-above", ids[2].0, 0_usize))));
        set_page(&mut h);
        h.request("ui.key", json!({"key": "a"}), t);
        h.step();
        assert_eq!(names(&h)[1], None);
        // Text editing owns A even if a face was the last pointer target.
        let search = h.request("ui.clickWidget", json!({"id": "field:search"}), t);
        assert_eq!(search["ok"], true, "{search}");
        h.step();
        h.request("ui.key", json!({"key": "a"}), t);
        h.step();
        assert_eq!(names(&h)[1], None);
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        // Hovering a tile is enough for the A key, without clicking or moving keyboard focus there.
        let tile_id = format!("more-face:{}:0", ids[2].0);
        let rect = h.app.widgets.iter().find(|(id, _)| id == &tile_id).unwrap().1;
        h.request("ui.move", json!({"x": rect.center().x, "y": rect.center().y}), t);
        h.step();
        h.request("ui.key", json!({"key": "a"}), t);
        h.step();
        assert_eq!(names(&h)[1..4], [Some("Jane Doe".into()), Some("Jane Doe".into()), Some("Jane Doe".into())]);
        assert_eq!(h.app.session.undo.len(), undo + 1);
        h.request("engine.execute", json!({"command": "edit.undo"}), t);
        h.step();
        h.request("ui.move", json!({"x": 1, "y": 1}), t);
        set_page(&mut h);
        // Keyboard focus on the candidate makes the same batch action available without hovering it.
        h.view.ctx.memory_mut(|m| m.request_focus(egui::Id::new(("more-face", ids[2].0, 0_usize))));
        h.request("ui.key", json!({"key": "a"}), t);
        h.step();
        assert_eq!(
            names(&h),
            vec![
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                Some("Jane Doe".into()),
                None,
                None,
                Some("John Roe".into())
            ]
        );
        assert_eq!(h.app.session.undo.len(), undo + 1);
        h.request("engine.execute", json!({"command": "view.photoGrid"}), t);
        h.request("ui.key", json!({"key": "a"}), t);
        h.step();
        assert_eq!(h.app.session.undo.len(), undo + 1, "A outside the person page cannot name more faces");
    }

    /// The People view lists the unnamed faces below the named people: select some (a click each, or all), type a name,
    /// press Enter, and they are all named at once, in one undo step; the new person appears among the named.
    #[test]
    fn unnamed_faces_are_selected_and_named_together() {
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(3).collect();
        let face = |x: f64, name: Option<&str>| lcc_meta::Region {
            rect: lcc_geom::Rect { x0: x, y0: 0.2, x1: x + 0.2, y1: 0.55 },
            kind: lcc_meta::RegionKind::Face,
            name: name.map(str::to_string),
            description: None,
        };
        for (id, regions) in [
            (ids[0], vec![face(0.1, Some("Jane Doe")), face(0.5, None)]),
            (ids[1], vec![face(0.3, None), face(0.6, None)]),
            (ids[2], vec![face(0.3, None)]),
        ] {
            let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
            meta.regions = regions;
            h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        }
        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.settle(SETTLE);
        h.step();
        h.step();
        let tiles = |h: &mut Headless| h.request("ui.widgets", json!({"filter": "unnamed-face:"}), t)["result"].as_array().map_or(0, Vec::len);
        // the named person is a card, and the four unnamed faces are tiles below
        let r = h.request("ui.clickWidget", json!({"id": "person:Jane Doe"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.step();
        h.step();
        assert_eq!(tiles(&mut h), 4);
        // select two (nothing is named by selecting), and the naming bar appears
        for (id, i) in [(ids[1], 0), (ids[2], 0)] {
            let r = h.request("ui.clickWidget", json!({"id": format!("unnamed-face:{}:{i}", id.0)}), t);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
            h.step();
        }
        assert_eq!(h.app.ui.unnamed_selected.len(), 2);
        let bar = h.request("ui.widgets", json!({"filter": "unnamed:"}), t).to_string();
        assert!(bar.contains("unnamed:name") && bar.contains("unnamed:clear"), "{bar}");
        assert!(h.request("ui.widgets", json!({"filter": "field:unnamedName"}), t).to_string().contains("field:unnamedName"));
        // type a name and press Enter: both faces are named, in one undo step, and the selection is gone
        let undo = h.app.session.undo.len();
        h.request("ui.text", json!({"text": "Ann Example"}), t);
        h.step();
        h.request("ui.key", json!({"key": "enter"}), t);
        h.step();
        h.step();
        let named = |h: &Headless, id: lcc_catalog::PhotoId, i: usize| h.app.session.catalog.photo(id).unwrap().meta.regions[i].name.clone();
        assert_eq!((named(&h, ids[1], 0), named(&h, ids[2], 0)), (Some("Ann Example".to_string()), Some("Ann Example".to_string())));
        assert_eq!(named(&h, ids[1], 1), None, "a face that was not selected stays unnamed");
        assert_eq!(h.app.session.undo.len(), undo + 1, "one step for both");
        assert!(h.app.ui.unnamed_selected.is_empty() && h.app.ui.unnamed_name.is_empty());
        h.settle(SETTLE);
        h.step();
        assert_eq!(tiles(&mut h), 2, "the named faces left the unnamed list");
        assert_eq!(h.app.session.catalog.people().len(), 2, "and the new person is among the named");
        // select all, then clear: nothing is named
        h.request("ui.clickWidget", json!({"id": "unnamed:selectAll"}), t);
        h.step();
        h.step();
        assert_eq!(h.app.ui.unnamed_selected.len(), 2);
        h.request("ui.clickWidget", json!({"id": "unnamed:clear"}), t);
        h.step();
        assert!(h.app.ui.unnamed_selected.is_empty());
        // undo gives the two faces back
        h.request("engine.execute", json!({"command": "edit.undo"}), t);
        h.step();
        assert_eq!(named(&h, ids[1], 0), None);
        // ignore the selected faces: they leave the list, and the People header offers to restore them
        h.settle(SETTLE);
        h.step();
        let before = tiles(&mut h);
        h.request("ui.clickWidget", json!({"id": "unnamed:selectAll"}), t);
        h.step();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "unnamed:ignore"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.settle(SETTLE);
        h.step();
        h.step();
        assert!(tiles(&mut h) < before, "ignored faces left the unnamed list");
        let r = h.request("ui.clickWidget", json!({"id": "people:restoreIgnored"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.settle(SETTLE);
        h.step();
        h.step();
        assert_eq!(tiles(&mut h), before, "restored");
    }

    /// A double click on an unnamed face opens its photo in the loupe, as in the grid (#1664); a single click still only
    /// selects it, and Escape from that photo goes back to the People view.
    #[test]
    fn double_clicking_an_unnamed_face_opens_its_photo() {
        use crate::state::ViewMode;
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(2).collect();
        for id in &ids {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = vec![lcc_meta::Region {
                rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.5, y1: 0.55 },
                kind: lcc_meta::RegionKind::Face,
                name: None,
                description: None,
            }];
            h.app.session.commit("setup", Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
        }
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0].0]}}), t);
        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.settle(SETTLE);
        h.step();
        h.step();
        let tile = format!("unnamed-face:{}:0", ids[1].0);
        // one click selects the face and stays in People
        let r = h.request("ui.clickWidget", json!({"id": tile}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!((h.app.ui.view, h.app.session.active()), (ViewMode::People, Some(ids[0])));
        assert_eq!(h.app.ui.unnamed_selected.len(), 1);
        h.request("ui.clickWidget", json!({"id": "unnamed:clear"}), t);
        // half a second of frames, so egui does not count the next two clicks as the third and fourth of one burst
        for _ in 0..30 {
            h.step();
        }
        // two open the photo the face is in
        let r = h.request("ui.clickWidget", json!({"id": tile, "count": 2}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!((h.app.ui.view, h.app.session.active()), (ViewMode::Detail, Some(ids[1])));
        assert_eq!(h.app.session.catalog.photo(ids[1]).unwrap().meta.regions[0].name, None, "nothing is named");
        // Escape goes back to the faces, not to the grid
        h.request("ui.key", json!({"key": "escape"}), t);
        h.step();
        assert_eq!((h.app.ui.view, h.app.ui.person_page.clone()), (ViewMode::People, None));
    }

    /// The name field under the unnamed faces is a shared text field: right-click ▸ Paste puts the
    /// clipboard's name in, and Esc leaves the field and clears the selection, as Clear does.
    #[test]
    fn the_unnamed_faces_name_field_pastes_and_escape_clears() {
        use lcc_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.catalog.photos().map(|p| p.id).next().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lcc_meta::Region {
            rect: lcc_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.5, y1: 0.55 },
            kind: lcc_meta::RegionKind::Face,
            name: None,
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        h.request("engine.execute", json!({"command": "view.people"}), t);
        h.settle(SETTLE);
        h.view.clipboard = "Wedding Guest".into();
        for (widget, button) in [
            (format!("unnamed-face:{}:0", id.0), "left"),
            ("field:unnamedName".to_string(), "left"),
            ("field:unnamedName".to_string(), "right"),
            ("field:unnamedName:paste".to_string(), "left"),
        ] {
            let r = h.request("ui.clickWidget", json!({"id": widget, "button": button}), t);
            assert_eq!(r["ok"], true, "{widget}: {r}");
            h.step();
            h.step();
            h.step();
        }
        assert_eq!(h.app.ui.unnamed_name, "Wedding Guest");
        assert_eq!(h.app.ui.unnamed_selected.len(), 1, "still naming");
        h.request("ui.key", json!({"key": "Escape"}), t);
        h.step();
        h.step();
        assert!(h.app.ui.unnamed_selected.is_empty() && h.app.ui.unnamed_name.is_empty(), "Esc cleared the selection and the name");
    }

    /// A screenful of faces larger than the picture cache's usual budget (96) is all kept: with a fixed budget the same few
    /// tiles were evicted and re-requested every frame and stayed blank.
    #[test]
    fn a_screenful_of_small_faces_is_not_evicted_and_left_blank() {
        use lcc_catalog::Op;
        let mut h = demo([1500.0, 1000.0]);
        let t = Duration::from_secs(20);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).collect();
        // seven unnamed faces in every photo of the demo library
        for id in &ids {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = (0..7)
                .map(|k| lcc_meta::Region {
                    rect: lcc_geom::Rect { x0: 0.05 + 0.12 * k as f64, y0: 0.2, x1: 0.15 + 0.12 * k as f64, y1: 0.45 },
                    kind: lcc_meta::RegionKind::Face,
                    name: None,
                    description: None,
                })
                .collect();
            h.app.session.commit("setup", Op::SetMeta { id: *id, meta: Box::new(meta) }).unwrap();
        }
        // the smallest faces, so a lot of them fit on the screen
        h.request("ui.set", json!({"thumbSize": 90.0, "view": "people"}), t);
        h.settle(SETTLE);
        h.step();
        let drawn = h.request("ui.widgets", json!({"filter": "unnamed-face:"}), t)["result"].as_array().map_or(0, Vec::len);
        assert!(drawn > 96, "the test needs more faces on screen than the usual budget, got {drawn}");
        // the pictures are made and kept, well beyond the old budget (a few demo photos share a scene, so two faces can share a
        // picture: the count is a little under the number of tiles)
        let started = Instant::now();
        while h.app.renderer.variant_textures() < drawn - 20 && started.elapsed() < Duration::from_secs(60) {
            h.step();
            std::thread::sleep(Duration::from_millis(20));
        }
        let kept = h.app.renderer.variant_textures();
        assert!(kept > 96 + 30, "{kept} pictures kept for {drawn} faces on screen");
        // and they stay: many more frames, and none is evicted to be asked for again
        for _ in 0..30 {
            h.step();
        }
        assert!(h.app.renderer.variant_textures() >= kept, "{} pictures kept after more frames, {kept} before", h.app.renderer.variant_textures());
    }

    /// Profile browser: live variant thumbnails, hover previews in the loupe without touching the
    /// photo or its history, click applies, the star toggles the favourite.
    #[test]
    fn profile_browser_previews_on_hover_and_applies_on_click() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        let r = h.request("engine.execute", json!({"command": "panel.profiles"}), t);
        assert_eq!(r["result"]["open"], true, "{r}");
        h.settle(SETTLE);
        let id = h.app.session.active().unwrap();
        let photo = |h: &Headless| h.app.session.catalog.photo(id).unwrap().clone();
        let before = photo(&h);
        h.step_until(SETTLE, |h| h.app.renderer.variant_textures() >= 6);
        assert!(h.app.renderer.variant_textures() >= 6, "variant thumbnails rendered: {}", h.app.renderer.variant_textures());
        // hover: the loupe shows the look, nothing is committed
        let r = h.request("ui.hoverWidget", json!({"id": "profileCell:lc.vivid"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        // the hover render can land after a quiet spell on a loaded machine (FreeBSD CI): wait for it
        h.step_until(SETTLE, |h| h.app.loupe_shown.map(|l| l.1) == Some("hover"));
        assert_eq!(h.app.hover_preview.as_ref().map(|p| p.label.as_str()), Some("Profile: Vivid"));
        assert_eq!(h.app.loupe_shown.map(|l| l.1), Some("hover"));
        let hover = h.app.renderer.textures.get(&crate::render::Slot::Hover).expect("hover render");
        assert_eq!(hover.photo, id);
        let after = photo(&h);
        assert_eq!((after.develop.clone(), after.history.len()), (before.develop.clone(), before.history.len()), "hover leaves the photo alone");
        assert_eq!(h.app.session.undo.len(), 0);
        // moving away ends the preview
        h.request("ui.move", json!({"x": 5, "y": 500}), t);
        h.step();
        assert!(h.app.hover_preview.is_none());
        // click applies (one history step); the star toggles the favourite
        h.request("ui.clickWidget", json!({"id": "profileCell:lc.vivid"}), t);
        assert_eq!(photo(&h).develop.profile.id, "lc.vivid");
        assert_eq!(photo(&h).history.len(), before.history.len() + 1);
        h.request("ui.clickWidget", json!({"id": "profileStar:lc.portrait"}), t);
        assert_eq!(h.app.session.profile_favorites, ["lc.portrait"]);
        assert_eq!(photo(&h).develop.profile.id, "lc.vivid", "the star doesn't apply");
        h.settle(SETTLE);
        // hovering the applied profile shows the applied look (no preview), not the last other profile hovered
        h.request("ui.hoverWidget", json!({"id": "profileCell:lc.portrait"}), t);
        h.step();
        assert!(h.app.hover_preview.is_some(), "another profile previews");
        h.request("ui.hoverWidget", json!({"id": "profileCell:lc.vivid"}), t);
        h.step();
        assert!(h.app.hover_preview.is_none(), "the applied profile is shown, not the last hovered one");
        h.settle(SETTLE);
    }

    /// Profile groups start folded except the one holding the photo's profile; folding or
    /// unfolding one is kept in the UI state (ui.json), so it survives a restart.
    #[test]
    fn profile_groups_start_folded_and_remember_their_state() {
        // (tall enough for the last group under Develop's histogram and tool row)
        let mut h = demo([1400.0, 1200.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.profiles"}), t);
        h.settle(SETTLE);
        let current = h.app.session.active().and_then(|id| h.app.session.develop_of(id)).map(|d| d.profile.id.clone()).unwrap();
        let cells = |h: &Headless| h.app.widgets.iter().filter(|(w, _)| w.starts_with("profileCell:")).map(|(w, _)| w.clone()).collect::<Vec<_>>();
        let shown = cells(&h);
        assert!(shown.iter().any(|w| w.ends_with(&current)), "the current profile's group is open: {shown:?}");
        let groups: Vec<String> = h.app.widgets.iter().filter(|(w, _)| w.starts_with("profileGroup:")).map(|(w, _)| w.clone()).collect();
        assert!(groups.len() > 1, "{groups:?}");
        // a folded group opens on a click, and that is remembered
        let folded = groups.iter().rfind(|g| !g.starts_with("profileGroup:lut:")).cloned().unwrap();
        let before = cells(&h).len();
        h.request("ui.clickWidget", json!({"id": folded}), t);
        h.settle(SETTLE);
        assert!(cells(&h).len() > before, "{folded} opened");
        let saved = serde_json::to_value(&h.app.ui).unwrap();
        assert_eq!(saved["groupOpen"][folded.replacen("profileGroup:", "profile:", 1)], true, "{}", saved["groupOpen"]);
    }

    /// The preset applied to a photo is still marked (and its Amount slider back) after visiting
    /// another photo, which shows none (issue #2392); an edit to the photo drops the mark.
    /// The fork's preset groups start folded (one holding the applied preset opens): open the
    /// group holding `preset` so its row is on screen.
    fn open_preset_group(h: &mut Headless, preset: &str) {
        let group = h.app.session.presets.iter().find(|p| p.id == preset).map(|p| p.group.clone()).unwrap();
        h.app.ui.group_open.insert(format!("preset:{group}"), true);
    }

    #[test]
    fn applied_preset_is_marked_again_on_returning_to_the_photo() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.presets"}), t);
        open_preset_group(&mut h, "lc.warm-glow");
        h.settle(SETTLE);
        let a = h.app.session.active().unwrap();
        let b = h.app.session.visible_cloned().into_iter().find(|id| *id != a).unwrap();
        let has = |h: &Headless, w: &str| h.app.widgets.iter().any(|(id, _)| id == w);
        let marked = |h: &Headless| {
            let r = h.app.widgets.iter().find(|(id, _)| id == "presetApplied").map(|(_, r)| *r)?;
            h.app.widgets.iter().find(|(id, wr)| id.starts_with("preset:") && *wr == r).map(|(id, _)| id.clone())
        };
        let r = h.request("ui.clickWidget", json!({"id": "preset:lc.warm-glow"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(marked(&h).as_deref(), Some("preset:lc.warm-glow"));
        assert!(has(&h, "slider:presetAmount"));
        for (photo, want) in [(b, None), (a, Some("preset:lc.warm-glow"))] {
            h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [photo.0]}}), t);
            h.settle(SETTLE);
            h.step();
            assert_eq!(marked(&h).as_deref(), want, "on photo {photo:?}");
            assert_eq!(has(&h, "slider:presetAmount"), want.is_some(), "the Amount slider follows the mark");
        }
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        h.settle(SETTLE);
        h.step();
        assert_eq!(marked(&h), None, "an edit after the preset drops the mark");
    }

    /// A preset applied while the active photo lies outside the selection changes only the
    /// selection, so the active photo isn't marked as having it.
    #[test]
    fn applied_preset_is_not_marked_on_an_active_photo_outside_the_selection() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.presets"}), t);
        open_preset_group(&mut h, "lc.warm-glow");
        h.settle(SETTLE);
        let a = h.app.session.active().unwrap();
        let b = h.app.session.visible_cloned().into_iter().find(|id| *id != a).unwrap();
        h.app.session.selection.ids = vec![b];
        h.app.session.selection.active = Some(a);
        let before = h.app.session.develop_of(a).unwrap();
        let r = h.request("ui.clickWidget", json!({"id": "preset:lc.warm-glow"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        h.step();
        assert_eq!(h.app.session.active(), Some(a));
        assert!(*h.app.session.develop_of(a).unwrap() == *before, "the active photo wasn't changed");
        assert!(!h.app.widgets.iter().any(|(id, _)| id == "presetApplied"), "nor marked");
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [b.0]}}), t);
        h.settle(SETTLE);
        h.step();
        assert!(h.app.widgets.iter().any(|(id, _)| id == "presetApplied"), "the selected photo is");
    }

    /// Presets column: resting on a preset previews it in the loupe without a history entry;
    /// thumbnails are variant renders; Create Preset includes only the checked groups.
    #[test]
    fn preset_hover_previews_and_create_preset_groups() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        // (preset groups start folded)
        let groups: Vec<String> = h.app.session.presets.iter().map(|p| format!("preset:{}", p.group)).collect();
        h.app.ui.group_open.extend(groups.into_iter().map(|g| (g, true)));
        crate::panels::presets::set_shown(&mut h.app, false);
        h.request("engine.execute", json!({"command": "panel.presets"}), t);
        h.settle(SETTLE);
        let id = h.app.session.active().unwrap();
        let photo = |h: &Headless| h.app.session.catalog.photo(id).unwrap().clone();
        let before = photo(&h);
        h.request("ui.hoverWidget", json!({"id": "preset:lc.bw-high-contrast"}), t);
        h.settle(SETTLE);
        h.step_until(SETTLE, |h| h.app.loupe_shown.map(|l| l.1) == Some("hover"));
        assert_eq!(h.app.hover_preview.as_ref().map(|p| p.label.as_str()), Some("Preset: High Contrast B&W"));
        assert_eq!(h.app.loupe_shown.map(|l| l.1), Some("hover"));
        let after = photo(&h);
        assert_eq!(after.develop, before.develop);
        assert_eq!(after.history.len(), before.history.len(), "no history entry while hovering");
        assert!(h.app.session.undo.is_empty());
        // hovering down across group headers within the presets list retains the preview (no flicker)
        h.request("ui.hoverWidget", json!({"id": "presetGroup:Creative"}), t);
        h.step();
        assert_eq!(h.app.hover_preview.as_ref().map(|p| p.label.as_str()), Some("Preset: High Contrast B&W"));
        // hovering another preset switches smoothly to it
        h.request("ui.hoverWidget", json!({"id": "preset:lc.warm-glow"}), t);
        h.settle(SETTLE);
        h.step();
        assert_eq!(h.app.hover_preview.as_ref().map(|p| p.label.as_str()), Some("Preset: Warm Glow"));
        // thumbnails: variant textures for the visible presets
        h.app.ui.preset_thumbs = true;
        // (off the left column, where the presets now are)
        let away = h.app.image_rect.map_or(egui::pos2(800.0, 500.0), |r| r.center());
        h.request("ui.move", json!({"x": away.x, "y": away.y}), t);
        h.settle(SETTLE);
        h.step();
        assert!(h.app.hover_preview.is_none());
        assert!(h.app.renderer.variant_textures() >= 5, "{}", h.app.renderer.variant_textures());
        // Create Preset with a group checklist: untick Light, keep Effects
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "effects.clarity", "value": 25}}), t);
        h.app.ui.dialog = Some(crate::state::Dialog::create_preset(&h.app.session));
        h.step();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "presetInclude:light"}), t);
        assert_eq!(r["ok"], true, "{r}");
        match &h.app.ui.dialog {
            Some(crate::state::Dialog::CreatePreset { groups, .. }) => {
                assert!(!groups.iter().any(|g| g.starts_with("light")) && groups.iter().any(|g| g == "effects.clarity"), "{groups:?}");
                assert!(!groups.iter().any(|g| g == "whiteBalance"), "White Balance off by default");
                assert!(!groups.iter().any(|g| g.starts_with("crop") || g == "masks"), "crop and masks off by default");
            }
            d => panic!("dialog: {d:?}"),
        }
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let p = h.app.session.presets.iter().find(|p| !p.builtin).expect("created").clone();
        assert!(p.settings.get("light").is_none() && p.settings["effects"]["clarity"] == 25.0, "{}", p.settings);
        h.settle(SETTLE);
    }

    /// A collection of thousands of presets (a Lightroom library imported whole) lists only the
    /// rows in view: a few dozen widgets and thumbnails a frame, not one per preset (every frame
    /// built all of them and froze the app for seconds); scrolling brings later rows in.
    #[test]
    fn presets_panel_with_thousands_of_presets_builds_only_visible_rows() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let many = (0..5000)
            .map(|i| lcc_develop::Preset {
                id: format!("user.big.{i}"),
                name: format!("Look {i}"),
                group: format!("Big {:02}", i / 500),
                settings: json!({"light": {"exposure": (i % 20) as f64 / 10.0}}),
                favorite: false,
                builtin: false,
            })
            .collect();
        assert_eq!(h.app.session.add_presets(many).len(), 5000);
        // groups start folded: unfold them all, so thousands of rows could be built
        h.app.ui.group_open.extend((0..10).map(|g| (format!("preset:Big {g:02}"), true)));
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.presets"}), t);
        h.app.ui.preset_thumbs = true;
        h.settle(SETTLE);
        h.step();
        let rows = |h: &Headless| h.app.widgets.iter().filter(|(w, _)| w.starts_with("preset:")).map(|(w, _)| w.clone()).collect::<Vec<_>>();
        let first = rows(&h);
        assert!((5..60).contains(&first.len()), "{} rows built", first.len());
        assert!(h.app.renderer.variant_textures() < 60, "{} thumbnails", h.app.renderer.variant_textures());
        assert!(h.app.widgets.iter().any(|(w, _)| w == "presetGroup:Big 09"), "every group header is listed");
        let started = std::time::Instant::now();
        h.step();
        eprintln!("presets panel frame with 5000 presets: {:?}", started.elapsed());
        // scroll far down over the list: later presets come into view
        let r = h.request("ui.hoverWidget", json!({"id": first[first.len() / 2]}), t);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..20 {
            h.request("ui.scroll", json!({"dy": -2000}), t);
            h.step();
        }
        h.step();
        let later = rows(&h);
        assert!((5..60).contains(&later.len()), "{} rows built", later.len());
        let deep = later.iter().filter_map(|w| w.strip_prefix("preset:user.big.")).filter_map(|n| n.parse::<usize>().ok()).max();
        assert!(deep.is_some_and(|n| n > 500), "scrolled rows: {later:?}");
    }

    /// The filter bar drives `library.filter` and saves the view as a smart album.
    #[test]
    fn filter_bar_filters_and_saves_a_smart_album() {
        let mut h = demo([1400.0, 800.0]);
        let t = Duration::from_secs(10);
        let all = h.app.session.visible_cloned().len();
        // shown from the start, as Lightroom Classic's Library Filter bar
        assert!(h.app.ui.filter_bar);
        h.settle(SETTLE);
        for w in ["filter:star3", "filter:pick", "filter:label-red", "filter:label-red"] {
            let r = h.request("ui.clickWidget", json!({"id": w}), t);
            assert_eq!(r["ok"], true, "{w}: {r}");
        }
        let f = h.app.session.filter.clone();
        assert_eq!((f.rating, f.flag, f.label), (3, Some(lcc_catalog::Flag::Pick), None), "a second click clears the label");
        let n = h.app.session.visible_cloned().len();
        assert!(n > 0 && n < all);
        h.request("ui.clickWidget", json!({"id": "button:filterSave"}), t);
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::NewSmartAlbum { .. })));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let smart = h.app.session.catalog.albums().find(|a| a.is_smart()).expect("smart album").id;
        assert_eq!(h.app.session.catalog.album_count(smart), n);
        let r = h.request("ui.clickWidget", json!({"id": "dropdown:filter-faces"}), t);
        assert_eq!(r["ok"], true, "dropdown:filter-faces: {r}");
        h.request("engine.execute", json!({"command": "library.filter", "params": {"faces": "1"}}), t);
        assert_eq!(h.app.session.filter.faces.as_deref(), Some("1"));
        let chips = lcc_engine::filter_chips(&h.app.session.filter, &h.app.session.catalog);
        assert!(chips.iter().any(|c| c.label == "Faces: 1"));
        h.request("ui.clickWidget", json!({"id": "button:filterClear"}), t);
        assert_eq!(h.app.session.filter, Default::default());
        assert_eq!(h.app.session.visible_cloned().len(), all);
        h.settle(SETTLE);
        // With `label` and `labels` both set (a saved single-label filter, or an agent), the bar lights only
        // the labels that pass both, and a click starts from those, not their union.
        use lcc_catalog::ColorLabel;
        h.app.session.execute("library.filter", &json!({"label": "red", "labels": ["red", "yellow"]})).unwrap();
        // a frame so the bar is laid out for the new filter before the click
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "filter:label-green"}), t);
        assert_eq!(r["ok"], true, "{r}");
        let f = h.app.session.filter.clone();
        assert_eq!((f.label, f.labels), (None, vec![ColorLabel::Red, ColorLabel::Green]));
        // nothing passes both: nothing is lit, so a click shows just that label
        h.app.session.execute("library.filter", &json!({"label": "red", "labels": ["yellow"]})).unwrap();
        h.step();
        h.request("ui.clickWidget", json!({"id": "filter:label-green"}), t);
        let f = h.app.session.filter.clone();
        assert_eq!((f.label, f.labels), (None, vec![ColorLabel::Green]));
        h.settle(SETTLE);
    }

    /// The photo grid shows date headers (registered as `group:<date>` widgets); clicking one
    /// selects that day's photos; month headers when zoomed out; none when grouping is off.
    #[test]
    fn grid_groups_by_capture_date() {
        let mut h = demo([1300.0, 800.0]);
        let t = Duration::from_secs(10);
        // (a new library starts ungrouped, as Lightroom Classic's Grid)
        h.app.session.execute("library.sort", &json!({"group": "auto"})).unwrap();
        h.settle(SETTLE);
        let groups = h.app.session.execute("library.groups", &json!({})).unwrap();
        let first = groups[0].clone();
        let key = first["key"].as_str().unwrap().to_string();
        let r = h.request("ui.clickWidget", json!({"id": format!("group:{key}")}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.selection.ids.len() as u64, first["count"].as_u64().unwrap());
        // zoomed out: month headers
        h.app.ui.thumb_size = 120.0;
        h.settle(SETTLE);
        let vis = h.app.session.visible_cloned();
        let month = h.app.session.catalog.date_runs(&vis, h.app.session.sort.key, lcc_catalog::GroupBy::Month);
        let r = h.request("ui.clickWidget", json!({"id": format!("group:{}", month[0].key)}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.request("engine.execute", json!({"command": "library.sort", "params": {"group": "none"}}), t);
        let r = h.request("ui.clickWidget", json!({"id": format!("group:{}", month[0].key)}), t);
        assert_eq!(r["ok"], false, "no headers: {r}");
        h.settle(SETTLE);
    }

    /// Keywords: the left-panel tree filters (children included), opens levels, the rename dialog
    /// renames library-wide, and the Keywords panel adds a suggestion.
    #[test]
    fn keyword_list_filters_renames_and_suggests() {
        // tall: the demo library's own keywords come first in the list
        let mut h = demo([1300.0, 1800.0]);
        // Host folders arrive asynchronously above Keywords. Keep this keyword fixture's
        // geometry independent of their existence and the background stat timing.
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            h.app.ui.hidden_locations.extend(
                ["Pictures", "Desktop", "Downloads", ""]
                    .map(|sub| if sub.is_empty() { home.clone() } else { std::path::Path::new(&home).join(sub).to_string_lossy().to_string() }),
            );
        }
        let t = Duration::from_secs(10);
        let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        let ex = |h: &mut Headless, c: &str, p: Value| h.request("engine.execute", json!({"command": c, "params": p}), Duration::from_secs(10));
        ex(&mut h, "photo.setMeta", json!({"ids": [vis[0], vis[1]], "addKeywords": ["travel|italy"]}));
        ex(&mut h, "photo.setMeta", json!({"ids": [vis[2]], "addKeywords": ["travel|france"]}));
        h.request("ui.set", json!({"leftPanel": true, "right": "none"}), t);
        // the Keyword List alone in Library's right column, at its top
        h.app.ui.hidden_panels = vec![crate::module::PanelId::QuickDevelop, crate::module::PanelId::Keywording];
        // clicks land on last frame's layout: let the keyword list settle first (on a loaded machine a
        // row could still move, and the click then hit its neighbour, e.g. "sunrise")
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "keywordShow:travel"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.filter.keyword.as_deref(), Some("travel"));
        assert_eq!(h.app.session.visible_cloned().len(), 3);
        // open the level, filter by the child
        h.request("ui.clickWidget", json!({"id": "keywordRowToggle:travel"}), t);
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "keywordShow:travel|italy"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.visible_cloned().len(), 2);
        h.app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: "travel".into(), to: "trips".into() });
        h.settle(SETTLE);
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.filter.keyword.as_deref(), Some("trips|italy"));
        assert_eq!(h.app.session.visible_cloned().len(), 2);
        // Keywords panel: suggestions co-occurring with the photo's keywords
        h.request("engine.execute", json!({"command": "library.clearFilter"}), t);
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[1]]}}), t);
        ex(&mut h, "photo.setMeta", json!({"ids": [vis[0]], "addKeywords": ["gelato"]}));
        h.request("ui.set", json!({"right": "keywords"}), t);
        assert!(h.settle(SETTLE), "keyword suggestions did not settle");
        let r = h.request("ui.clickWidget", json!({"id": "kwSuggest:gelato"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.session.catalog.photo(lcc_catalog::PhotoId(vis[1])).unwrap().meta.keywords.contains(&"gelato".to_string()));
        h.settle(SETTLE);
    }

    /// Rename Keyword's name field is a shared text field. It opens with the name selected, so
    /// typing replaces it, and Return renames; right-click ▸ Paste works too; Esc cancels the
    /// dialog and renames nothing.
    #[test]
    fn the_rename_keyword_field_selects_pastes_and_cancels() {
        let mut h = demo([1200.0, 800.0]);
        let t = Duration::from_secs(10);
        let first = h.app.session.visible_cloned()[0].0;
        h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [first], "addKeywords": ["travel"]}}), t);
        let keywords = |h: &Headless| h.app.session.catalog.photo(lcc_catalog::PhotoId(first)).unwrap().meta.keywords.clone();
        let open = |h: &mut Headless, name: &str| {
            h.app.ui.dialog = Some(crate::state::Dialog::RenameKeyword { from: name.into(), to: name.into() });
            h.settle(SETTLE);
        };
        let ask = |h: &mut Headless, method: &str, params: serde_json::Value| {
            let r = h.request(method, params.clone(), t);
            assert_eq!(r["ok"], true, "{method} {params}: {r}");
            h.settle(SETTLE);
        };
        // typing replaces the selected name
        open(&mut h, "travel");
        ask(&mut h, "ui.text", json!({"text": "trips"}));
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        assert!(keywords(&h).contains(&"trips".to_string()) && !keywords(&h).contains(&"travel".to_string()), "{:?}", keywords(&h));
        assert_eq!(h.app.ui.dialog, None);
        // the menu's Paste
        open(&mut h, "trips");
        h.view.clipboard = "weddings".into();
        ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordName", "button": "right"}));
        ask(&mut h, "ui.clickWidget", json!({"id": "field:keywordName:paste"}));
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        assert!(keywords(&h).contains(&"weddings".to_string()), "{:?}", keywords(&h));
        // Esc cancels
        open(&mut h, "weddings");
        ask(&mut h, "ui.text", json!({"text": "parties"}));
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
        assert_eq!(h.app.ui.dialog, None, "Esc closed the dialog");
        assert!(keywords(&h).contains(&"weddings".to_string()), "renamed nothing: {:?}", keywords(&h));
    }

    /// Return confirms the one-field dialogs: New Album, Rename Album, New Smart Album, Merge
    /// Keywords and a text prompt (here: renaming an album). Their name opens selected, so typing
    /// replaces it. Return did nothing: the field took the focus back on every frame, so egui never
    /// reported the focus Return gives up.
    #[test]
    fn return_confirms_the_one_field_dialogs() {
        use crate::state::Dialog;
        let mut h = demo([1200.0, 800.0]);
        let t = Duration::from_secs(10);
        let first = h.app.session.visible_cloned()[0].0;
        h.request("engine.execute", json!({"command": "photo.setMeta", "params": {"ids": [first], "addKeywords": ["holiday"]}}), t);
        let album = |h: &Headless, name: &str| h.app.session.catalog.albums().find(|a| a.name == name).map(|a| a.id.0);
        let garden = album(&h, "Garden").expect("the demo's Garden album");
        let confirm = |h: &mut Headless, dialog: Dialog, typed: &str| {
            h.app.ui.dialog = Some(dialog);
            h.settle(SETTLE);
            for (method, params) in [("ui.text", json!({"text": typed})), ("ui.key", json!({"key": "Enter"}))] {
                let r = h.request(method, params, t);
                assert_eq!(r["ok"], true, "{r}");
                h.settle(SETTLE);
            }
            assert_eq!(h.app.ui.dialog, None, "Return confirmed the dialog ({typed})");
        };
        confirm(&mut h, Dialog::NewAlbum { name: String::new(), folder: false, parent: None, include_selected: true }, "Weddings");
        assert!(album(&h, "Weddings").is_some(), "New Album");
        confirm(&mut h, Dialog::RenameAlbum { id: garden, name: "Garden".into() }, "Flowers");
        assert_eq!(album(&h, "Flowers"), Some(garden), "Rename Album");
        confirm(&mut h, Dialog::NewSmartAlbum { name: "Smart Album".into(), parent: None }, "Everything");
        assert!(album(&h, "Everything").is_some(), "New Smart Album");
        confirm(&mut h, Dialog::MergeKeywords { from: vec!["holiday".into()], into: String::new() }, "travel");
        let keywords = h.app.session.catalog.photo(lcc_catalog::PhotoId(first)).unwrap().meta.keywords.clone();
        assert!(keywords.contains(&"travel".to_string()) && !keywords.contains(&"holiday".to_string()), "Merge Keywords: {keywords:?}");
        let prompt = Dialog::TextPrompt {
            title: "Rename Album".into(),
            hint: "Name".into(),
            value: "Flowers".into(),
            command: "album.rename".into(),
            params: json!({"id": garden}),
            key: "name".into(),
        };
        confirm(&mut h, prompt, "Botanical");
        assert_eq!(album(&h, "Botanical"), Some(garden), "the text prompt");
    }

    /// The sidebar's file-system checks run on worker threads and add rows when they land, which
    /// moves every row below them: `busy()` counts them, so `settle` waits for them before a test
    /// reads widget positions (a click aimed at a stale rect hits the neighbouring row).
    #[test]
    fn settle_waits_for_file_system_checks() {
        use std::sync::atomic::{AtomicBool, Ordering};
        static RELEASE: AtomicBool = AtomicBool::new(false);
        fn slow_check(_: &str) -> bool {
            let t0 = Instant::now();
            while !RELEASE.load(Ordering::Acquire) && t0.elapsed() < Duration::from_secs(60) {
                std::thread::sleep(Duration::from_millis(1));
            }
            true
        }
        let mut h = demo([800.0, 600.0]);
        h.settle(SETTLE);
        let check = |h: &mut Headless| {
            let raw = HeadlessView::raw_input(h.size, 1.0, 0.0, vec![]);
            let mut got = None;
            h.view.run(raw, |ui| got = crate::panels::left::fs_cached(ui, "test-slow", "x", f64::INFINITY, slow_check));
            got
        };
        assert_eq!(check(&mut h), None, "the answer is worked out off the UI thread");
        assert!(h.busy(), "a pending file-system check is pending work");
        RELEASE.store(true, Ordering::Release);
        assert!(h.settle(SETTLE));
        assert!(!h.busy());
        assert_eq!(check(&mut h), Some(true));
    }

    /// A file-system check that panics is no pending work for ever: the count drops (so `busy()` and
    /// `settle` go on) and the check can be asked again.
    #[test]
    fn a_file_system_check_that_panics_is_not_pending_for_ever() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        fn panics(_: &str) -> bool {
            ASKED.fetch_add(1, Ordering::SeqCst);
            std::panic::resume_unwind(Box::new("a listing that dies"))
        }
        let mut h = demo([800.0, 600.0]);
        h.settle(SETTLE);
        let check = |h: &mut Headless| {
            let raw = HeadlessView::raw_input(h.size, 1.0, 0.0, vec![]);
            h.view.run(raw, |ui| {
                let _ = crate::panels::left::fs_cached(ui, "test-panics", "x", 0.0, panics);
            });
        };
        check(&mut h);
        let t0 = Instant::now();
        while crate::panels::left::fs_cached_running(&h.view.ctx) > 0 && t0.elapsed() < SETTLE {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(crate::panels::left::fs_cached_running(&h.view.ctx), 0, "the dead check no longer counts as running");
        check(&mut h);
        let t0 = Instant::now();
        while ASKED.load(Ordering::SeqCst) < 2 && t0.elapsed() < SETTLE {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(ASKED.load(Ordering::SeqCst), 2, "and it is asked again, not stuck as running");
    }

    /// Photo > Rename Photos…: the dialog previews and renames the selection.
    #[test]
    fn rename_dialog_renames_the_selection() {
        let mut h = demo([1200.0, 800.0]);
        let t = Duration::from_secs(10);
        let vis: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": vis}}), t);
        let tree = h.request("ui.menu.tree", json!({}), t);
        assert!(tree.to_string().contains("Rename 2 Photos…"), "live menu label");
        h.request("engine.execute", json!({"command": "dialog.rename", "params": {"template": "Trip-{seq:2}", "start": 5}}), t);
        h.settle(SETTLE);
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let name = |id: u64| h.app.session.catalog.photo(lcc_catalog::PhotoId(id)).unwrap().file_name.clone();
        assert!(name(vis[0]).starts_with("Trip-05."), "{}", name(vis[0]));
        assert!(name(vis[1]).starts_with("Trip-06."), "{}", name(vis[1]));
        h.settle(SETTLE);
    }

    /// The Export dialog hands its batch to a worker thread: the UI keeps drawing frames, shows
    /// progress, and reports the result when the files are written.
    /// Lightroom Classic's "Choose folder later": a first export picks no folder for the person;
    /// the folder dialog comes up when the export starts and the files go where it says.
    /// Cancelling it exports nothing; a host without a folder dialog asks for `dir`.
    #[test]
    fn export_choose_folder_later_asks_when_the_export_starts() {
        let out = lcc_testkit::TempDir::new("lc-choose-later");
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (a, folder) = (asked.clone(), out.to_string_lossy().into_owned());
        h.app.services.pick_folder = Some(Box::new(move || {
            a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(folder.clone())
        }));
        let written = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let w = written.clone();
        h.app.services.write_shared = Some(Arc::new(move |p: &str, _: &[u8]| {
            w.lock().unwrap().push(p.to_string());
            Ok(())
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), t);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        let Some(crate::state::Dialog::Export { opts, full_size, resize, .. }) = &mut h.app.ui.dialog else { panic!("no Export dialog") };
        assert_eq!(opts.destination_mode, lcc_engine::export::DestinationMode::ChooseLater, "no folder picked for a first export");
        *full_size = false;
        *resize = lcc_engine::export::Resize::long_edge(48);
        assert!(h.request("ui.widgets", json!({"filter": "exportChooseFolder"}), t)["result"].as_array().is_some_and(Vec::is_empty));
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 0, "not asked before the export starts");
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 1, "asked once the export starts");
        assert!(h.step_until(Duration::from_secs(60), |h| h.app.export.is_none()));
        let w = written.lock().unwrap().clone();
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().all(|p| std::path::Path::new(p).starts_with(&out)), "{w:?}");
        // the next export asks again (the setting is kept, not the folder)
        let last = h.app.session.last_export.clone().unwrap_or_default();
        assert_eq!(last["destinationMode"], "chooseLater", "{last}");

        // cancelled: nothing is exported, and that is not an error
        h.app.services.pick_folder = Some(Box::new(|| None));
        let r = h.request("engine.execute", json!({"command": "app.export", "params": {"destinationMode": "chooseLater", "longEdge": 48}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.export.is_none());
        assert_eq!(written.lock().unwrap().len(), 2);

        // no folder dialog here: an error that says what to pass
        h.app.services.pick_folder = None;
        let r = h.request("engine.execute", json!({"command": "app.export", "params": {"destinationMode": "chooseLater"}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["error"].as_str().is_some_and(|e| e.contains("`dir`")), "{r}");
    }

    /// The Export dialog's sections fold from their headers (Lightroom Classic's order), keep a
    /// one-line summary on the header, start open (where, name, format) or folded (size,
    /// sharpening, metadata, watermark), and keep the person's choice the next time.
    #[test]
    fn export_screen_shows_every_section_in_order_and_its_rail_jumps_to_one() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        let shown =
            |h: &mut Headless, id: &str| h.request("ui.widgets", json!({"filter": id}), t)["result"].as_array().is_some_and(|a| !a.is_empty());
        let top = |h: &mut Headless, id: &str| {
            let r = h.request("ui.widgets", json!({"filter": id}), t);
            r["result"][0]["rect"][1].as_f64().unwrap_or(f64::NAN)
        };
        // Export Location comes first, File Settings after File Naming
        assert!(top(&mut h, "section:exportLocation") < top(&mut h, "section:exportNaming"));
        assert!(top(&mut h, "section:exportNaming") < top(&mut h, "section:exportFileSettings"));
        assert!(shown(&mut h, "combo:exportDestinationMode"));
        // the Export screen shows every section open, one page (no folds to find a setting behind)
        assert!(shown(&mut h, "button:exportNamingTags") && shown(&mut h, "button:exportFormat-0"));
        assert!(shown(&mut h, "check:exportFullSize"), "Image Sizing is open");
        // the rail's Metadata scrolls the page to it
        let before = top(&mut h, "section:exportMetadata");
        h.request("ui.clickWidget", json!({"id": "export:group:Metadata"}), t);
        h.settle(SETTLE);
        let after = top(&mut h, "section:exportMetadata");
        assert!(after < before, "scrolled up to Metadata ({before} → {after})");
        assert!(shown(&mut h, "button:exportMetadata-0"));
        // Wide enough, the dialog lists its presets in a column (a click loads one, keeping where to export);
        // in a narrow window it falls back to the Preset menu, and stays on screen.
        let has = |h: &mut Headless, id: &str| h.request("ui.widgets", json!({"filter": id}), t)["result"].as_array().is_some_and(|a| !a.is_empty());
        assert!(has(&mut h, "button:exportPreset-builtin-0"), "the preset column");
        assert!(!has(&mut h, "combo:exportPreset"));
        // JPEG (Small): 2048 px long edge; where to export is left as it was
        h.request("ui.clickWidget", json!({"id": "button:exportPreset-builtin-0"}), t);
        h.settle(SETTLE);
        let Some(crate::state::Dialog::Export { opts, full_size, resize, .. }) = &h.app.ui.dialog else { panic!("no Export dialog") };
        assert!(!*full_size && resize.value == 2048.0, "{resize:?}");
        assert_eq!(opts.destination_mode, lcc_engine::export::DestinationMode::ChooseLater);
        h.request("ui.dialog.cancel", json!({}), t);

        let mut h = demo([560.0, 700.0]);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        assert!(has(&mut h, "combo:exportPreset"), "the Preset menu in a narrow window");
        assert!(!has(&mut h, "button:exportPreset-builtin-0"));
        let r = h.request("ui.widgets", json!({"filter": "dialog:window"}), t);
        let w = &r["result"][0]["rect"];
        assert!(w[0].as_f64().unwrap() >= 0.0 && w[0].as_f64().unwrap() + w[2].as_f64().unwrap() <= 560.0, "{w}");
    }

    #[test]
    fn export_dialog_keeps_its_width() {
        // A dialog row sized as "available width − a guessed button width" made the auto-sized
        // window grow a little every frame when the real button was wider (issue #8, Linux).
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.app.services.pick_folder = Some(Box::new(|| None));
        h.app.services.pick_files = Some(Box::new(Vec::new));
        // (every section open: Image Sizing and Watermarking start folded)
        for k in ["Sizing", "Sharpening", "Metadata", "Watermark"] {
            h.app.ui.group_open.insert(format!("export:{k}"), true);
        }
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        // the widest variant: watermark graphic and every optional row
        if let Some(crate::state::Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            let mut wm = lcc_engine::export::Watermark { image: "logo.png".into(), ..Default::default() };
            wm.text.clear();
            opts.watermark = Some(wm);
        }
        let width = |h: &mut Headless| {
            let r = h.request("ui.widgets", json!({}), t);
            r["result"]
                .as_array()
                .and_then(|a| a.iter().find(|w| w["id"] == "dialog:window"))
                .map(|w| w["rect"][2].as_f64().unwrap())
                .expect("dialog on screen")
        };
        for _ in 0..5 {
            h.step();
        }
        let w0 = width(&mut h);
        for _ in 0..60 {
            h.step();
        }
        let w1 = width(&mut h);
        assert!((w1 - w0).abs() < 0.5, "the export dialog grew from {w0} to {w1}");
        // (a screen that fills the window, never past it)
        assert!(w1 <= 1400.0, "{w1}");
    }

    /// With a watermark on, the Export dialog shows the first photo with the watermark on it.
    #[test]
    fn export_dialog_previews_the_watermark() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        // (Watermarking starts folded)
        h.app.ui.group_open.insert("export:Watermark".into(), true);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        let preview = |h: &mut Headless| {
            let r = h.request("ui.widgets", json!({}), t);
            r["result"].as_array().and_then(|a| a.iter().find(|w| w["id"] == "image:exportWatermarkPreview")).map(|w| w["rect"].clone())
        };
        h.settle(SETTLE);
        assert!(preview(&mut h).is_none(), "no preview without a watermark");
        if let Some(crate::state::Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            opts.watermark = Some(lcc_engine::export::Watermark { text: "© Example".into(), ..Default::default() });
        }
        let t0 = Instant::now();
        // (the preview is rendered off the UI thread: generous on a loaded machine)
        while preview(&mut h).is_none() && t0.elapsed() < Duration::from_secs(120) {
            h.step();
            std::thread::sleep(Duration::from_millis(2));
        }
        let rect = preview(&mut h).expect("the watermark preview is shown");
        assert!(rect[2].as_f64().unwrap() > 50.0 && rect[3].as_f64().unwrap() > 50.0, "{rect}");
    }

    /// Issue #1952: a previous export saved with a depth its format doesn't have (an API call made
    /// before `app.export` refused it) opens at a depth the format offers, so the dialog's Export,
    /// which has no depth to pick for JPEG, doesn't fail; a depth the format has is kept.
    #[test]
    fn export_dialog_drops_a_depth_the_format_lacks() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let opened = |h: &mut Headless, last: Value| {
            h.app.ui.dialog = None;
            h.app.session.last_export = Some(last);
            h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
            match &h.app.ui.dialog {
                Some(crate::state::Dialog::Export { opts, .. }) => opts.clone(),
                _ => panic!("the export dialog is open"),
            }
        };
        let o = opened(&mut h, json!({"format": "jpeg", "bitDepth": 16}));
        assert_eq!(o.bit_depth, None);
        lcc_engine::export::ExportOptions::from_params(&o.to_json()).expect("the dialog's settings export");
        assert_eq!(opened(&mut h, json!({"format": "tiff", "bitDepth": 32})).bit_depth, Some(32));
    }

    /// Review of #1966: a `path` whose extension names no format exports JPEG here, so its
    /// `bitDepth` is checked against JPEG's, not let through to an 8-bit file.
    #[test]
    fn export_checks_the_depth_of_the_format_a_bare_path_gets() {
        let out = lcc_testkit::TempDir::new("lc-bare-path-depth");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        h.app.services.write = Some(Box::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        for name in ["output", "output.xyz"] {
            let p = json!({"path": out.join(name).to_string_lossy(), "longEdge": 32, "bitDepth": 10});
            let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
            assert_eq!(r["ok"], false, "{name}: {r}");
            assert!(r["error"].as_str().unwrap().contains("`bitDepth` must be 8 for jpeg"), "{name}: {r}");
        }
        assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "nothing written");
        // the format such a path gets is still JPEG
        let p = json!({"path": out.join("output").to_string_lossy(), "longEdge": 32, "bitDepth": 8});
        let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
        assert_eq!(r["ok"], true, "{r}");
        let written = std::fs::read_dir(&out).unwrap().next().unwrap().unwrap().path();
        assert!(std::fs::read(&written).unwrap().starts_with(&[0xFF, 0xD8]), "{written:?} is a JPEG");
        let _ = std::fs::remove_dir_all(&out);
    }

    #[test]
    fn export_dialog_runs_in_the_background() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let w = written.clone();
        h.app.services.write_shared = Some(std::sync::Arc::new(move |p: &str, _: &[u8]| {
            std::thread::sleep(Duration::from_millis(150)); // a slow disk: the UI must not wait for it
            w.lock().unwrap().push(p.to_string());
            Ok(())
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), t);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        if let Some(crate::state::Dialog::Export { opts, full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
            opts.destination_mode = lcc_engine::export::DestinationMode::SpecificFolder;
            *full_size = false;
            *resize = lcc_engine::export::Resize::long_edge(64);
            *dir = "/lc-test-out".into();
        }
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.export.is_some(), "running in the background");
        let running = h.request("ui.inspect", json!({}), t);
        assert_eq!(running["result"]["export"]["running"]["total"], 3, "{}", running["result"]["export"]);
        // …and as a row in the activity stack (issue #345)
        assert_eq!(running["result"]["activity"][0]["kind"], "export", "{}", running["result"]["activity"]);
        let t0 = Instant::now();
        while h.app.export.is_some() && t0.elapsed() < Duration::from_secs(60) {
            h.step();
        }
        assert!(h.app.export.is_none(), "finished");
        let w = written.lock().unwrap().clone();
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(w.iter().all(|p| p.starts_with("/lc-test-out/")));
        let after = h.request("ui.inspect", json!({}), t)["result"].clone();
        assert_eq!(after["activity"], json!([]), "the row is gone");
        let last = after["export"]["last"].clone();
        assert_eq!(last["files"].as_array().map(Vec::len), Some(3), "{last}");
        assert!(last["files"][0]["width"].as_u64().is_some_and(|w| w <= 64));
    }

    /// Issue #2965: an export asked for while another runs waits its turn instead of being refused; it shows as a
    /// waiting row in the activity stack and runs when the first ends, and ✕ on a waiting export drops it.
    #[test]
    fn a_second_export_waits_for_the_first() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let w = written.clone();
        h.app.services.write_shared = Some(std::sync::Arc::new(move |p: &str, _: &[u8]| {
            std::thread::sleep(Duration::from_millis(150)); // slow enough that the first is still running
            w.lock().unwrap().push(p.to_string());
            Ok(())
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
        let export =
            |h: &mut Headless, ids: &[u64], dir: &str| h.app.run("app.export", json!({"ids": ids, "dir": dir, "background": true, "longEdge": 64}));
        export(&mut h, &ids[..2], "/lc-queue-a").unwrap();
        assert!(h.app.export.is_some(), "the first runs in the background");
        let second = export(&mut h, &ids[2..], "/lc-queue-b").unwrap();
        assert_eq!((second["queued"].clone(), second["total"].clone()), (json!(1), json!(1)), "{second}");
        let third = export(&mut h, &ids[..1], "/lc-queue-c").unwrap();
        assert_eq!(third["queued"], 2, "{third}");
        let seen = h.request("ui.inspect", json!({}), t)["result"].clone();
        assert_eq!(seen["export"]["queued"], 2, "{}", seen["export"]);
        let rows = seen["activity"].as_array().cloned().unwrap_or_default();
        assert_eq!(rows.len(), 3, "the running export and the two waiting: {rows:?}");
        // ✕ on the last waiting one drops it
        let last = rows.iter().rev().find(|r| r["label"].as_str().is_some_and(|l| l.starts_with("Waiting to export"))).expect("a waiting row");
        h.app.run("activity.cancel", json!({"id": last["id"]})).unwrap();
        let t0 = Instant::now();
        while (h.app.export.is_some() || !h.app.export_queue.is_empty()) && t0.elapsed() < Duration::from_secs(90) {
            h.step();
        }
        assert!(h.app.export.is_none() && h.app.export_queue.is_empty(), "all done");
        let w = written.lock().unwrap().clone();
        let count = |d: &str| w.iter().filter(|p| p.starts_with(d)).count();
        assert_eq!((count("/lc-queue-a/"), count("/lc-queue-b/"), count("/lc-queue-c/")), (2, 1, 0), "{w:?}");
        let first_b = w.iter().position(|p| p.starts_with("/lc-queue-b/")).unwrap();
        assert_eq!(w[..first_b].iter().filter(|p| p.starts_with("/lc-queue-a/")).count(), 2, "in turn: {w:?}");
        assert_eq!(h.request("ui.inspect", json!({}), t)["result"]["activity"], json!([]), "no rows left");
    }

    /// Issue #236: the Export dialog's Add to Apple Photos is there on macOS only; there the
    /// background export hands the files it wrote to Photos (a fake runner: tests never start
    /// osascript or Photos) and the toast says how it went.
    #[test]
    fn export_dialog_adds_to_apple_photos_on_macos() {
        let out = lcc_testkit::TempDir::new("lc-photos-dialog");
        let mut h = demo([1200.0, 1100.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&out);
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        let calls = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let c = calls.clone();
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "A/L0/001\nB/L0/001\n".into(), stderr: String::new() })
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": ids}}), t);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        let shown = h.request("ui.widgets", json!({"filter": "exportAddToPhotos"}), t)["result"].as_array().map_or(0, Vec::len);
        assert_eq!(shown > 0, cfg!(target_os = "macos"), "the option is offered where Photos is");
        if !cfg!(target_os = "macos") {
            return;
        }
        h.request("ui.clickWidget", json!({"id": "exportAddToPhotos"}), t);
        h.settle(SETTLE);
        if let Some(crate::state::Dialog::Export { opts, full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
            assert!(opts.add_to_photos, "the checkbox turns it on");
            opts.destination_mode = lcc_engine::export::DestinationMode::SpecificFolder;
            opts.photos_album = "Trip “2026”".into();
            *full_size = false;
            *resize = lcc_engine::export::Resize::long_edge(48);
            *dir = out.to_string_lossy().into_owned();
        }
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let t0 = Instant::now();
        while h.app.export.is_some() && t0.elapsed() < Duration::from_secs(60) {
            h.step();
        }
        let last = h.request("ui.inspect", json!({}), t)["result"]["export"]["last"].clone();
        let written: Vec<String> = last["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap().to_string()).collect();
        assert_eq!(written.len(), 2, "{last}");
        assert_eq!(last["applePhotos"]["imported"], 2, "{last}");
        assert_eq!(last["applePhotos"]["album"], "Trip “2026”");
        let args = calls.lock().unwrap().clone();
        assert_eq!(args.len(), 1);
        assert_eq!(args[0][args[0].len() - 2..], written[..], "the files written go to Photos");
        let toast = h.app.ui.toast().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains("2 added to Apple Photos album “Trip “2026””"), "{toast}");
        // Export with Previous repeats it
        assert_eq!(h.app.session.last_export.as_ref().unwrap()["addToPhotos"], true);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Codex review of #643: the progress line while Photos imports was translated on the export's
    /// worker thread, whose language is always English.
    #[test]
    fn the_photos_progress_line_is_in_the_users_language() {
        let out = lcc_testkit::TempDir::new("lc-photos-language");
        if !cfg!(target_os = "macos") {
            return;
        }
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&out);
        h.app.ui.language = crate::i18n::Locale::De;
        crate::i18n::set_language(crate::i18n::Locale::De);
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let (entered, photos_asked) = std::sync::mpsc::channel::<()>();
        let entered = std::sync::Mutex::new(entered);
        h.app.session.apple_photos = Some(Arc::new(move |_: &[String], _| {
            let _ = entered.lock().unwrap().send(());
            let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(30));
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
        let params = json!({"ids": ids, "dir": out.path(), "longEdge": 48, "addToPhotos": true, "background": true});
        let r = h.request("engine.execute", json!({"command": "app.export", "params": params}), t);
        assert_eq!(r["ok"], true, "{r}");
        // Photos has been asked, and the window hasn't drawn a frame since (Codex review of #643):
        // the export's row already has no ✕, and a cancel is refused rather than half-honoured
        photos_asked.recv_timeout(Duration::from_secs(60)).expect("the export asks Photos");
        let row = h.app.session.activity.list().into_iter().find(|r| r.kind == "export").expect("the export's row");
        assert!(!row.cancellable, "{row:?}");
        assert!(h.app.session.activity.cancel(row.id).is_err(), "Photos can't be stopped once asked");
        let current = h.app.export.as_ref().map(|e| e.progress.lock().unwrap().1.clone()).unwrap_or_default();
        assert_eq!(current, crate::i18n::Locale::De.tr("Adding to Apple Photos…"));
        // the row says so on the next frame (issue #345)
        h.step();
        let row = h.app.session.activity.list().into_iter().find(|r| r.kind == "export").expect("the export's row");
        assert_eq!((row.detail.as_str(), row.cancellable), ("Wird zu Apple Fotos hinzugefügt…", false), "{row:?}");
        release.send(()).unwrap();
        let t0 = Instant::now();
        while h.app.export.is_some() && t0.elapsed() < Duration::from_secs(90) {
            h.step();
        }
        let last = h.app.last_export_result.clone().expect("the export reports");
        assert_eq!((last["cancelled"].as_bool(), last["applePhotos"]["imported"].as_u64()), (Some(false), Some(1)), "{last}");
        crate::i18n::set_language(crate::i18n::Locale::En);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Photo ▸ Render to DNG renders on a worker: the request answers at once, the window keeps
    /// drawing frames with the activity row up, and the DNGs arrive stacked on their originals.
    #[test]
    fn render_to_dng_keeps_the_window_drawing() {
        let out = lcc_testkit::TempDir::new("lc-render-dng-ui");
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(30);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(4).map(|p| p.0).collect();
        let started = Instant::now();
        let r = h.request("engine.execute", json!({"command": "photo.renderToDng", "params": {"ids": ids, "dir": out.to_string_lossy()}}), t);
        assert_eq!((r["ok"].as_bool(), r["result"]["background"].as_bool()), (Some(true), Some(true)), "{r}");
        assert!(h.app.render_dng.is_some(), "the renders run on a worker");
        let row = h.app.session.activity.list().into_iter().find(|r| r.kind == "renderDng").expect("its activity row");
        assert!(row.cancellable);
        let mut frames = 0;
        while h.app.render_dng.is_some() && started.elapsed() < Duration::from_secs(120) {
            h.step();
            frames += 1;
        }
        assert!(h.app.render_dng.is_none(), "the batch finished");
        assert!(frames > 1, "frames were drawn while the DNGs rendered ({frames})");
        let last = h.app.last_render_dng.clone().expect("the batch reports");
        let copies = last["copies"].as_array().unwrap();
        assert_eq!(copies.len(), 4, "{last}");
        for c in copies {
            let new = lcc_catalog::PhotoId(c["id"].as_u64().unwrap());
            let orig = lcc_catalog::PhotoId(c["original"].as_u64().unwrap());
            let st = h.app.session.catalog.stack_of(new).expect("stacked");
            assert!(st.photos.first() == Some(&new) && st.photos.contains(&orig));
        }
        assert!(h.app.session.activity.list().iter().all(|r| r.kind != "renderDng"));
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Issue #236 review: control requests run on the UI thread, which must not wait minutes for
    /// Photos. `export.addToPhotos` and `app.export {addToPhotos}` without `background` answer at
    /// once with the running job; the app announces the outcome when Photos is done.
    #[test]
    fn a_slow_photos_import_never_blocks_the_window() {
        let out = lcc_testkit::TempDir::new("lc-photos-slow-ui");
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(30);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let file = out.join("a.jpg");
        std::fs::write(&file, b"jpeg").unwrap();
        h.app.services.write = Some(Box::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        h.app.session.apple_photos_background = true; // as the desktop app sets it
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            // a regression (Photos on the UI thread) fails the timing below instead of hanging; Photos
            // is held far longer than the answer may take, so a slow runner can't pass for a block
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(120));
            let n = args.len() - args.iter().position(|a| a == lcc_engine::apple_photos::MARKER).unwrap() - 2;
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".repeat(n), stderr: String::new() })
        }));
        let t0 = Instant::now();
        let r =
            h.request("engine.execute", json!({"command": "export.addToPhotos", "params": {"paths": [file.to_string_lossy()], "album": "Slow"}}), t);
        // (the first frames of a fresh window take seconds on a slow CI runner, e.g. Windows')
        assert!(t0.elapsed() < Duration::from_secs(25), "answered at once: {r}");
        assert_eq!(r["result"]["running"], true, "{r}");
        go.send(()).unwrap();
        while !h.app.ui.toast().is_some_and(|t| t.0.contains("Apple Photos")) {
            assert!(t0.elapsed() < Duration::from_secs(90), "the outcome is announced");
            h.step();
        }
        assert_eq!(h.app.ui.toast().unwrap().0, "1 added to Apple Photos album “Slow”");
        if cfg!(target_os = "macos") {
            let t1 = Instant::now();
            let p = json!({"dir": out.to_string_lossy(), "longEdge": 32, "addToPhotos": true});
            let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
            assert!(t1.elapsed() < Duration::from_secs(10), "the export answers without waiting for Photos: {r}");
            assert_eq!(r["result"]["applePhotos"]["running"], true, "{r}");
            assert_eq!(r["result"]["applePhotos"]["job"], 2, "{r}");
            go.send(()).unwrap();
            while !h.app.ui.toast().is_some_and(|t| t.0.starts_with("1 added to Apple Photos") && !t.0.contains("Slow")) {
                assert!(t1.elapsed() < Duration::from_secs(20), "the outcome is announced");
                h.step();
            }
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Review of #236: a headless run (snapshot scripts) ending while an import it started with
    /// `wait: false` is still running waits for it, on every exit path (dropping the `Headless`).
    #[test]
    fn a_headless_run_waits_for_its_photos_import_before_ending() {
        let dir = lcc_testkit::TempDir::new("lc-photos-headless-drop");
        let mut h = demo([800.0, 600.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.jpg");
        std::fs::write(&file, b"jpeg").unwrap();
        let marker = dir.join("imported");
        let m = marker.clone();
        // Photos "imports" when `go` arrives (at most 20 s): released 0.8 s into the drop below
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        h.app.session.apple_photos = Some(Arc::new(move |_: &[String], _| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
            std::fs::write(&m, b"done").unwrap();
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
        }));
        let r =
            h.request("engine.execute", json!({"command": "export.addToPhotos", "params": {"paths": [file.to_string_lossy()], "wait": false}}), t);
        assert_eq!(r["result"]["running"], true, "{r}");
        assert!(!marker.exists(), "still importing");
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(800));
            let _ = go.send(());
        });
        drop(h);
        assert!(marker.exists(), "the import finished before the run ended");
        release.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review of #236: a snapshot that ends (an error in its script, say) while its background
    /// export is still writing waits through the writing and then the import the export starts.
    #[test]
    fn a_headless_run_waits_for_its_exports_photos_import() {
        let dir = lcc_testkit::TempDir::new("lc-photos-headless-export-drop");
        if !cfg!(target_os = "macos") {
            return; // addToPhotos is refused before exporting elsewhere
        }
        let mut h = demo([800.0, 600.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        let marker = dir.join("imported");
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| {
            std::thread::sleep(Duration::from_millis(300)); // a slow disk: still writing at the drop
            lcc_engine::export::write_file(p, bytes)
        }));
        let m = marker.clone();
        h.app.session.apple_photos = Some(Arc::new(move |_: &[String], _| {
            std::thread::sleep(Duration::from_millis(500));
            std::fs::write(&m, b"done").unwrap();
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
        }));
        let p = json!({"dir": dir.join("out").to_string_lossy(), "longEdge": 32, "addToPhotos": true, "background": true});
        let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.session.apple_photos_imports.running().is_some(), "reserved while writing");
        drop(h);
        assert!(marker.exists(), "the export's import finished before the run ended");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review of #236: a background export adding to Photos holds the import from before it
    /// writes. While its files are being written, `export.addToPhotos` is refused with the
    /// export's job number; the export then adds its files under that job.
    #[test]
    fn a_background_export_holds_photos_while_it_writes() {
        let out = lcc_testkit::TempDir::new("lc-photos-hold-ui");
        if !cfg!(target_os = "macos") {
            return; // addToPhotos is refused before exporting elsewhere
        }
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let other = out.join("other.jpg");
        std::fs::write(&other, b"jpeg").unwrap();
        // the writer waits for `go`: the export is still writing while we try another import
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        h.app.services.write_shared = Some(Arc::new(move |p: &str, bytes: &[u8]| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
            lcc_engine::export::write_file(p, bytes)
        }));
        let calls = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let c = calls.clone();
        h.app.session.apple_photos_background = true;
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            Ok(lcc_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
        }));
        let p = json!({"dir": out.join("export").to_string_lossy(), "longEdge": 32, "addToPhotos": true, "photosAlbum": "Held", "background": true});
        let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.export.is_some(), "writing in the background");
        let r = h.request("engine.execute", json!({"command": "export.addToPhotos", "params": {"paths": [other.to_string_lossy()]}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert!(r["error"].as_str().unwrap().contains("An export is about to add its files to Apple Photos (import 1)"), "{r}");
        go.send(()).unwrap();
        let t0 = Instant::now();
        while h.app.export.is_some() {
            assert!(t0.elapsed() < Duration::from_secs(20), "the export finishes");
            h.step();
        }
        let last = h.app.last_export_result.clone().unwrap();
        assert_eq!((last["applePhotos"]["job"].as_u64(), last["applePhotos"]["imported"].as_u64()), (Some(1), Some(1)), "{last}");
        let args = calls.lock().unwrap().clone();
        assert_eq!(args.len(), 1, "only the export's files went to Photos");
        assert!(args[0].last().unwrap().contains("export"), "{:?}", args[0].last());
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Custom Order in the filmstrip: dragging a photo along the strip of a regular album shown
    /// in Loupe moves it in Custom Order.
    #[test]
    fn drag_photos_along_the_filmstrip_to_reorder() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(4).map(|p| p.0).collect();
        let r = h.request("engine.execute", json!({"command": "album.create", "params": {"name": "Strip"}}), t);
        let album = r["result"]["id"].as_u64().unwrap_or_else(|| panic!("{r}"));
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]]}}), t);
        h.request("engine.execute", json!({"command": "album.addPhotos", "params": {"id": album, "ids": ids}}), t);
        h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "album", "id": album}}), t);
        h.request("engine.execute", json!({"command": "library.sort", "params": {"key": "custom"}}), t);
        let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [shown[0]]}}), t);
        h.request("engine.execute", json!({"command": "view.detail"}), t);
        h.settle(SETTLE);
        let rect = |h: &mut Headless, id: &str| {
            let w = h.request("ui.widgets", json!({"filter": id}), t);
            let r = w["result"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["id"] == id))
                .map(|x| x["rect"].clone())
                .unwrap_or_else(|| panic!("{id}: {w}"));
            let f = |i: usize| r[i].as_f64().unwrap();
            (f(0), f(1), f(2), f(3))
        };
        let (x, y, w, hh) = rect(&mut h, &format!("film:{}", shown[3]));
        let (tx, ty, _, th) = rect(&mut h, &format!("film:{}", shown[1]));
        let r = h.request("ui.drag", json!({"x": x + w / 2.0, "y": y + hh / 2.0, "toX": tx + 6.0, "toY": ty + th / 2.0, "steps": 12}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let now: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        assert_eq!(now, vec![shown[0], shown[3], shown[1], shown[2]]);
        assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
    }

    /// Custom Order: dragging a thumbnail onto the left half of another one in a regular album
    /// moves it there, and the sort becomes Custom Order (Lightroom Classic's drag to reorder).
    #[test]
    fn drag_photos_to_reorder_an_album() {
        let out = lcc_testkit::TempDir::new("lc-ui-custom-order");
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(4).map(|p| p.0).collect();
        let r = h.request("engine.execute", json!({"command": "album.create", "params": {"name": "Story"}}), t);
        let album = r["result"]["id"].as_u64().unwrap_or_else(|| panic!("{r}"));
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [ids[0]]}}), t);
        h.request("engine.execute", json!({"command": "album.addPhotos", "params": {"id": album, "ids": ids}}), t);
        h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "album", "id": album}}), t);
        h.request("engine.execute", json!({"command": "library.sort", "params": {"key": "fileName", "ascending": true}}), t);
        h.settle(SETTLE);
        let shown: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        let rect = |h: &mut Headless, id: &str| {
            let w = h.request("ui.widgets", json!({"filter": id}), t);
            let r = w["result"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["id"] == id))
                .map(|x| x["rect"].clone())
                .unwrap_or_else(|| panic!("{id}: {w}"));
            let f = |i: usize| r[i].as_f64().unwrap();
            (f(0), f(1), f(2), f(3))
        };
        let (x, y, w, hh) = rect(&mut h, &format!("thumb:{}", shown[3]));
        let (tx, ty, _, th) = rect(&mut h, &format!("thumb:{}", shown[1]));
        let r = h.request("ui.drag", json!({"x": x + w / 2.0, "y": y + hh / 2.0, "toX": tx + 6.0, "toY": ty + th / 2.0, "steps": 12}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.session.sort.key, lcc_catalog::SortKey::Custom);
        let now: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        assert_eq!(now, vec![shown[0], shown[3], shown[1], shown[2]]);
        assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
        // exporting the selection numbers it in Custom Order, however it was selected
        let _ = std::fs::remove_dir_all(&out);
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lcc_engine::export::write_file(p, bytes)));
        let picked: Vec<u64> = now.iter().rev().copied().collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": picked}}), t);
        let p = json!({"dir": out.to_string_lossy(), "longEdge": 32, "naming": "{seq}_{name}", "background": true});
        let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
        assert_eq!(r["ok"], true, "{r}");
        let t0 = Instant::now();
        while h.app.export.is_some() {
            assert!(t0.elapsed() < Duration::from_secs(30), "the export finishes");
            h.step();
        }
        for (i, id) in now.iter().enumerate() {
            let name = h.app.session.catalog.photo(lcc_catalog::PhotoId(*id)).unwrap().file_name.clone();
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s).to_string();
            let want = format!("{:03}_{stem}", i + 1);
            let found = std::fs::read_dir(&out).unwrap().filter_map(|e| e.ok()).any(|e| e.file_name().to_string_lossy().starts_with(&want));
            assert!(found, "{want} in {:?}", std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect::<Vec<_>>());
        }
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Issue #10: a raw shown from its embedded JPEG (an undecodable raw variant) says so — a badge
    /// on its grid thumbnail, a pill on the loupe, a notice in Edit and Info — and other photos
    /// show none of it.
    #[test]
    fn preview_only_raw_shows_badge_and_notices() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let vis = h.app.session.visible_cloned();
        let (po, other) = (vis[0], vis[1]);
        let ph = h.app.session.catalog.photo(po).unwrap().clone();
        h.app
            .session
            .catalog
            .apply(lcc_catalog::Op::SetContent {
                id: po,
                width: ph.width,
                height: ph.height,
                file_size: ph.file_size,
                content_hash: ph.content_hash.clone(),
                preview_only: Some("Nikon Huffman-compressed NEF (no clean-room description available)".into()),
                preview_checked: lcc_catalog::APP_VERSION.into(),
            })
            .unwrap();
        h.settle(SETTLE);
        let ids = |h: &mut Headless, filter: &str| -> Vec<String> {
            let w = h.request("ui.widgets", json!({"filter": filter}), t);
            w["result"].as_array().unwrap_or_else(|| panic!("{w}")).iter().filter_map(|x| x["id"].as_str().map(str::to_string)).collect()
        };
        let badges = ids(&mut h, "badge:previewOnly:");
        assert_eq!(badges, vec![format!("badge:previewOnly:{}", po.0)], "only the preview-only photo has the badge");
        // the loupe and the Edit panel
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [po.0]}}), t);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.app.ui.right = crate::state::RightPanel::Edit;
        h.settle(SETTLE);
        let n = ids(&mut h, "notice:previewOnly:");
        assert!(n.contains(&"notice:previewOnly:loupe".to_string()) && n.contains(&"notice:previewOnly:edit".to_string()), "{n:?}");
        // with a Reload from Disk button (issue #662)
        assert_eq!(ids(&mut h, "button:reloadPreviewOnly:"), vec!["button:reloadPreviewOnly:edit".to_string()]);
        // Info
        h.app.ui.right = crate::state::RightPanel::Info;
        h.settle(SETTLE);
        assert!(ids(&mut h, "notice:previewOnly:").contains(&"notice:previewOnly:info".to_string()));
        assert_eq!(ids(&mut h, "button:reloadPreviewOnly:"), vec!["button:reloadPreviewOnly:info".to_string()]);
        // …which re-reads the file and says what came of it (a demo photo has none to re-read)
        let r = h.request("ui.clickWidget", json!({"id": "button:reloadPreviewOnly:info"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.ui.toast().cloned().map(|x| x.0).as_deref(), Some("This raw variant still can't be decoded"));
        // a regular photo: no notice
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [other.0]}}), t);
        h.app.ui.right = crate::state::RightPanel::Edit;
        h.settle(SETTLE);
        assert!(ids(&mut h, "notice:previewOnly:").is_empty());
    }

    #[test]
    fn crop_default_browsing_is_read_only_and_drag_is_one_edit() {
        let mut h = demo([1200.0, 800.0]);
        let photos: Vec<_> = h.app.session.visible_cloned().into_iter().filter(|id| !h.app.session.catalog.photo(*id).unwrap().is_edited()).collect();
        h.app.run("library.select", json!({"ids": [photos[0].0]})).unwrap();
        h.app.ui.right = crate::state::RightPanel::Crop;
        h.app.run("crop.aspect", json!({"aspect": "1x1"})).unwrap();
        let saved = serde_json::to_value(&h.app.ui).unwrap();
        for &photo in &photos[1..3] {
            h.app.run("library.select", json!({"ids": [photo.0]})).unwrap();
            let before = h.app.session.catalog.photo(photo).unwrap().clone();
            let undo = h.app.session.undo.len();
            h.settle(SETTLE);
            let after = h.app.session.catalog.photo(photo).unwrap();
            assert_eq!(after.develop, before.develop);
            assert_eq!(after.history.len(), before.history.len());
            assert!(!after.is_edited());
            assert_eq!(h.app.session.undo.len(), undo);
            assert_eq!(h.app.crop_tool_aspect(), Some((100, 100)));
        }
        let photo = photos[2];
        let before = h.app.session.develop_of(photo).unwrap();
        let undo = h.app.session.undo.len();
        h.app.run("develop.beginInteraction", json!({"label": "Crop"})).unwrap();
        h.app.begin_crop().unwrap();
        h.app.run("crop.drag", json!({"handle": "bottomRight", "from": [0.8, 0.8], "to": [0.7, 0.7]})).unwrap();
        h.app.run("develop.endInteraction", json!({})).unwrap();
        assert_eq!(h.app.session.develop_of(photo).unwrap().crop.aspect, Some((100, 100)));
        assert_eq!(h.app.session.undo.len(), undo + 1);
        h.app.run("edit.undo", json!({})).unwrap();
        assert_eq!(h.app.session.develop_of(photo).unwrap(), before);
        h.app.ui = serde_json::from_value(saved).unwrap();
        assert_eq!(h.app.crop_tool_aspect(), Some((100, 100)));
        h.app.run("crop.aspect", json!({"aspect": "toggle"})).unwrap();
        assert_eq!(h.app.ui.crop_default_aspect, Some(json!("free")));
        assert_eq!(h.app.session.develop_of(photo).unwrap(), before);
        h.app.ui.crop_default_aspect = Some(json!("original"));
        let p = h.app.session.catalog.photo(photo).unwrap();
        assert_eq!(h.app.crop_tool_aspect(), Some((p.width * 100, p.height * 100)));
        h.settle(SETTLE);
        assert_eq!(h.app.session.develop_of(photo).unwrap(), before);
    }

    /// While cropping: O cycles the guides, ⇧O mirrors them, A locks / unlocks the aspect.
    #[test]
    fn crop_keys() {
        let mut h = demo([1200.0, 800.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.crop"}), t);
        assert_eq!(h.app.ui.crop_overlay, crate::state::CropOverlay::Thirds);
        let mask_overlay = h.app.ui.mask_overlay;
        h.request("ui.key", json!({"key": "O"}), t);
        assert_eq!(h.app.ui.crop_overlay, crate::state::CropOverlay::Grid);
        h.request("ui.key", json!({"key": "O", "shift": true}), t);
        assert_eq!(h.app.ui.crop_overlay_orient, 1);
        assert_eq!(h.app.ui.mask_overlay, mask_overlay, "O doesn't toggle the mask overlay while cropping");
        // an untouched photo starts locked to its own shape (#991): A unlocks, A again locks
        let locked = |h: &Headless| h.app.crop_tool_aspect().is_some();
        assert!(locked(&h));
        h.request("ui.key", json!({"key": "A"}), t);
        assert!(!locked(&h));
        assert_eq!(h.app.ui.crop_default_aspect, Some(json!("free")));
        h.request("ui.key", json!({"key": "A"}), t);
        assert!(locked(&h));
        assert!(!h.app.ui.visualize_spots);
        // the crop mask strength is a setting that rejects unknown levels
        assert_eq!(h.app.ui.crop_mask_opacity, 60);
        h.request("engine.execute", json!({"command": "view.cropMaskOpacity", "params": {"opacity": 20}}), t);
        assert_eq!(h.app.ui.crop_mask_opacity, 20);
        h.request("engine.execute", json!({"command": "view.cropMaskOpacity", "params": {"opacity": 33}}), t);
        assert_eq!(h.app.ui.crop_mask_opacity, 20, "an unknown level is refused");
        // Choose Overlays to Cycle…: O visits only the ticked guides, and one must stay ticked
        h.app.ui.crop_overlay = crate::state::CropOverlay::Thirds;
        use crate::state::CropOverlay as O;
        let set = |h: &mut Headless, names: serde_json::Value| {
            h.request("engine.execute", json!({"command": "view.cropOverlays", "params": {"overlays": names}}), t)
        };
        let r = set(&mut h, json!(["thirds", "diagonal", "spiral"]));
        assert_eq!(r["ok"], true, "{r}");
        let mut seen = Vec::new();
        for _ in 0..4 {
            h.request("engine.execute", json!({"command": "view.cropOverlay"}), t);
            seen.push(h.app.ui.crop_overlay);
        }
        assert_eq!(seen, [O::Diagonal, O::Spiral, O::Thirds, O::Diagonal]);
        let r = set(&mut h, json!([]));
        assert_eq!(r["ok"], false, "{r}");
        let r = set(&mut h, json!(["bogus"]));
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(h.app.ui.crop_overlay_cycle, [O::Thirds, O::Diagonal, O::Spiral], "a refused list changes nothing");
        // Return commits a tool panel back to Edit; elsewhere it does nothing
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "no-op outside tools");
    }

    /// Info panel: typing into a field survives frames and is saved when it loses focus; the new
    /// accessibility and place fields reach the photo.
    #[test]
    fn info_fields_keep_typing_and_save() {
        let mut h = demo([1300.0, 1000.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "info", "moduleBar": false}), t);
        h.settle(SETTLE);
        for (key, text) in [("altText", "A lake at dawn"), ("usageTerms", "Editorial use only"), ("city", "Zermatt")] {
            let r = h.request("ui.clickWidget", json!({"id": format!("field:{key}")}), t);
            assert_eq!(r["ok"], true, "{r}");
            h.request("ui.key", json!({"key": "A", "cmd": true}), t);
            h.request("ui.text", json!({"text": text}), t);
            h.step();
            h.step();
            // still being edited: not saved yet, not lost either
            h.request("ui.key", json!({"key": "Tab"}), t);
            h.settle(Duration::from_secs(5));
        }
        let m = &h.app.session.catalog.photo(h.app.session.active().unwrap()).unwrap().meta;
        assert_eq!((m.alt_text.as_str(), m.city.as_str()), ("A lake at dawn", "Zermatt"));
        assert_eq!(m.usage_terms, "Editorial use only");
        // the copyright status picker is on the panel too
        let w = h.request("ui.widgets", json!({"filter": "copyrightStatus"}), t);
        assert!(w["result"].to_string().contains("field:copyrightStatus"), "{w}");
        // Info panel fields are shared text fields. The caption takes several lines: Return starts a new one and
        // only leaving the field saves it. Esc gives up an edit, and right-click ▸ Paste works there too.
        h.settle(SETTLE);
        let meta = |h: &Headless| h.app.session.catalog.photo(h.app.session.active().unwrap()).unwrap().meta.clone();
        let ask = |h: &mut Headless, method: &str, params: serde_json::Value| {
            let r = h.request(method, params.clone(), t);
            assert_eq!(r["ok"], true, "{method} {params}: {r}");
            h.step();
            h.step();
        };
        let (title, undo) = (meta(&h).title, h.app.session.undo.len());
        // the caption: two lines, saved on leaving
        ask(&mut h, "ui.clickWidget", json!({"id": "field:caption"}));
        ask(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
        ask(&mut h, "ui.text", json!({"text": "Harbour at dusk"}));
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        ask(&mut h, "ui.text", json!({"text": "Lisbon, 2026"}));
        assert_eq!(h.app.session.undo.len(), undo, "Return in the caption saves nothing yet");
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        assert_eq!(meta(&h).caption, "Harbour at dusk\nLisbon, 2026");
        // the title: Esc gives the edit up
        let undo = h.app.session.undo.len();
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title"}));
        ask(&mut h, "ui.text", json!({"text": " (draft)"}));
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
        h.settle(SETTLE);
        assert_eq!((meta(&h).title, h.app.session.undo.len()), (title, undo), "Esc saved nothing");
        // the menu
        h.view.clipboard = "Wedding in Sintra".into();
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title"}));
        ask(&mut h, "ui.key", json!({"key": "A", "cmd": true}));
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title", "button": "right"}));
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title:paste"}));
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        assert_eq!(meta(&h).title, "Wedding in Sintra");
    }

    /// Local: a folder's photos show without joining the library; the breadcrumb, Include
    /// subfolders and Add to My Photos work from the grid header.
    #[test]
    fn browse_a_local_folder() {
        let dir = lcc_testkit::TempDir::new("lc-ui-browse");
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        for (i, p) in [dir.join("a.png"), dir.join("inner/b.png")].iter().enumerate() {
            let mut img = img.clone();
            img.data[0][0] = i as u8; // different bytes per file
            std::fs::write(p, lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        }
        let library_before = h.app.session.catalog.photos().filter(|p| !p.local).count();
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        // the folder is read in the background: the view switches at once, the photos follow
        assert_eq!(r["result"]["scanning"], true, "{r}");
        assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder);
        h.settle(SETTLE);
        assert!(h.app.scan.is_none() && h.app.import.is_none());
        // subfolders come with a first browse of a folder: both photos
        assert_eq!(h.app.session.visible_cloned().len(), 2);
        assert!(h.settle(SETTLE), "a thumbnail that can't load must not keep the renderer busy");
        // this test session has no file hooks: the thumbnail fails once, is remembered, and isn't retried
        let id = h.app.session.visible_cloned()[0];
        assert!(h.app.renderer.failure(crate::render::Slot::Thumb(id)).is_some());
        let done = h.app.renderer.completed;
        for _ in 0..30 {
            h.step();
        }
        assert_eq!(h.app.renderer.completed, done, "no retry loop");
        let w = h.request("ui.widgets", json!({"filter": "crumb:"}), t);
        assert!(w["result"].as_array().is_some_and(|a| !a.is_empty()), "breadcrumb: {w}");
        // the toggle browses flat, and back
        assert_eq!(h.request("ui.clickWidget", json!({"id": "check:includeSubfolders"}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_cloned().len(), 1, "off: only the folder's own file");
        assert_eq!(h.request("ui.clickWidget", json!({"id": "check:includeSubfolders"}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_cloned().len(), 2);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:addToLibrary"}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.app.session.catalog.photos().filter(|p| !p.local).count(), library_before + 2);
        // Truncated ancestors remain clickable and still open the original, unabridged path.
        let inner = dir.join("inner");
        h.request("engine.execute", json!({"command": "library.browse", "params": {"path": inner.to_string_lossy()}}), t);
        h.settle(SETTLE);
        let parent_index = dir.to_string_lossy().split(['/', '\\']).filter(|p| !p.is_empty()).count() - 1;
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("crumb:{parent_index}")}), t)["ok"], true);
        h.settle(SETTLE);
        // the same folder, whatever the platform's separators (issue #538)
        assert_eq!(std::path::Path::new(&h.app.session.browse.as_ref().unwrap().path), dir.path());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Local: Remove from Local hides a sidebar location (nothing on disk changes) and the
    /// "Show hidden locations" row puts it back. The folder is browsed and kept the way Browse
    /// Folder… does it, in the temporary folder — on Windows beneath Home, whose tree must not
    /// open down to the hidden folder and push the restore row out of view.
    #[test]
    fn local_location_can_be_hidden_and_restored() {
        let dir2 = lcc_testkit::TempDir::new("lc-ui-hide-b");
        let dir = lcc_testkit::TempDir::new("lc-ui-hide");
        let size = [1300.0, 900.0];
        let mut h = demo(size);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let rects = |h: &mut Headless| -> Vec<(String, Vec<f64>)> {
            let w = h.request("ui.widgets", json!({"filter": "source:local:"}), t);
            w["result"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| (x["id"].as_str().unwrap().to_string(), x["rect"].as_array().unwrap().iter().filter_map(|v| v.as_f64()).collect()))
                .collect()
        };
        let ids = |h: &mut Headless| -> Vec<String> { rects(h).into_iter().map(|(id, _)| id).collect() };
        h.request("ui.set", json!({"leftPanel": true}), t);
        h.app.ui.toggle_sidebar_section("panel:navigator");
        // the Local Folders panel at the top of the column (it comes last by default)
        h.app.ui.panel_order = vec![crate::module::PanelId::Local];
        h.hide_home_above(&dir);
        // Browse Folder… browses the picked folder and keeps it in Local within one frame; a
        // request runs frames, so keep it first (no frame sees it browsed but not yet kept)
        let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": path}}), t);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": path}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let row = format!("source:local:{path}");
        assert!(h.step_until(t, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "the browsed folder is listed: {:?}", ids(&mut h));
        // the "Show hidden locations" row appears only while something is hidden
        let hidden_before = !h.app.ui.hidden_locations.is_empty();
        assert_eq!(ids(&mut h).iter().any(|i| i == "source:local:restoreHidden"), hidden_before);
        // hide it while it is still being browsed
        let r = h.request("engine.execute", json!({"command": "local.hide", "params": {"path": path}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        // give any listing a reveal would wait for time to arrive
        for _ in 0..20 {
            h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        h.settle(SETTLE);
        let after = rects(&mut h);
        let after_ids: Vec<&str> = after.iter().map(|(id, _)| id.as_str()).collect();
        assert!(!after_ids.contains(&format!("source:local:{path}").as_str()), "{after_ids:?}");
        let restore = after.iter().find(|(id, _)| id == "source:local:restoreHidden").map(|(_, r)| r.clone());
        let restore = restore.unwrap_or_else(|| panic!("no restore row: {after_ids:?}"));
        assert!(restore[1] >= 0.0 && restore[1] + restore[3] <= f64::from(size[1]), "the restore row is in view: {restore:?} {after_ids:?}");
        assert!(h.app.session.browse.is_some(), "hiding does not stop browsing");
        assert!(dir.is_dir(), "the folder itself is untouched");
        // a second hidden place: the row lists them, and each comes back on its own (issue #2114:
        // the row used to bring every hidden place back at once)
        let _ = std::fs::remove_dir_all(&dir2);
        std::fs::create_dir_all(&dir2).unwrap();
        let path2 = dir2.to_string_lossy().to_string();
        for (command, params) in [("local.addRoot", json!({"path": path2})), ("local.hide", json!({"path": path2}))] {
            let r = h.request("engine.execute", json!({"command": command, "params": params}), t);
            assert_eq!(r["ok"], true, "{r}");
        }
        h.settle(SETTLE);
        let menu = |h: &mut Headless| -> Vec<String> {
            let w = h.request("ui.widgets", json!({"filter": "local:restoreHidden:"}), t);
            w["result"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap().to_string()).collect()
        };
        assert!(menu(&mut h).is_empty(), "the list opens from the row");
        assert_eq!(h.request("ui.clickWidget", json!({"id": "source:local:restoreHidden"}), t)["ok"], true);
        h.settle(SETTLE);
        let items = menu(&mut h);
        assert!(items.contains(&format!("local:restoreHidden:{path}")) && items.contains(&"local:restoreHidden:all".to_string()), "{items:?}");
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("local:restoreHidden:{path}")}), t)["ok"], true);
        h.settle(SETTLE);
        assert!(!h.app.ui.hidden_locations.iter().any(|x| x == &path), "{:?}", h.app.ui.hidden_locations);
        assert!(h.app.ui.hidden_locations.iter().any(|x| x == &path2), "the other place stays hidden");
        assert!(ids(&mut h).iter().any(|i| i == "source:local:restoreHidden"), "still something to restore");
        // two hidden again: Restore All brings every one back
        let r = h.request("engine.execute", json!({"command": "local.hide", "params": {"path": path}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.hidden_locations.len() >= 2, "{:?}", h.app.ui.hidden_locations);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "source:local:restoreHidden"}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "local:restoreHidden:all"}), t)["ok"], true);
        h.settle(SETTLE);
        assert!(h.app.ui.hidden_locations.is_empty(), "{:?}", h.app.ui.hidden_locations);
        assert!(!ids(&mut h).iter().any(|i| i == "source:local:restoreHidden"), "nothing left to restore");
        // one hidden: the row brings it back itself, no list
        let r = h.request("engine.execute", json!({"command": "local.hide", "params": {"path": path2}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "source:local:restoreHidden"}), t)["ok"], true);
        h.settle(SETTLE);
        assert!(menu(&mut h).is_empty(), "no list for a single hidden place");
        assert!(h.app.ui.hidden_locations.is_empty(), "{:?}", h.app.ui.hidden_locations);
        if !hidden_before {
            // (with Home hidden for this test, the folder is back inside Home's tree instead)
            assert!(ids(&mut h).contains(&format!("source:local:{path}")));
        }
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// Issue #966: clicking a folder kept in Favorites (Browse Folder…) reads its photos into
    /// the view, the way it did before; it used to leave the view empty ("No photos in this folder").
    #[test]
    fn favorite_folder_click_shows_its_photos() {
        let dir = lcc_testkit::TempDir::new("lc-ui-fav");
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        for i in 0..2u8 {
            let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80 + i, 255]);
            std::fs::write(dir.join(format!("p{i}.png")), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        }
        let path = dir.to_string_lossy().to_string();
        h.request("ui.set", json!({"leftPanel": true}), t);
        h.app.ui.toggle_sidebar_section("panel:navigator");
        h.app.ui.panel_order = vec![crate::module::PanelId::Local];
        h.hide_home_above(&dir);
        let r = h.request("engine.execute", json!({"command": "local.addRoot", "params": {"path": path}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let row = format!("source:local:{path}");
        assert!(h.step_until(t, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "the favourite is listed");
        assert_eq!(h.request("ui.clickWidget", json!({"id": row}), t)["ok"], true);
        assert_eq!(h.app.session.source, lcc_engine::LibrarySource::Folder);
        assert!(h.step_until(t, |h| h.app.scan.is_none() && h.app.import.is_none()));
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_cloned().len(), 2, "the favourite's photos show");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Issue #966: a folder that can't be listed (no permission) says so instead of showing as
    /// an empty folder, and keeps the photos the library already knows from it.
    #[cfg(unix)]
    #[test]
    fn unreadable_folder_browse_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let dir = lcc_testkit::TempDir::new("lc-ui-unreadable");
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_dir(&dir).is_ok() {
            // running as root: every folder can be read
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.step_until(t, |h| h.app.scan.is_none() && h.app.import.is_none()));
        h.settle(SETTLE);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(h.app.ui.toasts.iter().any(|t| t.0.contains(&*dir.to_string_lossy())), "the unreadable folder is reported: {:?}", h.app.ui.toasts);
    }

    /// Local: a kept folder stays a root while its subfolders (and other locations) are
    /// browsed — the subfolder is highlighted inside its tree, its siblings stay listed — and
    /// the kept folders survive a save/load of the UI state (a restart).
    #[test]
    fn kept_local_root_stays_while_browsing_below_and_elsewhere() {
        let base = lcc_testkit::TempDir::new("lc-ui-roots");
        let mut h = demo([1300.0, 1400.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&base);
        for d in ["Photos/2026/20260101", "Photos/2026/20260114", "Other"] {
            std::fs::create_dir_all(base.join(d)).unwrap();
        }
        let s = |p: std::path::PathBuf| p.to_string_lossy().to_string();
        // joined by component: the sidebar's ids spell paths with the platform's separator
        let year = base.join("Photos").join("2026");
        let (photos, day1, day2, other) = (s(base.join("Photos")), s(year.join("20260101")), s(year.join("20260114")), s(base.join("Other")));
        h.hide_home_above(&base);
        let exec = |h: &mut Headless, c: &str, p: Value| h.request("engine.execute", json!({"command": c, "params": p}), t);
        let rects = |h: &mut Headless| -> std::collections::HashMap<String, f64> {
            let w = h.request("ui.widgets", json!({"filter": "lc-ui-roots-"}), t);
            w["result"].as_array().unwrap().iter().map(|x| (x["id"].as_str().unwrap().to_string(), x["rect"][0].as_f64().unwrap_or(0.0))).collect()
        };
        h.request("ui.set", json!({"leftPanel": true}), t);
        // Home may also own this temporary directory. Keep the fixture tree
        // independent of the user's real folders and their async listings.
        if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            assert_eq!(exec(&mut h, "local.hide", json!({"path": home}))["ok"], true);
        }
        assert_eq!(exec(&mut h, "local.addRoot", json!({"path": photos}))["ok"], true);
        assert_eq!(exec(&mut h, "local.addRoot", json!({"path": format!("{photos}/")}))["result"]["roots"].as_array().map(Vec::len), Some(1));
        exec(&mut h, "library.browse", json!({"path": day1}));
        h.settle(SETTLE);
        h.step();
        let row = format!("source:local:{day2}");
        assert!(h.step_until(t, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "the sibling listing did not arrive");
        let r = rects(&mut h);
        assert!(r.contains_key(&format!("source:local:{photos}")), "the kept root stays: {r:?}");
        assert!(r.contains_key(&format!("source:local:{day2}")), "the sibling stays reachable: {r:?}");
        let (root_x, child_x) = (r[&format!("folderToggle:{photos}")], r[&format!("folderToggle:{day1}")]);
        assert!(child_x > root_x, "the browsed folder is inside the root's tree, not a root of its own ({child_x} vs {root_x})");
        assert_eq!(h.request("ui.clickWidget", json!({"id": format!("source:local:{day2}")}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.app.session.browse.as_ref().map(|b| b.path.clone()), Some(day2.clone()), "the sibling is browsed");
        // another location: the kept root stays listed
        assert_eq!(exec(&mut h, "library.browse", json!({"path": other}))["ok"], true);
        h.settle(SETTLE);
        h.step();
        let row = format!("source:local:{other}");
        assert!(h.step_until(t, |h| h.app.widgets.iter().any(|(w, _)| *w == row)), "the other location did not arrive");
        let r = rects(&mut h);
        assert!(r.contains_key(&format!("source:local:{photos}")) && r.contains_key(&format!("source:local:{other}")), "{r:?}");
        // restart: the kept roots come back with the saved UI state
        let saved = serde_json::to_string(&h.app.ui).unwrap();
        let back: crate::state::UiState = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.local_roots, vec![photos.clone()]);
        let _ = std::fs::remove_dir_all(&base);
        h.settle(SETTLE);
    }

    /// Versions panel: resting on a version previews it in the loupe without changing the photo;
    /// a click restores it.
    #[test]
    fn versions_preview_on_hover_and_restore_on_click() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let exec = |h: &mut Headless, c: &str, p: Value| h.request("engine.execute", json!({"command": c, "params": p}), t);
        h.request("ui.set", json!({"view": "detail", "right": "versions"}), t);
        exec(&mut h, "develop.set", json!({"values": {"light.exposure": 1.0}}));
        exec(&mut h, "version.create", json!({"name": "Bright"}));
        exec(&mut h, "develop.set", json!({"values": {"light.exposure": -1.0}}));
        h.settle(SETTLE);
        let exposure = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure;
        assert_eq!(h.request("ui.hoverWidget", json!({"id": "version:0"}), t)["ok"], true);
        h.step();
        assert_eq!(h.app.hover_preview.as_ref().map(|p| p.settings.light.exposure), Some(1.0));
        assert_eq!(exposure(&h), -1.0, "hovering changes nothing");
        assert_eq!(h.request("ui.clickWidget", json!({"id": "version:0"}), t)["ok"], true);
        h.settle(Duration::from_secs(5));
        assert_eq!(exposure(&h), 1.0);
    }

    /// A slider drag that ends just past the window's edge (the integration drops that release)
    /// doesn't keep the slider captured: back over the photo the slider stays put, and a click
    /// on the photo zooms as usual (the Grain sliders at the bottom of the panel made it easy).
    #[test]
    fn a_slider_released_outside_the_window_lets_go() {
        let mut h = demo([1300.0, 1600.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        let r = h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"grain.amount": 40.0}}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.app.ui.open_sections = vec!["effects".to_string()];
        h.settle(SETTLE);
        let amount = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().grain.amount;
        let slider = h.app.widgets.iter().find(|(id, _)| id == "slider:grain.amount").map(|(_, r)| *r).expect("the Grain slider");
        let canvas = h.app.canvas_rect.expect("the photo").center();
        assert!(matches!(h.app.ui.zoom, crate::state::Zoom::Fit));
        // drag the slider, then off the bottom of the window, where the button goes up unseen
        let (a, b) = (slider.center(), slider.center() + egui::vec2(40.0, 0.0));
        let edge = egui::pos2(b.x, 1599.0);
        let m = egui::Modifiers::NONE;
        h.app.synthetic.extend([
            egui::Event::PointerMoved(a),
            egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m },
            egui::Event::PointerMoved(b),
            egui::Event::PointerMoved(edge),
            egui::Event::PointerGone,
        ]);
        h.settle(SETTLE);
        let dragged = amount(&h);
        assert!(dragged > 40.0, "the drag moved the slider ({dragged})");
        // back over the photo, and click it
        h.app.synthetic.push(egui::Event::PointerMoved(canvas + egui::vec2(5.0, 0.0)));
        h.settle(SETTLE);
        // (without the repair egui still drags the slider here: the ↔ cursor sticks and the
        // drag's undo step stays open)
        assert_eq!(h.view.ctx.dragged_id(), None, "nothing is dragged once the pointer is back");
        assert!(h.app.session.interaction.is_none(), "the drag's undo step is closed");
        assert_ne!(h.last_cursor, egui::CursorIcon::ResizeHorizontal, "the slider's cursor is gone");
        h.app.synthetic.extend([
            egui::Event::PointerMoved(canvas),
            egui::Event::PointerButton { pos: canvas, button: egui::PointerButton::Primary, pressed: true, modifiers: m },
            egui::Event::PointerButton { pos: canvas, button: egui::PointerButton::Primary, pressed: false, modifiers: m },
        ]);
        h.settle(SETTLE);
        assert_eq!(amount(&h), dragged, "the slider let go");
        assert!(!matches!(h.app.ui.zoom, crate::state::Zoom::Fit), "the click on the photo zoomed in");
    }

    /// Click a slider's value and type one (issue #322): Return sets it as one undo step, Esc
    /// keeps the old value, and the keys typed don't reach the shortcuts (1 = one star).
    #[test]
    fn slider_values_can_be_typed() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        h.settle(SETTLE);
        let active = h.app.session.active().unwrap();
        let exposure = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure;
        let rating = |h: &Headless| h.app.session.catalog.photo(active).unwrap().rating;
        let (undo0, rating0) = (h.app.session.undo.len(), rating(&h));
        assert_eq!(h.request("ui.clickWidget", json!({"id": "sliderValue:light.exposure"}), t)["ok"], true);
        h.request("ui.text", json!({"text": "1,5"}), t);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert!((exposure(&h) - 1.5).abs() < 1e-9, "{}", exposure(&h));
        assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
        assert_eq!(rating(&h), rating0, "typing 1 set no rating");
        // Esc keeps the value
        assert_eq!(h.request("ui.clickWidget", json!({"id": "sliderValue:light.exposure"}), t)["ok"], true);
        h.request("ui.text", json!({"text": "-2"}), t);
        h.request("ui.key", json!({"key": "Escape"}), t);
        assert!((exposure(&h) - 1.5).abs() < 1e-9, "{}", exposure(&h));
        // something that isn't a number changes nothing
        assert_eq!(h.request("ui.clickWidget", json!({"id": "sliderValue:light.exposure"}), t)["ok"], true);
        h.request("ui.text", json!({"text": "bright"}), t);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert!((exposure(&h) - 1.5).abs() < 1e-9, "{}", exposure(&h));
        assert_eq!(h.app.session.undo.len(), undo0 + 1);
        // the typed value is a shared text field: right-click ▸ Paste replaces the value (selected when the
        // field opens) and Return applies it; the menu doesn't close the field
        h.view.clipboard = "0.75".into();
        for (id, button) in
            [("sliderValue:light.exposure", "left"), ("sliderField:light.exposure", "right"), ("sliderField:light.exposure:paste", "left")]
        {
            let r = h.request("ui.clickWidget", json!({"id": id, "button": button}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
            h.settle(SETTLE);
        }
        h.request("ui.key", json!({"key": "Enter"}), t);
        h.settle(SETTLE);
        assert!((exposure(&h) - 0.75).abs() < 1e-9, "pasted: {}", exposure(&h));
        // ↑ / ↓ over a slider nudge it; ⇧Z picks and moves to the next photo
        assert_eq!(h.request("ui.hoverWidget", json!({"id": "slider:light.exposure"}), t)["ok"], true);
        h.request("ui.key", json!({"key": "Up"}), t);
        h.request("ui.key", json!({"key": "Up", "shift": true}), t);
        assert!((exposure(&h) - 1.05).abs() < 1e-9, "nudged up: {}", exposure(&h));
        h.request("ui.key", json!({"key": "Down"}), t);
        assert!((exposure(&h) - 1.00).abs() < 1e-9, "nudged down: {}", exposure(&h));
        h.request("ui.key", json!({"key": "Z", "shift": true}), t);
        assert_eq!(h.app.session.catalog.photo(active).unwrap().flag, lcc_catalog::Flag::Pick);
        assert_ne!(h.app.session.active(), Some(active), "advanced");
    }

    /// A real click on a slider's value spans frames (press, then release a few frames later, as a
    /// hand does): the field it opens stays open for typing (issues #59, upstream #1435 / #1669).
    #[test]
    fn a_slider_value_opens_for_typing_on_a_real_click() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        h.settle(SETTLE);
        let exposure = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure;
        let r = h.app.widgets.iter().find(|(id, _)| id == "sliderValue:light.exposure").map(|(_, r)| *r).unwrap();
        let at = r.center();
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        // a quick click, a usual one, one held for a second, and one whose hand shook a little
        for (text, frames, jitter) in [("0.5", 1, 0.0), ("1,25", 4, 0.0), ("-0.75", 70, 0.0), ("0.25", 6, 2.0)] {
            h.app.synthetic.push(egui::Event::PointerMoved(at));
            h.app.synthetic.push(button(true));
            for i in 0..frames {
                if jitter > 0.0 {
                    h.app.synthetic.push(egui::Event::PointerMoved(at + egui::vec2(if i % 2 == 0 { jitter } else { -jitter }, 0.0)));
                }
                h.step();
            }
            h.app.synthetic.push(egui::Event::PointerMoved(at));
            h.app.synthetic.push(button(false));
            for _ in 0..5 {
                h.step();
            }
            assert_eq!(
                h.view.ctx.memory(|m| m.focused()),
                Some(crate::text_field::id("sliderField:light.exposure")),
                "the field stays open after a {frames}-frame press"
            );
            h.request("ui.text", json!({"text": text}), t);
            h.request("ui.key", json!({"key": "Enter"}), t);
            let want: f64 = text.replace(',', ".").parse().unwrap();
            assert!((exposure(&h) - want).abs() < 1e-9, "{text}: {}", exposure(&h));
        }
    }

    /// Reset takes precedence over the value being typed when its button is clicked.
    #[test]
    fn reset_button_wins_over_an_active_numeric_draft() {
        let mut h = demo([960.0, 600.0]);
        let t = Duration::from_secs(10);
        let ask = |h: &mut Headless, method: &str, params: serde_json::Value| {
            let r = h.request(method, params, t);
            assert_eq!(r["ok"], true, "{method}: {r}");
            r["result"].clone()
        };
        ask(&mut h, "engine.execute", json!({"command": "app.gpu", "params": {"enabled": false}}));
        ask(&mut h, "ui.set", json!({"settings": {"gpu": false, "previewEdge": 1280}}));
        ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": [1], "active": 1}}));
        ask(
            &mut h,
            "ui.set",
            json!({"view": "detail", "right": "edit", "leftPanel": false, "rightWidth": 330, "openSections": ["light"], "histogram": false}),
        );
        let active = h.app.session.active().unwrap();
        for (text, terminal) in [("0.75", "Enter"), ("-1,5", "Escape")] {
            ask(&mut h, "ui.clickWidget", json!({"id": "sliderValue:light.exposure"}));
            ask(&mut h, "ui.text", json!({"text": text}));
            ask(&mut h, "ui.key", json!({"key": terminal}));
            let value = ask(&mut h, "engine.execute", json!({"command": "develop.get", "params": {}}));
            assert_eq!(value["light"]["exposure"].as_f64(), Some(0.75), "{terminal}: {value}");
        }
        ask(&mut h, "ui.clickWidget", json!({"id": "sliderValue:light.exposure"}));
        ask(&mut h, "ui.text", json!({"text": "2.0"}));
        assert_eq!(h.view.ctx.memory(|m| m.focused()), Some(crate::text_field::id("sliderField:light.exposure")));
        assert!(h.app.widgets.iter().any(|(id, _)| id == "sliderField:light.exposure"));
        assert!(h.painted_text().iter().any(|text| text == "2.0"), "the replacement draft must be visible before Reset");
        assert_eq!(h.app.session.develop_of(active).unwrap().light.exposure, 0.75, "the draft is not committed yet");
        ask(&mut h, "ui.clickWidget", json!({"id": "button:reset"}));
        let reset = ask(&mut h, "engine.execute", json!({"command": "develop.get", "params": {}}));
        assert_eq!(
            reset["light"]["exposure"].as_f64(),
            Some(0.0),
            "Reset must win over the active numeric draft: {reset}; history={:?}; last_inverse={:?}; focus={:?}",
            h.app.session.undo.iter().map(|entry| entry.label.as_str()).collect::<Vec<_>>(),
            h.app.session.undo.last().map(|entry| &entry.op),
            h.view.ctx.memory(|m| m.focused()),
        );
    }

    /// The eye on a section header switches the section off and on again, one undo step each
    /// (issue #316).
    #[test]
    fn section_eye_switches_a_section_off() {
        // (tall enough for Tone Curve's header under the taller histogram)
        let mut h = demo([1300.0, 1200.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        // Lightroom's own panel heights (the Witchcraft sections push Tone Curve below this window)
        h.request("engine.execute", json!({"command": "view.gradingMode", "params": {"mode": "eboda"}}), t);
        h.settle(SETTLE);
        // (Tone Curve: Basic has no switch, as in Lightroom Classic)
        let light_on = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().section_enabled("curve");
        for expected in [false, true] {
            // the eye shows while the pointer is on the header
            assert_eq!(h.request("ui.hoverWidget", json!({"id": "section:curve"}), t)["ok"], true);
            assert_eq!(h.request("ui.clickWidget", json!({"id": "sectionEye:curve"}), t)["ok"], true);
            assert_eq!(light_on(&h), expected);
        }
        assert!(!h.app.ui.section_open("curve"), "the click didn't open the section");
    }

    /// ⌘Q (File → Quit) closes the window.
    #[test]
    fn cmd_q_quits() {
        let mut h = demo([900.0, 600.0]);
        assert!(!h.quit_requested());
        h.request("ui.key", json!({"key": "Q", "cmd": true}), Duration::from_secs(10));
        h.step();
        h.step();
        assert!(h.quit_requested());
    }

    /// File → Import from Folder… on a folder that was browsed in Local offers its photos, adding
    /// them in place (they aren't "In catalog" yet), and confirming brings them into the library.
    #[test]
    fn import_from_folder_offers_the_photos_browsed_in_local() {
        let dir = lcc_testkit::TempDir::new("lc-ui-browsed-import");
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        for (i, name) in ["a.png", "b.png"].iter().enumerate() {
            let mut img = img.clone();
            img.data[0][0] = i as u8; // different bytes per file
            std::fs::write(dir.join(name), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        }
        let library_before = h.app.session.catalog.photos().filter(|p| !p.local).count();
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.session.catalog.photos().filter(|p| p.local).count(), 2, "both browsed");
        let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review: {:?}", h.app.ui.dialog) };
        assert_eq!(opts.candidates.len(), 2);
        assert!(opts.candidates.iter().all(|c| c.duplicate.is_none()), "browsed is not \"In library\": {:?}", opts.candidates);
        assert_eq!(opts.selected_paths().len(), 2, "both offered, checked");
        assert_eq!(h.request("ui.dialog.confirm", json!({}), t)["ok"], true);
        h.settle(SETTLE);
        assert_eq!(h.app.session.catalog.photos().filter(|p| !p.local).count(), library_before + 2, "both joined the library");
        assert_eq!(h.app.session.catalog.photos().filter(|p| p.local).count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// File → Import from Folder… opens the import review for a folder (searched recursively).
    #[test]
    fn add_folder_opens_the_import_review() {
        let dir = lcc_testkit::TempDir::new("lc-addfolder");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let sub = dir.join("day 2");
        std::fs::create_dir_all(&sub).unwrap();
        let img = lcc_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lcc_engine::export::encode_image(
            &img,
            &lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() },
        )
        .unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        std::fs::write(sub.join("b.png"), &png).unwrap();
        let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review: {:?}", h.app.ui.dialog) };
        assert_eq!(opts.candidates.len(), 2, "both files, the subfolder's too");
        assert!(!opts.copy, "a folder is added in place by default");
        // the review names its source; scanning doesn't save a Local location
        assert_eq!(opts.sources, vec![dir.to_string_lossy().to_string()]);
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        assert_eq!(crate::import::source_summary(&opts.sources), format!("Folder “{name}” (and its subfolders)"));
        let r = h.request("ui.widgets", json!({}), t);
        assert!(r.to_string().contains("label:importSource"), "source shown");
        assert!(h.app.ui.local_roots.is_empty(), "no Local shortcut saved");
        // The source facts were cached by a worker with the source-language default. They must
        // still be rendered in German after switching, with the folder name kept verbatim.
        assert_eq!(h.request("engine.execute", json!({"command": "app.language.german"}), t)["ok"], true);
        h.settle(SETTLE);
        fn texts(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(shape) => out.push(shape.galley.job.text.clone()),
                egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| texts(shape, out)),
                _ => {}
            }
        }
        let mut painted = Vec::new();
        for shape in &h.view.shapes {
            texts(&shape.shape, &mut painted);
        }
        assert!(painted.iter().any(|text| text == &format!("Ordner „{name}“ (und seine Unterordner)")), "{painted:?}");
        for label in ["Quelle", "Übertragen", "Stichwörter", "Vorgaben"] {
            assert!(painted.iter().any(|text| text == label), "{label}: {painted:?}");
        }
        assert_eq!(h.request("engine.execute", json!({"command": "app.language.english"}), t)["ok"], true);
        // a camera / card folder: copied into the library by default
        h.app.ui.dialog = None;
        let r = h.request("engine.execute", json!({"command": "file.addFromDevice", "params": {"path": sub.to_string_lossy()}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert!(opts.copy);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Switching Settings tabs keeps the window still: the tab row and the page stay where they
    /// are, though the tabs are of different heights (LR-PREF-WINDOW).
    #[test]
    fn settings_tabs_keep_the_window_in_place() {
        let mut h = demo([1300.0, 820.0]);
        let t = Duration::from_secs(10);
        let at = |h: &Headless, id: &str| h.app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r).expect(id);
        let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let (rows, rail) = (at(&h, "settings:rows"), at(&h, "button:settingsTab-general"));
        for tab in ["display", "import", "performance", "general", "interface", "display"] {
            h.request("ui.clickWidget", json!({"id": format!("button:settingsTab-{tab}")}), t);
            h.settle(SETTLE);
            match &h.app.ui.dialog {
                Some(crate::state::Dialog::Settings { tab: now }) => assert_eq!(now, tab),
                other => panic!("{tab}: {other:?}"),
            }
            assert_eq!(at(&h, "settings:rows"), rows, "{tab}: the page moved");
            assert_eq!(at(&h, "button:settingsTab-general"), rail, "{tab}: the tab row moved");
        }
    }

    /// Settings ▸ Sync draws a shared library, an offer and exchange states without panicking, and
    /// the offer's buttons and the share checkbox are on screen.
    #[test]
    fn sync_tab_shows_offers_and_exchanges() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "sync"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        h.app.peer_sync.set_status_for_test(json!({
            "running": false, "name": "Here",
            "paired": [{"deviceId": "a", "name": "Laptop", "online": true}, {"deviceId": "b", "name": "Desk", "online": false},
                       {"deviceId": "c", "name": "Old", "online": false}],
            "library": {"name": "Main", "syncId": "x", "sharedWith": ["a", "b"]},
            "offers": [{"deviceId": "b", "name": "Desk", "library": "Trips"}],
            "exchanges": {"a": {"running": true}, "b": {"running": false, "last": now - 7200, "error": "connection lost", "taken": 3, "copies": 1},
                          "c": {"running": false, "last": 1, "error": null, "taken": 0, "copies": 0}}
        }));
        h.settle(SETTLE);
        for id in ["check:syncShare-a", "check:syncShare-b", "button:syncOfferAccept-b", "button:syncOfferDecline-b"] {
            assert!(h.app.widgets.iter().any(|(w, _)| w == id), "{id} on screen");
        }
    }

    /// Settings → Import → "Show the import review when a camera or card is connected": a card that
    /// was already in doesn't ask, a new one opens Import from Device (copying), and a busy window
    /// asks once it's free.
    #[test]
    fn a_connected_card_opens_the_import_review() {
        let root = lcc_testkit::TempDir::new("lc-card");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dcim = root.join("DCIM/100NIKON");
        std::fs::create_dir_all(&dcim).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dcim.join("DSC_0001.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let card = lcc_engine::devices::Device {
            name: "NIKON Z 8".into(),
            path: root.join("DCIM").to_string_lossy().to_string(),
            root: root.to_string_lossy().to_string(),
        };
        h.app.ui.dialog = None;
        // first look: the card was already in
        crate::import::cards_seen(&mut h.app, vec![card.clone()]);
        assert!(h.app.scan.is_none() && h.app.ui.dialog.is_none(), "a card that was already in doesn't ask");
        crate::import::cards_seen(&mut h.app, vec![card.clone()]);
        assert!(h.app.scan.is_none());
        // taken out, then put back while another dialog is open: it waits
        crate::import::cards_seen(&mut h.app, Vec::new());
        h.app.ui.dialog = Some(crate::state::Dialog::About);
        crate::import::cards_seen(&mut h.app, vec![card.clone()]);
        assert!(h.app.scan.is_none(), "a busy window isn't interrupted");
        h.app.ui.dialog = None;
        crate::import::cards_seen(&mut h.app, vec![card.clone()]);
        assert!(h.app.scan.is_some(), "the card is scanned once the window is free");
        assert!(h.step_until(t, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. }))));
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert!(opts.copy, "a card is copied into the library");
        // with the option off nothing is remembered, so turning it on again starts with a first look
        assert!(!h.app.session.import_defaults.show_on_card);
        h.step();
        assert!(h.app.ui.known_cards.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The folder scan runs in the background: the request returns at once and the review opens
    /// when the scan finishes (a folder on a network share must not freeze the window).
    /// Clicking the folder being read again keeps the running read (and its progress).
    #[test]
    fn browsing_the_same_folder_again_does_not_restart() {
        let dir = lcc_testkit::TempDir::new("lc-browse-again");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dir.join("a.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (g, n) = (gate.clone(), started.clone());
        h.app.session.media.file_probe = Some(std::sync::Arc::new(move |_: &str| {
            n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            while !g.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(lcc_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
        }));
        let browse =
            |h: &mut Headless| h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(browse(&mut h)["result"]["scanning"], true);
        // the read is under way (held at its one file), however long a loaded machine takes to get there
        // (frames run meanwhile: the read hands its files on from the frame loop)
        h.step_until(SETTLE, |_| started.load(std::sync::atomic::Ordering::Relaxed) > 0);
        assert_eq!(started.load(std::sync::atomic::Ordering::Relaxed), 1, "the read started");
        assert_eq!(browse(&mut h)["result"]["scanning"], true);
        // a second read would reach the folder's one file as the first did, a few frames after it started
        // (the first is still held there, so nothing else probes meanwhile)
        for _ in 0..30 {
            h.step();
        }
        assert_eq!(started.load(std::sync::atomic::Ordering::Relaxed), 1, "the second click must not start another read");
        gate.store(true, std::sync::atomic::Ordering::Relaxed);
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_cloned().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Browsing a folder from the UI (read on a worker thread) records it as browsed, as `library.browse` does:
    /// untouched Local photos are only forgotten from folders not browsed for the set number of days.
    #[test]
    fn browsing_from_the_ui_records_the_folder() {
        let dir = lcc_testkit::TempDir::new("lc-browse-stamp");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let day = dir.join("2026/10/14");
        std::fs::create_dir_all(&day).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(day.join("a.png"), lcc_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let root = dir.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
        let day_s = day.to_string_lossy().to_string();
        assert!(h.app.session.catalog.last_browsed(&root).is_none());
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": root, "subfolders": true}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.session.visible_cloned().len(), 1);
        assert!(h.app.session.catalog.last_browsed(&root).is_some(), "the browsed folder");
        assert!(h.app.session.catalog.last_browsed(&day_s).is_some(), "the folder its photo is in");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Browsing a folder again from the sidebar forgets the untouched Local records of files deleted from it by
    /// another app, as `library.browse` does (issue #1392).
    #[test]
    fn browsing_again_from_the_ui_forgets_deleted_files() {
        let dir = lcc_testkit::TempDir::new("lc-browse-gone");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() };
        let png = lcc_engine::export::encode_image(&img, &o).unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        std::fs::write(dir.join("b.png"), &png).unwrap();
        let path = dir.to_string_lossy().to_string();
        let browse = |h: &mut Headless| {
            let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": path}}), t);
            assert_eq!(r["result"]["scanning"], true, "{r}");
            h.settle(SETTLE);
        };
        browse(&mut h);
        assert_eq!(h.app.session.visible_cloned().len(), 2);
        // another app deletes the photo that isn't selected (a selected or open one stays, shown as missing)
        let held: Vec<lcc_catalog::PhotoId> =
            h.app.session.selection.ids.iter().copied().chain(h.app.session.selection.active).chain(h.app.session.previous_active).collect();
        let name = |id: &lcc_catalog::PhotoId| h.app.session.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_default();
        let gone = if held.iter().any(|id| name(id) == "b.png") { "a.png" } else { "b.png" };
        std::fs::remove_file(dir.join(gone)).unwrap();
        // a later session: the browse's "Add 2 Photos" step is no longer in the undo history (photos it refers to
        // are kept, like selected ones)
        h.app.session.undo.clear();
        h.app.session.redo.clear();
        // the folder clicked again in the sidebar
        browse(&mut h);
        assert_eq!(h.app.session.visible_cloned().len(), 1, "the deleted file's record is forgotten");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_folder_scans_in_the_background() {
        let dir = lcc_testkit::TempDir::new("lc-scanbg");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lcc_engine::export::encode_image(
            &img,
            &lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() },
        )
        .unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.scan.is_none());
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancel closes the scan at once and opens no review; Add from Device while a scan runs is
    /// refused and leaves the running scan's options alone.
    #[test]
    fn folder_scan_cancel_and_busy() {
        let dir = lcc_testkit::TempDir::new("lc-scancancel");
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lcc_engine::export::encode_image(
            &img,
            &lcc_engine::export::ExportOptions { format: lcc_engine::export::ExportFormat::Png, ..Default::default() },
        )
        .unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        // a scan that can't finish until the test lets it: the probe waits on a flag
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let g = gate.clone();
        h.app.session.media.file_probe = Some(std::sync::Arc::new(move |_: &str| {
            while !g.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(lcc_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
        }));
        let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        let r = h.request("engine.execute", json!({"command": "file.addFromDevice", "params": {"path": dir.to_string_lossy()}}), t);
        assert_ne!(r["ok"], true, "a second scan is refused: {r}");
        assert!(!h.app.scan.as_ref().unwrap().copy, "the running scan keeps its options");
        let r = h.request("ui.inspect", json!({}), t);
        assert!(r["result"]["scan"].is_object(), "{r}");
        // Cancel is the ✕ of the scan's row in the activity stack (shown once it is half a second old)
        let id = h.app.session.activity.list().first().map(|t| t.id).expect("a scan row");
        let cross = format!("activity:cancel:{id}");
        assert!(h.step_until(t, |h| h.app.widgets.iter().any(|(w, _)| *w == cross)), "the row shows");
        let r = h.request("ui.clickWidget", json!({"id": cross}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        assert!(h.app.scan.is_none(), "✕ closes the scan at once");
        gate.store(true, std::sync::atomic::Ordering::Relaxed);
        h.settle(SETTLE);
        assert!(h.app.ui.dialog.is_none(), "a cancelled scan opens no review");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ⇧⌘V opens Paste Selected Settings (prefilled with the copied groups); unchecking a group
    /// leaves it alone. ⌘F puts typing into the search field.
    #[test]
    fn paste_selected_settings_and_find() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        let exec = |h: &mut Headless, c: &str, p: Value| h.request("engine.execute", json!({"command": c, "params": p}), t);
        exec(&mut h, "library.select", json!({"ids": [ids[0]]}));
        exec(&mut h, "develop.set", json!({"values": {"light.exposure": 1.0, "color.vibrance": 30}}));
        exec(&mut h, "develop.copy", json!({"groups": ["light", "color"]}));
        exec(&mut h, "library.select", json!({"ids": [ids[1]]}));
        // (⇧⌘V pastes at once in the Classic set; the dialog is Paste Selected Settings…)
        exec(&mut h, "dialog.pasteSettings", json!({}));
        h.settle(SETTLE);
        let Some(crate::state::Dialog::PasteSettings { groups }) = &h.app.ui.dialog else { panic!("dialog not open: {:?}", h.app.ui.dialog) };
        assert_eq!(lcc_develop::SettingsPick::from_ids(groups), lcc_develop::SettingsPick::from_ids(&["light", "color"]));
        let r = h.request("ui.clickWidget", json!({"id": "pasteGroup:color"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.request("ui.dialog.confirm", json!({}), t)["ok"], true);
        let d = h.app.session.develop_of(lcc_catalog::PhotoId(ids[1])).unwrap();
        assert_eq!((d.light.exposure, d.color.vibrance), (1.0, 0.0));
        // ⌘F, then typing filters the grid by text
        h.request("ui.set", json!({"view": "grid"}), t);
        h.request("ui.key", json!({"key": "F", "cmd": true}), t);
        h.settle(SETTLE);
        h.request("ui.text", json!({"text": "zz-no-such-photo"}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.ui.search, "zz-no-such-photo");
        assert!(h.app.session.visible_cloned().is_empty());
    }

    /// Issue #61: Shift+Cmd+S opens Synchronize Settings (also Photo ▸ Develop Settings ▸ Sync
    /// Settings…); with only Exposure ticked the other selected photos get the exposure and nothing else.
    #[test]
    fn sync_settings_shortcut_syncs_only_exposure() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let photo_menu = crate::menubar::menu_bar(&h.app).into_iter().find(|(m, _)| m == "Photo").unwrap().1;
        let in_develop_settings = photo_menu.iter().any(|n| match n {
            crate::menubar::MenuNode::Submenu { label, children } => {
                label == "Develop Settings"
                    && children.iter().any(|c| matches!(c, crate::menubar::MenuNode::Item { id, .. } if id == "dialog.syncSettings"))
            }
            _ => false,
        });
        assert!(in_develop_settings, "Photo ▸ Develop Settings ▸ Sync Settings… is missing");
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
        let exec = |h: &mut Headless, c: &str, p: Value| h.request("engine.execute", json!({"command": c, "params": p}), t);
        exec(&mut h, "library.select", json!({"ids": ids, "active": ids[0]}));
        exec(&mut h, "module.develop", json!({}));
        let before: Vec<_> = ids[1..].iter().map(|id| (*h.app.session.develop_of(lcc_catalog::PhotoId(*id)).unwrap()).clone()).collect();
        exec(&mut h, "develop.set", json!({"id": ids[0], "values": {"light.exposure": 1.25, "light.contrast": 40, "color.vibrance": 30}}));
        h.settle(SETTLE);
        assert_eq!(h.request("ui.key", json!({"key": "S", "cmd": true, "shift": true}), t)["ok"], true);
        h.settle(SETTLE);
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::SyncSettings { .. })), "{:?}", h.app.ui.dialog);
        for w in ["syncGroup:none", "syncGroup:open:light", "syncGroup:light.exposure"] {
            assert_eq!(h.request("ui.clickWidget", json!({"id": w}), t)["ok"], true, "{w}");
            h.settle(SETTLE);
        }
        assert_eq!(h.request("ui.dialog.confirm", json!({}), t)["ok"], true);
        for (id, old) in ids[1..].iter().zip(before) {
            let mut d = (*h.app.session.develop_of(lcc_catalog::PhotoId(*id)).unwrap()).clone();
            assert_eq!(d.light.exposure, 1.25);
            d.light.exposure = old.light.exposure;
            assert_eq!(d, old, "only the exposure of photo {id} changes");
        }
        // Develop's Sync… button opens the same dialog, and the switch beside it turns Auto Sync on
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "button:syncBar"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::SyncSettings { .. })), "{:?}", h.app.ui.dialog);
        h.request("ui.dialog.cancel", json!({}), t);
        h.settle(SETTLE);
        assert!(!h.app.session.auto_sync);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:autoSyncSwitch"}), t)["ok"], true);
        h.settle(SETTLE);
        assert!(h.app.session.auto_sync);
    }

    /// Colour labels: Info-panel swatches set/clear the label, the names dialog renames them and
    /// the Photo menu shows the names.
    #[test]
    fn label_swatches_and_names() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
        h.request("ui.set", json!({"view": "detail", "right": "info"}), t);
        let r = h.request("ui.clickWidget", json!({"id": "label:green"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().label, Some(lcc_catalog::ColorLabel::Green));
        h.request("engine.execute", json!({"command": "dialog.labelNames"}), t);
        if let Some(crate::state::Dialog::LabelNames { names, .. }) = &mut h.app.ui.dialog {
            names[2] = "Approved".into();
        } else {
            panic!("no dialog");
        }
        h.settle(SETTLE);
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let tree = h.request("ui.menu.tree", json!({}), t).to_string();
        assert!(tree.contains("Approved (Green)"), "menu shows the name");
        h.request("ui.clickWidget", json!({"id": "label:green"}), t);
        assert_eq!(h.app.session.catalog.photo(id).unwrap().label, None, "clicking the current label clears it");
        h.settle(SETTLE);
    }

    /// File → Import Photos… opens the import review: candidates with thumbnails, the duplicate is
    /// unchecked; unchecking a cell and confirming imports the rest in batches, into a new album
    /// with keywords, as one undo step.
    #[test]
    fn import_review_dialog() {
        let dir = lcc_testkit::TempDir::new("lc-ui-import");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5u8 {
            let (w, h) = (40usize, 30usize);
            let data: Vec<[u8; 4]> = (0..w * h).map(|k| [(k % w * 6) as u8, i * 40, (k / w * 8) as u8, 255]).collect();
            let img = lcc_raster::Rgba8 { width: w, height: h, data };
            let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
        }
        std::fs::copy(dir.join("img0.png"), dir.join("img0-copy.png")).unwrap();
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
        let t = Duration::from_secs(10);
        let n0 = h.app.session.catalog.len();
        let undo0 = h.app.session.undo.len();
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        h.step_until(SETTLE, |h| h.app.renderer.textures.keys().filter(|s| matches!(s, crate::render::Slot::Import(_))).count() >= 5);
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert_eq!(opts.candidates.len(), 6);
        assert_eq!(opts.candidates.iter().filter(|c| c.duplicate.is_some()).count(), 1);
        assert!(h.app.renderer.textures.keys().filter(|s| matches!(s, crate::render::Slot::Import(_))).count() >= 5, "thumbnails");
        // uncheck the first photo; name a new album and keywords
        let r = h.request("ui.clickWidget", json!({"id": "import:0"}), t);
        assert_eq!(r["ok"], true, "{r}");
        if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
            assert_eq!(opts.selected_paths().len(), 4, "duplicate and unchecked photo left out");
            opts.new_album = "Card 1".into();
            opts.keywords = "cardone, 2026".into();
        } else {
            panic!("no import dialog");
        }
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.import.is_none(), "finished");
        assert_eq!(h.app.session.catalog.len(), n0 + 4);
        let album = h.app.session.catalog.albums().find(|a| a.name == "Card 1").expect("album").id;
        assert_eq!(h.app.session.catalog.album_count(album), 4);
        assert!(h.app.session.catalog.photos().filter(|p| p.meta.keywords.contains(&"cardone".to_string())).count() == 4);
        assert_eq!(h.app.session.undo.len(), undo0 + 1, "one undo step");
        h.request("engine.execute", json!({"command": "edit.undo"}), t);
        assert_eq!(h.app.session.catalog.len(), n0);
        h.settle(SETTLE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The import review offers what to do with files that are in Recently Deleted (issues #298,
    /// #706): they are checked, to be restored, by default; Leave them unchecks them; confirming
    /// restores the photo (with its edits) or, with Import as new, imports the file afresh.
    #[test]
    fn import_review_offers_recently_deleted_files() {
        for (choice, tag) in [("restore", "r"), ("fresh", "f")] {
            let dir = lcc_testkit::TempDir::new(&format!("lc-ui-import-trash-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..2u8 {
                let data: Vec<[u8; 4]> = (0..40 * 30).map(|k| [(k % 40 * 6) as u8, i * 90, 7, 255]).collect();
                let img = lcc_raster::Rgba8 { width: 40, height: 30, data };
                let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
                std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
            }
            let services = crate::Services { png: None, ..Default::default() };
            let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), services);
            app.ui.view = crate::state::ViewMode::PhotoGrid;
            let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
            let t = Duration::from_secs(10);
            let r =
                h.request("engine.execute", json!({"command": "library.import", "params": {"paths": [dir.join("img0.png").to_string_lossy()]}}), t);
            let id = r["result"]["imported"][0].as_u64().unwrap();
            h.request("engine.execute", json!({"command": "photo.rate", "params": {"ids": [id], "rating": 3}}), t);
            h.request("engine.execute", json!({"command": "photo.delete", "params": {"ids": [id]}}), t);
            h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), t);
            h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
            let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("no import review") };
            assert_eq!(opts.on_deleted, "restore", "the default brings a deleted photo back");
            assert_eq!(opts.selected_paths().len(), 2, "the trashed file is checked from the start: it will be restored");
            opts.set_on_deleted("");
            assert_eq!(opts.selected_paths().len(), 1, "Leave them: only the new file");
            opts.set_on_deleted(choice);
            assert_eq!(opts.selected_paths().len(), 2, "the trashed file is checked again once a choice is made");
            // a per-cell choice survives switching between the two options, and "Leave them" unchecks
            let trashed = (0..opts.candidates.len()).find(|i| opts.is_trashed(*i)).unwrap();
            opts.checked[trashed] = false;
            opts.set_on_deleted(if choice == "restore" { "fresh" } else { "restore" });
            assert_eq!(opts.selected_paths().len(), 1, "still unchecked");
            opts.set_on_deleted("");
            assert_eq!(opts.selected_paths().len(), 1);
            opts.set_on_deleted(choice);
            assert_eq!(opts.selected_paths().len(), 2);
            let r = h.request("ui.dialog.confirm", json!({}), t);
            assert_eq!(r["ok"], true, "{r}");
            // frames until the import has finished, so its (timed) toast is read before it expires
            let t0 = std::time::Instant::now();
            while h.app.import.is_some() && t0.elapsed() < Duration::from_secs(30) {
                h.step();
            }
            assert!(h.app.import.is_none(), "finished");
            let toast = h.app.ui.toast().cloned().map(|t| t.0).unwrap_or_default();
            let photos = &h.app.session.catalog;
            assert_eq!(photos.photos().filter(|p| !p.deleted).count(), 2, "{choice}");
            let old = photos.photo(lcc_catalog::PhotoId(id));
            if choice == "restore" {
                assert!(toast.contains("1 restored"), "{toast}");
                assert!(old.is_some_and(|p| !p.deleted && p.rating == 3), "restored with its edits");
                assert_eq!(photos.len(), 2);
            } else {
                assert!(old.is_none(), "the trashed record is gone");
                assert_eq!(photos.len(), 2);
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// With "Leave them", a file in Recently Deleted is left unchecked but a click on its checkbox
    /// (or on its "In Recently Deleted" badge) still checks it, and importing it restores the
    /// photo (issue #1285).
    #[test]
    fn import_review_checks_a_recently_deleted_file_by_hand() {
        let dir = lcc_testkit::TempDir::new("lc-ui-import-trash-click");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..2u8 {
            let data: Vec<[u8; 4]> = (0..40 * 30).map(|k| [(k % 40 * 6) as u8, i * 90, 7, 255]).collect();
            let img = lcc_raster::Rgba8 { width: 40, height: 30, data };
            let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
        }
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::new().with_fs(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("engine.execute", json!({"command": "library.import", "params": {"paths": [dir.join("img0.png").to_string_lossy()]}}), t);
        let id = r["result"]["imported"][0].as_u64().unwrap();
        h.request("engine.execute", json!({"command": "photo.rate", "params": {"ids": [id], "rating": 3}}), t);
        h.request("engine.execute", json!({"command": "photo.delete", "params": {"ids": [id]}}), t);
        h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), t);
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("no import review") };
        let trashed = (0..opts.candidates.len()).find(|i| opts.is_trashed(*i)).expect("a file in Recently Deleted");
        opts.set_on_deleted("");
        h.settle(SETTLE);
        let checked = |h: &Headless| matches!(&h.app.ui.dialog, Some(crate::state::Dialog::Import { opts }) if opts.checked[trashed]);
        assert!(!checked(&h), "Leave them: unchecked");
        let cell = |h: &mut Headless| {
            let r = h.request("ui.widgets", json!({"filter": format!("import:{trashed}")}), t);
            let w = r["result"].as_array().and_then(|a| a.iter().find(|w| w["id"] == format!("import:{trashed}"))).cloned().expect("cell on screen");
            [0usize, 1, 2, 3].map(|i| w["rect"][i].as_f64().unwrap())
        };
        // the checkbox: 16 pt, 6 pt in from the cell's top-left corner
        let [x, y, ..] = cell(&mut h);
        h.request("ui.click", json!({"x": x + 14.0, "y": y + 14.0}), t);
        h.settle(SETTLE);
        assert!(checked(&h), "a click on the checkbox checks the file");
        // the badges ("Will be restored" at the top right, "In Recently Deleted" under the
        // checkbox) don't swallow the click either
        let [x, y, w, _] = cell(&mut h);
        h.request("ui.click", json!({"x": x + w - 14.0, "y": y + 12.0}), t);
        h.settle(SETTLE);
        assert!(!checked(&h), "a click on the badge unchecks it again");
        h.request("ui.click", json!({"x": x + w - 14.0, "y": y + 34.0}), t);
        h.settle(SETTLE);
        assert!(checked(&h));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let t0 = std::time::Instant::now();
        while h.app.import.is_some() && t0.elapsed() < Duration::from_secs(30) {
            h.step();
        }
        assert!(h.app.import.is_none(), "finished");
        let photos = &h.app.session.catalog;
        assert!(photos.photo(lcc_catalog::PhotoId(id)).is_some_and(|p| !p.deleted && p.rating == 3), "restored with its edits");
        assert_eq!(photos.photos().filter(|p| !p.deleted).count(), 2);
        assert_eq!(photos.len(), 2, "restored, not imported a second time");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The import review fills the window: the photo grid takes the room, and the Copy options
    /// (more rows) never push the Import button off the window.
    #[test]
    fn import_review_fills_the_window_and_keeps_its_button() {
        let mut h = demo([1400.0, 1000.0]);
        let t = Duration::from_secs(10);
        let candidates = (0..60)
            .map(|i| lcc_engine::import::ImportCandidate {
                path: format!("/lc-test/img{i}.png"),
                name: format!("img{i}.png"),
                error: Some("not read in this test".into()),
                ..Default::default()
            })
            .collect();
        h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(crate::import::ImportDialog::new(candidates)) });
        let rect = |h: &mut Headless, id: &str| {
            let r = h.request("ui.widgets", json!({}), t);
            let w = r["result"].as_array().and_then(|a| a.iter().find(|w| w["id"] == id)).unwrap_or_else(|| panic!("{id} on screen"));
            [0usize, 1, 2, 3].map(|i| w["rect"][i].as_f64().unwrap())
        };
        for _ in 0..10 {
            h.step();
        }
        let grid = rect(&mut h, "import:grid");
        assert!(grid[3] > 600.0 && grid[2] > 700.0, "the grid takes the room: {grid:?}");
        let go = rect(&mut h, "import:go");
        assert!(go[1] + go[3] <= 1000.0 && go[0] + go[2] <= 1400.0, "the button is on screen: {go:?}");
        for copy in [true, false] {
            if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
                opts.copy = copy;
            }
            for _ in 0..10 {
                h.step();
            }
            assert_eq!(rect(&mut h, "import:go"), go, "copy {copy}: the button stays put");
        }
    }

    /// Importing while an album is shown (issue #1446): the new photos aren't in it, so the view
    /// switches to All Photos with the first of them active, instead of an active photo the grid
    /// and the filmstrip don't show.
    #[test]
    fn an_import_from_an_album_view_shows_the_first_new_photo() {
        let dir = lcc_testkit::TempDir::new("lc-ui-import-show");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3u8 {
            let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[i * 40, 3, 9, 255]; 64] };
            let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("IMG_{i:02}.png")), png).unwrap();
        }
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let album = h.app.session.catalog.albums().find(|a| !a.folder && !a.is_smart()).map(|a| a.id.0).expect("a demo album");
        let r = h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "album", "id": album}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let before: Vec<lcc_catalog::PhotoId> = h.app.session.catalog.photos().map(|p| p.id).collect();
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.import.is_none(), "finished");
        let active = h.app.session.selection.active.expect("a photo is active");
        assert!(!before.contains(&active), "the active photo is a new one: {active:?}");
        assert_eq!(h.app.session.source, lcc_engine::LibrarySource::All);
        assert!(h.app.session.visible_cloned().contains(&active), "the grid shows the active photo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Import dialog, copy mode: a chosen destination, one folder, renamed copies numbered across
    /// the import's batches.
    #[test]
    fn import_copy_renames_into_destination() {
        let base = lcc_testkit::TempDir::new("lc-ui-import-copy");
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("out"));
        std::fs::create_dir_all(&src).unwrap();
        // more files than a batch holds (see `batch_size`)
        for i in 0..20u8 {
            let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[i * 12, 3, 9, 255]; 64] };
            let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(src.join(format!("IMG_{i:02}.png")), png).unwrap();
        }
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo().with_fs(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [src.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import review") };
        assert_eq!(opts.candidates.len(), 20);
        for id in ["button:importCopy", "button:importDest"] {
            let r = h.request("ui.clickWidget", json!({"id": id}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
        }
        if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
            assert!(opts.copy);
            assert_eq!(opts.destination, dest.to_string_lossy(), "Choose… sets the folder");
            opts.organize = "flat".into();
            opts.rename = "Trip-{seq:2}".into();
        } else {
            panic!("no import dialog");
        }
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.import.is_none(), "finished");
        let mut names: Vec<String> = std::fs::read_dir(&dest).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        names.sort();
        let want: Vec<String> = (1..=20).map(|i| format!("Trip-{i:02}.png")).collect();
        assert_eq!(names, want, "numbered across batches of {}", lcc_engine::import::batch_size());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Import review, Copy, Folders → Custom template…: the default `{date:%Y}/{date:%Y%m%d}`
    /// files copies under `2026/20260114/`, the example line shows the full destination, and a
    /// template that would climb out of the destination is refused.
    #[test]
    fn import_copy_into_a_custom_folder_template() {
        let base = lcc_testkit::TempDir::new("lc-ui-import-tpl");
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("out"));
        std::fs::create_dir_all(&src).unwrap();
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[40, 3, 9, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(src.join("IMG_01.png"), png).unwrap();
        // undated files are filed by the file's date (noon UTC: 14 January in every zone the tests run in)
        std::fs::File::options()
            .write(true)
            .open(src.join("IMG_01.png"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_768_392_000))
            .unwrap();
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut session = lcc_engine::Session::with_demo().with_fs();
        session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
        let mut app = LccApp::new(session, services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [src.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        for id in ["button:importCopy", "button:importDest"] {
            let r = h.request("ui.clickWidget", json!({"id": id}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
        }
        if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
            opts.organize = "custom".into();
            opts.folder_template = "../{date:%Y}".into();
        }
        h.step();
        // refused: the dialog stays open, nothing is imported
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_ne!(r["ok"], true, "{r}");
        assert!(h.app.import.is_none());
        let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("dialog closed") };
        opts.folder_template = crate::import::DEFAULT_FOLDER_TEMPLATE.into();
        let opts = opts.clone();
        let sep = std::path::MAIN_SEPARATOR;
        let example = crate::import::example_destination(&h.app, &opts).expect("example");
        assert_eq!(example, format!("{}{sep}2026{sep}20260114{sep}IMG_01.png", dest.to_string_lossy()));
        h.step();
        let r = h.request("ui.widgets", json!({}), t);
        assert!(r.to_string().contains("label:importExample"), "example shown");
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(dest.join("2026").join("20260114").join("IMG_01.png").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Import review, Move: an explicit choice with its explanation, the confirm button says
    /// Move; the files land in the folder template, renamed, and leave the card.
    #[test]
    fn import_move_into_a_folder_template() {
        let base = lcc_testkit::TempDir::new("lc-ui-import-move");
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("Photos"));
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..2u8 {
            let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[40 + i, 3, 9, 255]; 64] };
            let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(src.join(format!("IMG_0{i}.png")), png).unwrap();
            std::fs::File::options()
                .write(true)
                .open(src.join(format!("IMG_0{i}.png")))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_768_392_000))
                .unwrap();
        }
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut session = lcc_engine::Session::with_demo().with_fs();
        session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
        let mut app = LccApp::new(session, services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [src.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        for id in ["button:importMove", "button:importDest"] {
            let r = h.request("ui.clickWidget", json!({"id": id}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
        }
        let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("no import dialog") };
        assert!(opts.copy && opts.move_files, "Move chosen");
        opts.organize = "custom".into();
        opts.folder_template = crate::import::DEFAULT_FOLDER_TEMPLATE.into();
        opts.rename = "{date:%Y%m%d}_{seq:3}".into();
        let opts = opts.clone();
        let sep = std::path::MAIN_SEPARATOR;
        let example = crate::import::example_destination(&h.app, &opts).expect("example");
        assert_eq!(example, format!("{}{sep}2026{sep}20260114{sep}20260114_001.png", dest.to_string_lossy()));
        h.step();
        let r = h.request("ui.widgets", json!({}), t);
        assert!(r.to_string().contains("label:importModeHelp"), "the Move explanation is shown");
        assert!(!r.to_string().contains("check:importDng"), "no Copy as DNG when moving");
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.import.is_none(), "finished");
        let day = dest.join("2026").join("20260114");
        assert!(day.join("20260114_001.png").is_file() && day.join("20260114_002.png").is_file());
        assert_eq!(std::fs::read_dir(&src).unwrap().count(), 0, "moved off the card");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Import review, Copy: Tags beside the Rename field lists the template tags; clicking one
    /// inserts it at the text cursor (not appended, not replacing the template).
    #[test]
    fn import_rename_tags_insert_at_the_cursor() {
        let dir = lcc_testkit::TempDir::new("lc-ui-import-tags");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lcc_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
        let png = lcc_codecs::encode_png(&lcc_codecs::EncodeImage::rgba8(&img), &lcc_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join("IMG_0007.png"), png).unwrap();
        let lib = dir.join("lib");
        let mut session = lcc_engine::Session::with_demo().with_fs();
        session.open_library(&lib, false).unwrap();
        let mut app = LccApp::new(session, crate::Services { png: None, ..Default::default() });
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 1000.0], 1.0);
        let t = Duration::from_secs(10);
        let r =
            h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.join("IMG_0007.png").to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "button:importCopy"}), t);
        assert_eq!(r["ok"], true, "{r}");
        if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
            opts.rename = "Trip-_x".into();
        }
        // Paint the edited template before locating the Tags button in the resized dialog.
        assert!(h.settle(SETTLE), "Copy dialog layout should settle before locating Tags");
        // the cursor sits after "Trip-"
        let id = egui::Id::new("import-rename");
        let mut st = egui::text_edit::TextEditState::default();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(5))));
        st.store(&h.view.ctx, id);
        let r = h.request("ui.clickWidget", json!({"id": "button:importRenameTags"}), t);
        assert_eq!(r["ok"], true, "{r}");
        let seq3 = lcc_engine::rename::TOKENS.iter().position(|x| x.tag == "{seq:3}").unwrap();
        let r = h.request("ui.clickWidget", json!({"id": format!("button:importRenameTag-{seq3}")}), t);
        assert_eq!(r["ok"], true, "{r}");
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import dialog") };
        assert_eq!(opts.rename, "Trip-{seq:3}_x", "inserted at the cursor");
        // a second tag goes after the first (the cursor moved past it)
        let r = h.request("ui.clickWidget", json!({"id": "button:importRenameTag-0"}), t);
        assert_eq!(r["ok"], true, "{r}");
        let Some(crate::state::Dialog::Import { opts }) = &h.app.ui.dialog else { panic!("no import dialog") };
        assert_eq!(opts.rename, "Trip-{seq:3}{name}_x");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Help ▸ About shows only its main section (name, version, licences, links): no Contributors
    /// or Models section, and no tabs to reach one.
    #[test]
    fn about_dialog_shows_only_its_main_section() {
        // an empty library: the dialog needs no photos, and no decodes compete with other tests
        let services = crate::Services { png: None, ..Default::default() };
        let mut h = Headless::new(LccApp::new(lcc_engine::Session::new(), services), [1300.0, 820.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("ui.menu.invoke", json!({"id": "app.about"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::About));
        let img = h.snapshot(SETTLE);
        assert_eq!(img.size, [1300, 820]);
        let ids: Vec<&str> = h.app.widgets.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"link:about-website"), "the main section's links show: {ids:?}");
        assert!(!ids.iter().any(|id| id.starts_with("button:aboutTab-")), "no section tabs: {ids:?}");
    }

    /// HDR display output (#47): with an HDR display, an HDR edit's loupe is drawn from its
    /// linear float render (values above SDR white reach the window's HDR texture); an SDR
    /// display, or an SDR edit, keeps the 8-bit loupe and uploads nothing.
    #[test]
    fn an_hdr_display_shows_hdr_edits_from_their_float_render() {
        use std::cell::RefCell;
        use std::rc::Rc;
        struct Fake(Option<f32>, Rc<RefCell<Vec<f32>>>);
        impl crate::HdrOutput for Fake {
            fn headroom(&mut self) -> Option<f32> {
                self.0
            }
            fn upload(&mut self, _view: u8, width: usize, height: usize, rgb: &[f32]) -> Result<egui::TextureId, String> {
                assert_eq!(rgb.len(), width * height * 3);
                self.1.borrow_mut().push(rgb.iter().copied().fold(0.0, f32::max));
                Ok(egui::TextureId::User(7))
            }
        }
        let t = Duration::from_secs(10);
        for (headroom, hdr_edit) in [(Some(4.0), true), (None, true), (Some(4.0), false)] {
            let uploads = Rc::new(RefCell::new(Vec::new()));
            let services = crate::Services { png: None, hdr_output: Some(Box::new(Fake(headroom, uploads.clone()))), ..Default::default() };
            let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
            app.ui.view = crate::state::ViewMode::PhotoGrid;
            let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
            h.request("ui.set", json!({"view": "detail"}), t);
            if hdr_edit {
                h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), t);
                h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 2.0}}}), t);
            }
            let hdr_shown = |h: &Headless| h.app.loupe_shown.is_some_and(|(_, s)| s == "hdr");
            if headroom.is_some() && hdr_edit {
                assert!(h.step_until(SETTLE, hdr_shown), "the loupe shows the HDR texture");
                assert!(uploads.borrow().iter().any(|m| *m > 1.0), "highlights above SDR white: {:?}", uploads.borrow());
                assert!(!h.app.hdr_display_sdr, "no 'SDR window' note on the HDR button");
            } else {
                h.settle(SETTLE);
                assert!(!hdr_shown(&h) && uploads.borrow().is_empty(), "{headroom:?} {hdr_edit}: 8-bit loupe");
                assert_eq!(h.app.hdr_display_sdr, headroom.is_none());
            }
        }
    }

    /// A final display-sized texture, an oversampled HDR texture and a small drag draft
    /// all place the same photo in the same physical-pixel rectangle.
    #[test]
    fn adjustment_pixel_bounds_stay_fixed_across_texture_sizes_and_output_modes() {
        use crate::render::Slot;
        use lcc_catalog::{Op, Photo, PhotoId, Source};
        use std::sync::Arc;
        struct Hdr;
        impl crate::HdrOutput for Hdr {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                Ok(egui::TextureId::User(700 + u64::from(view)))
            }
        }
        for native in [[3264, 2448], [6000, 4000], [3265, 2449]] {
            for ppp in [1.0, 1.25, 1.5, 2.0] {
                for hdr in [false, true] {
                    let mut session = lcc_engine::Session::new();
                    session.media.file_loader = Some(Arc::new(move |_, _, _| {
                        let (w, h) = (2560, (2560.0 * native[1] as f64 / native[0] as f64).round() as usize);
                        let mut image = lcc_raster::Rgb32f::new(w, h);
                        image.data.fill([0.4, 0.3, 0.2]);
                        Ok((image, Default::default()))
                    }));
                    let photo = Photo::new(
                        PhotoId(1),
                        Source::File { path: "pixel-bounds.jpg".into() },
                        "pixel-bounds.jpg",
                        "JPEG",
                        native[0],
                        native[1],
                        "2026-10-22",
                    );
                    session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
                    session.selection = lcc_engine::Selection::single(PhotoId(1));
                    session.execute("develop.hdr", &json!({"enabled": hdr})).unwrap();
                    let services = crate::Services { hdr_output: hdr.then(|| Box::new(Hdr) as Box<dyn crate::HdrOutput>), ..Default::default() };
                    let mut h = Headless::new(LccApp::new(session, services), [1000.3, 700.5], ppp);
                    h.app.ui.view = crate::state::ViewMode::Detail;
                    let bounds = |h: &Headless| {
                        let tex = if h.app.loupe_shown.is_some_and(|(_, source)| source == "hdr") {
                            egui::TextureId::User(700)
                        } else {
                            h.app.renderer.textures[&Slot::Main].id()
                        };
                        h.view
                            .painted_shapes()
                            .iter()
                            .find_map(|shape| match shape {
                                egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == tex => Some(shape.visual_bounding_rect()),
                                _ => None,
                            })
                            .expect("the photo's actual painted quad")
                    };
                    // (the texture on screen: an HDR drag draws HDR drafts alone, no 8-bit ones)
                    let shown_size = |h: &Headless| {
                        if h.app.loupe_shown.is_some_and(|(_, source)| source == "hdr") {
                            h.app.renderer.hdr_view[&crate::render::HDR_LOUPE].size
                        } else {
                            h.app.renderer.textures[&Slot::Main].size
                        }
                    };
                    assert!(h.settle(SETTLE));
                    let still = bounds(&h);
                    let still_size = shown_size(&h);
                    assert_eq!(Some(still), h.app.image_rect);
                    h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
                    assert!(h.settle(SETTLE));
                    assert_eq!(bounds(&h), still, "drag: {native:?}, {ppp}x, HDR {hdr}");
                    h.app.run("develop.set", json!({"values": {"light.exposure": 0.25}})).unwrap();
                    // (a drag drafts once it changes the photo, not when it begins, and the full
                    // render replaces the draft soon after: watch the frames in between)
                    let mut drafted = false;
                    for _ in 0..2000 {
                        h.step();
                        if shown_size(&h) != still_size {
                            drafted = true;
                            assert_eq!(bounds(&h), still, "draft: {native:?}, {ppp}x, HDR {hdr}");
                            break;
                        }
                    }
                    assert!(drafted, "the test exercises a real texture-size transition");
                    assert!(h.settle(SETTLE));
                    assert_eq!(bounds(&h), still, "update: {native:?}, {ppp}x, HDR {hdr}");
                    h.app.run("develop.endInteraction", json!({})).unwrap();
                    assert!(h.settle(SETTLE));
                    assert_eq!(bounds(&h), still, "release: {native:?}, {ppp}x, HDR {hdr}");
                }
            }
        }
    }

    /// Gap 14: zoomed in (1:1 tiles) an HDR edit is still drawn above SDR white, from an HDR
    /// render of the part on screen; the Before side (the photo without its edits) stays SDR;
    /// View ▸ HDR Preview off shows the SDR rendition everywhere, and back on the HDR again.
    #[test]
    fn hdr_edits_stay_hdr_zoomed_in_and_hdr_preview_switches_them_off() {
        use crate::render::{HDR_BEFORE, HDR_LOUPE, HDR_ZOOMED, PANE_AFTER};
        use std::cell::RefCell;
        use std::rc::Rc;
        struct Fake(Rc<RefCell<Vec<(u8, f32)>>>);
        impl crate::HdrOutput for Fake {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, view: u8, width: usize, height: usize, rgb: &[f32]) -> Result<egui::TextureId, String> {
                assert_eq!(rgb.len(), width * height * 3);
                self.0.borrow_mut().push((crate::render::hdr_slot_view(view), rgb.iter().copied().fold(0.0, f32::max)));
                Ok(egui::TextureId::User(7 + u64::from(view)))
            }
        }
        let t = Duration::from_secs(10);
        let uploads = Rc::new(RefCell::new(Vec::new()));
        let services = crate::Services { png: None, hdr_output: Some(Box::new(Fake(uploads.clone()))), ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), t);
        h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 2.0}}}), t);
        let hdr_shown = |h: &Headless| h.app.loupe_shown.is_some_and(|(_, s)| s == "hdr");
        assert!(h.step_until(SETTLE, hdr_shown), "the fit loupe shows the HDR texture");
        h.settle(SETTLE);
        // Every adjustment keeps HDR visible while new float drafts are in flight, and draws
        // them alone: no 8-bit draft of each step beside them.
        h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        for control in ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks", "color.saturation"] {
            let before = uploads.borrow().len();
            let value = if control == "light.exposure" { 3.0 } else { 20.0 };
            h.app.run("develop.set", json!({"values": {control: value}})).unwrap();
            assert!(
                h.step_until(SETTLE, |h| {
                    assert!(hdr_shown(h), "SDR frame during {control}");
                    uploads.borrow().len() > before
                }),
                "live HDR update: {control}"
            );
            assert!(h.app.session.interaction.is_some());
            assert!(!h.app.renderer.is_pending(crate::render::Slot::Main), "an 8-bit draft beside the HDR ones: {control}");
        }
        h.app.run("develop.endInteraction", json!({})).unwrap();
        let before = uploads.borrow().len();
        h.step();
        assert!(hdr_shown(&h), "release keeps HDR");
        assert!(h.step_until(SETTLE, |h| {
            assert!(hdr_shown(h), "SDR frame while final render completes");
            uploads.borrow().len() > before
        }));
        // HDR Preview off: the 8-bit SDR rendition, on: HDR again
        let r = h.request("ui.menu.invoke", json!({"id": "view.hdrPreview"}), t);
        assert!(!h.app.ui.hdr_preview, "{r:?}");
        h.settle(SETTLE);
        assert!(!hdr_shown(&h), "HDR Preview off draws the SDR rendition");
        h.request("ui.menu.invoke", json!({"id": "view.hdrPreview"}), t);
        assert!(h.app.ui.hdr_preview);
        assert!(h.step_until(SETTLE, hdr_shown), "HDR Preview on again");
        // zoomed in: the part on screen gets its own HDR render, highlights above SDR white; until
        // it comes the whole frame's HDR shows (softer), never 8-bit tiles of another brightness
        h.request("ui.menu.invoke", json!({"id": "view.zoom100"}), t);
        let no_tiles = |h: &Headless| h.app.renderer.tiles_pending() == 0 && h.app.renderer.tiles.is_empty();
        let zoomed = |h: &Headless| {
            assert!(hdr_shown(h) && no_tiles(h), "an 8-bit tile while zooming in");
            h.app.region_view.is_some() && uploads.borrow().iter().any(|(v, m)| *v == HDR_ZOOMED + PANE_AFTER && *m > 1.0)
        };
        assert!(h.step_until(SETTLE, zoomed), "zoomed HDR window: {:?}", uploads.borrow());
        h.settle(SETTLE);
        let zoomed_uploads = || uploads.borrow().iter().filter(|(v, _)| *v == HDR_ZOOMED + PANE_AFTER).count();
        let before = zoomed_uploads();
        h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        h.app.run("develop.set", json!({"values": {"light.exposure": 2.5}})).unwrap();
        assert!(
            h.step_until(SETTLE, |h| {
                assert!(hdr_shown(h), "zoomed interaction keeps the HDR underlay");
                assert!(!h.app.renderer.is_pending(crate::render::Slot::Window(PANE_AFTER)), "an 8-bit window draft under the HDR window");
                zoomed_uploads() > before
            }),
            "zoomed drafts upload through HDR too"
        );
        h.app.run("develop.endInteraction", json!({})).unwrap();
        h.settle(SETTLE);
        // a pan: the window there was stays where it belongs while the new part's renders (no
        // 8-bit tiles), and panning back shows the first one again at once, rendering nothing
        let id = h.app.session.active().unwrap();
        let shown = |h: &Headless| h.app.renderer.hdr_view.get(&(HDR_ZOOMED + PANE_AFTER)).filter(|v| v.photo == id).map(|v| (v.key, v.window));
        let (first, old) = (shown(&h).expect("a zoomed HDR window"), h.app.region_view.expect("zoomed").window);
        let before = zoomed_uploads();
        h.request("ui.set", json!({"pan": [0.2, 0.3]}), t);
        assert_ne!(h.app.region_view.expect("zoomed").window, old, "the pan shows another part");
        assert!(
            h.step_until(SETTLE, |h| {
                assert!(no_tiles(h), "an 8-bit tile after a pan");
                assert!(shown(h).is_some(), "a pan leaves no HDR window up");
                zoomed_uploads() > before
            }),
            "the new part's HDR window"
        );
        h.settle(SETTLE);
        assert_ne!(shown(&h), Some(first));
        let before = zoomed_uploads();
        h.request("ui.set", json!({"pan": [0.5, 0.5]}), t);
        h.settle(SETTLE);
        assert_eq!(shown(&h), Some(first), "back where it was: the window it showed");
        assert_eq!(zoomed_uploads(), before, "shown again without rendering it again");
        // Before / After side by side, fit: the After stays HDR, the original is SDR
        h.request("ui.menu.invoke", json!({"id": "view.zoomFit"}), t);
        h.request("ui.menu.invoke", json!({"id": "view.beforeAfter"}), t);
        h.settle(SETTLE);
        assert!(uploads.borrow().iter().all(|(v, _)| *v != HDR_BEFORE), "the Before (no edits) is not an HDR edit");
        assert!(uploads.borrow().iter().any(|(v, _)| *v == HDR_LOUPE));
        assert!(h.app.renderer.hdr_view.contains_key(&HDR_LOUPE));
    }

    /// LR-HDR-SDRPREVIEW: Preview for SDR Display (under Visualize HDR) shows an HDR edit's SDR
    /// rendition on an HDR display, as View ▸ HDR Preview off does (one switch, two places); the
    /// SDR Rendition sliders say how to see them while the screen shows HDR. On a window that
    /// can't show HDR the box stays on and can't be turned off.
    #[test]
    fn preview_for_sdr_display_shows_the_sdr_rendition_of_an_hdr_edit() {
        struct Fake(Option<f32>);
        impl crate::HdrOutput for Fake {
            fn headroom(&mut self) -> Option<f32> {
                self.0
            }
            fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                Ok(egui::TextureId::User(7 + u64::from(view)))
            }
        }
        let t = Duration::from_secs(10);
        for headroom in [Some(4.0), None] {
            let services = crate::Services { png: None, hdr_output: Some(Box::new(Fake(headroom))), ..Default::default() };
            let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
            app.ui.view = crate::state::ViewMode::PhotoGrid;
            let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
            h.request("ui.set", json!({"view": "detail"}), t);
            h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), t);
            h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 2.0}}}), t);
            h.app.ui.toggle_flyout("sdrRendition");
            let hdr_shown = |h: &Headless| h.app.loupe_shown.is_some_and(|(_, s)| s == "hdr");
            let note_shown = |h: &Headless| h.app.widgets.iter().any(|(id, _)| id == "note:sdrSlidersHidden");
            assert!(h.settle(SETTLE));
            if headroom.is_none() {
                assert!(!hdr_shown(&h) && !note_shown(&h), "an SDR window shows the SDR rendition, sliders usable");
                let r = h.request("ui.clickWidget", json!({"id": "check:hdrSdrPreview"}), t);
                h.settle(SETTLE);
                assert!(!hdr_shown(&h), "the box can't be turned off on an SDR window: {r}");
                continue;
            }
            assert!(h.step_until(SETTLE, hdr_shown), "an HDR display shows the HDR render");
            assert!(note_shown(&h), "the SDR sliders say how to see them");
            let r = h.request("ui.clickWidget", json!({"id": "check:hdrSdrPreview"}), t);
            assert_eq!(r["ok"], true, "{r}");
            assert!(h.settle(SETTLE));
            assert!(!h.app.ui.hdr_preview, "the same switch as View ▸ HDR Preview");
            assert!(!hdr_shown(&h), "the SDR rendition shows");
            assert!(!note_shown(&h));
            // its SDR sliders now change what is on screen
            let before = h.app.renderer.textures.get(&crate::render::Slot::Main).map(|x| x.key);
            h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"hdr.sdrBrightness": 60.0}}}), t);
            assert!(h.settle(SETTLE));
            assert_ne!(h.app.renderer.textures.get(&crate::render::Slot::Main).map(|x| x.key), before);
            assert!(!hdr_shown(&h));
            h.request("ui.clickWidget", json!({"id": "check:hdrSdrPreview"}), t);
            assert!(h.step_until(SETTLE, hdr_shown), "off again: HDR");
            assert!(h.app.ui.hdr_preview);
        }
    }

    /// A slider drag of an HDR edit on an HDR display: the views that show the edit (the After
    /// of a Before / After split, the second window) stay HDR and update live; what the drag
    /// does not change (another photo's Compare tile) is not rendered again at its start or end.
    #[test]
    fn hdr_drags_update_the_views_of_the_edit_and_leave_the_rest() {
        use crate::render::{HDR_BEFORE, HDR_COMPARE, HDR_LOUPE, HDR_SECOND, Slot};
        use std::cell::RefCell;
        use std::rc::Rc;
        struct Fake(Rc<RefCell<Vec<u8>>>);
        impl crate::HdrOutput for Fake {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, view: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                self.0.borrow_mut().push(crate::render::hdr_slot_view(view));
                Ok(egui::TextureId::User(7 + u64::from(view)))
            }
        }
        let t = Duration::from_secs(10);
        let uploads = Rc::new(RefCell::new(Vec::new()));
        let services = crate::Services { png: None, hdr_output: Some(Box::new(Fake(uploads.clone()))), ..Default::default() };
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
        let count = |view: u8| uploads.borrow().iter().filter(|v| **v == view).count();
        let vis = h.app.session.visible_cloned();
        h.request("ui.set", json!({"view": "detail"}), t);
        for id in [vis[0], vis[1]] {
            h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
            h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), t);
            h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 2.0}}}), t);
        }
        let id = h.app.session.active().unwrap();
        // a drag of two steps: each step's HDR frame of `view` arrives, and `shown` holds on every frame
        let drag = |h: &mut Headless, view: u8, shown: &dyn Fn(&Headless) -> bool, what: &str| {
            h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
            for value in [2.5, 3.0] {
                let before = count(view);
                h.app.run("develop.set", json!({"values": {"light.exposure": value}})).unwrap();
                assert!(
                    h.step_until(SETTLE, |h| {
                        assert!(shown(h), "{what}: an SDR frame in the drag");
                        count(view) > before
                    }),
                    "{what}: live HDR updates"
                );
            }
            h.app.run("develop.endInteraction", json!({})).unwrap();
            h.settle(SETTLE);
            assert!(shown(h), "{what}: HDR after the release");
        };
        // Before / After side by side: the After stays HDR through the drag; the Before (no
        // edits) is not an HDR edit
        h.request("ui.set", json!({"beforeAfter": "sideBySide"}), t);
        let after_hdr = |h: &Headless| h.app.loupe_shown.is_some_and(|(_, s)| s == "hdr");
        assert!(h.step_until(SETTLE, after_hdr));
        h.settle(SETTLE);
        drag(&mut h, HDR_LOUPE, &after_hdr, "Before / After");
        assert_eq!(count(HDR_BEFORE), 0);
        h.request("ui.set", json!({"beforeAfter": "off"}), t);
        // the second window: its HDR frame is up on every frame of the drag
        h.app.run("second.live", json!({})).unwrap();
        let second_hdr = |h: &Headless| {
            let wanted = h.app.renderer.wanted(Slot::HdrView(HDR_SECOND));
            h.app
                .renderer
                .hdr_view
                .get(&HDR_SECOND)
                .is_some_and(|v| v.photo == id && wanted.is_none_or(|k| h.app.renderer.hdr_frame(HDR_SECOND, id, k).is_some()))
        };
        assert!(h.step_until(SETTLE, |h| count(HDR_SECOND) > 0 && second_hdr(h)), "the second window shows the HDR edit");
        h.settle(SETTLE);
        drag(&mut h, HDR_SECOND, &second_hdr, "second window");
        h.app.run("view.secondWindow", json!({"show": false})).unwrap();
        // Compare: the select (not being edited) keeps its HDR render through a drag of the candidate
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0].0, vis[1].0]}}), t);
        h.request("engine.execute", json!({"command": "view.compare"}), t);
        let select = vis[0];
        let select_view = |h: &Headless| (HDR_COMPARE..HDR_COMPARE + 2).find(|v| h.app.renderer.hdr_view.get(v).is_some_and(|x| x.photo == select));
        assert!(h.step_until(SETTLE, |h| select_view(h).is_some()), "the select's HDR tile");
        h.settle(SETTLE);
        let view = select_view(&h).unwrap();
        let (held, rendered) = (h.app.renderer.hdr_view[&view].key, count(view));
        h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
        h.app.run("develop.set", json!({"values": {"light.exposure": 3.5}})).unwrap();
        for _ in 0..10 {
            h.step();
            assert!(!h.app.renderer.is_pending(Slot::HdrView(view)), "the select's tile renders again in the drag");
        }
        h.app.run("develop.endInteraction", json!({})).unwrap();
        h.settle(SETTLE);
        assert_eq!((h.app.renderer.hdr_view[&view].key, count(view)), (held, rendered), "the select is not rendered again");
    }

    /// HDR display output touches the loupe only: the navigator / filmstrip thumbnail and the
    /// loupe's 8-bit render of an HDR edit are the same pixels with and without an HDR display
    /// (8-bit SDR rendition, drawn at SDR white), so nothing but the loupe's HDR image can go
    /// above SDR white.
    #[test]
    fn an_hdr_display_leaves_thumbnails_and_8_bit_renders_as_on_sdr() {
        use crate::render::Slot;
        struct Fake;
        impl crate::HdrOutput for Fake {
            fn headroom(&mut self) -> Option<f32> {
                Some(4.0)
            }
            fn upload(&mut self, _: u8, _: usize, _: usize, _: &[f32]) -> Result<egui::TextureId, String> {
                Ok(egui::TextureId::User(7))
            }
        }
        let t = Duration::from_secs(10);
        let mut seen = Vec::new();
        for hdr in [true, false] {
            let hdr_output: Option<Box<dyn crate::HdrOutput>> = if hdr { Some(Box::new(Fake)) } else { None };
            let services = crate::Services { png: None, hdr_output, ..Default::default() };
            let mut session = lcc_engine::Session::with_demo();
            // newest first, the newest photo open: written around it
            session.execute("library.sort", &json!({"ascending": false})).unwrap();
            session.selection = lcc_engine::Selection::single(session.visible_cloned()[0]);
            let mut app = LccApp::new(session, services);
            app.ui.view = crate::state::ViewMode::PhotoGrid;
            app.renderer.keep_pixels = true;
            let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
            h.request("ui.set", json!({"view": "detail"}), t);
            h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true}}), t);
            h.request("engine.execute", json!({"command": "develop.set", "params": {"values": {"light.exposure": 1.5}}}), t);
            let id = h.app.session.active().unwrap();
            let ready = |h: &Headless| {
                h.app.renderer.textures.get(&Slot::Thumb(id)).is_some_and(|t| t.pixels.is_some())
                    && h.app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.pixels.is_some() && t.photo == id)
                    && !h.app.renderer.is_pending(Slot::Main)
            };
            assert!(h.step_until(SETTLE, ready));
            h.settle(SETTLE);
            let px = |s: Slot| h.app.renderer.textures[&s].pixels.as_ref().unwrap().pixels.clone();
            seen.push((px(Slot::Thumb(id)), px(Slot::Main)));
        }
        // a thumbnail or render may come from the GPU in one session and the CPU in the other (whichever
        // is ready first); those agree within 2 levels, so allow that rounding and nothing more
        let close = |a: &[egui::Color32], b: &[egui::Color32]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_array().iter().zip(y.to_array()).all(|(p, q)| p.abs_diff(q) <= 2))
        };
        assert!(close(&seen[0].0, &seen[1].0), "thumbnail differs with an HDR display");
        assert!(close(&seen[0].1, &seen[1].1), "the loupe's 8-bit render differs with an HDR display");
    }

    /// Settings ▸ Display / `app.displayProfile`: previews are shown through the monitor profile
    /// (the loupe rendered for the display, thumbnails converted from sRGB, the histogram still
    /// sRGB); a file that can't be used is an error, or, as the setting, reported with previews
    /// left sRGB.
    #[test]
    fn display_profile_converts_previews() {
        use crate::render::Slot;
        let dir = lcc_testkit::TempDir::new("app-display-ui");
        std::fs::create_dir_all(&dir).unwrap();
        let p3 = dir.join("p3.icc");
        std::fs::write(&p3, lcc_codecs::icc::write_named(lcc_codecs::NamedSpace::DisplayP3)).unwrap();
        let bad = dir.join("bad.icc");
        std::fs::write(&bad, b"not a profile").unwrap();
        let (p3, bad) = (p3.to_string_lossy().to_string(), bad.to_string_lossy().to_string());

        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        let thumb_slot =
            |h: &Headless| h.app.renderer.textures.keys().copied().filter(|s| matches!(s, Slot::Thumb(_))).min_by_key(|s| format!("{s:?}"));
        let ready = |h: &Headless| {
            h.app.renderer.textures.get(&Slot::Main).is_some_and(|t| t.pixels.is_some())
                && thumb_slot(h).is_some()
                && !h.app.renderer.is_pending(Slot::Main)
        };
        assert!(h.step_until(SETTLE, ready));
        h.settle(SETTLE);
        let rgba =
            |c: &egui::ColorImage| lcc_raster::Rgba8 { width: c.size[0], height: c.size[1], data: c.pixels.iter().map(|p| p.to_array()).collect() };
        let tex = |h: &Headless, s: Slot| rgba(h.app.renderer.textures.get(&s).and_then(|t| t.pixels.clone()).as_deref().unwrap());
        let thumb = thumb_slot(&h).unwrap();
        let (main0, thumb0) = (tex(&h, Slot::Main), tex(&h, thumb));
        let hist0 = h.app.renderer.textures[&Slot::Main].histogram.clone().unwrap();

        // the command loads it (and keeps it as the setting); textures are made again
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": p3}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["result"]["kind"], "matrix");
        assert_eq!(h.app.ui.settings.display_profile, p3);
        let d = h.app.renderer.display().cloned().unwrap();
        assert!(h.step_until(SETTLE, |h| ready(h) && h.app.renderer.textures.contains_key(&thumb)));
        h.settle(SETTLE);
        let (main1, thumb1) = (tex(&h, Slot::Main), tex(&h, thumb));
        let mut want = thumb0.clone();
        d.profile.from_srgb(&mut want).unwrap();
        let max_diff = |a: &lcc_raster::Rgba8, b: &lcc_raster::Rgba8| {
            a.data.iter().zip(&b.data).flat_map(|(p, q)| (0..3).map(move |k| (p[k] as i32 - q[k] as i32).abs())).max().unwrap_or(0)
        };
        assert_eq!((thumb1.width, thumb1.height), (thumb0.width, thumb0.height));
        assert!(max_diff(&thumb1, &want) <= 1, "thumbnails are converted from sRGB: {}", max_diff(&thumb1, &want));
        assert_ne!(thumb1, thumb0);
        // the loupe: rendered for the display; colours sRGB holds look as they did
        assert_eq!((main1.width, main1.height), (main0.width, main0.height));
        assert_ne!(main1, main0);
        let mut main_want = main0.clone();
        d.profile.from_srgb(&mut main_want).unwrap();
        let mean = main1.data.iter().zip(&main_want.data).map(|(p, q)| (0..3).map(|k| (p[k] as f64 - q[k] as f64).abs()).sum::<f64>()).sum::<f64>()
            / (3 * main1.data.len()) as f64;
        assert!(mean < 1.5, "{mean}");
        let hist1 = h.app.renderer.textures[&Slot::Main].histogram.clone().unwrap();
        assert_eq!(hist1.total, hist0.total, "the histogram stays sRGB");

        // a bad file: an error, nothing changes
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": bad}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(h.app.renderer.display_id(), Some(d.id()));
        assert_eq!(h.app.ui.settings.display_profile, p3);
        // …chosen as the setting (Settings ▸ Display), it is reported and previews go back to sRGB
        h.app.ui.settings.display_profile = bad.clone();
        h.step();
        assert!(h.app.display_error.as_deref().is_some_and(|e| e.contains("not a valid ICC profile")), "{:?}", h.app.display_error);
        assert_eq!(h.app.renderer.display_id(), None);
        // the Display tab shows it
        let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "display"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        // none: sRGB again
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": null}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(r["result"]["path"].is_null());
        assert!(h.app.ui.settings.display_profile.is_empty() && h.app.display_error.is_none());
        assert!(h.step_until(SETTLE, |h| ready(h) && h.app.renderer.textures.contains_key(&thumb)));
        h.settle(SETTLE);
        assert_eq!(tex(&h, thumb), thumb0, "back to the sRGB thumbnail");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// With no profile chosen, previews follow the profile of the screen showing the window (the
    /// host's `screen_profile`: macOS doesn't colour-manage the window, issue #787), read again so
    /// a move to another screen is followed. A chosen profile wins; choosing none goes back to the
    /// screen's at once; a screen without a profile is sRGB.
    #[test]
    fn previews_follow_the_screen_profile_while_none_is_chosen() {
        use lcc_codecs::{NamedSpace, icc::write_named};
        use std::sync::{Arc, Mutex};
        let id = |space| lcc_engine::display::Display::from_bytes("", &write_named(space)).unwrap().id();
        let dir = lcc_testkit::TempDir::new("app-screen-profile");
        std::fs::create_dir_all(&dir).unwrap();
        let chosen = dir.join("prophoto.icc");
        std::fs::write(&chosen, write_named(NamedSpace::ProPhoto)).unwrap();
        let chosen = chosen.to_string_lossy().to_string();

        let screen = Arc::new(Mutex::new(Some(write_named(NamedSpace::DisplayP3))));
        let mut h = demo([1000.0, 700.0]);
        let read = screen.clone();
        h.app.services.screen_profile = Some(Box::new(move || read.lock().unwrap().clone()));
        let t = Duration::from_secs(10);
        h.step();
        assert_eq!(h.app.renderer.display_id(), Some(id(NamedSpace::DisplayP3)), "the screen's profile");
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {}}), t);
        assert_eq!((r["result"]["screen"].clone(), r["result"]["path"].clone()), (json!(true), Value::Null), "{r}");
        assert!(h.app.ui.settings.display_profile.is_empty(), "nothing is saved as the setting");

        // the window moves to another screen
        *screen.lock().unwrap() = Some(write_named(NamedSpace::AdobeRgb));
        assert!(h.step_until(SETTLE, |h| h.app.renderer.display_id() == Some(id(NamedSpace::AdobeRgb))));

        // a chosen profile wins over the screen's
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": chosen}}), t);
        assert_eq!(r["ok"], true, "{r}");
        *screen.lock().unwrap() = Some(write_named(NamedSpace::DisplayP3));
        for _ in 0..90 {
            h.step();
        }
        assert_eq!(h.app.renderer.display_id(), Some(id(NamedSpace::ProPhoto)));

        // none chosen again: the screen's profile, at once
        let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": null}}), t);
        assert_eq!(r["result"]["screen"], true, "{r}");
        assert_eq!(h.app.renderer.display_id(), Some(id(NamedSpace::DisplayP3)));

        // a screen without a profile: sRGB
        *screen.lock().unwrap() = None;
        assert!(h.step_until(SETTLE, |h| h.app.renderer.display_id().is_none()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #3252: Settings ▸ Performance ▸ Custom… opens a cache size of one's own, in GB; a custom size is shown as such, not as
    /// 2 GB. (The Performance tab is open.)
    fn custom_cache_size(h: &mut Headless, t: Duration) {
        let custom_field = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "drag:settingsCacheCustom");
        assert!(!custom_field(h));
        crate::settings_screen::reveal(h, "button:settingsCacheCustom");
        h.request("ui.clickWidget", json!({"id": "button:settingsCacheCustom"}), t);
        h.step();
        assert!(custom_field(h), "the size field");
        assert_eq!(h.app.session.cache_mb, 512, "nothing changed until a size is chosen");
        // typed in: nothing is saved before the edit is over (typing 16 must not set 1 GB on the way)
        crate::settings_screen::reveal(h, "drag:settingsCacheCustom");
        let r = h.request("ui.clickWidget", json!({"id": "drag:settingsCacheCustom"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.request("ui.key", json!({"key": "a", "cmd": true}), t);
        h.request("ui.text", json!({"text": "1"}), t);
        h.step();
        h.request("ui.text", json!({"text": "6"}), t);
        h.step();
        assert_eq!(h.app.session.cache_mb, 512, "still being typed");
        h.request("ui.key", json!({"key": "enter"}), t);
        h.step();
        h.step();
        assert_eq!(h.app.session.cache_mb, 16384, "saved when the edit is over");
        h.app.session.execute("library.preferences", &json!({"cacheMb": 16384})).unwrap();
        h.step();
        h.step();
        assert!(custom_field(h) && h.app.session.cache_mb == 16384, "16 GB stays, shown as custom");
        crate::settings_screen::reveal(h, "button:settingsCache-4");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-4"}), t);
        h.step();
        assert!(!custom_field(h) && h.app.session.cache_mb == 4096);
        crate::settings_screen::reveal(h, "button:settingsCache-1");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-1"}), t);
        assert_eq!(h.app.session.cache_mb, 512);
    }

    /// A custom size still being typed is dropped by a preset chosen meanwhile, and saved when the section is left. (The
    /// Performance tab is open, at 512 MB; it is again at the end.)
    fn custom_cache_size_pending(h: &mut Headless, t: Duration) {
        let type_size = |h: &mut Headless, text: &str| {
            crate::settings_screen::reveal(h, "button:settingsCacheCustom");
            h.request("ui.clickWidget", json!({"id": "button:settingsCacheCustom"}), t);
            h.step();
            crate::settings_screen::reveal(h, "drag:settingsCacheCustom");
            h.request("ui.clickWidget", json!({"id": "drag:settingsCacheCustom"}), t);
            h.step();
            h.request("ui.key", json!({"key": "a", "cmd": true}), t);
            h.request("ui.text", json!({"text": text}), t);
            h.step();
        };
        // 3 typed, then 1 GB chosen: Custom… again starts from 1 GB, and the 3 is not saved behind it
        type_size(h, "3");
        crate::settings_screen::reveal(h, "button:settingsCache-2");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-2"}), t);
        h.step();
        assert_eq!(h.app.session.cache_mb, 1024);
        crate::settings_screen::reveal(h, "button:settingsCacheCustom");
        h.request("ui.clickWidget", json!({"id": "button:settingsCacheCustom"}), t);
        for _ in 0..3 {
            h.step();
        }
        assert_eq!(h.app.session.cache_mb, 1024, "the preset stays");
        // 5 typed, then another section: saved on the way out
        type_size(h, "5");
        assert_eq!(h.app.session.cache_mb, 1024, "still being typed");
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-import"}), t);
        h.step();
        assert_eq!(h.app.session.cache_mb, 5120, "saved when the section is left");
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-performance"}), t);
        crate::settings_screen::reveal(h, "button:settingsCache-1");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-1"}), t);
        assert_eq!(h.app.session.cache_mb, 512);
    }

    /// Settings (⌘,): tabs switch, app settings change the UI state, library settings go through
    /// the engine; the delete confirmation guards ⌫.
    #[test]
    fn settings_dialog_by_keyboard_and_clicks() {
        let mut h = demo([1300.0, 820.0]);
        let t = Duration::from_secs(10);
        let r = h.request("ui.key", json!({"key": ",", "cmd": true}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "general".into() }));
        // (the old tabs' ids open the section that holds them: Performance is in Display & Speed)
        for tab in ["import", "performance", "display", "interface", "general"] {
            let r = h.request("ui.clickWidget", json!({"id": format!("button:settingsTab-{tab}")}), t);
            assert_eq!(r["ok"], true, "{tab}: {r}");
            let Some(crate::state::Dialog::Settings { tab: now }) = &h.app.ui.dialog else { panic!("{tab}: Settings closed") };
            assert_eq!(crate::settings_screen::section_of(now), crate::settings_screen::section_of(tab), "{tab}");
        }
        // General: confirm before delete (on by default), startup view
        assert!(h.app.ui.settings.confirm_delete);
        crate::settings_screen::reveal(&mut h, "check:settings.confirmDelete");
        h.request("ui.clickWidget", json!({"id": "check:settings.confirmDelete"}), t);
        assert!(!h.app.ui.settings.confirm_delete);
        crate::settings_screen::reveal(&mut h, "check:settings.confirmDelete");
        h.request("ui.clickWidget", json!({"id": "check:settings.confirmDelete"}), t);
        assert!(h.app.ui.settings.confirm_delete);
        crate::settings_screen::reveal(&mut h, "button:settingsStartup-2");
        h.request("ui.clickWidget", json!({"id": "button:settingsStartup-2"}), t);
        assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::SquareGrid);
        crate::settings_screen::reveal(&mut h, "button:settingsStartup-3");
        h.request("ui.clickWidget", json!({"id": "button:settingsStartup-3"}), t);
        assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Detail);
        // Interface: filmstrip names off, grid badges always
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-interface"}), t);
        crate::settings_screen::reveal(&mut h, "check:settings.filmNames");
        h.request("ui.clickWidget", json!({"id": "check:settings.filmNames"}), t);
        assert!(!h.app.ui.settings.film_names);
        crate::settings_screen::reveal(&mut h, "button:settingsGridBadges-1");
        h.request("ui.clickWidget", json!({"id": "button:settingsGridBadges-1"}), t);
        assert_eq!(h.app.ui.settings.grid_badges, crate::state::GridBadges::Always);
        // Performance: the thumbnail cache size goes to the library preferences
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-performance"}), t);
        crate::settings_screen::reveal(&mut h, "button:settingsCache-1");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-1"}), t);
        assert_eq!(h.app.session.cache_mb, 512);
        // Automatic: a share of the cache drive's free space
        crate::settings_screen::reveal(&mut h, "button:settingsCache-0");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-0"}), t);
        assert_eq!(h.app.session.cache_mb, 0);
        crate::settings_screen::reveal(&mut h, "button:settingsCache-1");
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-1"}), t);
        custom_cache_size(&mut h, t);
        custom_cache_size_pending(&mut h, t);
        crate::settings_screen::reveal(&mut h, "button:settingsPreview-3");
        h.request("ui.clickWidget", json!({"id": "button:settingsPreview-3"}), t);
        assert_eq!(h.app.ui.settings.preview_limit, 3840);
        crate::settings_screen::reveal(&mut h, "button:settingsMemory-2");
        h.request("ui.clickWidget", json!({"id": "button:settingsMemory-2"}), t);
        assert_eq!(h.app.ui.settings.memory_mb, 1024);
        h.step();
        assert_eq!(lcc_engine::memory::budget(), 1024 << 20, "applied through app.memoryBudget");
        crate::settings_screen::reveal(&mut h, "button:settingsMemory-0");
        h.request("ui.clickWidget", json!({"id": "button:settingsMemory-0"}), t);
        h.step();
        assert_eq!(lcc_engine::memory::budget(), lcc_engine::memory::default_budget(), "back to automatic");
        // Import: per-camera defaults
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-import"}), t);
        crate::settings_screen::reveal(&mut h, "check:settings.perCamera");
        h.request("ui.clickWidget", json!({"id": "check:settings.perCamera"}), t);
        assert!(h.app.session.import_defaults.per_camera);
        let img = h.snapshot(SETTLE);
        assert_eq!(img.size, [1300, 820]);
        // Escape closes; ⌫ now asks first
        h.request("ui.key", json!({"key": "escape"}), t);
        assert_eq!(h.app.ui.dialog, None);
        let active = h.app.session.active().unwrap();
        h.request("ui.key", json!({"key": "delete"}), t);
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ConfirmDelete { count: 1, disk_files: 0 }));
        assert!(!h.app.session.catalog.photo(active).unwrap().deleted);
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(h.app.session.catalog.photo(active).unwrap().deleted);
        // app settings survive a save/load round trip of the UI state (ui.json)
        let saved = serde_json::to_string(&h.app.ui).unwrap();
        let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
        assert_eq!(back.settings, h.app.ui.settings);
        assert_eq!(back.view, crate::state::ViewMode::Detail, "startup view applied on load");
        h.settle(SETTLE);
    }

    /// F: full-screen preview (arrows step, I cycles the info overlay, Esc exits); ⌘I cycles the
    /// overlay in Detail; ⇧⌘F asks the host for window full screen.
    #[test]
    fn full_screen_preview_covers_the_screen_and_f11_toggles_full_screen() {
        // issue #2: F must take the window full screen (over the taskbar) and give it back after
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0]]}}), t);
        assert!(!h.app.window_is_fullscreen);
        h.request("ui.key", json!({"key": "f"}), t);
        h.settle(SETTLE);
        assert!(h.app.ui.fullscreen);
        assert!(h.app.window_is_fullscreen, "the preview takes the window full screen");
        h.request("ui.key", json!({"key": "escape"}), t);
        h.settle(SETTLE);
        assert!(!h.app.ui.fullscreen);
        assert!(!h.app.window_is_fullscreen, "leaving the preview gives the window back");
        // already full screen: the preview leaves it full screen when it ends
        h.request("ui.key", json!({"key": "F11"}), t);
        h.settle(SETTLE);
        assert!(h.app.window_is_fullscreen, "F11 enters full screen");
        h.request("ui.key", json!({"key": "f"}), t);
        h.request("ui.key", json!({"key": "f"}), t);
        h.settle(SETTLE);
        assert!(h.app.window_is_fullscreen);
        h.request("ui.key", json!({"key": "F11"}), t);
        h.settle(SETTLE);
        assert!(!h.app.window_is_fullscreen, "F11 leaves full screen");
        // in the preview: arrows step, I cycles the info overlay, Esc exits; ⌘I cycles the overlay in Detail
        use crate::state::InfoOverlay;
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [vis[0]]}}), t);
        h.request("ui.key", json!({"key": "f"}), t);
        assert!(h.app.ui.fullscreen);
        h.request("ui.key", json!({"key": "right"}), t);
        assert_eq!(h.app.session.active().map(|p| p.0), Some(vis[1]));
        let right = h.app.ui.right;
        h.request("ui.key", json!({"key": "i"}), t);
        assert_eq!(h.app.ui.info_overlay, InfoOverlay::Basic);
        assert_eq!(h.app.ui.right, right, "I doesn't open the Info panel in full screen");
        let img = h.snapshot(SETTLE);
        // no chrome: the frame's corners are black
        for (x, y) in [(2usize, 2usize), (997, 2), (2, 697), (997, 697)] {
            let c = img.pixels[y * 1000 + x];
            assert!(c.r() < 8 && c.g() < 8 && c.b() < 8, "({x},{y}) = {c:?}");
        }
        assert!(h.app.widgets.iter().any(|(w, _)| w == "canvas:infoOverlay"));
        h.request("ui.key", json!({"key": "escape"}), t);
        assert!(!h.app.ui.fullscreen);
        assert_eq!(h.app.ui.view, crate::state::ViewMode::PhotoGrid, "Esc leaves full screen only, back to where it was entered");
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("ui.key", json!({"key": "i", "cmd": true}), t);
        assert_eq!(h.app.ui.info_overlay, InfoOverlay::Exposure);
        h.request("ui.key", json!({"key": "i", "cmd": true}), t);
        assert_eq!(h.app.ui.info_overlay, InfoOverlay::Off);
        h.settle(SETTLE);
    }

    /// #911 (toast labels only): cycling the overlay announces each mode in the UI language.
    #[test]
    fn info_overlay_toasts_are_painted_in_the_users_language() {
        use crate::i18n::Locale;

        struct RestoreLanguage(Locale);
        impl Drop for RestoreLanguage {
            fn drop(&mut self) {
                crate::i18n::set_language(self.0);
            }
        }
        let _restore = RestoreLanguage(crate::i18n::language());
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        assert_eq!(h.request("ui.set", json!({"view": "detail", "infoOverlay": "off"}), t)["ok"], true);
        for (command, locale) in [("app.language.german", Locale::De), ("app.language.french", Locale::Fr), ("app.language.english", Locale::En)] {
            assert_eq!(h.request("engine.execute", json!({"command": command}), t)["ok"], true);
            assert_eq!(h.app.ui.language, locale);
            assert_eq!(crate::i18n::language(), locale);
            for (mode, key) in [("basic", "Info Overlay: File"), ("exposure", "Info Overlay: Exposure"), ("off", "Info Overlay Off")] {
                let expected = locale.tr(key);
                assert_eq!(h.request("ui.key", json!({"key": "i", "cmd": true}), t)["ok"], true);
                assert_eq!(serde_json::to_value(h.app.ui.info_overlay).unwrap(), json!(mode));
                assert_eq!(h.app.ui.toast().expect("overlay toast").0, expected);
                h.step();
                let painted = h.painted_text();
                assert!(painted.iter().any(|text| text == expected), "{painted:?}");
            }
        }
    }

    /// ⌘J opens View ▸ View Options; its menus choose Info 1's and Info 2's lines, Use Defaults puts
    /// them back, and `view.viewOptions` sets them for an agent (refusing a line it doesn't know).
    #[test]
    fn view_options_choose_the_info_overlay_lines() {
        use crate::state::{LoupeInfo, LoupeInfoField as F};
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        assert_eq!(h.request("ui.key", json!({"key": "j", "cmd": true}), t)["ok"], true);
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ViewOptions));
        h.step();
        assert_eq!(h.request("ui.clickWidget", json!({"id": "combo:viewOptionsInfo1Line2"}), t)["ok"], true);
        h.step();
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:viewOptionsInfo1Line2-lens"}), t)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.settings.loupe_info.info1, [F::FileName, F::Lens, F::Dimensions]);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "button:viewOptionsDefaults1"}), t)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.settings.loupe_info.info1, LoupeInfo::INFO1);

        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"info2": ["camera", "none", "fileName"]}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.settings.loupe_info.info2, [F::Camera, F::None, F::FileName]);
        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"info2": ["rating", "none", "none"]}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(h.app.ui.settings.loupe_info.info2, [F::Camera, F::None, F::FileName], "a refused change changes nothing");
    }

    /// View Options' Show Briefly When Photo Changes: a tick box under each Info (ticking one
    /// unticks the other) and `view.viewOptions {briefly}`; with the overlay off, the loupe shows
    /// the chosen Info after → changes the photo, and hides it again a few seconds later.
    #[test]
    fn show_briefly_when_photo_changes_in_view_options() {
        use crate::state::{BRIEF_INFO_SECS, InfoOverlay};
        let mut h = demo([1400.0, 1100.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        assert_eq!(h.request("ui.key", json!({"key": "j", "cmd": true}), t)["ok"], true);
        h.step();
        h.settle(SETTLE); // the dialog opens over a few frames
        assert_eq!(h.request("ui.clickWidget", json!({"id": "check:viewOptions.briefly2"}), t)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.settings.loupe_info.briefly, InfoOverlay::Exposure);
        assert_eq!(h.request("ui.clickWidget", json!({"id": "check:viewOptions.briefly1"}), t)["ok"], true);
        h.step();
        assert_eq!(h.app.ui.settings.loupe_info.briefly, InfoOverlay::Basic, "one Info at most");
        h.app.ui.dialog = None;
        h.step();

        let overlay = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "canvas:infoOverlay");
        assert_eq!(h.app.ui.info_overlay, InfoOverlay::Off);
        h.time += BRIEF_INFO_SECS + 1.0;
        h.step();
        h.step();
        assert!(!overlay(&h), "hidden once the time has passed");
        let before = h.app.session.active();
        h.request("ui.key", json!({"key": "right"}), t);
        assert_ne!(h.app.session.active(), before);
        h.step();
        assert!(overlay(&h), "shown after → changes the photo");
        h.time += BRIEF_INFO_SECS + 1.0;
        h.step();
        h.step();
        assert!(!overlay(&h), "and hidden again");

        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"briefly": "off"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["result"]["loupeInfo"]["briefly"], "off", "{r}");
        h.request("ui.key", json!({"key": "right"}), t);
        h.step();
        assert!(!overlay(&h), "off: nothing shows when the photo changes");
        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"briefly": "info3"}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert_eq!(h.app.ui.settings.loupe_info.briefly, InfoOverlay::Off, "a refused change changes nothing");
    }

    /// With Reduce motion on, a click-zoom lands at once: the animation flag clears on the next frame.
    #[test]
    fn reduce_motion_zooms_at_once() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        h.app.ui.settings.reduce_motion = true;
        h.step();
        assert!(crate::motion::reduced(&h.view.ctx));
        h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), t);
        h.step();
        h.step();
        assert!(!h.app.ui.zoom_anim, "no glide with Reduce motion");
    }

    /// A click on the image zooms to the chosen click-zoom ratio (1:1 by default), animating the
    /// loupe while rendering once at the final size; Z uses the same ratio; a second click returns to Fit.
    #[test]
    fn click_zoom_ratio_animates() {
        use crate::render::Slot;
        use crate::state::Zoom;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.ui.click_zoom, 100, "1:1 by default");
        let r = h.request("engine.execute", json!({"command": "view.clickZoom", "params": {"ratio": 3}}), t);
        assert_eq!(r["result"]["ratio"], 3, "{r}");
        let r = h.request("engine.execute", json!({"command": "view.clickZoom", "params": {"ratio": 5}}), t);
        assert_ne!(r["ok"], true, "{r}");
        let fit = h.app.image_rect.unwrap();
        h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Percent(300.0));
        assert!(h.app.ui.zoom_anim, "animation started");
        // every animation frame asks for the same (final-size) render
        let mut keys = std::collections::HashSet::new();
        for _ in 0..40 {
            h.step();
            keys.extend(h.app.renderer.wanted(Slot::Main));
            if !h.app.ui.zoom_anim {
                break;
            }
        }
        assert_eq!(keys.len(), 1, "one render for the whole animation: {keys:?}");
        h.settle(SETTLE);
        assert!(!h.app.ui.zoom_anim, "animation finished");
        assert!(h.app.image_rect.unwrap().width() > fit.width());
        h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Fit);
        // Z zooms to the same ratio as a click
        h.request("ui.key", json!({"key": "z"}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Percent(300.0));
        h.request("ui.key", json!({"key": "z"}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Fit);
        h.settle(SETTLE);
    }

    /// A hover preview gets the loupe's display fit: it is the size the photo is drawn at, as the
    /// loupe render is, not a preview-level render the GPU shrinks.
    #[test]
    fn a_hover_preview_is_fitted_to_the_drawn_size() {
        use crate::render::Slot;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.profiles"}), t);
        h.settle(SETTLE);
        h.step_until(SETTLE, |h| !h.app.renderer.is_pending(Slot::Main));
        let r = h.request("ui.hoverWidget", json!({"id": "profileCell:lc.vivid"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step_until(SETTLE, |h| h.app.loupe_shown.map(|l| l.1) == Some("hover"));
        let size = |s: Slot| h.app.renderer.textures.get(&s).map(|t| t.size);
        let (main, hover) = (size(Slot::Main).expect("loupe render"), size(Slot::Hover).expect("hover render"));
        assert!(main[0].max(main[1]) < lcc_engine::SourceLevel::Preview.max_edge(), "the loupe render is fitted: {main:?}");
        assert!(hover[0].abs_diff(main[0]) <= 1 && hover[1].abs_diff(main[1]) <= 1, "hover {hover:?} vs loupe {main:?}");
    }

    /// Zoomed in under a window render, the hidden whole-frame render stays canvas-sized instead of
    /// rising to the preview level.
    #[test]
    fn a_zoomed_view_keeps_its_whole_frame_render_canvas_sized() {
        use crate::render::Slot;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        h.request("ui.clickWidget", json!({"id": "canvas:image", "fx": 0.5, "fy": 0.5}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.ui.zoom, crate::state::Zoom::Percent(100.0));
        h.step_until(SETTLE, |h| !h.app.renderer.is_pending(Slot::Main));
        let main = h.app.renderer.textures.get(&Slot::Main).expect("whole-frame render").size;
        assert!(main[0].max(main[1]) < lcc_engine::SourceLevel::Preview.max_edge(), "{main:?}");
    }

    #[test]
    fn trackpad_navigation_keeps_the_image_cursor_between_events() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), t);
        h.request("engine.execute", json!({"command": "view.zoom100"}), t);
        h.settle(SETTLE);
        let anchor = h.app.canvas_rect.unwrap().center();
        h.request("ui.move", json!({"x": anchor.x, "y": anchor.y}), t);
        for (tool, expected) in [
            ("", egui::CursorIcon::Grab),
            // the colour pickers hide the system pointer: the eyedropper is painted instead
            ("wbPicker", egui::CursorIcon::None),
            // (Color Range is a mask tool: outside Masking it is put away, so it has no cursor to keep here)
            ("pointColor", egui::CursorIcon::None),
            ("tat:curve", egui::CursorIcon::ResizeVertical),
        ] {
            h.app.ui.tool = tool.into();
            for event in [
                None,
                Some(egui::Event::Zoom(1.01)),
                None,
                Some(egui::Event::Zoom(1.01)),
                Some(egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(10.0, -10.0),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                }),
                None,
            ] {
                let raw = HeadlessView::raw_input(h.size, h.pixels_per_point, h.time, event.into_iter().collect());
                let mut cursor = egui::CursorIcon::Default;
                h.view.run(raw, |ui| {
                    h.app.logic(ui.ctx());
                    h.app.ui(ui);
                    cursor = ui.ctx().output(|output| output.cursor_icon);
                });
                h.time += FRAME_DT;
                assert_eq!(cursor, expected, "cursor changed between navigation events with tool {tool:?}");
            }
        }
    }

    #[test]
    fn trackpad_pinch_anchors_fractional_zoom_and_scroll_pans() {
        use crate::state::Zoom;
        let mut h = demo([1000.0, 700.0]);
        h.pixels_per_point = 2.0;
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none", "leftPanel": false, "filmstrip": false}), t);
        h.request("engine.execute", json!({"command": "view.zoom100"}), t);
        h.settle(SETTLE);
        let before = h.app.image_rect.unwrap();
        let anchor = h.app.canvas_rect.unwrap().center() + egui::vec2(30.0, -20.0);
        h.request("ui.move", json!({"x": anchor.x, "y": anchor.y}), t);
        let point = (anchor - before.min) / before.size();
        h.request("ui.zoom", json!({"factor": 1.001}), t);
        let after = h.app.image_rect.unwrap();
        assert!((after.width() / before.width() - 1.001).abs() < 0.00001);
        assert!(((anchor - after.min) / after.size() - point).length() < 0.00001, "point under pointer moved");
        assert!(matches!(h.app.ui.zoom, Zoom::Percent(p) if (p - 100.1).abs() < 0.001), "{:?}", h.app.ui.zoom);
        assert!(!h.app.ui.zoom_anim, "pinch follows the fingers immediately");

        h.request("ui.scroll", json!({"dx": 40.0, "dy": -30.0}), t);
        for _ in 0..40 {
            h.step();
        }
        let panned = h.app.image_rect.unwrap();
        assert!(panned.left() > after.left() && panned.top() < after.top(), "two-finger pan must move both axes");
        h.request("ui.scroll", json!({"dx": 100000.0, "dy": -100000.0}), t);
        for _ in 0..40 {
            h.step();
        }
        let edge = h.app.image_rect.unwrap();
        let area = h.app.canvas_rect.unwrap().shrink(24.0);
        assert!((edge.left() - area.left()).abs() < 0.01 && (edge.bottom() - area.bottom()).abs() < 0.01, "{edge:?} vs {area:?}");

        h.request("ui.zoom", json!({"factor": 1e20}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Percent(crate::state::MAX_ZOOM));
        h.request("ui.zoom", json!({"factor": 0.000001}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Fit);
        assert_eq!(h.app.ui.pan, (0.5, 0.5));
        // Panel / toolbar gestures must not move the image.
        h.request("ui.move", json!({"x": 10, "y": 10}), t);
        h.request("ui.zoom", json!({"factor": 2}), t);
        h.request("ui.scroll", json!({"dx": 100, "dy": 100}), t);
        assert_eq!(h.app.ui.zoom, Zoom::Fit);
        assert_eq!(h.app.ui.pan, (0.5, 0.5));
    }

    #[test]
    fn mouse_wheel_zooms_the_loupe_with_and_without_ctrl() {
        use crate::state::Zoom;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none", "leftPanel": false, "filmstrip": false}), t);
        h.settle(SETTLE);
        for (unit, y, modifiers) in [
            (egui::MouseWheelUnit::Line, 1.0, egui::Modifiers::NONE),
            (egui::MouseWheelUnit::Line, 1.0, egui::Modifiers::CTRL),
            (egui::MouseWheelUnit::Point, 50.0, egui::Modifiers::CTRL),
        ] {
            h.request("ui.set", json!({"zoom": "fit", "pan": [0.5, 0.5]}), t);
            h.request("ui.hoverWidget", json!({"id": "canvas:image"}), t);
            h.app.synthetic.push(egui::Event::MouseWheel { unit, delta: egui::vec2(0.0, y), phase: egui::TouchPhase::Move, modifiers });
            for _ in 0..10 {
                h.step();
            }
            assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "wheel {unit:?} {modifiers:?} must zoom in, got {:?}", h.app.ui.zoom);
        }
    }

    #[test]
    fn trackpad_navigation_works_in_tools_compare_reference_and_before_after() {
        use crate::state::Zoom;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "leftPanel": false, "filmstrip": false}), t);
        h.settle(SETTLE);
        let active = h.app.session.active().unwrap();
        let settings = h.app.session.catalog.photo(active).unwrap().develop.clone();
        for right in ["crop", "masking", "remove", "redEye", "none"] {
            h.request("ui.set", json!({"right": right, "zoom": "fit", "pan": [0.5, 0.5]}), t);
            h.request("ui.hoverWidget", json!({"id": "canvas:image"}), t);
            h.request("ui.zoom", json!({"factor": 2}), t);
            assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "pinch in {right}");
            assert_eq!(h.app.session.catalog.photo(active).unwrap().develop, settings, "navigation must not edit the photo");
        }
        h.request("ui.set", json!({"fullscreen": true, "zoom": "fit"}), t);
        h.request("ui.hoverWidget", json!({"id": "canvas:image"}), t);
        h.request("ui.zoom", json!({"factor": 2}), t);
        assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "full-screen pinch");

        h.request("ui.set", json!({"fullscreen": false, "zoom": "fit", "beforeAfter": "sideBySide"}), t);
        let canvas = h.app.canvas_rect.unwrap();
        h.request("ui.move", json!({"x": canvas.left() + canvas.width() * 0.25, "y": canvas.center().y}), t);
        h.request("ui.zoom", json!({"factor": 2}), t);
        assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "pinch over the Before pane");

        h.request("ui.set", json!({"beforeAfter": "off", "zoom": "fit"}), t);
        h.request("engine.execute", json!({"command": "view.compare"}), t);
        // The left Compare pane must navigate too; zoom/pan are shared with the right pane.
        let canvas = h.app.canvas_rect.unwrap();
        h.request("ui.move", json!({"x": canvas.left() + canvas.width() * 0.25, "y": canvas.center().y}), t);
        h.request("ui.zoom", json!({"factor": 2}), t);
        assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "Compare pinch");
        let pan = h.app.ui.pan;
        h.request("ui.scroll", json!({"dx": -50, "dy": -50}), t);
        assert_ne!(h.app.ui.pan, pan, "Compare pan");

        h.request("ui.set", json!({"zoom": "fit", "pan": [0.5, 0.5]}), t);
        h.request("engine.execute", json!({"command": "view.reference"}), t);
        h.request("ui.move", json!({"x": canvas.left() + canvas.width() * 0.25, "y": canvas.center().y}), t);
        h.request("ui.zoom", json!({"factor": 2}), t);
        assert!(matches!(h.app.ui.zoom, Zoom::Percent(_)), "Reference pinch");
    }

    #[test]
    fn navigation_rejects_invalid_input_and_preserves_old_zoom_state() {
        use crate::state::Zoom;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        assert_eq!(serde_json::from_value::<Zoom>(json!({"percent": 100})).unwrap(), Zoom::Percent(100.0));
        for factor in [json!(0), json!(-1), json!(1e308), json!("big"), Value::Null] {
            let r = h.request("ui.zoom", json!({"factor": factor}), t);
            assert_eq!(r["ok"], false, "{r}");
        }
        for params in [
            json!({"zoom": {"percent": -1}}),
            json!({"zoom": {"percent": 1e308}}),
            json!({"zoom": "oops"}),
            json!({"zoom": {"percent": 123.4}, "pan": [-1, 0.5]}),
            json!({"pan": [0.5, 1e308]}),
            json!({"pan": [0.5]}),
        ] {
            assert!(h.app.run("view.navigate", params).is_err());
            assert_eq!(h.app.ui.zoom, Zoom::Fit, "failed request changed zoom");
            assert_eq!(h.app.ui.pan, (0.5, 0.5), "failed request changed pan");
        }
        h.app.run("view.navigate", json!({"zoom": {"percent": 123.4}, "pan": [0.4, 0.6]})).unwrap();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(123.4));
        assert_eq!(h.app.ui.pan, (0.4, 0.6));
        h.app.run("view.zoomIn", json!({})).unwrap();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(200.0));
        h.app.run("view.zoomOut", json!({})).unwrap();
        assert_eq!(h.app.ui.zoom, Zoom::Percent(100.0));

        let area = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(1000.0, 700.0));
        for pan in [(0.0, 0.0), (1.0, 1.0)] {
            let image = crate::panels::detail::fit_rect(area, 10.0, Zoom::Percent(100.0), [4000, 400], 1.0, pan);
            assert_eq!(image.center().y, area.center().y, "smaller axis must stay centred");
            assert!(image.left() <= area.left() && image.right() >= area.right(), "pan exposed space outside the photo");
        }
    }

    /// The Navigator appears when zoomed in; clicking it pans to that point.
    #[test]
    fn navigator_pans_the_zoomed_loupe() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert!(!h.app.widgets.iter().any(|(w, _)| w == "canvas:navigator"), "hidden at Fit");
        h.request("engine.execute", json!({"command": "view.zoom100"}), t);
        h.settle(SETTLE);
        assert!(h.app.widgets.iter().any(|(w, _)| w == "canvas:navigator"), "navigator shown when zoomed");
        h.request("ui.clickWidget", json!({"id": "canvas:navigator", "fx": 0.1, "fy": 0.2}), t);
        let (u, v) = h.app.ui.pan;
        assert!((u - 0.1).abs() < 0.03 && (v - 0.2).abs() < 0.03, "pan {:?}", h.app.ui.pan);
        assert_eq!(h.app.ui.zoom, crate::state::Zoom::Percent(100.0), "the click didn't reach the loupe (which would zoom out)");
        h.settle(SETTLE);
        let nav = h.app.widgets.iter().rev().find(|(w, _)| w == "canvas:navigator").map(|(_, r)| *r).unwrap();
        let to = nav.min + egui::vec2(nav.width() * 0.8, nav.height() * 0.7);
        h.request("ui.dragWidget", json!({"id": "canvas:navigator", "fx": 0.5, "fy": 0.5, "toX": to.x, "toY": to.y}), t);
        let (u, v) = h.app.ui.pan;
        assert!((u - 0.8).abs() < 0.03 && (v - 0.7).abs() < 0.03, "drag pan {:?}", h.app.ui.pan);
        h.request("engine.execute", json!({"command": "view.navigator"}), t);
        h.settle(SETTLE);
        assert!(!h.app.widgets.iter().any(|(w, _)| w == "canvas:navigator"), "toggled off");
    }

    /// While an edit renders, the loupe shows the last render: it says "Rendering…" once that has
    /// lasted a moment (not at once after a spell away from the loupe, when no frames were drawn),
    /// and stops when the render lands.
    #[test]
    fn the_loupe_says_rendering_while_an_edit_renders() {
        use crate::render::Slot;
        use std::{cell::Cell, rc::Rc};
        let pill = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "loupe:rendering");
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert!(!pill(&h), "a finished render");
        let open = Rc::new(Cell::new(false));
        h.app.renderer.set_offload(Box::new(super::RenderGate::new(open.clone())));
        // half a second in the grid: the loupe draws no frames
        h.request("ui.set", json!({"view": "photoGrid"}), t);
        for _ in 0..30 {
            h.step();
        }
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        h.request("ui.set", json!({"view": "detail"}), t);
        for _ in 0..12 {
            h.step();
        }
        assert!(h.app.renderer.is_pending(Slot::Main), "the edit's render is held");
        assert!(!pill(&h), "not at once");
        for _ in 0..30 {
            h.step();
        }
        assert!(pill(&h), "the last render is on screen and the edit's is on its way");
        open.set(true);
        assert!(h.step_until(SETTLE, |h| !pill(h)), "gone once the render lands");
        assert!(!h.app.renderer.is_pending(Slot::Main));
    }

    /// View Options' Show message when loading or rendering photos (on by default): turned off in
    /// the window, the loupe does not say "Rendering…" for the first moments of an edit's render (only one that stays slow says so: see the next test); `view.viewOptions
    /// {loupeMessages}` turns it back on for an agent (refusing a value that is not true or false).
    #[test]
    fn view_options_turn_the_rendering_message_off() {
        use crate::render::Slot;
        use std::{cell::Cell, rc::Rc};
        let pill = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "loupe:rendering");
        let mut h = demo([1400.0, 1100.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert!(h.app.ui.settings.loupe_messages, "on by default");
        assert_eq!(h.request("ui.key", json!({"key": "j", "cmd": true}), t)["ok"], true);
        h.settle(SETTLE); // the dialog opens over a few frames
        assert_eq!(h.request("ui.clickWidget", json!({"id": "check:viewOptions.loupeMessages"}), t)["ok"], true);
        h.step();
        assert!(!h.app.ui.settings.loupe_messages, "the tick box turns it off");
        h.app.ui.dialog = None;
        let open = Rc::new(Cell::new(false));
        h.app.renderer.set_offload(Box::new(super::RenderGate::new(open.clone())));
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        for _ in 0..45 {
            h.step();
        }
        assert!(h.app.renderer.is_pending(Slot::Main), "the edit's render is held");
        assert!(!pill(&h), "no message while it is off");

        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"loupeMessages": "yes"}}), t);
        assert_eq!(r["ok"], false, "{r}");
        assert!(!h.app.ui.settings.loupe_messages, "a refused change changes nothing");
        let r = h.request("engine.execute", json!({"command": "view.viewOptions", "params": {"loupeMessages": true}}), t);
        assert_eq!(r["result"]["loupeMessages"], true, "{r}");
        assert_eq!(h.app.ui.dialog, None, "a value sets it without opening the window");
        assert!(h.step_until(SETTLE, pill), "the message shows again while the render is held");
        open.set(true);
        assert!(h.step_until(SETTLE, |h| !pill(h)), "gone once the render lands");
    }

    /// With the message off, a render that is still on its way after a long wait (several seconds, not the
    /// half second the message waits) says "Rendering…" anyway: the old photo on screen with no sign of work
    /// looks like a frozen app. A short wait stays quiet, as the setting asks.
    #[test]
    fn a_render_that_stays_slow_says_so_even_with_the_message_off() {
        use crate::render::Slot;
        use std::{cell::Cell, rc::Rc};
        let pill = |h: &Headless| h.app.widgets.iter().any(|(w, _)| w == "loupe:rendering");
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        h.app.ui.settings.loupe_messages = false;
        let open = Rc::new(Cell::new(false));
        h.app.renderer.set_offload(Box::new(super::RenderGate::new(open.clone())));
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        for _ in 0..45 {
            h.step();
        }
        assert!(h.app.renderer.is_pending(Slot::Main), "the edit's render is held");
        assert!(!pill(&h), "a short wait stays quiet with the message off");
        assert!(h.step_until(SETTLE, pill), "a long wait says so");
        open.set(true);
        assert!(h.step_until(SETTLE, |h| !pill(h)), "gone once the render lands");
    }

    /// With nothing to show yet, the loupe says "Rendering…" in its middle only while View Options'
    /// Show message when loading or rendering photos is on.
    #[test]
    fn an_empty_loupe_says_rendering_only_with_the_message_on() {
        use std::{cell::Cell, rc::Rc};
        for on in [true, false] {
            let mut h = demo([1000.0, 700.0]);
            let t = Duration::from_secs(10);
            h.app.ui.settings.loupe_messages = on;
            h.app.renderer.set_offload(Box::new(super::RenderGate::new(Rc::new(Cell::new(false)))));
            h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
            // drop every texture (quick previews and thumbnails land outside the gate) until a
            // frame has nothing to show
            let empty = (0..60).any(|_| {
                h.app.renderer.textures.clear();
                h.step();
                h.app.loupe_shown.map(|l| l.1) == Some("none")
            });
            assert!(empty, "a frame with nothing rendered");
            let said = h.painted_text().iter().any(|text| text == "Rendering…");
            assert_eq!(said, on, "message on: {on}");
        }
    }

    /// A render that fails while the loupe shows a stand-in (the embedded JPEG, a thumbnail) left
    /// the stand-in up as if it were the photo: the loupe says the photo can't be opened, with the
    /// error on hover, and keeps saying so while a library change retries it.
    #[test]
    fn the_loupe_says_so_when_the_render_behind_a_stand_in_fails() {
        use crate::render::Slot;
        use std::{cell::Cell, rc::Rc};
        let has = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
        h.settle(SETTLE);
        assert!(!has(&h, "loupe:renderFailed"));
        let first = h.app.session.active().expect("a photo is open").0;
        // every loupe render fails, as an undecodable raw's would; the rest render
        let open = Rc::new(Cell::new(true));
        h.app.renderer.set_offload(Box::new(super::RenderGate::failing(open.clone(), |s| s == Slot::Main)));
        // (not a neighbour of the open photo: the loupe would take its prepared render)
        let other = h.app.session.visible_cloned().get(3).copied().expect("a fourth photo").0;
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [other], "active": other}}), t);
        assert!(h.step_until(SETTLE, |h| has(h, "loupe:renderFailed")), "the failure is said");
        let shown = h.app.loupe_shown.map(|l| l.1);
        assert!(!matches!(shown, Some("render" | "none")), "over a stand-in: {shown:?}");
        assert!(!has(&h, "loupe:rendering"));
        let r = h.request("ui.inspect", json!({}), t);
        assert_eq!(r["result"]["loupe"]["failed"], "no decoder for this raw", "{}", r["result"]["loupe"]);
        // a library change (here a rating) retries it: the failure stays said, no "Rendering…"
        open.set(false);
        h.request("engine.execute", json!({"command": "photo.rate", "params": {"rating": 2}}), t);
        for _ in 0..45 {
            h.step();
        }
        assert!(h.app.renderer.is_pending(Slot::Main), "the retry is held");
        assert!(has(&h, "loupe:renderFailed") && !has(&h, "loupe:rendering"), "no blink while it retries");
        // back on a photo that rendered: nothing to say
        open.set(true);
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [first], "active": first}}), t);
        h.app.renderer.set_offload(Box::new(super::RenderGate::new(open.clone())));
        assert!(h.step_until(SETTLE, |h| h.app.loupe_shown.is_some_and(|l| l.1 == "render") && !has(h, "loupe:renderFailed")));
    }

    #[test]
    fn screenshot_request_is_answered_without_a_window() {
        let mut h = demo([800.0, 500.0]);
        let r = h.request("ui.screenshot", json!({}), SETTLE);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(r["result"]["width"], 800);
        assert_eq!(r["result"]["height"], 500);
        let r = h.request("ui.resize", json!({"width": 640, "height": 400}), Duration::from_secs(5));
        assert_eq!(r["ok"], true);
        let r = h.request("ui.screenshot", json!({"headless": true}), SETTLE);
        assert_eq!(r["result"]["width"], 640, "{r}");
    }

    /// A size typed in the Export dialog is the size exported: the pixel fields kept only multiples
    /// of 16, so 1080 became 1088 and 3000 became 3008.
    #[test]
    fn export_sizes_keep_the_pixels_typed() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        h.settle(SETTLE);
        let type_value = |h: &mut Headless, id: &str, text: &str| {
            let r = h.request("ui.clickWidget", json!({"id": id}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
            h.request("ui.text", json!({"text": text}), t);
            h.request("ui.key", json!({"key": "Enter"}), t);
            h.settle(SETTLE);
        };
        let resize = |h: &Headless| match &h.app.ui.dialog {
            Some(crate::state::Dialog::Export { resize, .. }) => *resize,
            _ => panic!("the dialog closed"),
        };
        if let Some(crate::state::Dialog::Export { full_size, resize, .. }) = &mut h.app.ui.dialog {
            *full_size = false;
            *resize = lcc_engine::export::Resize::long_edge(2048);
        }
        h.settle(SETTLE);
        type_value(&mut h, "sliderValue:export.sizePx", "3000");
        assert_eq!(resize(&h).value, 3000.0);
        if let Some(crate::state::Dialog::Export { resize, .. }) = &mut h.app.ui.dialog {
            resize.mode = lcc_engine::export::ResizeMode::WidthHeight;
            resize.height = 2048;
        }
        h.settle(SETTLE);
        type_value(&mut h, "sliderValue:export.sizeW", "1080");
        type_value(&mut h, "sliderValue:export.sizeH", "1350");
        assert_eq!((resize(&h).value, resize(&h).height), (1080.0, 1350));
    }
}
