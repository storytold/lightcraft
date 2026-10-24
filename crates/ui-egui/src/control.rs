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
//! - `ui.screenshot {path?, headless?}` (headless: CPU-rendered, no compositor needed), `ui.resize {width, height}`, `ui.render {id?, size?, path?}`, `app.quit`

use std::sync::mpsc::Sender;

use serde_json::{Value, json};

use crate::LightcraftApp;

pub type ControlResponse = Value;

pub struct ControlRequest {
    pub method: String,
    pub params: Value,
    pub reply: Sender<ControlResponse>,
    /// Who has the request: [`PENDING`], [`TAKEN`] (the app runs it) or [`ABANDONED`] (the
    /// sender gave up on it, so the app must not run it late: issue #718).
    pub state: std::sync::Arc<std::sync::atomic::AtomicU8>,
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
        (Self { method: method.into(), params, reply: tx, state: Default::default() }, rx)
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

pub fn all_commands(app: &LightcraftApp) -> Value {
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
    for (id, label, sc, menu) in crate::menus::ui_commands() {
        let sc = crate::shortcuts::binding(keymap, id, *sc);
        v.push(json!({"id": id, "label": label, "shortcut": sc, "menu": [menu], "enabled": crate::menus::ui_enabled(app, id), "ui": true}));
    }
    Value::Array(v)
}

fn rect_json(r: egui::Rect) -> Value {
    json!([r.left(), r.top(), r.width(), r.height()])
}

pub fn inspect(app: &LightcraftApp, ctx: &egui::Context) -> Value {
    let r = ctx.content_rect();
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
        "perf": {"viewFollowed": app.session.view_followed, "viewWorkedOut": app.session.view_worked_out, "frameMs": app.perf.frame_ms, "logicMs": app.perf.logic_ms, "updateMs": app.perf.update_ms, "maxUpdateMs": app.perf.max_update_ms, "fps": app.perf.fps, "lastRenderMs": app.renderer.last_main_ms, "renderQueue": app.renderer.queued(), "rendersInFlight": app.renderer.in_flight(), "pendingSlots": app.renderer.pending_slots(), "mergeRunning": app.merge.busy(), "lastMerge": app.merge.last_result, "rendersDone": app.renderer.completed, "thumbTextures": app.renderer.thumb_textures(), "variantTextures": app.renderer.variant_textures(), "gpu": (lightcraft_engine::gpu::ready() && lightcraft_engine::gpu::available()).then(lightcraft_engine::gpu::adapter_name).flatten(), "gpuReason": lightcraft_engine::gpu::unavailable_reason(), "gpuFallback": lightcraft_engine::gpu::last_fallback()},
        "loupe": app.loupe_shown.map(|(p, src)| json!({
            "photo": p.0,
            "source": src,
            "pending": app.renderer.is_pending(crate::render::Slot::Main),
            // why its render failed (the loupe then shows a stand-in, or nothing, for good)
            "failed": app.renderer.failure_of(crate::render::Slot::Main, p),
            // zoomed past the whole-frame render: the window rendered at no more than 100 % (pixels of its frame)
            "region": app.region_view.map(|r| json!({"full": [r.full.0, r.full.1], "window": [r.window.x, r.window.y, r.window.w, r.window.h], "pending": app.renderer.is_pending(crate::render::Slot::Region)})),
            // …and the same for the Before side of a Before/After view
            "regionBefore": app.region_before_view.map(|r| json!({"full": [r.full.0, r.full.1], "window": [r.window.x, r.window.y, r.window.w, r.window.h], "pending": app.renderer.is_pending(crate::render::Slot::RegionBefore)})),
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
        "export": {"running": app.export.as_ref().map(crate::export_task::ExportTask::status), "last": app.last_export_result},
        "import": app.import.as_ref().map(crate::import::ImportTask::status),
        "tasks": app.tasks.labels(),
        // every long-running task in flight, as the activity stack shows them (`activity.list`)
        "activity": app.session.activity.list(),
        // commands waiting on a native file dialog shown off the UI thread (`pick`)
        "fileDialogs": app.pending_picks.iter().map(|p| p.command.clone()).collect::<Vec<_>>(),
        "memory": memory(app),
    })
}

/// Bytes held by the engine's caches and the renderer's (`ui.inspect` → `memory`).
pub fn memory(app: &LightcraftApp) -> Value {
    let mut v = serde_json::to_value(app.session.memory_report()).unwrap_or_default();
    if let (Some(o), Value::Object(r)) = (v.as_object_mut(), app.renderer.memory()) {
        o.extend(r);
    }
    v
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

/// Maximum click count allowed in a single `ui.click` or `ui.clickWidget` request.
/// Legitimate UI automation uses 1 (single), 2 (double), or 3 (triple). 10 allows
/// burst testing while keeping pointer event draining strictly under 0.35 s at 60 FPS.
pub const MAX_CLICK_COUNT: u32 = 10;

/// Maximum intermediate steps allowed for a synthetic drag gesture in `ui.drag` or `ui.dragWidget`.
/// Default is 10; 50 provides smooth interpolation while keeping draining time sub-second (~0.88 s at 60 FPS).
pub const MAX_DRAG_STEPS: u32 = 50;

/// Aggregate pending synthetic event budget in `app.synthetic`.
/// Pointer events drain 1 per frame; capping the queue at 128 bounds worst-case draining latency
/// to ~2.1 s at 60 FPS while easily accommodating consecutive drags or multi-point gestures.
pub const MAX_PENDING_EVENTS: usize = 128;

fn parse_bounded_u32(p: &Value, key: &str, default: u32, min: u32, max: u32, method: &str) -> Result<u32, String> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::Number(n)) => {
            if let Some(val) = n.as_u64() {
                if val < min as u64 {
                    Err(format!("{method}: `{key}` must be at least {min}"))
                } else if val > max as u64 {
                    Err(format!("{method}: `{key}` must be at most {max} (got {val})"))
                } else {
                    Ok(val as u32)
                }
            } else if let Some(i) = n.as_i64() {
                if i < min as i64 {
                    Err(format!("{method}: `{key}` must be at least {min} (got {i})"))
                } else {
                    Err(format!("{method}: `{key}` must be an integer between {min} and {max}"))
                }
            } else {
                Err(format!("{method}: `{key}` must be an integer between {min} and {max}"))
            }
        }
        Some(val) => Err(format!("{method}: `{key}` must be an integer, not {val}")),
    }
}

fn parse_click_count(p: &Value, method: &str) -> Result<u32, String> {
    parse_bounded_u32(p, "count", 1, 1, MAX_CLICK_COUNT, method)
}

fn parse_drag_steps(p: &Value, method: &str) -> Result<u32, String> {
    parse_bounded_u32(p, "steps", 10, 1, MAX_DRAG_STEPS, method)
}

fn ensure_event_budget(app: &LightcraftApp, additional: usize, method: &str) -> Result<(), String> {
    let pending = app.synthetic.len();
    if pending.saturating_add(additional) > MAX_PENDING_EVENTS {
        Err(format!(
            "{method}: synthetic event budget exceeded: {pending} pending events, cannot enqueue {additional} more (maximum is {MAX_PENDING_EVENTS})"
        ))
    } else {
        Ok(())
    }
}

fn push_event(app: &mut LightcraftApp, event: egui::Event, method: &str) -> Result<(), String> {
    ensure_event_budget(app, 1, method)?;
    app.synthetic.push(event);
    Ok(())
}

/// Returns true if a pointer button is currently held down in egui or scheduled to remain held down in `app.synthetic`.
fn pointer_is_down(app: &LightcraftApp, ctx: &egui::Context) -> bool {
    let mut net_synthetic_down = 0i32;
    for ev in &app.synthetic {
        if let egui::Event::PointerButton { pressed, .. } = ev {
            if *pressed {
                net_synthetic_down += 1;
            } else {
                net_synthetic_down -= 1;
            }
        }
    }
    let egui_down = if ctx.input(|i| i.pointer.any_down()) { 1 } else { 0 };
    (egui_down + net_synthetic_down) > 0
}

fn push_drag(app: &mut LightcraftApp, a: egui::Pos2, b: egui::Pos2, steps: u32, m: egui::Modifiers) {
    let count = (steps as usize).saturating_add(3);
    app.synthetic.reserve(count);
    app.synthetic.push(egui::Event::PointerMoved(a));
    app.synthetic.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        app.synthetic.push(egui::Event::PointerMoved(a + (b - a) * t));
    }
    app.synthetic.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
}

fn widget_rect(app: &LightcraftApp, id: &str) -> Option<egui::Rect> {
    app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

pub fn handle(app: &mut LightcraftApp, ctx: &egui::Context, req: &ControlRequest) -> Outcome {
    let p = &req.params;
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    let f = |k: &str| p.get(k).and_then(Value::as_f64);
    match req.method.as_str() {
        "engine.execute" | "ui.menu.invoke" | "command" => {
            let Some(id) = s("command").or(s("id")) else { return err("missing `command`") };
            let params = p.get("params").cloned().filter(|v| !v.is_null()).unwrap_or(json!({}));
            let r = app.run(id, params);
            ctx.request_repaint();
            wrap(r)
        }
        "engine.commands" => ok(all_commands(app)),
        "ui.menu.list" => ok(serde_json::to_value(crate::menus::menu_entries(app)).unwrap_or_default()),
        "ui.menu.tree" => {
            ok(Value::Array(crate::menubar::menu_bar(app).into_iter().map(|(title, items)| json!({"label": title, "children": items})).collect()))
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
            if req.method == "ui.clickWidget" {
                let n = match parse_click_count(p, "ui.clickWidget") {
                    Ok(n) => n,
                    Err(e) => return err(e),
                };
                let Some(r) = widget_rect(app, id) else { return err(format!("no widget `{id}` on screen (see ui.widgets)")) };
                let release_needed = pointer_is_down(app, ctx);
                let additional = (if release_needed { 1 } else { 0 }) + 1 + 2 * (n as usize);
                if let Err(e) = ensure_event_budget(app, additional, "ui.clickWidget") {
                    return err(e);
                }
                let m = modifiers(p);
                let at = egui::pos2(r.left() + r.width() * f("fx").unwrap_or(0.5) as f32, r.top() + r.height() * f("fy").unwrap_or(0.5) as f32);
                let button = if s("button") == Some("right") { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
                app.synthetic.reserve(additional);
                if release_needed {
                    app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: egui::Modifiers::NONE });
                }
                app.synthetic.push(egui::Event::PointerMoved(at));
                for _ in 0..n {
                    app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: true, modifiers: m });
                    app.synthetic.push(egui::Event::PointerButton { pos: at, button, pressed: false, modifiers: m });
                }
                ctx.request_repaint();
                ok(json!({"rect": rect_json(r)}))
            } else if req.method == "ui.dragWidget" {
                let steps = match parse_drag_steps(p, "ui.dragWidget") {
                    Ok(s) => s,
                    Err(e) => return err(e),
                };
                let Some(r) = widget_rect(app, id) else { return err(format!("no widget `{id}` on screen (see ui.widgets)")) };
                let release_needed = pointer_is_down(app, ctx);
                let additional = (if release_needed { 1 } else { 0 }) + (steps as usize) + 3;
                if let Err(e) = ensure_event_budget(app, additional, "ui.dragWidget") {
                    return err(e);
                }
                let m = modifiers(p);
                let at = egui::pos2(r.left() + r.width() * f("fx").unwrap_or(0.5) as f32, r.top() + r.height() * f("fy").unwrap_or(0.5) as f32);
                let to = egui::pos2(
                    f("toX").map(|v| v as f32).unwrap_or(at.x + f("dx").unwrap_or(0.0) as f32),
                    f("toY").map(|v| v as f32).unwrap_or(at.y + f("dy").unwrap_or(0.0) as f32),
                );
                if release_needed {
                    app.synthetic.push(egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                push_drag(app, at, to, steps, m);
                ctx.request_repaint();
                ok(json!({"rect": rect_json(r)}))
            } else {
                let Some(r) = widget_rect(app, id) else { return err(format!("no widget `{id}` on screen (see ui.widgets)")) };
                let at = egui::pos2(r.left() + r.width() * f("fx").unwrap_or(0.5) as f32, r.top() + r.height() * f("fy").unwrap_or(0.5) as f32);
                if let Err(e) = push_event(app, egui::Event::PointerMoved(at), "ui.hoverWidget") {
                    return err(e);
                }
                ctx.request_repaint();
                ok(json!({"rect": rect_json(r)}))
            }
        }
        "ui.move" => {
            let pos = egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32);
            if let Err(e) = push_event(app, egui::Event::PointerMoved(pos), "ui.move") {
                return err(e);
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.click" => {
            let n = match parse_click_count(p, "ui.click") {
                Ok(n) => n,
                Err(e) => return err(e),
            };
            let a = egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32);
            let release_needed = pointer_is_down(app, ctx);
            let additional = (if release_needed { 1 } else { 0 }) + 1 + 2 * (n as usize);
            if let Err(e) = ensure_event_budget(app, additional, "ui.click") {
                return err(e);
            }
            let m = modifiers(p);
            let button = if s("button") == Some("right") { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
            app.synthetic.reserve(additional);
            if release_needed {
                app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: false, modifiers: egui::Modifiers::NONE });
            }
            app.synthetic.push(egui::Event::PointerMoved(a));
            for _ in 0..n {
                app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: true, modifiers: m });
                app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: false, modifiers: m });
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.drag" => {
            let steps = match parse_drag_steps(p, "ui.drag") {
                Ok(s) => s,
                Err(e) => return err(e),
            };
            let release_needed = pointer_is_down(app, ctx);
            let additional = (if release_needed { 1 } else { 0 }) + (steps as usize) + 3;
            if let Err(e) = ensure_event_budget(app, additional, "ui.drag") {
                return err(e);
            }
            let a = egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32);
            let b = egui::pos2(f("toX").unwrap_or(0.0) as f32, f("toY").unwrap_or(0.0) as f32);
            let m = modifiers(p);
            if release_needed {
                app.synthetic.push(egui::Event::PointerButton {
                    pos: a,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            push_drag(app, a, b, steps, m);
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.pointer" => {
            // events in normalized image coordinates of the displayed photo frame
            let Some(img) = app.image_rect else { return err("no image on screen (switch to Detail view)") };
            let Some(events) = p.get("events").and_then(Value::as_array) else { return err("missing `events`") };
            let mut event_count: usize = 0;
            let mut is_pure_release = !events.is_empty();
            for e in events {
                match e.get("kind").and_then(Value::as_str).unwrap_or("move") {
                    "down" => {
                        event_count = event_count.saturating_add(2);
                        is_pure_release = false;
                    }
                    "up" => {
                        event_count = event_count.saturating_add(1);
                    }
                    _ => {
                        event_count = event_count.saturating_add(1);
                        is_pure_release = false;
                    }
                }
            }
            if event_count > (MAX_DRAG_STEPS as usize).saturating_mul(2) {
                return err(format!("ui.pointer: too many events in gesture (got {}, maximum is {})", events.len(), MAX_DRAG_STEPS * 2));
            }
            // A single pure release event ("up") relieving an in-flight held pointer is permitted through even if the queue
            // is at the budget limit (up to MAX_PENDING_EVENTS + 1), ensuring that a gesture release cannot be dropped and
            // leave the pointer stuck down, while strictly preventing repeated release requests or multi-event batches from
            // growing the queue without bound.
            let is_emergency_release = is_pure_release && pointer_is_down(app, ctx) && event_count == 1 && app.synthetic.len() <= MAX_PENDING_EVENTS;

            if !is_emergency_release {
                match ensure_event_budget(app, event_count, "ui.pointer") {
                    Ok(()) => {}
                    Err(e) => return err(e),
                }
            }
            let m = modifiers(p);
            app.synthetic.reserve(event_count);
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
            if let Err(e) = ensure_event_budget(app, 2, "ui.key") {
                return err(e);
            }
            let m = modifiers(p);
            app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m });
            app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: m });
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
            let t = s("text").unwrap_or("").to_string();
            if let Err(e) = push_event(app, egui::Event::Text(t), "ui.text") {
                return err(e);
            }
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
                Some("cut") => {
                    if let Err(e) = push_event(app, egui::Event::Cut, "ui.clipboard") {
                        return err(e);
                    }
                }
                Some("copy") => {
                    if let Err(e) = push_event(app, egui::Event::Copy, "ui.clipboard") {
                        return err(e);
                    }
                }
                Some("paste") => match s("text") {
                    Some(text) => {
                        if let Err(e) = push_event(app, egui::Event::Paste(text.to_string()), "ui.clipboard") {
                            return err(e);
                        }
                    }
                    None => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
                },
                _ => return err("ui.clipboard: `action` is cut, copy or paste"),
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.scroll" => {
            let ev = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(f("dx").unwrap_or(0.0) as f32, f("dy").unwrap_or(0.0) as f32),
                phase: egui::TouchPhase::Move,
                modifiers: modifiers(p),
            };
            if let Err(e) = push_event(app, ev, "ui.scroll") {
                return err(e);
            }
            ctx.request_repaint();
            ok(Value::Null)
        }
        "ui.zoom" => {
            let Some(factor) = f("factor").map(|v| v as f32).filter(|v| v.is_finite() && *v > 0.0) else {
                return err("ui.zoom: factor must be a finite positive number (1 = unchanged)");
            };
            if let Err(e) = push_event(app, egui::Event::Zoom(factor), "ui.zoom") {
                return err(e);
            }
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
            lightcraft_develop::presets::deep_merge(&mut v, p);
            match serde_json::from_value::<crate::UiState>(v) {
                Ok(mut u) => {
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
                    u.collapsed_sidebar.clear();
                    u.toast = app.ui.toast.clone();
                    u.dialog = app.ui.dialog.clone();
                    u.status = app.ui.status.clone();
                    crate::docking::cancel_replaced_gesture(app, u.docking.as_ref());
                    app.ui = u;
                    crate::i18n::set_language(app.ui.language);
                    ctx.request_repaint();
                    ok(serde_json::to_value(&app.ui).unwrap_or_default())
                }
                Err(e) => err(e),
            }
        }
        "ui.dialog.confirm" => match app.ui.dialog.take() {
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
                wrap(r)
            }
            None => err("no dialog open"),
        },
        "ui.dialog.cancel" => {
            app.ui.dialog = None;
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
            ctx.request_repaint();
            Outcome::Screenshot { path: s("path").map(str::to_string), headless: p.get("headless").and_then(Value::as_bool).unwrap_or(false) }
        }
        "ui.render" => {
            let id = match p.get("id").filter(|v| !v.is_null()) {
                Some(v) => match v.as_u64() {
                    Some(n) => Some(lightcraft_catalog::PhotoId(n)),
                    None => return err("`id` must be a photo id (a whole number)"),
                },
                None => app.session.active(),
            };
            let Some(id) = id else { return err("no photo") };
            if let Some(Err(e)) = s("path").map(|path| app.session.check_write_target(path)) {
                return err(e);
            }
            let size = p.get("size").and_then(Value::as_u64).unwrap_or(1600) as usize;
            match app.session.render_now(id, size, size) {
                Ok(r) => match (s("path"), app.services.png.as_ref()) {
                    (Some(path), Some(png)) => {
                        let bytes = png(&r.image);
                        match app.services.write.as_mut() {
                            Some(w) => wrap(w(path, &bytes).map(|_| json!({"path": path, "width": r.image.width, "height": r.image.height}))),
                            None => err("no writer"),
                        }
                    }
                    _ => ok(json!({"width": r.image.width, "height": r.image.height, "histogram": r.histogram})),
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

/// Shown when an export has nowhere to go (no folder typed or chosen, and no home folder to default to).
pub const NO_EXPORT_FOLDER: &str = "Choose an export folder first.";

/// Default export folder: `~/Pictures/LightCraft Exports` (`%USERPROFILE%\Pictures\LightCraft Exports`
/// on Windows, where `HOME` usually isn't set). Empty when no home folder is known.
pub fn default_export_dir() -> String {
    let home = if cfg!(windows) { std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) } else { std::env::var_os("HOME") };
    home.filter(|h| !h.is_empty())
        .map(|h| std::path::PathBuf::from(h).join("Pictures").join("LightCraft Exports").to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Export the selected photos (UI command `app.export`). Params: see
/// [`lightcraft_engine::export::ExportOptions::from_json`], plus `dir` (output folder) or `path`
/// (exact output file, single photo), `ids` (default: the selection, else the active photo).
pub fn export_active(app: &mut LightcraftApp, p: &Value) -> Result<Value, String> {
    use lightcraft_engine::export::{Destination, ExportOptions, export_batch};
    let p = &app.session.export_params(p)?;
    // the format follows `path` (JPEG when it names none), and `bitDepth` is checked against it
    let opts = ExportOptions::from_params(p).map_err(|e| e.to_string())?;
    let ids: Vec<_> = match p.get("ids").filter(|v| !v.is_null()) {
        // `ids` that aren't photo ids are an error, not "export the selection"
        Some(v) => v
            .as_array()
            .ok_or("app.export: `ids` must be a list of photo ids")?
            .iter()
            .map(|v| v.as_u64().map(lightcraft_catalog::PhotoId).ok_or("app.export: `ids` must be photo ids (whole numbers)"))
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
    let dir = match p.get("dir").and_then(Value::as_str) {
        Some(d) => d.trim().to_string(),
        None => {
            let last_dir =
                app.session.last_export.as_ref().and_then(|l| l.get("dir")).and_then(Value::as_str).map(str::trim).filter(|d| !d.is_empty());
            last_dir.map(str::to_string).unwrap_or_else(default_export_dir)
        }
    };
    let own_folders = opts.destination_mode == lightcraft_engine::export::DestinationMode::SameAsOriginal;
    if dir.is_empty() && exact.is_none() && !own_folders {
        return Err(NO_EXPORT_FOLDER.into());
    }
    let to = Destination { dir: dir.clone(), exact };
    // Add to Apple Photos: refused here, before anything is written, when it can't be done
    let after = app.session.after_export(&opts)?;
    let background = p.get("background").and_then(Value::as_bool).unwrap_or(false) && app.services.write_shared.is_some();
    let out = if background {
        let items = lightcraft_engine::export::prepare_batch(&mut app.session, &ids, &opts)?;
        crate::export_task::start(app, items, opts, to, after)?
    } else {
        let w = app.services.write.as_mut().ok_or("no writer")?;
        let files = export_batch(&mut app.session, &ids, &opts, &to, &mut |path, bytes| w(path, bytes), &|path| std::path::Path::new(path).exists())?;
        let mut out = json!({"files": files});
        // Photos may take minutes (and waits for the permission prompt): in the desktop app on a
        // worker, never on this (UI) thread; the app announces the outcome and
        // `export.photosImports` reports it. A window-less run (snapshot scripts) waits.
        if let Some(photos) = after.finish(&app.session, &files) {
            out["applePhotos"] = photos;
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
    let _ = app.session.save_prefs();
    Ok(out)
}

pub fn save_screenshot(app: &mut LightcraftApp, image: &egui::ColorImage, path: Option<&str>) -> Value {
    let [w, h] = image.size;
    let Some(path) = path else {
        return json!({"ok": true, "result": {"width": w, "height": h}});
    };
    let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
    let Some(img) = lightcraft_raster::Rgba8::from_bytes(w, h, &rgba) else {
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
mod tests {
    use super::*;

    fn test_app() -> LightcraftApp {
        LightcraftApp::new(lightcraft_engine::Session::new(), Default::default())
    }

    fn run_control(app: &mut LightcraftApp, method: &str, params: Value) -> Value {
        let ctx = egui::Context::default();
        let (req, _rx) = ControlRequest::new(method, params);
        match handle(app, &ctx, &req) {
            Outcome::Done(v) => v,
            Outcome::Screenshot { .. } => json!({"screenshot": true}),
        }
    }

    #[test]
    fn parse_click_count_validation() {
        assert_eq!(parse_click_count(&json!({}), "ui.click").unwrap(), 1);
        assert_eq!(parse_click_count(&json!({"count": null}), "ui.click").unwrap(), 1);
        assert_eq!(parse_click_count(&json!({"count": 1}), "ui.click").unwrap(), 1);
        assert_eq!(parse_click_count(&json!({"count": 2}), "ui.click").unwrap(), 2);
        assert_eq!(parse_click_count(&json!({"count": MAX_CLICK_COUNT}), "ui.click").unwrap(), MAX_CLICK_COUNT);

        // Above limit
        let err = parse_click_count(&json!({"count": MAX_CLICK_COUNT + 1}), "ui.click").unwrap_err();
        assert!(err.contains("at most 10"), "{err}");
        let err = parse_click_count(&json!({"count": 1_000_000_000u64}), "ui.click").unwrap_err();
        assert!(err.contains("at most 10"), "{err}");
        let err = parse_click_count(&json!({"count": u64::MAX}), "ui.click").unwrap_err();
        assert!(err.contains("at most 10"), "{err}");

        // Zero, negative, invalid types
        assert!(parse_click_count(&json!({"count": 0}), "ui.click").unwrap_err().contains("at least 1"));
        assert!(parse_click_count(&json!({"count": -1}), "ui.click").unwrap_err().contains("at least 1"));
        assert!(parse_click_count(&json!({"count": "three"}), "ui.click").unwrap_err().contains("must be an integer"));
        assert!(parse_click_count(&json!({"count": true}), "ui.click").unwrap_err().contains("must be an integer"));
        assert!(parse_click_count(&json!({"count": 1.5}), "ui.click").unwrap_err().contains("must be an integer"));
    }

    #[test]
    fn parse_drag_steps_validation() {
        assert_eq!(parse_drag_steps(&json!({}), "ui.drag").unwrap(), 10);
        assert_eq!(parse_drag_steps(&json!({"steps": null}), "ui.drag").unwrap(), 10);
        assert_eq!(parse_drag_steps(&json!({"steps": 1}), "ui.drag").unwrap(), 1);
        assert_eq!(parse_drag_steps(&json!({"steps": 10}), "ui.drag").unwrap(), 10);
        assert_eq!(parse_drag_steps(&json!({"steps": MAX_DRAG_STEPS}), "ui.drag").unwrap(), MAX_DRAG_STEPS);

        // Above limit
        let err = parse_drag_steps(&json!({"steps": MAX_DRAG_STEPS + 1}), "ui.drag").unwrap_err();
        assert!(err.contains("at most 50"), "{err}");
        let err = parse_drag_steps(&json!({"steps": 1_000_000_000u64}), "ui.drag").unwrap_err();
        assert!(err.contains("at most 50"), "{err}");
        let err = parse_drag_steps(&json!({"steps": u64::MAX}), "ui.drag").unwrap_err();
        assert!(err.contains("at most 50"), "{err}");

        // Zero, negative, invalid types
        assert!(parse_drag_steps(&json!({"steps": 0}), "ui.drag").unwrap_err().contains("at least 1"));
        assert!(parse_drag_steps(&json!({"steps": -5}), "ui.drag").unwrap_err().contains("at least 1"));
        assert!(parse_drag_steps(&json!({"steps": "many"}), "ui.drag").unwrap_err().contains("must be an integer"));
        assert!(parse_drag_steps(&json!({"steps": false}), "ui.drag").unwrap_err().contains("must be an integer"));
        assert!(parse_drag_steps(&json!({"steps": 2.5}), "ui.drag").unwrap_err().contains("must be an integer"));
    }

    #[test]
    fn ui_click_bounds_and_queue_preservation() {
        let mut app = test_app();

        // 1. Normal click (default count = 1) -> 3 events
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 3);
        app.synthetic.clear();

        // 2. Double click -> 5 events
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0, "count": 2}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 5);
        app.synthetic.clear();

        // 3. Accepted maximum limit (count = 10) -> 21 events
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0, "count": MAX_CLICK_COUNT}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 21);

        // 4. Over limit (count = 11): rejected, queue length unchanged
        let len_before = app.synthetic.len();
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0, "count": MAX_CLICK_COUNT + 1}));
        assert_eq!(res["ok"], false);
        assert!(res["error"].as_str().unwrap().contains("at most 10"));
        assert_eq!(app.synthetic.len(), len_before, "queue must not be partially mutated");

        // 5. Huge integer: rejected before event generation, queue unchanged
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0, "count": 1_000_000_000}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);

        // 6. Invalid type: rejected, queue unchanged
        let res = run_control(&mut app, "ui.click", json!({"x": 100.0, "y": 200.0, "count": "bad"}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);
    }

    #[test]
    fn ui_drag_bounds_and_queue_preservation() {
        let mut app = test_app();

        // 1. Normal drag (default steps = 10) -> 13 events
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 13);
        app.synthetic.clear();

        // 2. Custom steps (steps = 4) -> 7 events
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0, "steps": 4}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 7);
        app.synthetic.clear();

        // 3. Accepted maximum limit (steps = 50) -> 53 events
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0, "steps": MAX_DRAG_STEPS}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 53);

        // 4. Over limit (steps = 51): rejected, queue length unchanged
        let len_before = app.synthetic.len();
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0, "steps": MAX_DRAG_STEPS + 1}));
        assert_eq!(res["ok"], false);
        assert!(res["error"].as_str().unwrap().contains("at most 50"));
        assert_eq!(app.synthetic.len(), len_before, "queue must not be partially mutated");

        // 5. Huge integer: rejected, queue unchanged
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0, "steps": 1_000_000_000}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);

        // 6. Invalid type: rejected, queue unchanged
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 50.0, "toY": 50.0, "steps": "many"}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);
    }

    #[test]
    fn ui_widget_control_bounds_and_rejection() {
        let mut app = test_app();
        app.widgets.push(("testButton".to_string(), egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 30.0))));

        // 1. Normal clickWidget (count = 1) -> 3 events
        let res = run_control(&mut app, "ui.clickWidget", json!({"id": "testButton"}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 3);
        app.synthetic.clear();

        // 2. clickWidget with count = 2 -> 5 events
        let res = run_control(&mut app, "ui.clickWidget", json!({"id": "testButton", "count": 2}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 5);
        app.synthetic.clear();

        // 3. clickWidget at limit (count = 10) -> 21 events
        let res = run_control(&mut app, "ui.clickWidget", json!({"id": "testButton", "count": MAX_CLICK_COUNT}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 21);

        // 4. clickWidget over limit (count = 11) -> rejected, queue unchanged
        let len_before = app.synthetic.len();
        let res = run_control(&mut app, "ui.clickWidget", json!({"id": "testButton", "count": MAX_CLICK_COUNT + 1}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);

        // 5. clickWidget invalid type -> rejected, queue unchanged
        let res = run_control(&mut app, "ui.clickWidget", json!({"id": "testButton", "count": "bad"}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);

        // 6. Normal dragWidget (default steps = 10) -> 13 events
        app.synthetic.clear();
        let res = run_control(&mut app, "ui.dragWidget", json!({"id": "testButton", "dx": 20.0, "dy": 0.0}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 13);
        app.synthetic.clear();

        // 7. dragWidget at limit (steps = 50) -> 53 events
        let res = run_control(&mut app, "ui.dragWidget", json!({"id": "testButton", "dx": 20.0, "dy": 0.0, "steps": MAX_DRAG_STEPS}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 53);

        // 8. dragWidget over limit (steps = 51) -> rejected, queue unchanged
        let len_before = app.synthetic.len();
        let res = run_control(&mut app, "ui.dragWidget", json!({"id": "testButton", "dx": 20.0, "dy": 0.0, "steps": MAX_DRAG_STEPS + 1}));
        assert_eq!(res["ok"], false);
        assert_eq!(app.synthetic.len(), len_before);
    }

    #[test]
    fn event_budget_prevents_unbounded_queue_growth() {
        let mut app = test_app();

        // Enqueue two maximum drags: 53 + 53 = 106 events (budget is 128)
        let res1 = run_control(&mut app, "ui.drag", json!({"x": 0.0, "y": 0.0, "toX": 10.0, "toY": 10.0, "steps": 50}));
        assert_eq!(res1["ok"], true);
        assert_eq!(app.synthetic.len(), 53);

        let res2 = run_control(&mut app, "ui.drag", json!({"x": 0.0, "y": 0.0, "toX": 10.0, "toY": 10.0, "steps": 50}));
        assert_eq!(res2["ok"], true);
        assert_eq!(app.synthetic.len(), 106);

        // A third drag of 50 steps requires 53 more events (106 + 53 = 159 > 128): rejected!
        let res3 = run_control(&mut app, "ui.drag", json!({"x": 0.0, "y": 0.0, "toX": 10.0, "toY": 10.0, "steps": 50}));
        assert_eq!(res3["ok"], false);
        assert!(res3["error"].as_str().unwrap().contains("budget exceeded"));
        assert_eq!(app.synthetic.len(), 106, "queue must not grow beyond budget or partially mutate");

        // A smaller drag that fits (e.g. 10 steps = 13 events -> 106 + 13 = 119 <= 128) succeeds
        let res4 = run_control(&mut app, "ui.drag", json!({"x": 0.0, "y": 0.0, "toX": 10.0, "toY": 10.0, "steps": 10}));
        assert_eq!(res4["ok"], true);
        assert_eq!(app.synthetic.len(), 119);

        // A single click requires 3 events (119 + 3 = 122 <= 128) succeeds
        let res5 = run_control(&mut app, "ui.click", json!({"x": 5.0, "y": 5.0}));
        assert_eq!(res5["ok"], true);
        assert_eq!(app.synthetic.len(), 122);

        // When queue is near limit, an excessive click (e.g. count = 4 -> 9 events, 122 + 9 = 131 > 128) is rejected
        let res6 = run_control(&mut app, "ui.click", json!({"x": 5.0, "y": 5.0, "count": 4}));
        assert_eq!(res6["ok"], false);
        assert!(res6["error"].as_str().unwrap().contains("budget exceeded"));
        assert_eq!(app.synthetic.len(), 122);
    }

    #[test]
    fn ui_pointer_bounds_and_budget() {
        let mut app = test_app();
        app.image_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0)));

        // Normal gesture (3 points: down, drag, up) -> 4 events (down pushes 2)
        let res = run_control(
            &mut app,
            "ui.pointer",
            json!({"events": [{"kind": "down", "x": 0.2, "y": 0.2}, {"kind": "drag", "x": 0.3, "y": 0.3}, {"kind": "up", "x": 0.3, "y": 0.3}]}),
        );
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 4);
        app.synthetic.clear();

        // Excessive gesture (> MAX_DRAG_STEPS * 2 events = 100 events): rejected
        let mut huge_events = Vec::new();
        for _ in 0..101 {
            huge_events.push(json!({"kind": "move", "x": 0.5, "y": 0.5}));
        }
        let res = run_control(&mut app, "ui.pointer", json!({"events": huge_events}));
        assert_eq!(res["ok"], false);
        assert!(res["error"].as_str().unwrap().contains("too many events"));
        assert_eq!(app.synthetic.len(), 0);
    }

    #[test]
    fn gesture_integrity_and_release_preservation() {
        let mut app = test_app();
        let ctx = egui::Context::default();
        app.image_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 800.0)));

        // --- Scenario 1: A pending down event in app.synthetic that has not yet reached egui ---
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(pointer_is_down(&app, &ctx), "pending down in queue must be recognized as held pointer");
        let res = run_control(&mut app, "ui.pointer", json!({"events": [{"kind": "up", "x": 0.5, "y": 0.5}]}));
        assert_eq!(res["ok"], true);
        assert!(!pointer_is_down(&app, &ctx), "pending down followed by pending up must be recognized as balanced");
        app.synthetic.clear();

        // --- Scenario 2: A release request when the queue is at capacity ---
        for _ in 0..MAX_PENDING_EVENTS - 1 {
            app.synthetic.push(egui::Event::PointerMoved(egui::pos2(10.0, 10.0)));
        }
        // Button down at the exact budget limit (128 events)
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        assert_eq!(app.synthetic.len(), MAX_PENDING_EVENTS);
        assert!(pointer_is_down(&app, &ctx));

        // Exactly one release request succeeds at capacity, bringing queue to MAX_PENDING_EVENTS + 1
        let res = run_control(&mut app, "ui.pointer", json!({"events": [{"kind": "up", "x": 0.5, "y": 0.5}]}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), MAX_PENDING_EVENTS + 1);
        assert!(matches!(app.synthetic.last().unwrap(), egui::Event::PointerButton { pressed: false, .. }));
        assert!(!pointer_is_down(&app, &ctx), "release is now queued");

        // --- Scenario 3: Repeated release requests before the first release is processed ---
        // A second release request while queue is at MAX_PENDING_EVENTS + 1 is REJECTED
        let res2 = run_control(&mut app, "ui.pointer", json!({"events": [{"kind": "up", "x": 0.5, "y": 0.5}]}));
        assert_eq!(res2["ok"], false);
        assert!(res2["error"].as_str().unwrap().contains("budget exceeded"));
        assert_eq!(app.synthetic.len(), MAX_PENDING_EVENTS + 1, "queue must not grow beyond MAX_PENDING_EVENTS + 1");

        // A batch with multiple release events (e.g. 5 'up's) is also rejected, not exempted
        let res3 =
            run_control(&mut app, "ui.pointer", json!({"events": [{"kind": "up"}, {"kind": "up"}, {"kind": "up"}, {"kind": "up"}, {"kind": "up"}]}));
        assert_eq!(res3["ok"], false);
        assert!(res3["error"].as_str().unwrap().contains("budget exceeded"));
        assert_eq!(app.synthetic.len(), MAX_PENDING_EVENTS + 1);

        // --- Scenario 4: An unrelated pointer gesture already queued ---
        app.synthetic.clear();
        // A fully self-contained drag: move, down, move, up
        app.synthetic.push(egui::Event::PointerMoved(egui::pos2(0.0, 0.0)));
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(0.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        app.synthetic.push(egui::Event::PointerMoved(egui::pos2(10.0, 10.0)));
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(!pointer_is_down(&app, &ctx), "balanced gesture is not down");
        // Pad queue to capacity
        while app.synthetic.len() < MAX_PENDING_EVENTS {
            app.synthetic.push(egui::Event::PointerMoved(egui::pos2(5.0, 5.0)));
        }
        // An 'up' request without an unreleased down must NOT be exempted from budget
        let res = run_control(&mut app, "ui.pointer", json!({"events": [{"kind": "up", "x": 0.5, "y": 0.5}]}));
        assert_eq!(res["ok"], false);
        assert!(res["error"].as_str().unwrap().contains("budget exceeded"));
        assert_eq!(app.synthetic.len(), MAX_PENDING_EVENTS);

        // --- Scenario 5: Recovery from an interrupted gesture ---
        app.synthetic.clear();
        // Gesture interrupted with down
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        assert!(pointer_is_down(&app, &ctx));

        // A subsequent ui.click auto-emits a release first before clicking
        let res = run_control(&mut app, "ui.click", json!({"x": 50.0, "y": 50.0}));
        assert_eq!(res["ok"], true);
        assert_eq!(app.synthetic.len(), 5); // 1 initial + 1 auto-release + 1 move + 2 click = 5
        assert!(matches!(app.synthetic[1], egui::Event::PointerButton { pressed: false, .. }), "dangling down must be cleanly released");
        assert!(!pointer_is_down(&app, &ctx));

        // A subsequent ui.drag also auto-emits a release if interrupted
        app.synthetic.clear();
        app.synthetic.push(egui::Event::PointerButton {
            pos: egui::pos2(10.0, 10.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        let res = run_control(&mut app, "ui.drag", json!({"x": 10.0, "y": 10.0, "toX": 20.0, "toY": 20.0, "steps": 4}));
        assert_eq!(res["ok"], true);
        assert!(matches!(app.synthetic[1], egui::Event::PointerButton { pressed: false, .. }), "dangling down released before drag begins");
    }
}
