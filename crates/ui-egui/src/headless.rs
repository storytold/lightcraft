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

/// Turn a logical viewport size into native pixels for the headless renderer. Both edges and
/// the scale must be finite and positive; each scaled edge is rounded like egui's scissor and
/// must be within `1..=MAX_IMAGE_EDGE`, and the product must fit both the raster and screenshot
/// pixel budgets. Returns a user-facing error instead of clamping.
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
    // The rasterizer's limit is not enough: a screenshot is also bounded by the RGBA budget it is
    // admitted against, so reject a size `ui.screenshot` would only refuse later.
    let max_pixels = softpaint::MAX_IMAGE_PIXELS.min(crate::MAX_SHOT_PIXELS);
    if w.checked_mul(h).is_none_or(|pixels| pixels > max_pixels) {
        return Err(format!("scaled viewport exceeds {max_pixels} pixels"));
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
        let mut commands = out.viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default();
        // 0.37: `ViewportCommand::Screenshot` carries a one-shot callback. A headless host answers
        // it here, after the pass it was issued in, exactly once.
        let mut shots = Vec::new();
        commands.retain(|command| match command {
            ViewportCommand::Screenshot(callback) => {
                shots.push(callback.clone());
                false
            }
            _ => true,
        });
        for callback in shots {
            callback.complete(std::sync::Arc::new(self.paint(&std::collections::HashMap::new())));
        }
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
    /// Test-only hook run after `raw_input_hook` and before `logic`/`ui`, as eframe does: it can
    /// create a prompt after the host input was already filtered for the previous frame.
    #[cfg(test)]
    after_raw_input_hook: Option<Box<dyn FnMut(&mut LightcraftApp)>>,
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
            #[cfg(test)]
            after_raw_input_hook: None,
        }
    }

    /// Run `hook` once, between the next frame's [`LightcraftApp::raw_input_hook`] and its
    /// `logic`/`ui` call. This is the production order in eframe: a prompt can be created there
    /// after the host input was already accepted for the frame.
    #[cfg(test)]
    pub(crate) fn set_after_raw_input_hook(&mut self, hook: impl FnMut(&mut LightcraftApp) + 'static) {
        self.after_raw_input_hook = Some(Box::new(hook));
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
        // A headless run's timers follow its simulated clock, not the host's wall clock.
        self.app.set_simulated_clock(Some(self.time));
        self.app.raw_input_hook(&self.view.ctx, &mut raw);
        #[cfg(test)]
        if let Some(mut hook) = self.after_raw_input_hook.take() {
            hook(&mut self.app);
        }
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
            // `ViewportCommand::Screenshot` never reaches here: `HeadlessView::run` answers the
            // one-shot callback after the pass it was issued in, so there is exactly one reply.
            match c {
                ViewportCommand::Screenshot(_) => {}
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
            || !self.app.pending_gestures.is_empty()
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
mod tests;
