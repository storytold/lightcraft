//! Headless UI rendering: run the app's UI in an offscreen [`egui::Context`] and rasterize it on
//! the CPU ([`crate::softpaint`]) — no window, no GPU, no compositor.
//!
//! Two users:
//! - [`Headless`]: a complete windowless app session (`lightcraft-cli snapshot`, tests). It plays
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
use crate::{ControlRequest, LightcraftApp};

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
    pub app: LightcraftApp,
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
}

/// A headless run (`lightcraft-cli snapshot`, tests) is a one-shot process: however it ends, it
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
    pub fn new(app: LightcraftApp, size: [f32; 2], pixels_per_point: f32) -> Self {
        let (size, pixels_per_point) = if viewport_pixels(size, pixels_per_point).is_ok() {
            (egui::Vec2::from(size), pixels_per_point)
        } else {
            (egui::vec2(1600.0, 1000.0), 1.0)
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = app.with_control(rx);
        app.headless_host = true;
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

    /// Is anything still in progress (renders, file-system checks the sidebar waits for, queued
    /// input)?
    pub fn busy(&self) -> bool {
        self.app.renderer.in_flight() > 0
            || crate::panels::left::fs_cached_running(&self.view.ctx) > 0
            || self.app.merge.busy()
            || self.app.scan.is_some()
            || self.app.sync.is_some()
            || self.app.sync_run.is_some()
            || self.app.import.is_some()
            || self.app.export.is_some()
            || self.app.session.denoise_busy()
            || !self.app.tasks.is_empty()
            || !self.app.synthetic.is_empty()
            || !self.events.is_empty()
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
        if !home.is_empty() && lightcraft_catalog::query::folder_within(&path.to_string_lossy(), &home) {
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
    jobs: Vec<(crate::render::Slot, lightcraft_engine::media::RenderJob)>,
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
    fn try_start(&mut self, slot: crate::render::Slot, job: lightcraft_engine::media::RenderJob) -> Option<lightcraft_engine::media::RenderJob> {
        if !self.open.get() {
            return Some(job);
        }
        self.jobs.push((slot, job));
        None
    }
    fn finished(&mut self) -> Vec<(crate::render::Slot, lightcraft_engine::media::RenderResult, f64)> {
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
    #[test]
    fn detail_offers_ai_denoise_for_raw_photos_and_leads_to_its_settings() {
        use crate::state::Dialog;
        use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-denoise-{}", std::process::id()));
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
        assert!(w.contains("\"denoise:setup\""), "a raw photo with no model is offered the setup: {w}");
        // One click lands in Settings, with local installation available even without a download offer.
        let r = h.request("ui.clickWidget", json!({"id": "denoise:setup"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "denoise".into() }));
        h.settle(SETTLE);
        let listed = h.app.session.execute("denoise.models.list", &json!({})).unwrap();
        assert_eq!(listed["models"].as_array().unwrap().len(), lightcraft_denoise::known::all().len(), "{listed}");
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
        assert!(!widgets(&mut h).contains("\"denoise:setup\""), "nothing is offered for a photo denoise does not apply to");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Install from file… with a model the engine rejects says why in the AI Denoise section (#1381), instead of
    /// leaving the button looking dead.
    #[test]
    fn a_rejected_model_file_says_why_in_settings() {
        use crate::state::Dialog;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-denoise-rejected-{}", std::process::id()));
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
        h.app.ui.dialog = Some(Dialog::Settings { tab: "denoise".into() });
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "denoise:installFile"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
        h.step();
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "denoise".into() }));
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
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
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
    fn control_resize_uses_effective_points_after_ui_zoom() {
        for scale in [0.9, 1.0, 2.0] {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
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
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
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
    }

    #[test]
    fn control_resize_rounds_in_native_points_after_a_fractional_zoom_out() {
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
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
        let app = LightcraftApp::new(lightcraft_engine::Session::new(), crate::Services::default());
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
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        Headless::new(app, size, 1.0)
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

    /// `ui.clipboard` acts in a text field: with none focused it is refused, rather than doing
    /// what ⌘V does there on one platform (paste edit settings) and nothing on another.
    #[test]
    fn the_clipboard_method_needs_a_text_field() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        let undo = h.app.session.undo.len();
        for action in ["copy", "paste"] {
            let r = h.request("ui.clipboard", json!({"action": action, "text": "travel"}), t);
            assert_eq!(r["ok"], false, "{action}: {r}");
            assert!(r["error"].as_str().is_some_and(|e| e.contains("text field")), "{r}");
        }
        h.settle(SETTLE);
        assert_eq!(h.app.session.undo.len(), undo, "nothing was pasted");
    }

    /// `ui.clickWidget` right-clicks with `button: "right"`, as `ui.click` does (MCP `click`
    /// passes it on with a widget): a filmstrip photo's context menu opens.
    #[test]
    fn click_widget_right_clicks() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.settle(SETTLE);
        let cell = h.app.widgets.iter().find(|(w, _)| w.starts_with("film:")).map(|(w, _)| w.clone()).expect("a filmstrip photo");
        assert!(!egui::Popup::is_any_open(&h.view.ctx));
        let r = h.request("ui.clickWidget", json!({"id": cell, "button": "right"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(egui::Popup::is_any_open(&h.view.ctx), "its context menu");
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
        let mut h = demo([1200.0, 760.0]);
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
        let pos = {
            let a = h.app.session.selection.active;
            h.app.session.visible_cloned().iter().position(|x| Some(*x) == a)
        }
        .unwrap();
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
        assert_eq!(
            {
                let a = h.app.session.selection.active;
                h.app.session.visible_cloned().iter().position(|x| Some(*x) == a)
            },
            Some(1)
        );
    }

    /// The search field is a shared text field: right-click ▸ Paste puts the clipboard's text in
    /// and filters by it.
    #[test]
    fn the_search_field_pastes_from_its_menu() {
        let mut h = demo([1200.0, 760.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        h.view.clipboard = "wedding".into();
        for (id, button) in [("field:search", "right"), ("field:search:paste", "left")] {
            let r = h.request("ui.clickWidget", json!({"id": id, "button": button}), t);
            assert_eq!(r["ok"], true, "{id}: {r}");
            h.settle(SETTLE);
        }
        assert_eq!(h.app.ui.search, "wedding");
        assert_eq!(h.app.session.filter.text, "wedding", "and the library is filtered by it");
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

    #[test]
    fn snapshots_are_deterministic_and_control_requests_work() {
        let shot = || {
            let mut h = demo([900.0, 600.0]);
            let r = h.request("ui.set", json!({"view": "detail"}), Duration::from_secs(10));
            assert_eq!(r["ok"], true, "{r}");
            // Compare only fully settled frames: a timed-out settle would compare half-rendered
            // pictures and fail as a misleading pixel diff (seen on loaded CI machines).
            assert!(h.settle(Duration::from_secs(300)), "the demo detail view did not settle within 300 s");
            h.paint()
        };
        let (a, b) = (shot(), shot());
        assert_eq!(a.size, b.size);
        // The photo may come from the GPU in one run and the CPU in the other (the GPU warms up in
        // the background), which differ by ≤ 1–2 LSB; anything more is a real difference.
        let diff = a.pixels.iter().zip(&b.pixels).filter(|(x, y)| x.to_array().iter().zip(y.to_array()).any(|(p, q)| p.abs_diff(q) > 2)).count();
        assert_eq!(diff, 0, "{diff} pixels differ between two runs");
    }

    /// Keyboard culling: Compare (rating keys hit the candidate, arrows move it, auto-advance),
    /// Survey (keys hit the active photo only), and auto-advance in Detail.
    #[test]
    fn compare_survey_and_auto_advance_by_keyboard() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let rating = |h: &Headless, id: u64| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().rating;
        let flag = |h: &Headless, id: u64| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().flag;
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
        assert_eq!(flag(&h, vis[1]), lightcraft_catalog::Flag::Reject);
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
        assert_eq!((rating(&h, vis[3]), flag(&h, vis[4])), (5, lightcraft_catalog::Flag::Pick));
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

    /// Naming a face in the loupe: point at an unnamed face, click its "Add name" label, type, Enter. Needs the
    /// recognition runtime only for the background indexer, so the naming itself is tested whatever the build.
    #[test]
    fn naming_a_face_in_the_loupe() {
        use crate::state::NameEdit;
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.active().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::Face,
            name: None,
            description: None,
        }];
        h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
        h.request("ui.set", json!({"view": "detail", "right": "none"}), t);
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

    /// Adding a face model: the dialog shows the file's terms, the model is installed only once they are
    /// accepted, and a file LightCraft cannot use only says why.
    #[test]
    fn adding_a_face_model_shows_its_terms_and_installs_only_once_accepted() {
        use crate::state::Dialog;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-facemodel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        let model = dir.join("Mine.onnx");
        std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
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
        assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "faces".into() }));
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

    /// Photo > Detect Faces without the detector offers the download: the terms first, nothing fetched before they are
    /// accepted, and the model's row in Settings has its Download button.
    #[test]
    fn detect_faces_without_the_detector_offers_the_download_and_fetches_nothing_before_acceptance() {
        use crate::state::Dialog;
        let mut h = demo([1400.0, 900.0]);
        let dir = std::env::temp_dir().join(format!("lc-ui-detector-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));

        let r = h.app.run("faces.detect", json!({}));
        assert!(r.is_err(), "no detector yet");
        let Some(dlg) = h.app.ui.dialog.clone() else { panic!("the download was not offered") };
        let Dialog::FaceModel { info, accepted, .. } = &dlg else { panic!("{dlg:?}") };
        assert!(!accepted);
        assert_eq!((info["download"].as_str(), info["host"].as_str()), (Some("yunet-2023mar"), Some("github.com")));
        assert_eq!(info["model"]["licence"]["name"], "MIT");
        // OK without accepting the terms starts nothing
        assert!(crate::panels::dialogs::confirm_dialog(&mut h.app, &dlg).is_err());
        assert!(h.app.session.face_downloads.snapshot().is_empty());
        assert!(!dir.join("models").join(".downloads").exists());
        assert!(h.app.caches.faces_dl_watch.is_empty());

        // Settings > Faces shows the detector with a Download button
        h.app.ui.dialog = Some(Dialog::Settings { tab: "faces".into() });
        h.step();
        // (the first request switches the widget list on, the next frame fills it)
        h.request("ui.widgets", json!({}), Duration::from_secs(10));
        h.step();
        let w = h.request("ui.widgets", json!({"filter": "faces:"}), Duration::from_secs(10));
        assert!(w.to_string().contains("faces:download:yunet-2023mar"), "{w}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Pressing Download in Settings shows the model's terms first and fetches nothing until they are accepted; a build
    /// that cannot run recognition models offers no download. The licence dialog replaces Settings, which it was opened from.
    #[test]
    fn download_shows_the_terms_first_and_fetches_nothing_until_accepted() {
        use crate::state::Dialog;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-facedl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "faces"}}), t);
        h.settle(SETTLE);
        h.step();
        let runtime = h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] == true;
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
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-facesetup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        h.app.session.face_models_dir = Some(dir.join("models"));
        let runtime = h.app.session.execute("faces.models.list", &json!({})).unwrap()["runtime"] == true;
        let id = h.app.session.active().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::Face,
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
        std::fs::write(&model, lightcraft_faces::synthetic::tiny_embedder_model(512)).unwrap();
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
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(3).collect();
        let face = |x: f64, name: &str| lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.25, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::Face,
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
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(7).collect();
        assert_eq!(ids.len(), 7);
        let region = |name: Option<&str>| lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.25, y0: 0.2, x1: 0.55, y1: 0.6 },
            kind: lightcraft_meta::RegionKind::Face,
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
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(3).collect();
        let face = |x: f64, name: Option<&str>| lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: x, y0: 0.2, x1: x + 0.2, y1: 0.55 },
            kind: lightcraft_meta::RegionKind::Face,
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
        let named = |h: &Headless, id: lightcraft_catalog::PhotoId, i: usize| h.app.session.catalog.photo(id).unwrap().meta.regions[i].name.clone();
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
    }

    /// A double click on an unnamed face opens its photo in the loupe, as in the grid (#1664); a single click still only
    /// selects it, and Escape from that photo goes back to the People view.
    #[test]
    fn double_clicking_an_unnamed_face_opens_its_photo() {
        use crate::state::ViewMode;
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).take(2).collect();
        for id in &ids {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = vec![lightcraft_meta::Region {
                rect: lightcraft_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.5, y1: 0.55 },
                kind: lightcraft_meta::RegionKind::Face,
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
        use lightcraft_catalog::Op;
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.catalog.photos().map(|p| p.id).next().unwrap();
        let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
        meta.regions = vec![lightcraft_meta::Region {
            rect: lightcraft_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.5, y1: 0.55 },
            kind: lightcraft_meta::RegionKind::Face,
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
        use lightcraft_catalog::Op;
        let mut h = demo([1500.0, 1000.0]);
        let t = Duration::from_secs(20);
        let ids: Vec<_> = h.app.session.catalog.photos().map(|p| p.id).collect();
        // seven unnamed faces in every photo of the demo library
        for id in &ids {
            let mut meta = h.app.session.catalog.photo(*id).unwrap().meta.clone();
            meta.regions = (0..7)
                .map(|k| lightcraft_meta::Region {
                    rect: lightcraft_geom::Rect { x0: 0.05 + 0.12 * k as f64, y0: 0.2, x1: 0.15 + 0.12 * k as f64, y1: 0.45 },
                    kind: lightcraft_meta::RegionKind::Face,
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

    /// `ui.inspect` says how hard the face scan is allowed to work and whether the window counts as in front, so a slow
    /// scan can be told from a stuck one.
    #[test]
    fn inspect_reports_the_face_scan_pace() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        h.settle(SETTLE);
        h.step();
        let r = h.request("ui.inspect", json!({}), t);
        let scan = &r["result"]["faceScan"];
        assert!(["pause", "light", "normal", "full"].contains(&scan["pace"].as_str().unwrap_or("")), "{scan}");
        assert!(scan["focused"].is_boolean() && scan["pending"].is_u64() && scan["indexed"].is_u64(), "{scan}");
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
    }

    /// Hovering the applied profile shows the applied look (no preview), not the last other profile hovered.
    #[test]
    fn hovering_the_applied_profile_drops_the_last_hover() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.profiles"}), t);
        h.settle(SETTLE);
        h.request("ui.clickWidget", json!({"id": "profileCell:lc.vivid"}), t);
        h.request("ui.hoverWidget", json!({"id": "profileCell:lc.portrait"}), t);
        h.step();
        assert!(h.app.hover_preview.is_some(), "another profile previews");
        h.request("ui.hoverWidget", json!({"id": "profileCell:lc.vivid"}), t);
        h.step();
        assert!(h.app.hover_preview.is_none(), "the applied profile is shown, not the last hovered one");
    }

    /// The preset applied to a photo is still marked (and its Amount slider back) after visiting
    /// another photo, which shows none (issue #2392); an edit to the photo drops the mark.
    #[test]
    fn applied_preset_is_marked_again_on_returning_to_the_photo() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.presets"}), t);
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
        h.request("ui.move", json!({"x": 5, "y": 500}), t);
        h.settle(SETTLE);
        h.step();
        assert!(h.app.hover_preview.is_none());
        assert!(h.app.renderer.variant_textures() >= 5, "{}", h.app.renderer.variant_textures());
        // Create Preset with a group checklist: untick Light, keep Effects
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "light.exposure", "value": 0.5}}), t);
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "effects.clarity", "value": 25}}), t);
        h.app.ui.dialog = Some(crate::state::Dialog::create_preset());
        h.step();
        h.step();
        let r = h.request("ui.clickWidget", json!({"id": "presetInclude:light"}), t);
        assert_eq!(r["ok"], true, "{r}");
        match &h.app.ui.dialog {
            Some(crate::state::Dialog::CreatePreset { groups, .. }) => {
                assert!(!groups.iter().any(|g| g == "light") && groups.iter().any(|g| g == "effects"), "{groups:?}");
                assert!(!groups.iter().any(|g| g == "crop" || g == "masks"), "crop and masks off by default");
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
            .map(|i| lightcraft_develop::Preset {
                id: format!("user.big.{i}"),
                name: format!("Look {i}"),
                group: format!("Big {:02}", i / 500),
                settings: json!({"light": {"exposure": (i % 20) as f64 / 10.0}}),
                favorite: false,
                builtin: false,
            })
            .collect();
        assert_eq!(h.app.session.add_presets(many).len(), 5000);
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
        h.request("engine.execute", json!({"command": "view.filterBar"}), t);
        assert!(h.app.ui.filter_bar);
        for w in ["filter:star3", "filter:pick", "filter:label-red", "filter:label-red"] {
            let r = h.request("ui.clickWidget", json!({"id": w}), t);
            assert_eq!(r["ok"], true, "{w}: {r}");
        }
        let f = h.app.session.filter.clone();
        assert_eq!((f.rating, f.flag, f.label), (3, Some(lightcraft_catalog::Flag::Pick), None), "a second click clears the label");
        let n = h.app.session.visible_cloned().len();
        assert!(n > 0 && n < all);
        h.request("ui.clickWidget", json!({"id": "button:filterSave"}), t);
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::NewSmartAlbum { .. })));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let smart = h.app.session.catalog.albums().find(|a| a.is_smart()).expect("smart album").id;
        assert_eq!(h.app.session.catalog.album_count(smart), n);
        h.request("ui.clickWidget", json!({"id": "button:filterClear"}), t);
        assert_eq!(h.app.session.filter, Default::default());
        assert_eq!(h.app.session.visible_cloned().len(), all);
        h.settle(SETTLE);
    }

    /// The photo grid shows date headers (registered as `group:<date>` widgets); clicking one
    /// selects that day's photos; month headers when zoomed out; none when grouping is off.
    #[test]
    fn grid_groups_by_capture_date() {
        let mut h = demo([1300.0, 800.0]);
        let t = Duration::from_secs(10);
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
        let month = h.app.session.catalog.date_runs(&vis, h.app.session.sort.key, lightcraft_catalog::GroupBy::Month);
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
        h.request("ui.set", json!({"leftPanel": true}), t);
        // clicks land on last frame's layout: let the keyword list settle first (on a loaded machine a
        // row could still move, and the click then hit its neighbour, e.g. "sunrise")
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "source:keyword:travel"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.session.filter.keyword.as_deref(), Some("travel"));
        assert_eq!(h.app.session.visible_cloned().len(), 3);
        // open the level, filter by the child
        h.request("ui.clickWidget", json!({"id": "keywordToggle:travel"}), t);
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "source:keyword:travel|italy"}), t);
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
        assert!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(vis[1])).unwrap().meta.keywords.contains(&"gelato".to_string()));
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
        let keywords = |h: &Headless| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(first)).unwrap().meta.keywords.clone();
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
        let keywords = h.app.session.catalog.photo(lightcraft_catalog::PhotoId(first)).unwrap().meta.keywords.clone();
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
        let name = |id: u64| h.app.session.catalog.photo(lightcraft_catalog::PhotoId(id)).unwrap().file_name.clone();
        assert!(name(vis[0]).starts_with("Trip-05."), "{}", name(vis[0]));
        assert!(name(vis[1]).starts_with("Trip-06."), "{}", name(vis[1]));
        h.settle(SETTLE);
    }

    /// The Export dialog hands its batch to a worker thread: the UI keeps drawing frames, shows
    /// progress, and reports the result when the files are written.
    #[test]
    fn export_dialog_keeps_its_width() {
        // A dialog row sized as "available width − a guessed button width" made the auto-sized
        // window grow a little every frame when the real button was wider (issue #8, Linux).
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.app.services.pick_folder = Some(Box::new(|| None));
        h.app.services.pick_files = Some(Box::new(Vec::new));
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        // the widest variant: watermark graphic and every optional row
        if let Some(crate::state::Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            let mut wm = lightcraft_engine::export::Watermark { image: "logo.png".into(), ..Default::default() };
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
        assert!(w1 < 700.0, "{w1}");
    }

    /// With a watermark on, the Export dialog shows the first photo with the watermark on it.
    #[test]
    fn export_dialog_previews_the_watermark() {
        let mut h = demo([1400.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), t);
        let preview = |h: &mut Headless| {
            let r = h.request("ui.widgets", json!({}), t);
            r["result"].as_array().and_then(|a| a.iter().find(|w| w["id"] == "image:exportWatermarkPreview")).map(|w| w["rect"].clone())
        };
        h.settle(SETTLE);
        assert!(preview(&mut h).is_none(), "no preview without a watermark");
        if let Some(crate::state::Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            opts.watermark = Some(lightcraft_engine::export::Watermark { text: "© LightCraft".into(), ..Default::default() });
        }
        let t0 = Instant::now();
        while preview(&mut h).is_none() && t0.elapsed() < Duration::from_secs(30) {
            h.step();
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
        lightcraft_engine::export::ExportOptions::from_params(&o.to_json()).expect("the dialog's settings export");
        assert_eq!(opened(&mut h, json!({"format": "tiff", "bitDepth": 32})).bit_depth, Some(32));
    }

    /// Review of #1966: a `path` whose extension names no format exports JPEG here, so its
    /// `bitDepth` is checked against JPEG's, not let through to an 8-bit file.
    #[test]
    fn export_checks_the_depth_of_the_format_a_bare_path_gets() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let out = std::env::temp_dir().join(format!("lc-bare-path-depth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        h.app.services.write = Some(Box::new(|p: &str, bytes: &[u8]| lightcraft_engine::export::write_file(p, bytes)));
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
        if let Some(crate::state::Dialog::Export { full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
            *full_size = false;
            *resize = lightcraft_engine::export::Resize::long_edge(64);
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

    /// Issue #236: the Export dialog's Add to Apple Photos is there on macOS only; there the
    /// background export hands the files it wrote to Photos (a fake runner: tests never start
    /// osascript or Photos) and the toast says how it went.
    #[test]
    fn export_dialog_adds_to_apple_photos_on_macos() {
        let mut h = demo([1200.0, 1100.0]);
        let t = Duration::from_secs(10);
        let out = std::env::temp_dir().join(format!("lc-photos-dialog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lightcraft_engine::export::write_file(p, bytes)));
        let calls = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let c = calls.clone();
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "A/L0/001\nB/L0/001\n".into(), stderr: String::new() })
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
            opts.photos_album = "Trip “2026”".into();
            *full_size = false;
            *resize = lightcraft_engine::export::Resize::long_edge(48);
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
        let toast = h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        assert!(toast.contains("2 added to Apple Photos album “Trip “2026””"), "{toast}");
        // Export with Previous repeats it
        assert_eq!(h.app.session.last_export.as_ref().unwrap()["addToPhotos"], true);
        let _ = std::fs::remove_dir_all(&out);
    }

    /// Codex review of #643: the progress line while Photos imports was translated on the export's
    /// worker thread, whose language is always English.
    #[test]
    fn the_photos_progress_line_is_in_the_users_language() {
        if !cfg!(target_os = "macos") {
            return;
        }
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let out = std::env::temp_dir().join(format!("lc-photos-language-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        h.app.ui.language = crate::i18n::Locale::De;
        crate::i18n::set_language(crate::i18n::Locale::De);
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| lightcraft_engine::export::write_file(p, bytes)));
        let (release, held) = std::sync::mpsc::channel::<()>();
        let held = std::sync::Mutex::new(held);
        let (entered, photos_asked) = std::sync::mpsc::channel::<()>();
        let entered = std::sync::Mutex::new(entered);
        h.app.session.apple_photos = Some(Arc::new(move |_: &[String], _| {
            let _ = entered.lock().unwrap().send(());
            let _ = held.lock().unwrap().recv_timeout(Duration::from_secs(30));
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
        }));
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(1).map(|p| p.0).collect();
        let params = json!({"ids": ids, "dir": out, "longEdge": 48, "addToPhotos": true, "background": true});
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

    /// Issue #236 review: control requests run on the UI thread, which must not wait minutes for
    /// Photos. `export.addToPhotos` and `app.export {addToPhotos}` without `background` answer at
    /// once with the running job; the app announces the outcome when Photos is done.
    #[test]
    fn a_slow_photos_import_never_blocks_the_window() {
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(30);
        let out = std::env::temp_dir().join(format!("lc-photos-slow-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let file = out.join("a.jpg");
        std::fs::write(&file, b"jpeg").unwrap();
        h.app.services.write = Some(Box::new(|p: &str, bytes: &[u8]| lightcraft_engine::export::write_file(p, bytes)));
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        h.app.session.apple_photos_background = true; // as the desktop app sets it
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            // a regression (Photos on the UI thread) fails the timing below instead of hanging
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
            let n = args.len() - args.iter().position(|a| a == lightcraft_engine::apple_photos::MARKER).unwrap() - 2;
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".repeat(n), stderr: String::new() })
        }));
        let t0 = Instant::now();
        let r =
            h.request("engine.execute", json!({"command": "export.addToPhotos", "params": {"paths": [file.to_string_lossy()], "album": "Slow"}}), t);
        assert!(t0.elapsed() < Duration::from_secs(5), "answered at once: {r}");
        assert_eq!(r["result"]["running"], true, "{r}");
        go.send(()).unwrap();
        while !h.app.ui.toast.as_ref().is_some_and(|t| t.0.contains("Apple Photos")) {
            assert!(t0.elapsed() < Duration::from_secs(20), "the outcome is announced");
            h.step();
        }
        assert_eq!(h.app.ui.toast.as_ref().unwrap().0, "1 added to Apple Photos album “Slow”");
        if cfg!(target_os = "macos") {
            let t1 = Instant::now();
            let p = json!({"dir": out.to_string_lossy(), "longEdge": 32, "addToPhotos": true});
            let r = h.request("engine.execute", json!({"command": "app.export", "params": p}), t);
            assert!(t1.elapsed() < Duration::from_secs(10), "the export answers without waiting for Photos: {r}");
            assert_eq!(r["result"]["applePhotos"]["running"], true, "{r}");
            assert_eq!(r["result"]["applePhotos"]["job"], 2, "{r}");
            go.send(()).unwrap();
            while !h.app.ui.toast.as_ref().is_some_and(|t| t.0.starts_with("1 added to Apple Photos") && !t.0.contains("Slow")) {
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
        let mut h = demo([800.0, 600.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-photos-headless-drop-{}", std::process::id()));
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
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
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
        if !cfg!(target_os = "macos") {
            return; // addToPhotos is refused before exporting elsewhere
        }
        let mut h = demo([800.0, 600.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-photos-headless-export-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let marker = dir.join("imported");
        h.app.services.write_shared = Some(Arc::new(|p: &str, bytes: &[u8]| {
            std::thread::sleep(Duration::from_millis(300)); // a slow disk: still writing at the drop
            lightcraft_engine::export::write_file(p, bytes)
        }));
        let m = marker.clone();
        h.app.session.apple_photos = Some(Arc::new(move |_: &[String], _| {
            std::thread::sleep(Duration::from_millis(500));
            std::fs::write(&m, b"done").unwrap();
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
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
        if !cfg!(target_os = "macos") {
            return; // addToPhotos is refused before exporting elsewhere
        }
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let out = std::env::temp_dir().join(format!("lc-photos-hold-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).unwrap();
        let other = out.join("other.jpg");
        std::fs::write(&other, b"jpeg").unwrap();
        // the writer waits for `go`: the export is still writing while we try another import
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        h.app.services.write_shared = Some(Arc::new(move |p: &str, bytes: &[u8]| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(20));
            lightcraft_engine::export::write_file(p, bytes)
        }));
        let calls = Arc::new(std::sync::Mutex::new(Vec::<Vec<String>>::new()));
        let c = calls.clone();
        h.app.session.apple_photos_background = true;
        h.app.session.apple_photos = Some(Arc::new(move |args: &[String], _| {
            c.lock().unwrap().push(args.to_vec());
            Ok(lightcraft_engine::apple_photos::Output { status: Some(0), stdout: "ID\n".into(), stderr: String::new() })
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

    /// Dragging grid photos onto an album row adds them to the album.
    #[test]
    fn drag_photos_onto_an_album() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("engine.execute", json!({"command": "view.leftPanel"}), t);
        let r = h.request("engine.execute", json!({"command": "album.create", "params": {"name": "Dropped"}}), t);
        let album = r["result"]["id"].as_u64().unwrap_or_else(|| panic!("{r}"));
        h.settle(SETTLE);
        let rect = |h: &mut Headless, id: &str| {
            let w = h.request("ui.widgets", json!({"filter": id}), t);
            let r = w["result"]
                .as_array()
                .and_then(|a| a.iter().find(|x| x["id"] == id))
                .map(|x| x["rect"].clone())
                .unwrap_or_else(|| panic!("{id}: {w}"));
            let f = |i: usize| r[i].as_f64().unwrap();
            (f(0) + f(2) / 2.0, f(1) + f(3) / 2.0)
        };
        let first = h.app.session.visible_cloned()[0].0;
        let (x, y) = rect(&mut h, &format!("thumb:{first}"));
        let (tx, ty) = rect(&mut h, &format!("source:album:{album}"));
        let r = h.request("ui.drag", json!({"x": x, "y": y, "toX": tx, "toY": ty, "steps": 12}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let members = h.app.session.catalog.album(lightcraft_catalog::AlbumId(album)).unwrap().photos.clone();
        assert_eq!(members, vec![lightcraft_catalog::PhotoId(first)]);
        assert!(h.app.ui.dragging_photos.is_none(), "the drag ended");
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
            .apply(lightcraft_catalog::Op::SetContent {
                id: po,
                width: ph.width,
                height: ph.height,
                file_size: ph.file_size,
                content_hash: ph.content_hash.clone(),
                preview_only: Some("Nikon Huffman-compressed NEF (no clean-room description available)".into()),
                preview_checked: lightcraft_catalog::APP_VERSION.into(),
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
        assert_eq!(h.app.ui.toast.clone().map(|x| x.0).as_deref(), Some("This raw variant still can't be decoded"));
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
    }

    /// Info panel: typing into a field survives frames and is saved when it loses focus; the new
    /// accessibility and place fields reach the photo.
    #[test]
    fn info_fields_keep_typing_and_save() {
        let mut h = demo([1300.0, 1000.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "info"}), t);
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
    }

    /// Info panel fields are shared text fields. The caption takes several lines: Return starts a
    /// new one and only leaving the field saves it. Esc gives up an edit (the title stays as it
    /// was, nothing saved), and right-click ▸ Paste works there too.
    #[test]
    fn info_fields_take_lines_esc_and_the_menu() {
        let mut h = demo([1300.0, 1000.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "info"}), t);
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

    /// With several photos selected the Info panel offers Target Photo / Selected Photos. Target
    /// Photo (the default) shows the active photo's values. Selected Photos leaves a field whose
    /// values differ empty, so what is typed there is what every selected photo gets, and keeps
    /// the value of a field they share; leaving a mixed field untouched saves nothing.
    #[test]
    fn info_shows_metadata_for_the_selected_photos() {
        let mut h = demo([1300.0, 1000.0]);
        let t = Duration::from_secs(10);
        let ask = |h: &mut Headless, method: &str, params: serde_json::Value| {
            let r = h.request(method, params.clone(), t);
            assert_eq!(r["ok"], true, "{method} {params}: {r}");
            h.step();
            h.step();
            r
        };
        let exec = |h: &mut Headless, command: &str, params: serde_json::Value| {
            ask(h, "engine.execute", json!({"command": command, "params": params}))["result"].clone()
        };
        let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(2).map(|p| p.0).collect();
        let (a, b) = (lightcraft_catalog::PhotoId(ids[0]), lightcraft_catalog::PhotoId(ids[1]));
        let meta = |h: &Headless, id| h.app.session.catalog.photo(id).unwrap().meta.clone();
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "title": "Dawn", "copyrightStatus": "copyrighted"}));
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "title": "Dusk"}));
        exec(&mut h, "photo.setMeta", json!({"ids": ids, "creator": "Ana"}));
        h.request("ui.set", json!({"view": "detail", "right": "info"}), t);
        exec(&mut h, "library.select", json!({"ids": ids, "active": ids[0]}));
        h.settle(SETTLE);
        let buttons = |h: &mut Headless| h.request("ui.widgets", json!({"filter": "infoFor"}), t)["result"].to_string();
        assert!(buttons(&mut h).contains("button:infoFor-selected"), "the choice shows with two photos selected");
        assert!(!h.app.ui.settings.info_selected_photos, "Target Photo by default");
        // Target Photo: the title field holds the active photo's title, and what is typed after it
        // goes to both photos
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title"}));
        ask(&mut h, "ui.key", json!({"key": "End"}));
        ask(&mut h, "ui.text", json!({"text": "!"}));
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        h.settle(SETTLE);
        assert_eq!((meta(&h, a).title, meta(&h, b).title), ("Dawn!".into(), "Dawn!".into()));
        // Selected Photos, from its button
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "title": "Dusk"}));
        ask(&mut h, "ui.clickWidget", json!({"id": "button:infoFor-selected"}));
        assert!(h.app.ui.settings.info_selected_photos);
        // a mixed field left untouched saves nothing
        let undo = h.app.session.undo.len();
        ask(&mut h, "ui.clickWidget", json!({"id": "field:title"}));
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        h.settle(SETTLE);
        assert_eq!((meta(&h, a).title, meta(&h, b).title, h.app.session.undo.len()), ("Dawn!".into(), "Dusk".into(), undo));
        // neither does switching the choice while a field is being edited, either way
        for on in [false, true] {
            ask(&mut h, "ui.clickWidget", json!({"id": "field:title"}));
            exec(&mut h, "view.infoSelectedPhotos", json!({"on": on}));
            ask(&mut h, "ui.key", json!({"key": "Tab"}));
            h.settle(SETTLE);
            assert_eq!((meta(&h, a).title, meta(&h, b).title, h.app.session.undo.len()), ("Dawn!".into(), "Dusk".into(), undo), "{on}");
        }
        // the mixed title starts empty; the shared creator keeps its value
        for (key, typed, want) in [("title", "Harbor", "Harbor"), ("creator", " Silva", "Ana Silva")] {
            ask(&mut h, "ui.clickWidget", json!({"id": format!("field:{key}")}));
            ask(&mut h, "ui.key", json!({"key": "End"}));
            ask(&mut h, "ui.text", json!({"text": typed}));
            ask(&mut h, "ui.key", json!({"key": "Tab"}));
            h.settle(SETTLE);
            let got = |h: &Headless, id| if key == "title" { meta(h, id).title } else { meta(h, id).creator };
            assert_eq!((got(&h, a), got(&h, b)), (want.to_string(), want.to_string()), "{key}");
        }
        // a mixed field typed in and left empty clears it on every selected photo
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "caption": "Old"}));
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "caption": "Other"}));
        h.settle(SETTLE);
        ask(&mut h, "ui.clickWidget", json!({"id": "field:caption"}));
        ask(&mut h, "ui.text", json!({"text": "x"}));
        ask(&mut h, "ui.key", json!({"key": "Backspace"}));
        ask(&mut h, "ui.key", json!({"key": "Tab"}));
        h.settle(SETTLE);
        assert_eq!((meta(&h, a).caption, meta(&h, b).caption), (String::new(), String::new()));
        // GPS: one photo's place and Show on Map only while the selected photos share it
        let map = |h: &mut Headless| h.request("ui.widgets", json!({"filter": "showOnMap"}), t)["result"].to_string().contains("showOnMap");
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[0]], "gps": [41.15, -8.61]}));
        exec(&mut h, "photo.setMeta", json!({"ids": [ids[1]], "gps": [38.72, -9.14]}));
        h.settle(SETTLE);
        assert!(!map(&mut h), "places differ");
        exec(&mut h, "photo.setMeta", json!({"ids": ids, "gps": [41.15, -8.61]}));
        h.settle(SETTLE);
        assert!(map(&mut h), "a shared place");
        // the command: refuses a value that isn't true or false, and turns Target Photo back on
        let r = h.request("engine.execute", json!({"command": "view.infoSelectedPhotos", "params": {"on": "yes"}}), t);
        assert_ne!(r["ok"], true, "{r}");
        assert!(h.app.ui.settings.info_selected_photos);
        assert_eq!(exec(&mut h, "view.infoSelectedPhotos", json!({"on": false}))["on"], false);
        assert!(!h.app.ui.settings.info_selected_photos);
        assert_eq!(exec(&mut h, "view.infoSelectedPhotos", json!({}))["on"], true, "no params toggles");
        // the choice is saved with the app settings; settings saved before it read as Target Photo
        let saved = serde_json::to_value(&h.app.ui.settings).unwrap();
        assert_eq!(saved["infoSelectedPhotos"], true);
        let read: crate::state::AppSettings = serde_json::from_value(saved).unwrap();
        assert!(read.info_selected_photos);
        let old: crate::state::AppSettings = serde_json::from_value(json!({"filmNames": false})).unwrap();
        assert!(!old.info_selected_photos);
        // one photo selected: no choice to make
        exec(&mut h, "library.select", json!({"ids": [ids[0]]}));
        h.settle(SETTLE);
        assert!(!buttons(&mut h).contains("infoFor-"), "{}", buttons(&mut h));
    }

    /// The fields the Info panel marks as mixed: each text field, the copyright status and GPS
    /// where a selected photo differs from the photo the panel shows, and none for one photo.
    #[test]
    fn info_mixed_fields_are_the_ones_that_differ() {
        let mut s = lightcraft_engine::Session::with_demo();
        let ids: Vec<u64> = s.visible_cloned().iter().take(3).map(|p| p.0).collect();
        let all = |key: &str, v: serde_json::Value| json!({"ids": ids, key: v});
        // (every field the panel compares starts the same on the three photos)
        let fields = [
            "title",
            "caption",
            "altText",
            "extendedDescription",
            "copyright",
            "usageTerms",
            "copyrightUrl",
            "creator",
            "location",
            "state",
            "country",
        ];
        for key in fields {
            s.execute("photo.setMeta", &all(key, json!(""))).unwrap();
        }
        for (key, v) in
            [("title", json!("Same")), ("city", json!("Porto")), ("copyrightStatus", json!("copyrighted")), ("gps", json!([41.15, -8.61]))]
        {
            s.execute("photo.setMeta", &all(key, v)).unwrap();
        }
        let photos: Vec<lightcraft_catalog::PhotoId> = ids.iter().map(|&i| lightcraft_catalog::PhotoId(i)).collect();
        let shown = s.catalog.photo(photos[0]).unwrap().meta.clone();
        assert!(crate::panels::right::mixed_meta(&s.catalog, &photos, &shown).is_empty());
        assert!(crate::panels::right::mixed_meta(&s.catalog, &photos[..1], &shown).is_empty());
        s.execute("photo.setMeta", &json!({"ids": [ids[2]], "city": "Lisbon", "caption": "x", "gps": [38.72, -9.14]})).unwrap();
        s.execute("photo.setMeta", &json!({"ids": [ids[1]], "copyrightStatus": "publicDomain"})).unwrap();
        let mut mixed = crate::panels::right::mixed_meta(&s.catalog, &photos, &shown);
        mixed.sort_unstable();
        assert_eq!(mixed, ["caption", "city", "copyrightStatus", "gps"]);
    }

    /// Local: a folder's photos show without joining the library; the breadcrumb, Include
    /// subfolders and Add to My Photos work from the grid header.
    #[test]
    fn browse_a_local_folder() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        for (i, p) in [dir.join("a.png"), dir.join("inner/b.png")].iter().enumerate() {
            let mut img = img.clone();
            img.data[0][0] = i as u8; // different bytes per file
            std::fs::write(p, lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        }
        let library_before = h.app.session.catalog.photos().filter(|p| !p.local).count();
        let r = h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        // the folder is read in the background: the view switches at once, the photos follow
        assert_eq!(r["result"]["scanning"], true, "{r}");
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::Folder);
        h.settle(SETTLE);
        assert!(h.app.scan.is_none() && h.app.import.is_none());
        assert_eq!(h.app.session.visible_cloned().len(), 1);
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
        assert_eq!(std::path::Path::new(&h.app.session.browse.as_ref().unwrap().path), dir.as_path());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Local: Remove from Local hides a sidebar location (nothing on disk changes) and the
    /// "Show hidden locations" row puts it back. The folder is browsed and kept the way Browse
    /// Folder… does it, in the temporary folder — on Windows beneath Home, whose tree must not
    /// open down to the hidden folder and push the restore row out of view.
    #[test]
    fn local_location_can_be_hidden_and_restored() {
        let size = [1300.0, 900.0];
        let mut h = demo(size);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-ui-hide-{}", std::process::id()));
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
        let dir2 = std::env::temp_dir().join(format!("lc-ui-hide-{}-b", std::process::id()));
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

    /// Local: a kept folder stays a root while its subfolders (and other locations) are
    /// browsed — the subfolder is highlighted inside its tree, its siblings stay listed — and
    /// the kept folders survive a save/load of the UI state (a restart).
    #[test]
    fn kept_local_root_stays_while_browsing_below_and_elsewhere() {
        let mut h = demo([1300.0, 1400.0]);
        let t = Duration::from_secs(10);
        let base = std::env::temp_dir().join(format!("lc-ui-roots-{}", std::process::id()));
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

    /// ↑ / ↓ over a slider nudge it (one undo step each); ⇧Z picks and moves to the next photo.
    #[test]
    fn slider_nudge_and_pick_advance() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        h.settle(SETTLE);
        let exposure = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure;
        assert_eq!(h.request("ui.hoverWidget", json!({"id": "slider:light.exposure"}), t)["ok"], true);
        h.request("ui.key", json!({"key": "Up"}), t);
        h.request("ui.key", json!({"key": "Up", "shift": true}), t);
        assert!((exposure(&h) - 0.30).abs() < 1e-9, "{}", exposure(&h));
        h.request("ui.key", json!({"key": "Down"}), t);
        assert!((exposure(&h) - 0.25).abs() < 1e-9, "{}", exposure(&h));
        let first = h.app.session.active().unwrap();
        h.request("ui.key", json!({"key": "Z", "shift": true}), t);
        assert_eq!(h.app.session.catalog.photo(first).unwrap().flag, lightcraft_catalog::Flag::Pick);
        assert_ne!(h.app.session.active(), Some(first), "advanced");
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
        ask(&mut h, "ui.set", json!({"settings": {"gpu": false, "previewEdge": 1280}, "presets": false}));
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

    /// A slider's typed value is a shared text field: right-click ▸ Paste replaces the value
    /// (selected when the field opens) and Return applies it; the menu doesn't close the field.
    #[test]
    fn a_slider_value_pastes_from_its_menu() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        h.settle(SETTLE);
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
        let exposure = h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().light.exposure;
        assert!((exposure - 0.75).abs() < 1e-9, "{exposure}");
    }

    /// The eye on a section header switches the section off and on again, one undo step each
    /// (issue #316).
    #[test]
    fn section_eye_switches_a_section_off() {
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail", "right": "edit"}), t);
        h.settle(SETTLE);
        let light_on = |h: &Headless| h.app.session.develop_of(h.app.session.active().unwrap()).unwrap().section_enabled("light");
        for expected in [false, true] {
            // the eye shows while the pointer is on the header
            assert_eq!(h.request("ui.hoverWidget", json!({"id": "section:light"}), t)["ok"], true);
            assert_eq!(h.request("ui.clickWidget", json!({"id": "sectionEye:light"}), t)["ok"], true);
            assert_eq!(light_on(&h), expected);
        }
        assert!(h.app.ui.section_open("light"), "the click didn't fold the section");
    }

    /// Return commits a tool panel back to Edit; elsewhere it does nothing.
    #[test]
    fn return_commits_the_crop_tool() {
        let mut h = demo([1200.0, 800.0]);
        let t = Duration::from_secs(10);
        h.request("ui.set", json!({"view": "detail"}), t);
        h.request("engine.execute", json!({"command": "panel.crop"}), t);
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Crop);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit);
        h.request("ui.key", json!({"key": "Enter"}), t);
        assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "no-op outside tools");
    }

    /// ⌘Q (File → Quit LightCraft) closes the window.
    #[test]
    fn cmd_q_quits() {
        let mut h = demo([900.0, 600.0]);
        assert!(!h.quit_requested());
        h.request("ui.key", json!({"key": "Q", "cmd": true}), Duration::from_secs(10));
        h.step();
        h.step();
        assert!(h.quit_requested());
    }

    /// File → Import from Folder… opens the import review for a folder (searched recursively).
    #[test]
    fn add_folder_opens_the_import_review() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-addfolder-{}", std::process::id()));
        let sub = dir.join("day 2");
        std::fs::create_dir_all(&sub).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(24, 16, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lightcraft_engine::export::encode_image(
            &img,
            &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
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
        for label in ["Quelle", "Übertragen", "Stichwörter", "Vorgabe"] {
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

    /// Switching Settings tabs keeps the window's top edge (and its tab row) where it is and never makes it
    /// narrower or taller, though the tabs hold different amounts: the window is one size on every tab.
    #[test]
    fn settings_tabs_keep_the_window_in_place() {
        let mut h = demo([1300.0, 820.0]);
        let t = Duration::from_secs(10);
        let window = |h: &Headless| h.app.widgets.iter().find(|(w, _)| w == "dialog:window").map(|(_, r)| *r).expect("the dialog");
        let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "general"}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let first = window(&h);
        let mut heights = Vec::new();
        for tab in ["display", "import", "performance", "general", "interface", "display"] {
            h.request("ui.clickWidget", json!({"id": format!("button:settingsTab-{tab}")}), t);
            h.settle(SETTLE);
            let now = window(&h);
            assert!((now.top() - first.top()).abs() < 1.0, "{tab}: top {} → {}", first.top(), now.top());
            assert!(now.width() >= first.width() - 1.0, "{tab}: width {} → {}", first.width(), now.width());
            heights.push(now.height());
        }
        assert!(heights.iter().all(|h| (h - first.height()).abs() < 1.0), "the tabs keep the height of {}: {heights:?}", first.height());
    }

    /// Settings → Import → "Show the import review when a camera or card is connected": a card that
    /// was already in doesn't ask, a new one opens Import from Device (copying), and a busy window
    /// asks once it's free.
    #[test]
    fn a_connected_card_opens_the_import_review() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let root = std::env::temp_dir().join(format!("lc-card-{}", std::process::id()));
        let dcim = root.join("DCIM/100NIKON");
        std::fs::create_dir_all(&dcim).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dcim.join("DSC_0001.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let card = lightcraft_engine::devices::Device {
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
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-browse-again-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(dir.join("a.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (g, n) = (gate.clone(), started.clone());
        h.app.session.media.file_probe = Some(std::sync::Arc::new(move |_: &str| {
            n.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            while !g.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(lightcraft_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
        }));
        let browse =
            |h: &mut Headless| h.request("engine.execute", json!({"command": "library.browse", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(browse(&mut h)["result"]["scanning"], true);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(browse(&mut h)["result"]["scanning"], true);
        std::thread::sleep(Duration::from_millis(200));
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
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-browse-stamp-{}", std::process::id()));
        let day = dir.join("2026/10/14");
        std::fs::create_dir_all(&day).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        std::fs::write(day.join("a.png"), lightcraft_engine::export::encode_image(&img, &o).unwrap()).unwrap();
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
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-browse-gone-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let o = lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() };
        let png = lightcraft_engine::export::encode_image(&img, &o).unwrap();
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
        let held: Vec<lightcraft_catalog::PhotoId> =
            h.app.session.selection.ids.iter().copied().chain(h.app.session.selection.active).chain(h.app.session.previous_active).collect();
        let name = |id: &lightcraft_catalog::PhotoId| h.app.session.catalog.photo(*id).map(|p| p.file_name.clone()).unwrap_or_default();
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
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-scanbg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lightcraft_engine::export::encode_image(
            &img,
            &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
        )
        .unwrap();
        std::fs::write(dir.join("a.png"), &png).unwrap();
        let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.to_string_lossy()}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        assert!(h.app.scan.is_some() || h.app.ui.dialog.is_some());
        h.settle(SETTLE);
        assert!(h.app.scan.is_none());
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancel closes the scan at once and opens no review; Add from Device while a scan runs is
    /// refused and leaves the running scan's options alone.
    #[test]
    fn folder_scan_cancel_and_busy() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let dir = std::env::temp_dir().join(format!("lc-scancancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [(x * 9) as u8, (y * 12) as u8, 80, 255]);
        let png = lightcraft_engine::export::encode_image(
            &img,
            &lightcraft_engine::export::ExportOptions { format: lightcraft_engine::export::ExportFormat::Png, ..Default::default() },
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
            Ok(lightcraft_engine::media::ProbeInfo { format: "PNG".into(), ..Default::default() })
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
        h.request("ui.key", json!({"key": "V", "cmd": true, "shift": true}), t);
        h.settle(SETTLE);
        let Some(crate::state::Dialog::PasteSettings { groups }) = &h.app.ui.dialog else { panic!("dialog not open: {:?}", h.app.ui.dialog) };
        assert_eq!(groups, &["light", "color"]);
        let r = h.request("ui.clickWidget", json!({"id": "pasteGroup:color"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.request("ui.dialog.confirm", json!({}), t)["ok"], true);
        let d = h.app.session.develop_of(lightcraft_catalog::PhotoId(ids[1])).unwrap();
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

    /// Info panel → Edit Capture Time…: a time-zone shift moves the selected photos.
    #[test]
    fn capture_time_dialog_shifts_time_zone() {
        let mut h = demo([1200.0, 900.0]);
        let t = Duration::from_secs(10);
        let id = h.app.session.visible_cloned()[0];
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
        h.request("ui.set", json!({"view": "detail", "right": "info"}), t);
        let before = h.app.session.catalog.photo(id).unwrap().captured.clone().unwrap();
        // The dock header leaves less vertical space; bring the metadata action into view.
        let panel = h.app.widgets.iter().find(|(id, _)| id == "panel:right_panel").unwrap().1;
        let clipped_action = h.app.widgets.iter().find(|(id, _)| id == "icon:editCaptureTime").unwrap().1;
        assert!(!panel.contains_rect(clipped_action), "the capture-time action starts below the visible panel: {clipped_action:?}");
        h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), t);
        h.request("ui.scroll", json!({"dy": -160.0}), t);
        h.settle(SETTLE);
        let action = h.app.widgets.iter().find(|(id, _)| id == "icon:editCaptureTime").unwrap().1;
        assert!(panel.contains_rect(action), "capture-time action must be visible before clicking: {action:?} in {panel:?}");
        let r = h.request("ui.clickWidget", json!({"id": "icon:editCaptureTime"}), t);
        assert_eq!(r["ok"], true, "{r}");
        let r = h.request("ui.clickWidget", json!({"id": "button:captureMode-2"}), t);
        assert_eq!(r["ok"], true, "{r}");
        if let Some(crate::state::Dialog::CaptureTime { zone, .. }) = &mut h.app.ui.dialog {
            *zone = -3.0;
        } else {
            panic!("dialog not open: {:?}", h.app.ui.dialog);
        }
        h.settle(SETTLE);
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        let after = h.app.session.catalog.photo(id).unwrap().captured.clone().unwrap();
        let secs = lightcraft_catalog::dates::iso_seconds;
        assert_eq!(secs(&after).unwrap() - secs(&before).unwrap(), -3 * 3600);
        h.settle(SETTLE);
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
        assert_eq!(h.app.session.catalog.photo(id).unwrap().label, Some(lightcraft_catalog::ColorLabel::Green));
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
        let dir = std::env::temp_dir().join(format!("lc-ui-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5u8 {
            let (w, h) = (40usize, 30usize);
            let data: Vec<[u8; 4]> = (0..w * h).map(|k| [(k % w * 6) as u8, i * 40, (k / w * 8) as u8, 255]).collect();
            let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
        }
        std::fs::copy(dir.join("img0.png"), dir.join("img0-copy.png")).unwrap();
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
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
            let dir = std::env::temp_dir().join(format!("lc-ui-import-trash-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..2u8 {
                let data: Vec<[u8; 4]> = (0..40 * 30).map(|k| [(k % 40 * 6) as u8, i * 90, 7, 255]).collect();
                let img = lightcraft_raster::Rgba8 { width: 40, height: 30, data };
                let png =
                    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
                std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
            }
            let services = crate::Services { png: None, ..Default::default() };
            let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), services);
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
            let toast = h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default();
            let photos = &h.app.session.catalog;
            assert_eq!(photos.photos().filter(|p| !p.deleted).count(), 2, "{choice}");
            let old = photos.photo(lightcraft_catalog::PhotoId(id));
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
        let dir = std::env::temp_dir().join(format!("lc-ui-import-trash-click-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..2u8 {
            let data: Vec<[u8; 4]> = (0..40 * 30).map(|k| [(k % 40 * 6) as u8, i * 90, 7, 255]).collect();
            let img = lightcraft_raster::Rgba8 { width: 40, height: 30, data };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("img{i}.png")), png).unwrap();
        }
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), services);
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
        assert!(photos.photo(lightcraft_catalog::PhotoId(id)).is_some_and(|p| !p.deleted && p.rating == 3), "restored with its edits");
        assert_eq!(photos.photos().filter(|p| !p.deleted).count(), 2);
        assert_eq!(photos.len(), 2, "restored, not imported a second time");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The import review opens bigger than the old fixed 6 × 2.5 grid and can be resized by its
    /// corner: the photo grid takes the new height, and the window then keeps its size (issue #337).
    #[test]
    fn import_review_dialog_resizes() {
        let mut h = demo([1400.0, 1000.0]);
        let t = Duration::from_secs(10);
        let candidates = (0..60)
            .map(|i| lightcraft_engine::import::ImportCandidate {
                path: format!("/lc-test/img{i}.png"),
                name: format!("img{i}.png"),
                error: Some("not read in this test".into()),
                ..Default::default()
            })
            .collect();
        h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(crate::import::ImportDialog::new(candidates)) });
        let rect = |h: &mut Headless| {
            let r = h.request("ui.widgets", json!({}), t);
            let w = r["result"].as_array().and_then(|a| a.iter().find(|w| w["id"] == "dialog:window")).expect("dialog on screen");
            [0usize, 1, 2, 3].map(|i| w["rect"][i].as_f64().unwrap())
        };
        for _ in 0..10 {
            h.step();
        }
        let r0 = rect(&mut h);
        assert!(r0[3] > 600.0, "{r0:?}");
        // the Copy options add rows below the grid: the grid makes room, the window doesn't grow
        for copy in [true, false] {
            if let Some(crate::state::Dialog::Import { opts }) = &mut h.app.ui.dialog {
                opts.copy = copy;
            }
            for _ in 0..10 {
                h.step();
            }
            let r = rect(&mut h);
            assert!((r[3] - r0[3]).abs() < 0.5, "copy {copy}: {r0:?} → {r:?}");
        }
        let (x, y) = (r0[0] + r0[2] - 3.0, r0[1] + r0[3] - 3.0);
        h.request("ui.drag", json!({"x": x, "y": y, "toX": x + 160.0, "toY": y + 120.0, "steps": 12}), t);
        for _ in 0..10 {
            h.step();
        }
        let r1 = rect(&mut h);
        assert!(r1[2] > r0[2] + 100.0 && r1[3] > r0[3] + 80.0, "dragging the corner resized it: {r0:?} → {r1:?}");
        for _ in 0..60 {
            h.step();
        }
        let r2 = rect(&mut h);
        assert!((r2[2] - r1[2]).abs() < 0.5 && (r2[3] - r1[3]).abs() < 0.5, "the dialog kept its size: {r1:?} → {r2:?}");
        assert!(matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })), "still open");
    }

    /// Adding a file again (dropped on the window, no review) whose photo is in Recently Deleted
    /// brings the photo back, selected in All Photos, and says so (issue #706) — it is not
    /// skipped as a duplicate; the trash view's menus offer Restore and Delete Permanently, and a
    /// re-add of a photo that is in the library selects it in All Photos.
    #[test]
    fn readding_a_deleted_photo_shows_it_in_recently_deleted() {
        let dir = std::env::temp_dir().join(format!("lc-ui-readd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8 { width: 16, height: 12, data: vec![[200, 120, 40, 255]; 16 * 12] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let file = dir.join("flower.png");
        std::fs::write(&file, png).unwrap();
        let paths = vec![file.to_string_lossy().to_string()];
        let services = crate::Services { png: None, ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1300.0, 900.0], 1.0);
        let t = Duration::from_secs(10);
        // frames until the import has finished, so its (timed) toast is read before it expires
        let finish_import = |h: &mut Headless| {
            let t0 = std::time::Instant::now();
            while h.app.import.is_some() && t0.elapsed() < Duration::from_secs(30) {
                h.step();
            }
            assert!(h.app.import.is_none(), "import finished");
        };
        crate::import::start_paths(&mut h.app, paths.clone()).unwrap();
        h.settle(SETTLE);
        let id = h.app.session.selection.active.expect("imported photo selected");
        let n0 = h.app.session.catalog.len();
        h.request("engine.execute", json!({"command": "photo.delete", "params": {"ids": [id.0]}}), t);
        assert!(h.app.session.catalog.photo(id).unwrap().deleted);
        let photo_items = |app: &LightcraftApp| -> Vec<String> {
            let bar = crate::menubar::menu_bar(app);
            let items = &bar.iter().find(|(title, _)| title == "Photo").expect("Photo menu").1;
            items.iter().filter_map(|n| if let crate::menubar::MenuNode::Item { id, .. } = n { Some(id.clone()) } else { None }).collect()
        };
        assert!(!photo_items(&h.app).contains(&"photo.restore".to_string()), "Restore only for deleted photos");

        crate::import::start_paths(&mut h.app, paths.clone()).unwrap();
        finish_import(&mut h);
        assert!(!h.app.session.catalog.photo(id).unwrap().deleted, "re-adding the file restores the photo");
        assert_eq!(h.app.session.catalog.len(), n0, "no second record");
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All);
        assert_eq!(h.app.session.selection.ids, vec![id], "the restored photo is selected");
        let toast = h.app.ui.toast.clone().expect("toast").0;
        assert!(toast.contains("Restored 1 photo from Recently Deleted"), "{toast}");
        assert!(!photo_items(&h.app).contains(&"photo.restore".to_string()));
        // undo of the re-add puts it back in Recently Deleted; the trash view's menus offer Restore
        h.request("engine.execute", json!({"command": "edit.undo"}), t);
        assert!(h.app.session.catalog.photo(id).unwrap().deleted, "one undo step");
        h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "recentlyDeleted"}}), t);
        h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), t);
        h.settle(SETTLE);
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::RecentlyDeleted);
        let items = photo_items(&h.app);
        assert!(items.contains(&"photo.restore".to_string()) && items.contains(&"photo.deletePermanently".to_string()), "{items:?}");
        assert!(!items.contains(&"photo.delete".to_string()), "{items:?}");
        // a right-click on its filmstrip thumbnail opens the photo menu
        h.request("ui.set", json!({"view": "detail"}), t);
        h.settle(SETTLE);
        let cell = h.app.widgets.iter().find(|(w, _)| *w == format!("film:{}", id.0)).map(|(_, r)| *r).expect("filmstrip cell");
        assert!(!egui::Popup::is_any_open(&h.view.ctx));
        let r = h.request("ui.click", json!({"x": cell.center().x, "y": cell.center().y, "button": "right"}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(egui::Popup::is_any_open(&h.view.ctx), "filmstrip context menu");

        // Empty Recently Deleted is offered while the trash is the source
        assert!(photo_items(&h.app).contains(&"library.emptyRecentlyDeleted".to_string()));
        let r = h.request("ui.menu.invoke", json!({"id": "photo.restore"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert!(!h.app.session.catalog.photo(id).unwrap().deleted, "restored");
        crate::import::start_paths(&mut h.app, paths).unwrap();
        finish_import(&mut h);
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All);
        assert_eq!(h.app.session.selection.ids, vec![id]);
        let toast = h.app.ui.toast.clone().expect("toast").0;
        assert!(toast.contains("All Photos"), "{toast}");
        assert!(!photo_items(&h.app).contains(&"library.emptyRecentlyDeleted".to_string()), "Empty is for the trash view only");

        // deleted permanently, the file imports afresh as a new photo
        h.request("engine.execute", json!({"command": "photo.delete", "params": {"ids": [id.0]}}), t);
        h.request("engine.execute", json!({"command": "photo.deletePermanently", "params": {"ids": [id.0]}}), t);
        assert!(h.app.session.catalog.photo(id).is_none());
        crate::import::start_paths(&mut h.app, vec![file.to_string_lossy().to_string()]).unwrap();
        h.settle(SETTLE);
        let fresh = h.app.session.selection.active.expect("re-imported photo selected");
        assert_ne!(fresh, id);
        assert!(!h.app.session.catalog.photo(fresh).unwrap().deleted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Importing while an album is shown (issue #1446): the new photos aren't in it, so the view
    /// switches to All Photos with the first of them active, instead of an active photo the grid
    /// and the filmstrip don't show.
    #[test]
    fn an_import_from_an_album_view_shows_the_first_new_photo() {
        let dir = std::env::temp_dir().join(format!("lc-ui-import-show-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..3u8 {
            let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[i * 40, 3, 9, 255]; 64] };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(dir.join(format!("IMG_{i:02}.png")), png).unwrap();
        }
        let mut h = demo([1300.0, 900.0]);
        let t = Duration::from_secs(10);
        let album = h.app.session.catalog.albums().find(|a| !a.folder && !a.is_smart()).map(|a| a.id.0).expect("a demo album");
        let r = h.request("engine.execute", json!({"command": "library.source", "params": {"kind": "album", "id": album}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let before: Vec<lightcraft_catalog::PhotoId> = h.app.session.catalog.photos().map(|p| p.id).collect();
        let r = h.request("engine.execute", json!({"command": "file.addPhotos", "params": {"paths": [dir.to_string_lossy()]}}), t);
        assert_eq!(r["result"]["scanning"], true, "{r}");
        h.step_until(SETTLE, |h| matches!(h.app.ui.dialog, Some(crate::state::Dialog::Import { .. })));
        let r = h.request("ui.dialog.confirm", json!({}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.import.is_none(), "finished");
        let active = h.app.session.selection.active.expect("a photo is active");
        assert!(!before.contains(&active), "the active photo is a new one: {active:?}");
        assert_eq!(h.app.session.source, lightcraft_engine::LibrarySource::All);
        assert!(h.app.session.visible_cloned().contains(&active), "the grid shows the active photo");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Import dialog, copy mode: a chosen destination, one folder, renamed copies numbered across
    /// the import's batches.
    #[test]
    fn import_copy_renames_into_destination() {
        let base = std::env::temp_dir().join(format!("lc-ui-import-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("out"));
        std::fs::create_dir_all(&src).unwrap();
        // more files than a batch holds (see `batch_size`)
        for i in 0..20u8 {
            let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[i * 12, 3, 9, 255]; 64] };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(src.join(format!("IMG_{i:02}.png")), png).unwrap();
        }
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo().with_fs(), services);
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
        assert_eq!(names, want, "numbered across batches of {}", lightcraft_engine::import::batch_size());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Import review, Copy, Folders → Custom template…: the default `{date:%Y}/{date:%Y%m%d}`
    /// files copies under `2026/20260114/`, the example line shows the full destination, and a
    /// template that would climb out of the destination is refused.
    #[test]
    fn import_copy_into_a_custom_folder_template() {
        let base = std::env::temp_dir().join(format!("lc-ui-import-tpl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("out"));
        std::fs::create_dir_all(&src).unwrap();
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[40, 3, 9, 255]; 64] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(src.join("IMG_01.png"), png).unwrap();
        // undated files are filed by the file's date (2026-01-14T05:58:48)
        std::fs::File::options()
            .write(true)
            .open(src.join("IMG_01.png"))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_768_370_328))
            .unwrap();
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut session = lightcraft_engine::Session::with_demo().with_fs();
        session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
        let mut app = LightcraftApp::new(session, services);
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
        let base = std::env::temp_dir().join(format!("lc-ui-import-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (src, dest) = (base.join("card"), base.join("Photos"));
        std::fs::create_dir_all(&src).unwrap();
        for i in 0..2u8 {
            let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[40 + i, 3, 9, 255]; 64] };
            let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
            std::fs::write(src.join(format!("IMG_0{i}.png")), png).unwrap();
            std::fs::File::options()
                .write(true)
                .open(src.join(format!("IMG_0{i}.png")))
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_768_370_328))
                .unwrap();
        }
        let dest_s = dest.to_string_lossy().to_string();
        let services = crate::Services { png: None, pick_folder: Some(Box::new(move || Some(dest_s.clone()))), ..Default::default() };
        let mut session = lightcraft_engine::Session::with_demo().with_fs();
        session.clock = Box::new(|| "2026-01-14T05:58:48".to_string());
        let mut app = LightcraftApp::new(session, services);
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
        let dir = std::env::temp_dir().join(format!("lc-ui-import-tags-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let img = lightcraft_raster::Rgba8 { width: 8, height: 8, data: vec![[90, 3, 9, 255]; 64] };
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        std::fs::write(dir.join("IMG_0007.png"), png).unwrap();
        let lib = dir.join("lib");
        let mut session = lightcraft_engine::Session::with_demo().with_fs();
        session.open_library(&lib, false).unwrap();
        let mut app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
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
        let seq3 = lightcraft_engine::rename::TOKENS.iter().position(|x| x.tag == "{seq:3}").unwrap();
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

    /// Help ▸ About: the About, Contributors and Models tabs switch and paint (credits are
    /// compiled in).
    #[test]
    fn about_dialog_tabs_show_the_credits() {
        // an empty library: the dialog needs no photos, and no decodes compete with other tests
        let services = crate::Services { png: None, ..Default::default() };
        let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), services), [1300.0, 820.0], 1.0);
        let t = Duration::from_secs(10);
        let r = h.request("ui.menu.invoke", json!({"id": "app.about"}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::About));
        // a new window sizes itself on its first frame: let it settle before clicking its tabs
        h.step();
        h.step();
        for (i, (tab, _)) in crate::panels::dialogs::ABOUT_TABS.iter().enumerate().rev() {
            let r = h.request("ui.clickWidget", json!({"id": format!("button:aboutTab-{tab}")}), t);
            assert_eq!(r["ok"], true, "{tab}: {r}");
            h.step();
            let shown = h.view.ctx.data_mut(|d| d.get_temp::<u8>(egui::Id::new("about_tab")));
            assert_eq!(shown.map(usize::from), Some(i), "{tab}");
        }
        h.request("ui.clickWidget", json!({"id": "button:aboutTab-contributors"}), t);
        let img = h.snapshot(SETTLE);
        assert_eq!(img.size, [1300, 820]);
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::About), "switching tabs keeps the dialog open");
    }

    /// Settings ▸ Display / `app.displayProfile`: previews are shown through the monitor profile
    /// (the loupe rendered for the display, thumbnails converted from sRGB, the histogram still
    /// sRGB); a file that can't be used is an error, or, as the setting, reported with previews
    /// left sRGB.
    #[test]
    fn display_profile_converts_previews() {
        use crate::render::Slot;
        let dir = std::env::temp_dir().join(format!("lightcraft-display-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p3 = dir.join("p3.icc");
        std::fs::write(&p3, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3)).unwrap();
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
        let rgba = |c: &egui::ColorImage| lightcraft_raster::Rgba8 {
            width: c.size[0],
            height: c.size[1],
            data: c.pixels.iter().map(|p| p.to_array()).collect(),
        };
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
        let max_diff = |a: &lightcraft_raster::Rgba8, b: &lightcraft_raster::Rgba8| {
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
        use lightcraft_codecs::{NamedSpace, icc::write_named};
        use std::sync::{Arc, Mutex};
        let id = |space| lightcraft_engine::display::Display::from_bytes("", &write_named(space)).unwrap().id();
        let dir = std::env::temp_dir().join(format!("lightcraft-screen-profile-{}", std::process::id()));
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

    /// Settings (⌘,): tabs switch, app settings change the UI state, library settings go through
    /// the engine; the delete confirmation guards ⌫.
    #[test]
    fn settings_dialog_by_keyboard_and_clicks() {
        let mut h = demo([1300.0, 820.0]);
        let t = Duration::from_secs(10);
        let r = h.request("ui.key", json!({"key": ",", "cmd": true}), t);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "general".into() }));
        for tab in ["import", "performance", "display", "interface", "general"] {
            let r = h.request("ui.clickWidget", json!({"id": format!("button:settingsTab-{tab}")}), t);
            assert_eq!(r["ok"], true, "{tab}: {r}");
            assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: tab.into() }));
        }
        // General: confirm before delete (on by default), startup view
        assert!(h.app.ui.settings.confirm_delete);
        h.request("ui.clickWidget", json!({"id": "check:settings.confirmDelete"}), t);
        assert!(!h.app.ui.settings.confirm_delete);
        h.request("ui.clickWidget", json!({"id": "check:settings.confirmDelete"}), t);
        assert!(h.app.ui.settings.confirm_delete);
        h.request("ui.clickWidget", json!({"id": "button:settingsStartup-2"}), t);
        assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Detail);
        // Interface: filmstrip names off, grid badges always
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-interface"}), t);
        // Appearance cards (one row per theme, with its favorite star) sit above these controls;
        // scroll the compact settings viewport past them.
        h.request("ui.hoverWidget", json!({"id": "radio:theme-charcoal"}), t);
        h.request("ui.scroll", json!({"dy": -700.0}), t);
        for _ in 0..4 {
            h.step();
        }
        h.request("ui.clickWidget", json!({"id": "check:settings.filmNames"}), t);
        assert!(!h.app.ui.settings.film_names);
        h.request("ui.clickWidget", json!({"id": "button:settingsGridBadges-1"}), t);
        assert_eq!(h.app.ui.settings.grid_badges, crate::state::GridBadges::Always);
        h.request("ui.scroll", json!({"dy": 700.0}), t);
        for _ in 0..4 {
            h.step();
        }
        // Performance: the thumbnail cache size goes to the library preferences
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-performance"}), t);
        h.request("ui.clickWidget", json!({"id": "button:settingsCache-0"}), t);
        assert_eq!(h.app.session.cache_mb, 512);
        h.request("ui.clickWidget", json!({"id": "button:settingsPreview-3"}), t);
        assert_eq!(h.app.ui.settings.preview_limit, 3840);
        h.request("ui.clickWidget", json!({"id": "button:settingsMemory-2"}), t);
        assert_eq!(h.app.ui.settings.memory_mb, 1024);
        h.step();
        assert_eq!(lightcraft_engine::memory::budget(), 1024 << 20, "applied through app.memoryBudget");
        h.request("ui.clickWidget", json!({"id": "button:settingsMemory-0"}), t);
        h.step();
        assert_eq!(lightcraft_engine::memory::budget(), lightcraft_engine::memory::default_budget(), "back to automatic");
        // Import: per-camera defaults
        h.request("ui.clickWidget", json!({"id": "button:settingsTab-import"}), t);
        h.request("ui.clickWidget", json!({"id": "check:settings.perCamera"}), t);
        assert!(h.app.session.import_defaults.per_camera);
        let img = h.snapshot(SETTLE);
        assert_eq!(img.size, [1300, 820]);
        // Escape closes; ⌫ now asks first
        h.request("ui.key", json!({"key": "escape"}), t);
        assert_eq!(h.app.ui.dialog, None);
        let active = h.app.session.active().unwrap();
        h.request("ui.key", json!({"key": "delete"}), t);
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::ConfirmDelete { count: 1 }));
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
    fn full_screen_preview_and_info_overlay_by_keyboard() {
        use crate::state::InfoOverlay;
        let mut h = demo([1000.0, 700.0]);
        let t = Duration::from_secs(10);
        let vis: Vec<u64> = h.app.session.visible_cloned().iter().map(|p| p.0).collect();
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
        h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), t);
        assert!(h.app.window_is_fullscreen);
        h.request("ui.key", json!({"key": "f", "cmd": true, "shift": true}), t);
        assert!(!h.app.window_is_fullscreen);
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
                assert_eq!(h.app.ui.toast.as_ref().expect("overlay toast").0, expected);
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
        assert!(main[0].max(main[1]) < lightcraft_engine::SourceLevel::Preview.max_edge(), "the loupe render is fitted: {main:?}");
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
        assert!(main[0].max(main[1]) < lightcraft_engine::SourceLevel::Preview.max_edge(), "{main:?}");
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
            ("wbPicker", egui::CursorIcon::Crosshair),
            ("colorRange", egui::CursorIcon::Crosshair),
            ("pointColor", egui::CursorIcon::Crosshair),
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
        assert_eq!(h.app.ui.zoom, Zoom::Percent(800.0));
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
        let other = h.app.session.visible_cloned().get(1).copied().expect("a second photo").0;
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
}
