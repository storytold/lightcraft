//! Programmatic control of the running app (agents, tests, MCP).
//!
//! Methods (JSON-lines over the host's transport):
//! - `engine.execute {command, params}` / `ui.menu.invoke`: run any engine or UI command
//! - `engine.commands`: engine + UI commands with enablement
//! - `ui.inspect`: UI state, panels, view, canvas/image rects, perf
//! - `ui.widgets {filter?}`: every registered widget id → rect (sliders, buttons, thumbnails…)
//! - `ui.clickWidget {id, count?}`: click a widget by automation id (real egui input)
//! - `ui.dragWidget {id, toX?, toY?, dx?, dy?}`: drag from a widget's centre
//! - `ui.move/click/drag {x, y, …}`: raw pointer input in screen points
//! - `ui.pointer {events: [{kind: down|drag|up, x, y}], space: "image"}`: gestures in normalized
//!   image coordinates (brush strokes, crop handles…) — mapped to screen and injected as real input
//! - `ui.key {key, shift?, alt?, cmd?}` / `ui.text {text}` / `ui.scroll {dx, dy}`
//! - `ui.set {view?, panel?, filmstrip?, leftPanel?, zoom?, …}` / `ui.dialog.confirm|cancel`
//! - `surface.*`: control surfaces (pairing, subscribe, input, layout, icons; `crate::surface`)
//! - `ui.screenshot {path?, headless?}` (headless: CPU-rendered, no compositor needed), `ui.resize {width, height}`, `ui.render {id?, size?, path?, inline?}`, `app.quit`

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::LccApp;

pub type ControlResponse = Value;

pub struct ControlRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<ControlResponse>,
    /// Who has the request: [`PENDING`], [`TAKEN`] (the app runs it) or [`ABANDONED`] (the
    /// sender gave up on it, so the app must not run it late: issue #718).
    pub state: std::sync::Arc<std::sync::atomic::AtomicU8>,
    /// The connection it came on (0 = in process: headless scripts and tests, trusted).
    pub conn: u64,
    /// Where events for that connection go, for `surface.subscribe` (none in process).
    pub sink: Option<crate::surface::EventSink>,
}

/// Queued, not yet run.
pub const PENDING: u8 = 0;
/// The app has started it: it will answer.
pub const TAKEN: u8 = 1;
/// Its sender gave up waiting (the window drew no frames): it is dropped unrun.
pub const ABANDONED: u8 = 2;

impl ControlRequest {
    pub fn new(method: impl Into<String>, params: Value) -> (Self, std::sync::mpsc::Receiver<ControlResponse>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Self { method: method.into(), params, reply: tx, state: Default::default(), conn: 0, sink: None }, rx)
    }

    /// The request came on connection `conn` (numbered from 1), whose pushed events go to `sink`.
    pub fn on_connection(mut self, conn: u64, sink: crate::surface::EventSink) -> Self {
        self.conn = conn;
        self.sink = Some(sink);
        self
    }

    /// Claim the request for running; false when its sender already gave up on it.
    pub fn take(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.state.compare_exchange(PENDING, TAKEN, Ordering::SeqCst, Ordering::SeqCst).is_ok() || self.state.load(Ordering::SeqCst) == TAKEN
    }

    /// Give up on the request; false when the app has already started it (it will answer).
    pub fn abandon(state: &std::sync::atomic::AtomicU8) -> bool {
        use std::sync::atomic::Ordering;
        state.compare_exchange(PENDING, ABANDONED, Ordering::SeqCst, Ordering::SeqCst).is_ok() || state.load(Ordering::SeqCst) == ABANDONED
    }
}

pub enum Outcome {
    Done(Value),
    Screenshot { path: Option<String>, headless: bool },
}

fn ok(v: Value) -> Outcome {
    Outcome::Done(json!({"ok": true, "result": v}))
}
fn err(e: impl std::fmt::Display) -> Outcome {
    Outcome::Done(json!({"ok": false, "error": e.to_string()}))
}
fn wrap(r: Result<Value, String>) -> Outcome {
    match r {
        Ok(v) => ok(v),
        Err(e) => err(e),
    }
}

/// Labels are source keys with `{app}` / `{upstream}` placeholders (the UI translates them by key); a listing for
/// an agent reports them as the user sees them.
fn brand_filled(listing: Value) -> Value {
    serde_json::from_str(&lcc_brand::fill(&listing.to_string())).unwrap_or(listing)
}

pub fn all_commands(app: &LccApp) -> Value {
    let keymap = &app.ui.settings.keymap;
    let mut v: Vec<Value> = app
        .session
        .commands()
        .into_iter()
        .map(|c| {
            let mut v = serde_json::to_value(&c).unwrap_or_default();
            if let Some(o) = v.as_object_mut() {
                o.insert("shortcut".into(), json!(crate::shortcuts::shortcut_of(keymap, c.id)));
            }
            v
        })
        .collect();
    for (id, label, _, menu) in crate::menus::ui_commands() {
        let sc = crate::shortcuts::shortcut_of(keymap, id);
        v.push(json!({"id": id, "label": label, "shortcut": sc, "menu": [menu], "enabled": crate::menus::ui_enabled(app, id), "ui": true}));
    }
    Value::Array(v)
}

fn rect_json(r: egui::Rect) -> Value {
    json!([r.left(), r.top(), r.width(), r.height()])
}

pub fn inspect(app: &LccApp, ctx: &egui::Context) -> Value {
    let r = ctx.content_rect();
    // (its own object: one more level of fields in the reply below passes `json!`'s recursion limit)
    let frame_gaps = json!({"hostFrameMs": app.perf.host_frame_ms, "gapMs": app.perf.gap_ms, "maxGapMs": app.perf.max_gap_ms, "maxGapHostMs": app.perf.max_gap_host_ms});
    json!({
        "ui": serde_json::to_value(&app.ui).unwrap_or_default(),
        "window": [r.width(), r.height()],
        "pixelsPerPoint": ctx.pixels_per_point(),
        "canvasRect": app.canvas_rect.map(rect_json),
        "imageRect": app.image_rect.map(rect_json),
        "scroll": {"grid": app.grid_scroll, "filmstrip": app.film_scroll},
        "active": app.session.active().map(|p| p.0),
        "selection": app.session.selection.ids.iter().map(|p| p.0).collect::<Vec<_>>(),
        "activeMask": app.session.active_mask,
        "widgetCount": app.widgets.len(),
        "perf": {"loupeRenders": app.renderer.loupe_log.iter().collect::<Vec<_>>(), "nowMs": crate::now_ms(), "refineAfterS": crate::region::StillRefine::refine_after(app.renderer.last_full_main_ms), "viewFollowed": app.session.view_followed, "viewWorkedOut": app.session.view_worked_out, "frameMs": app.perf.frame_ms, "logicMs": app.perf.logic_ms, "updateMs": app.perf.update_ms, "maxUpdateMs": app.perf.max_update_ms, "frameGaps": frame_gaps, "slowestFrame": app.slowest_frame, "fps": app.perf.fps, "lastRenderMs": app.renderer.last_main_ms, "adjustQuality": app.hq_drag.quality(app.session.interaction.as_ref().filter(|_| app.ui.settings.high_quality_adjustment_previews).map(|i| (i.photo, i.label.as_str()))).name(), "renderQueue": app.renderer.queued(), "rendersInFlight": app.renderer.in_flight(), "pendingSlots": app.renderer.pending_slots(), "mergeRunning": app.merge.busy(), "lastMerge": app.merge.last_result, "rendersDone": app.renderer.completed, "thumbTextures": app.renderer.thumb_textures(), "variantTextures": app.renderer.variant_textures(), "hdrWindowsKept": app.renderer.hdr_windows_kept(), "gpu": (lcc_engine::gpu::ready() && lcc_engine::gpu::available()).then(lcc_engine::gpu::adapter_name).flatten(), "gpuReason": lcc_engine::gpu::unavailable_reason(), "gpuFallback": lcc_engine::gpu::last_fallback(), "gpuWindowDevice": lcc_engine::gpu::on_window_device(), "screenTextures": app.renderer.has_screen_textures(), "mainOnGpu": app.renderer.textures.get(&crate::render::Slot::Main).is_some_and(|t| t.native.is_some()), "loupeSteps": app.loupe_timing.to_json()},
        "loupe": app.loupe_shown.map(|(p, src)| json!({
            "photo": p.0,
            "source": src,
            "pending": app.renderer.is_pending(crate::render::Slot::Main),
            // why its render failed (the loupe then shows a stand-in, or nothing, for good)
            "failed": app.renderer.failure_of(crate::render::Slot::Main, p),
            // zoomed past the whole-frame render: the window rendered at no more than 100 % (pixels of its frame)
            "region": app.region_view.map(|r| json!({"full": [r.full.0, r.full.1], "window": [r.window.x, r.window.y, r.window.w, r.window.h], "tile": r.tile, "pending": app.renderer.tiles_pending()})),
            // …and the same for the Before side of a Before/After view
            "regionBefore": app.region_before_view.map(|r| json!({"full": [r.full.0, r.full.1], "window": [r.window.x, r.window.y, r.window.w, r.window.h], "tile": r.tile, "pending": app.renderer.tiles_pending()})),
        })),
        "hoverPreview": app.hover_preview.as_ref().map(|h| h.label.clone()),
        "status": app.ui.status,
        "copied": app.copied,
        "notices": app.notices,
        "quitPrompt": app.quit_prompt.as_ref().map(crate::QuitPrompt::text),
        // how hard the background face scan may work right now, and why it is judged so (see `panels::faces::scan_pace`)
        "faceScan": {"pace": app.caches.faces_pace, "focused": app.caches.faces_in_front, "pending": app.caches.faces_pending, "indexed": app.caches.faces_indexed},
        "unsaved": app.session.unsaved().map(|(n, e)| json!({"ops": n, "error": e})),
        "libraryProblem": app.library_problem.as_ref().map(crate::panels::library_problem::LibraryProblem::to_json),
        "scan": app.scan.as_ref().map(crate::import::ScanTask::status),
        "export": {"running": app.export.as_ref().map(crate::export_task::ExportTask::status), "queued": app.export_queue.len(), "last": app.last_export_result},
        "import": app.import.as_ref().map(crate::import::ImportTask::status),
        "tasks": app.tasks.labels(),
        // every long-running task in flight, as the activity stack shows them (`activity.list`)
        "activity": app.session.activity.list(),
        // Classic: the list under the module bar's activity area is open (`view.activityList`)
        "activityList": app.activity_list,
        // commands waiting on a native file dialog shown off the UI thread (`pick`)
        "fileDialogs": app.pending_picks.iter().map(|p| p.command.clone()).collect::<Vec<_>>(),
        "memory": memory(app),
    })
}

/// Bytes held by the engine's caches and the renderer's (`ui.inspect` → `memory`).
pub fn memory(app: &LccApp) -> Value {
    let mut v = serde_json::to_value(app.session.memory_report()).unwrap_or_default();
    if let (Some(o), Value::Object(r)) = (v.as_object_mut(), app.renderer.memory()) {
        o.extend(r);
    }
    v
}

/// `ui.pointer` events: each an object whose `kind`, when present, is down, drag, move or up (upstream #4291).
fn pointer_batch_ok(events: &[Value]) -> Result<(), String> {
    for event in events {
        let event = event.as_object().ok_or("ui.pointer: every event must be an object")?;
        let kind = match event.get("kind") {
            None => "move",
            Some(kind) => kind.as_str().ok_or("ui.pointer: `kind` must be a string")?,
        };
        if !matches!(kind, "down" | "drag" | "move" | "up") {
            return Err(format!("ui.pointer: unknown event kind `{kind}` (down, drag, move or up)"));
        }
    }
    Ok(())
}

fn modifiers(p: &Value) -> egui::Modifiers {
    let b = |n: &str| p.get(n).and_then(Value::as_bool).unwrap_or(false);
    egui::Modifiers { alt: b("alt"), ctrl: b("ctrl"), shift: b("shift"), mac_cmd: b("cmd") && cfg!(target_os = "macos"), command: b("cmd") }
}

fn key_from(name: &str) -> Option<egui::Key> {
    egui::Key::from_name(name).or(match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Some(egui::Key::Enter),
        "esc" | "escape" => Some(egui::Key::Escape),
        "delete" | "backspace" => Some(egui::Key::Backspace),
        "left" => Some(egui::Key::ArrowLeft),
        "right" => Some(egui::Key::ArrowRight),
        "up" => Some(egui::Key::ArrowUp),
        "down" => Some(egui::Key::ArrowDown),
        "space" => Some(egui::Key::Space),
        "tab" => Some(egui::Key::Tab),
        "\\" | "backslash" => Some(egui::Key::Backslash),
        "/" | "slash" => Some(egui::Key::Slash),
        "[" => Some(egui::Key::OpenBracket),
        "]" => Some(egui::Key::CloseBracket),
        _ => None,
    })
}

/// Bounds for the numeric control-channel arguments: unbounded values let one request queue
/// billions of synthetic events or allocate a gigantic render and run the app out of memory.
pub const CLICK_COUNT: (u64, u64) = (1, 100);
pub const DRAG_STEPS: (u64, u64) = (1, 1000);
pub const RENDER_SIZE: (u64, u64) = (16, 16384);

/// Reads the non-negative integer `key` from `p` (`default` when absent or null), rejecting
/// non-integers and values outside `min..=max` with a clear error instead of clamping silently.
pub fn bounded_arg(p: &Value, key: &str, default: u64, (min, max): (u64, u64)) -> Result<u64, String> {
    let bad = || format!("`{key}` must be an integer between {min} and {max}");
    let n = match p.get(key) {
        None | Some(Value::Null) => return Ok(default),
        Some(v) => match (v.as_u64(), v.as_f64()) {
            (Some(n), _) => n,
            (None, Some(x)) if x.fract() == 0.0 && x >= 0.0 && x <= u64::MAX as f64 => x as u64,
            _ => return Err(bad()),
        },
    };
    if (min..=max).contains(&n) { Ok(n) } else { Err(bad()) }
}

fn push_drag(app: &mut LccApp, a: egui::Pos2, b: egui::Pos2, steps: u64, m: egui::Modifiers) {
    app.synthetic.push(egui::Event::PointerMoved(a));
    app.synthetic.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    for i in 1..=steps.max(1) {
        let t = i as f32 / steps.max(1) as f32;
        app.synthetic.push(egui::Event::PointerMoved(a + (b - a) * t));
    }
    app.synthetic.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
}

fn widget_rect(app: &LccApp, id: &str) -> Option<egui::Rect> {
    app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The largest coordinate or offset a gesture takes, in points (far past any screen).
const MAX_GESTURE_NUMBER: f64 = 1e6;

/// Every number of a gesture's parameters (and of `ui.pointer`'s events) is finite and within
/// ±[`MAX_GESTURE_NUMBER`]. A larger one turns into an infinite or NaN `f32` position or scroll
/// delta: egui asserts on NaN positions, and a release build would hand them to every widget.
/// `count` and `steps` are bounded where they are read instead (`bounded_arg` refuses a value past its range), and `ui.zoom`'s `factor` (a
/// multiplier, not a position) is checked by its own branch.
pub(crate) fn check_gesture_numbers(method: &str, p: &Value) -> Result<(), String> {
    fn check(method: &str, key: &str, v: &Value) -> Result<(), String> {
        match v {
            Value::Number(n) if !matches!(key, "count" | "steps" | "factor") => match n.as_f64() {
                Some(x) if x.abs() <= MAX_GESTURE_NUMBER => Ok(()),
                _ => Err(format!("{method}: `{key}` must be a number between -1000000 and 1000000")),
            },
            Value::Array(a) => a.iter().try_for_each(|v| check(method, key, v)),
            Value::Object(o) => o.iter().try_for_each(|(k, v)| check(method, k, v)),
            _ => Ok(()),
        }
    }
    check(method, "", p)
}

pub fn handle(app: &mut LccApp, ctx: &egui::Context, req: &ControlRequest) -> Outcome {
    if let Some(o) = crate::surface::handle(app, req) {
        return o;
    }
    let p = &req.params;
    if matches!(
        req.method.as_str(),
        "ui.click" | "ui.move" | "ui.drag" | "ui.clickWidget" | "ui.dragWidget" | "ui.hoverWidget" | "ui.pointer" | "ui.scroll"
    ) && let Err(e) = check_gesture_numbers(&req.method, p)
    {
        return err(e);
    }
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    let f = |k: &str| p.get(k).and_then(Value::as_f64);
    match req.method.as_str() {
        "engine.execute" | "ui.menu.invoke" | "command" => {
            let Some(id) = s("command").or(s("id")) else { return err("missing `command`") };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or(json!({}));
            // a request from outside the app (an agent, a script): the program comes only from
            // what the user set up
            if let Err(e) = lcc_engine::cmd::edit_in::refuse_program_choice(&mut app.session, id, &params) {
                return err(e);
            }
            let r = app.run(id, params);
            ctx.request_repaint();
            wrap(r)
        }
        "engine.commands" => ok(brand_filled(all_commands(app))),
        "ui.menu.list" => ok(brand_filled(serde_json::to_value(crate::menus::menu_entries(app)).unwrap_or_default())),
        "ui.menu.tree" => {
            let tree =
                Value::Array(crate::menubar::menu_bar(app).into_iter().map(|(title, items)| json!({"label": title, "children": items})).collect());
            ok(brand_filled(tree))
        }
        "ui.inspect" => ok(inspect(app, ctx)),
        "ui.widgets" => {
            let filter = s("filter").unwrap_or("");
            ok(Value::Array(
                app.widgets.iter().filter(|(id, _)| id.contains(filter)).map(|(id, r)| json!({"id": id, "rect": rect_json(*r)})).collect(),
            ))
        }
        "ui.clickWidget" | "ui.dragWidget" | "ui.hoverWidget" => {
            let Some(id) = s("id") else { return err("missing `id`") };
            let Some(r) = widget_rect(app, id) else { return err(format!("no widget `{id}` on screen (see ui.widgets)")) };
            // a widget laid out past the window's edge (a panel scrolled away from it) can't take a pointer
            // event: say so instead of answering ok for a click that lands on nothing
            if !ctx.content_rect().intersects(r) {
                return err(format!("widget `{id}` is outside the window (scrolled out of view): scroll it into view first (ui.scroll)"));
            }
            let m = modifiers(p);
            // optional relative position inside the widget (0..1)
            let at = egui::pos2(r.left() + r.width() * f("fx").unwrap_or(0.5) as f32, r.top() + r.height() * f("fy").unwrap_or(0.5) as f32);
            if req.method == "ui.hoverWidget" {
                app.synthetic.push(egui::Event::PointerMoved(at));
            } else if req.method == "ui.clickWidget" {
                let n = match bounded_arg(p, "count", 1, CLICK_COUNT) {
                    Ok(n) => n,
                    Err(e) => return err(e),
                };
                let button = if s("button") == Some("right") { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
                app.synthetic.push(egui::Event::PointerMoved(at));
                for _ in 0..n {
                    app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: m });
                    app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: m });
                }
            } else {
                let to = egui::pos2(
                    f("toX").map(|v| v as f32).unwrap_or(at.x + f("dx").unwrap_or(0.0) as f32),
                    f("toY").map(|v| v as f32).unwrap_or(at.y + f("dy").unwrap_or(0.0) as f32),
                );
                let steps = match bounded_arg(p, "steps", 10, DRAG_STEPS) {
                    Ok(n) => n,
                    Err(e) => return err(e),
                };
                push_drag(app, at, to, steps, m);
            }
            ctx.request_repaint();
            ok(json!({"rect": rect_json(r)}))
        }
        "ui.move" => {
            app.synthetic.push(egui::Event::PointerMoved(egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32)));
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.click" | "ui.drag" => {
            let (count, steps) = match (bounded_arg(p, "count", 1, CLICK_COUNT), bounded_arg(p, "steps", 10, DRAG_STEPS)) {
                (Ok(c), Ok(s)) => (c, s),
                (Err(e), _) | (_, Err(e)) => return err(e),
            };
            let a = egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32);
            let m = modifiers(p);
            let button = if s("button") == Some("right") { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
            if req.method == "ui.drag" {
                let b = egui::pos2(f("toX").unwrap_or(0.0) as f32, f("toY").unwrap_or(0.0) as f32);
                push_drag(app, a, b, steps, m);
            } else {
                app.synthetic.push(egui::Event::PointerMoved(a));
                for _ in 0..count {
                    app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: true, modifiers: m });
                    app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: false, modifiers: m });
                }
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.pointer" => {
            // events in normalized image coordinates of the displayed photo frame
            let Some(img) = app.image_rect else { return err("no image on screen (switch to Detail view)") };
            let Some(events) = p.get("events").and_then(Value::as_array) else { return err("missing `events`") };
            // Check the whole batch first: a mistyped release must not leave a press queued (upstream #4291).
            if let Err(e) = pointer_batch_ok(events) {
                return err(e);
            }
            let m = modifiers(p);
            for e in events {
                let x = e.get("x").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                let y = e.get("y").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                let pos = egui::pos2(img.left() + x * img.width(), img.top() + y * img.height());
                match e.get("kind").and_then(Value::as_str).unwrap_or("move") {
                    "down" => {
                        app.synthetic.push(egui::Event::PointerMoved(pos));
                        app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
                    }
                    "up" => {
                        app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, modifiers: m })
                    }
                    _ => app.synthetic.push(egui::Event::PointerMoved(pos)),
                }
            }
            ctx.request_repaint();
            ok(json!({"events": events.len()}))
        }
        "ui.key" => {
            let Some(k) = s("key").and_then(key_from) else { return err("unknown or missing `key`") };
            let m = modifiers(p);
            // {action?: "press" | "down" | "repeat" | "up"}: down … up holds the key across other
            // input (Space held over a drag); a press is both in one frame; repeat is the system's
            // auto-repeat while the key is held
            let key = |pressed: bool, repeat: bool| egui::Event::Key { key: k, physical_key: None, pressed, repeat, modifiers: m };
            match s("action").unwrap_or("press") {
                "press" => app.synthetic.extend([key(true, false), key(false, false)]),
                "down" => app.synthetic.push(key(true, false)),
                "repeat" => app.synthetic.push(key(true, true)),
                "up" => app.synthetic.push(key(false, false)),
                other => return err(format!("ui.key: action {other:?} (press, down, repeat or up)")),
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.dropFiles" => {
            // every entry must be a string: a script is not told `ok` for a part of what it sent
            let Some(paths) =
                p.get("paths").and_then(Value::as_array).and_then(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
            else {
                return err("ui.dropFiles: paths must be an array of strings");
            };
            if p.get("hover").and_then(Value::as_bool).unwrap_or(false) {
                app.drop_hover = paths;
            } else {
                // the drag ends with the drop, as it does for the OS
                app.drop_hover.clear();
                app.drop_pending = paths;
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.text" => {
            app.synthetic.push(egui::Event::Text(s("text").unwrap_or("").to_string()));
            ctx.request_repaint();
            ok(Value::Null)
        }
        // what the system does for ⌘X / ⌘C / ⌘V in a text field: paste `text`, or (without it)
        // what the system clipboard holds
        "ui.clipboard" => {
            // outside a text field ⌘C / ⌘V are the app's shortcuts (and the macOS menu bar's):
            // agents run those as commands (`develop.copy`, `develop.paste`)
            if !ctx.text_edit_focused() {
                return err("ui.clipboard: no text field has the focus (click one first)");
            }
            match s("action") {
                Some("cut") => app.synthetic.push(egui::Event::Cut),
                Some("copy") => app.synthetic.push(egui::Event::Copy),
                // a present but non-string `text` is an error, never a paste from the host clipboard (upstream #4295)
                Some("paste") => match p.get("text") {
                    Some(Value::String(text)) => app.synthetic.push(egui::Event::Paste(text.clone())),
                    None => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
                    Some(_) => return err("ui.clipboard: `text` must be a string"),
                },
                _ => return err("ui.clipboard: `action` is cut, copy or paste"),
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.scroll" => {
            app.synthetic.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(f("dx").unwrap_or(0.0) as f32, f("dy").unwrap_or(0.0) as f32),
                phase: egui::TouchPhase::Move,
                modifiers: modifiers(p),
            });
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.zoom" => {
            let Some(factor) = f("factor").map(|v| v as f32).filter(|v| v.is_finite() && *v > 0.0) else {
                return err("ui.zoom: factor must be a finite positive number (1 = unchanged)");
            };
            app.synthetic.push(egui::Event::Zoom(factor));
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.set" => {
            // an arrangement of the sidebar is checked, not made whole as a settings file's is
            let sidebar = match p.get("sidebar").map(crate::sidebar::SidebarLayout::parse).transpose() {
                Ok(s) => s,
                Err(e) => return err(e),
            };
            let mut v = serde_json::to_value(&app.ui).unwrap_or_default();
            lcc_develop::presets::deep_merge(&mut v, p);
            match serde_json::from_value::<crate::UiState>(v) {
                Ok(mut u) => {
                    // the editors Edit In starts are the user's to choose, in Settings (as for
                    // `engine.execute` above)
                    let (old, new) = (&app.ui.settings, &u.settings);
                    if (&old.external_editor, &old.photocraft_path) != (&new.external_editor, &new.photocraft_path) {
                        return err("ui.set: `settings.externalEditor` and `settings.photocraftPath` choose the program the app starts, so \
                             they're set only in Settings ▸ External Editing, not over MCP or the control channel"
                            .to_string());
                    }
                    if let Err(error) = crate::docking::set_widths(&mut u, p) {
                        return err(error);
                    }
                    if let Some(sidebar) = sidebar {
                        u.sidebar = sidebar;
                    }
                    // `collapsedSidebar`, as scripts from before `sidebar` set it: the folded sections
                    if let Some(ids) = p.get("collapsedSidebar").and_then(Value::as_array) {
                        use crate::sidebar::SidebarSection;
                        if p.get("sidebar").is_some() {
                            return err("ui.set: give `sidebar` or the older `collapsedSidebar`, not both".to_string());
                        }
                        if let Some(unknown) = ids.iter().find(|id| id.as_str().and_then(SidebarSection::from_id).is_none()) {
                            return err(format!("ui.set: `collapsedSidebar`: unknown section {unknown} ({})", SidebarSection::ids()));
                        }
                        for section in SidebarSection::ALL {
                            u.sidebar.set_collapsed(section, ids.iter().any(|id| id.as_str() == Some(section.id())));
                        }
                    }
                    // the sections' folds now live in `sidebar`; the Classic panels' `panel:*` folds stay
                    u.collapsed_sidebar.retain(|id| crate::sidebar::SidebarSection::from_id(id).is_none());
                    // a script's values get the same range checks as a settings file's
                    u.clamp_ranges();
                    u.toasts = app.ui.toasts.clone();
                    // (and the panel a tool panel took the place of, to put back when it closes)
                    u.tool_return = app.ui.tool_return;
                    u.right_seen = app.ui.right_seen;
                    u.dialog = app.ui.dialog.clone();
                    u.status = app.ui.status.clone();
                    // the masking mode (not settings): a tool armed for a new mask stays armed
                    u.mask_new = app.ui.mask_new.clone();
                    u.tool_photo = app.ui.tool_photo;
                    u.mask_drawing = app.ui.mask_drawing;
                    u.active_mask_component = app.ui.active_mask_component;
                    crate::docking::cancel_replaced_gesture(app, u.docking.as_ref());
                    app.ui = u;
                    crate::i18n::set_language(app.ui.language);
                    ctx.request_repaint();
                    ok(serde_json::to_value(&app.ui).unwrap_or_default())
                }
                Err(e) => err(e),
            }
        }
        "ui.dialog.confirm" => wrap(confirm_dialog(app)),
        "ui.dialog.cancel" => {
            app.ui.dialog = None;
            #[cfg(not(target_arch = "wasm32"))]
            if matches!(
                app.catalog_ui.dialog,
                Some(crate::catalog_ui::CatalogDialog::XmpConflict { .. } | crate::catalog_ui::CatalogDialog::Refine { .. })
            ) {
                app.catalog_ui.dialog = None;
            }
            ok(Value::Null)
        }
        "ui.resize" => {
            for name in ["width", "height"] {
                if p.get(name).is_some() && f(name).is_none() {
                    return err(format!("ui.resize: {name} must be a number"));
                }
            }
            let w = f("width").unwrap_or(1600.0) as f32;
            let h = f("height").unwrap_or(1000.0) as f32;
            let zoom = ctx.zoom_factor();
            let native_scale = ctx.input(|i| i.viewport().native_pixels_per_point).unwrap_or_else(|| ctx.pixels_per_point() / zoom);
            if let Err(e) = crate::headless::resized_viewport([w, h], native_scale, zoom) {
                return err(e);
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(w, h)));
            ok(Value::Null)
        }
        "ui.zoomFactor" => match f("factor") {
            // egui's interface scale (its keyboard shortcuts are off: Cmd+= / Cmd+- zoom the photo)
            Some(factor) if factor.is_finite() && (0.5..=3.0).contains(&factor) => {
                ctx.set_zoom_factor(factor as f32);
                ok(Value::Null)
            }
            _ => err("ui.zoomFactor: factor must be a number from 0.5 to 3"),
        },
        "ui.screenshot" => {
            // never over a photo's original
            if let Some(Err(e)) = s("path").map(|path| app.session.check_write_target(path)) {
                return err(e);
            }
            // views drawn straight from the GPU become pixels first: a window capture over native
            // textures stopped the window presenting (Linux / X11, RADV)
            app.renderer.views_to_pixels();
            ctx.request_repaint();
            Outcome::Screenshot { path: s("path").map(str::to_string), headless: p.get("headless").and_then(Value::as_bool).unwrap_or(false) }
        }
        "ui.render" => {
            let id = match p.get("id").filter(|v| !v.is_null()) {
                Some(v) => match v.as_u64() {
                    Some(n) => Some(lcc_catalog::PhotoId(n)),
                    None => return err("`id` must be a photo id (a whole number)"),
                },
                None => app.session.active(),
            };
            let Some(id) = id else { return err("no photo") };
            if let Some(Err(e)) = s("path").map(|path| app.session.check_write_target(path)) {
                return err(e);
            }
            let size = match bounded_arg(p, "size", 1600, RENDER_SIZE) {
                Ok(n) => n as usize,
                Err(e) => return err(e),
            };
            // `inline` returns the PNG in the reply: a client on another machine can't read `path`
            let inline = p.get("inline").and_then(Value::as_bool).unwrap_or(false);
            match app.session.render_now(id, size, size) {
                Ok(r) => match (s("path"), app.services.png.as_ref()) {
                    (Some(path), Some(png)) => {
                        let bytes = png(&r.image);
                        match app.services.write.as_mut() {
                            Some(w) => wrap(w(path, &bytes).map(|_| json!({"path": path, "width": r.image.width, "height": r.image.height}))),
                            None => err("no writer"),
                        }
                    }
                    (None, Some(png)) if inline => {
                        let bytes = png(&r.image);
                        ok(json!({"width": r.image.width, "height": r.image.height, "png": lcc_engine::base64::encode(&bytes)}))
                    }
                    (None, None) if inline => err("ui.render: this build has no PNG encoder for `inline`"),
                    // a `path` with no encoder must not report success without writing (upstream #4294)
                    (Some(_), None) => err("no encoder"),
                    (None, _) => ok(json!({"width": r.image.width, "height": r.image.height, "histogram": r.histogram})),
                },
                Err(e) => err(e),
            }
        }
        "app.quit" => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            ok(Value::Null)
        }
        other => err(format!("unknown method `{other}`")),
    }
}

/// OK on the open dialog (`ui.dialog.confirm`, a surface's OK key).
/// Confirms the open dialog as its button does (`ui.dialog.confirm`, and a control surface's Confirm key).
pub(crate) fn confirm_dialog(app: &mut LccApp) -> Result<Value, String> {
    match app.ui.dialog.take() {
        Some(d) => {
            let r = crate::panels::dialogs::confirm_dialog(app, &d);
            // the import review and the rule editor stay open on an error, as with their buttons
            if (r.is_err()
                && matches!(
                    d,
                    crate::state::Dialog::Import { .. }
                        | crate::state::Dialog::SamModel { .. }
                        | crate::state::Dialog::SmartRules { .. }
                        | crate::state::Dialog::SaveEffect { .. }
                        | crate::state::Dialog::CaptureTime { .. }
                ))
                || (r.is_ok() && crate::panels::dialogs::keeps_open(app, &d))
                || (r.is_err() && crate::lightroom_destination::keeps_open_on_error(app, &d))
            {
                app.ui.dialog = Some(d);
            }
            r
        }
        // Read / Save Metadata's overwrite question lives with the catalog dialogs: confirm it as its button does
        #[cfg(not(target_arch = "wasm32"))]
        None => match app.catalog_ui.dialog.take() {
            Some(crate::catalog_ui::CatalogDialog::XmpConflict { command, ids, .. }) => app.run(&command, json!({"ids": ids, "confirmed": true})),
            Some(crate::catalog_ui::CatalogDialog::Refine { .. }) => app.run("library.refine", json!({"confirmed": true})),
            other => {
                app.catalog_ui.dialog = other;
                Err("no dialog open".into())
            }
        },
        // the web build has no Read / Save Metadata overwrite question
        #[cfg(target_arch = "wasm32")]
        None => Err("no dialog open".into()),
    }
}

/// Shown when an export has nowhere to go (no folder typed or chosen, and no home folder to default to).
pub const NO_EXPORT_FOLDER: &str = "Choose an export folder first.";

/// Default export folder: `~/Pictures/<app> Exports` (`%USERPROFILE%\Pictures\<app> Exports`
/// on Windows, where `HOME` usually isn't set). Empty when no home folder is known.
pub fn default_export_dir() -> String {
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) } else { std::env::var_os("HOME") };
    home.filter(|h| !h.is_empty())
        .map(|h| std::path::PathBuf::from(h).join("Pictures").join(format!("{} Exports", lcc_brand::DISPLAY_NAME)).to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Export the selected photos (UI command `app.export`). Params: see
/// [`lcc_engine::export::ExportOptions::from_json`], plus `dir` (output folder) or `path`
/// (exact output file, single photo), `ids` (default: the selection, else the active photo).
pub fn export_active(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    use lcc_engine::export::{Destination, ExportOptions, export_batch};
    let asked = p;
    let p = &app.session.export_params(p)?;
    // the format follows `path` (JPEG when it names none), and `bitDepth` is checked against it
    let opts = ExportOptions::from_params(p).map_err(|e| e.to_string())?;
    let ids: Vec<_> = match p.get("ids").filter(|v| !v.is_null()) {
        // `ids` that aren't photo ids are an error, not "export the selection"
        Some(v) => v
            .as_array()
            .ok_or("app.export: `ids` must be a list of photo ids")?
            .iter()
            .map(|v| v.as_u64().map(lcc_catalog::PhotoId).ok_or("app.export: `ids` must be photo ids (whole numbers)"))
            .collect::<Result<_, _>>()?,
        None if !app.session.selection.ids.is_empty() => {
            // Batch order (and `{seq}`) follows the grid order.
            let sel: std::collections::HashSet<_> = app.session.selection.ids.iter().copied().collect();
            let mut v: Vec<_> = app.session.visible().iter().copied().filter(|id| sel.contains(id)).collect();
            let shown: std::collections::HashSet<_> = v.iter().copied().collect();
            v.extend(app.session.selection.ids.iter().filter(|id| !shown.contains(id)));
            v
        }
        None => app.session.active().into_iter().collect(),
    };
    if ids.is_empty() {
        return Err("no photo selected".into());
    }
    // An exact output file needs no folder. A folder given but left blank (the Export dialog's Folder
    // field cleared) is an error, like Lightroom refusing to export to an unspecified folder, rather
    // than a silent write into the working directory. No folder at all (e.g. File → Export with
    // Preset): the last export's, else the default. "Same folder as original photo" needs none:
    // each photo goes to its own folder.
    let exact = p.get("path").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string);
    // "Choose folder later": ask for the folder now, then run again with it (and these photos,
    // whatever is selected by the time the folder dialog closes)
    if opts.destination_mode == lcc_engine::export::DestinationMode::ChooseLater && exact.is_none() && p.get("dir").is_none() {
        let mut again = asked.clone();
        if let Some(o) = again.as_object_mut() {
            o.insert("ids".into(), json!(ids));
        }
        let req = crate::pick::PickRequest::folder(crate::i18n::tr("Choose Export Folder"));
        let picked = crate::pick::ask(app, "app.export", &again, "dir", req, |s| s.pick_folder.as_mut().map(|pick| pick().into_iter().collect()));
        return match picked {
            crate::pick::Picked::Now(paths) => match paths.into_iter().next() {
                Some(d) => {
                    crate::json_set(&mut again, "dir", json!(d));
                    export_active(app, &again)
                }
                // cancelled: nothing exported
                None => Ok(Value::Null),
            },
            crate::pick::Picked::Later => Ok(Value::Null),
            crate::pick::Picked::Unavailable => Err("no folder dialog here: pass `dir` for a chooseLater export".into()),
        };
    }
    let dir = match p.get("dir").and_then(Value::as_str) {
        Some(d) => d.trim().to_string(),
        None => {
            let last_dir =
                app.session.last_export.as_ref().and_then(|l| l.get("dir")).and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty());
            last_dir.map(str::to_string).unwrap_or_else(default_export_dir)
        }
    };
    let own_folders = opts.destination_mode == lcc_engine::export::DestinationMode::SameAsOriginal;
    if dir.is_empty() && exact.is_none() && !own_folders {
        return Err(NO_EXPORT_FOLDER.into());
    }
    let to = Destination { dir: dir.clone(), exact };
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false) && app.services.write_shared.is_some();
    // Add to Apple Photos: refused here, before anything is written, when it can't be done. An export that
    // waits behind a running one reserves Photos when it starts (the running one may hold it; issue #2965).
    let after = if background && app.export.is_some() { None } else { Some(app.session.after_export(&opts)?) };
    let out = if background {
        let items = lcc_engine::export::prepare_batch(&mut app.session, &ids, &opts)?;
        let r = crate::export_task::start(app, items, opts, to, after)?;
        let destination = p.get("pluginDestination").filter(|d| d.is_object()).cloned();
        if r.get("queued").is_some() {
            if let Some(q) = app.export_queue.back_mut() {
                q.plugin_destination = destination;
            }
        } else if let Some(t) = app.export.as_mut() {
            t.plugin_destination = destination;
        }
        r
    } else {
        let after = match after {
            Some(after) => after,
            None => app.session.after_export(&opts)?,
        };
        let w = app.services.write.as_mut().ok_or("no writer")?;
        let files = export_batch(&mut app.session, &ids, &opts, &to, &mut |path, bytes| w(path, bytes), &|path| std::path::Path::new(path).exists())?;
        lcc_engine::export::mark_exported(&mut app.session, &files);
        // plug-in export hooks and destination, as after a background export
        #[cfg(not(target_arch = "wasm32"))]
        let plugins = lcc_engine::cmd::plugins::after_export(&mut app.session, &files, p.get("pluginDestination"));
        #[cfg(target_arch = "wasm32")]
        let plugins: Option<Vec<Value>> = None;
        let mut out = json!({"files": files});
        if let Some(r) = plugins {
            crate::json_set(&mut out, "plugins", json!(r));
        }
        // Photos may take minutes (and waits for the permission prompt): in the desktop app on a
        // worker, never on this (UI) thread; the app announces the outcome and
        // `export.photosImports` reports it. A window-less run (snapshot scripts) waits.
        if let Some(photos) = after.finish(&app.session, &files) {
            crate::json_set(&mut out, "applePhotos", photos);
        }
        out
    };
    // remember for Export with Previous (and to prefill the dialog)
    let mut last = p.clone();
    if let Some(o) = last.as_object_mut() {
        o.remove("ids");
        o.remove("path");
        o.insert("dir".into(), json!(dir));
    }
    app.session.last_export = Some(last);
    // the Previous Export source in the Library panel
    app.session.record_export(&ids);
    let _ = app.session.save_prefs();
    Ok(out)
}

pub fn save_screenshot(app: &mut LccApp, image: &egui::ColorImage, path: Option<&str>) -> Value {
    let [w, h] = image.size;
    let Some(path) = path else {
        return json!({"ok": true, "result": {"width": w, "height": h}});
    };
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    let Some(img) = lcc_raster::Rgba8::from_bytes(w, h, &rgba) else {
        return json!({"ok": false, "error": "bad screenshot buffer"});
    };
    let Some(png) = app.services.png.as_ref() else { return json!({"ok": false, "error": "no encoder"}) };
    let bytes = png(&img);
    match app.services.write.as_mut() {
        Some(wr) => match wr(path, &bytes) {
            Ok(()) => json!({"ok": true, "result": {"path": path, "width": w, "height": h}}),
            Err(e) => json!({"ok": false, "error": e}),
        },
        None => json!({"ok": false, "error": "no writer configured"}),
    }
}

#[cfg(test)]
mod arg_bounds_tests {
    use super::*;

    #[test]
    fn huge_counts_steps_and_sizes_are_rejected() {
        for (key, b) in [("count", CLICK_COUNT), ("steps", DRAG_STEPS), ("size", RENDER_SIZE)] {
            for v in [json!(1e10), json!(10_000_000_000u64), json!(u64::MAX), json!(-1), json!(2.5), json!("5")] {
                let e = bounded_arg(&json!({ key: v }), key, b.0, b).unwrap_err();
                assert!(e.contains(key), "{e}");
            }
        }
    }

    #[test]
    fn defaults_and_in_range_values_pass() {
        assert_eq!(bounded_arg(&json!({}), "count", 1, CLICK_COUNT), Ok(1));
        assert_eq!(bounded_arg(&json!({"count": null}), "count", 1, CLICK_COUNT), Ok(1));
        assert_eq!(bounded_arg(&json!({"count": 2}), "count", 1, CLICK_COUNT), Ok(2));
        assert_eq!(bounded_arg(&json!({"steps": 20.0}), "steps", 10, DRAG_STEPS), Ok(20));
        assert_eq!(bounded_arg(&json!({"size": 16384}), "size", 1600, RENDER_SIZE), Ok(16384));
        assert!(bounded_arg(&json!({"count": 0}), "count", 1, CLICK_COUNT).is_err());
    }
}

/// Upstream #4291 / #4294: a bad `ui.pointer` batch queues nothing; a `ui.render` `path` needs an encoder.
#[cfg(test)]
mod hostile_gesture_tests {
    use super::*;

    fn reply(app: &mut LccApp, method: &str, params: Value) -> Value {
        let (req, _) = ControlRequest::new(method, params);
        let Outcome::Done(reply) = handle(app, &egui::Context::default(), &req) else { panic!("{method} must reply synchronously") };
        reply
    }

    fn app() -> LccApp {
        let mut app = LccApp::new(lcc_engine::Session::with_demo(), crate::Services { png: None, ..Default::default() });
        app.image_rect = Some(egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)));
        app
    }

    #[test]
    fn invalid_pointer_events_reject_the_whole_batch() {
        let mut app = app();
        app.synthetic = vec![egui::Event::Text("pending".into())];
        let bad_kinds = [json!("upp"), json!(null), json!(false), json!(0), json!([]), json!({})].map(|k| json!({"kind": k}));
        let bad_events = [json!(null), json!(false), json!(0), json!("up"), json!([])];
        for event in bad_kinds.into_iter().chain(bad_events) {
            let r = reply(&mut app, "ui.pointer", json!({"events": [{"kind": "down"}, event]}));
            assert_eq!(r["ok"], false, "{event}: {r}");
            assert!(r["error"].as_str().is_some_and(|e| e.contains("ui.pointer")), "{r}");
            assert_eq!(app.synthetic, vec![egui::Event::Text("pending".into())], "{event}: a press was queued");
        }
    }

    #[test]
    fn pointer_drag_move_and_default_kinds_keep_their_events() {
        let mut app = app();
        app.synthetic.clear();
        let r = reply(&mut app, "ui.pointer", json!({"events": [{"kind": "down"}, {"kind": "drag"}, {"kind": "move"}, {}, {"kind": "up"}]}));
        assert_eq!(r["ok"], true, "{r}");
        let presses = app.synthetic.iter().filter(|e| matches!(e, egui::Event::PointerButton { .. })).count();
        assert_eq!(presses, 2, "one press and one release");
    }

    #[test]
    fn render_to_a_path_needs_a_png_encoder() {
        let mut app = app();
        let path = std::env::temp_dir().join(format!("render-no-encoder-{}.png", std::process::id()));
        let r = reply(&mut app, "ui.render", json!({"size": 16, "path": path.to_string_lossy()}));
        assert_eq!(r["ok"], false, "{r}");
        assert!(!path.exists(), "nothing written");
        let r = reply(&mut app, "ui.render", json!({"size": 16}));
        assert_eq!(r["ok"], true, "{r}");
        assert!(r["result"]["width"].as_u64().is_some_and(|w| w > 0), "{r}");
    }
}
