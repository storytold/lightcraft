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
//! - `ui.key {key, shift?, alt?, cmd?}` / `ui.text {text}` / `ui.scroll {dx, dy, source?, unit?, phase?}`
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
    /// A capture was admitted: `target` pins the destination identity until the write.
    Screenshot {
        target: Option<lightcraft_engine::originals::WriteTarget>,
        headless: bool,
    },
    /// A synthetic-input request that must wait for a fresh layout and for its last event to be
    /// consumed before it is answered (see `LightcraftApp::admit_gesture`).
    Gesture {
        method: String,
        params: Value,
    },
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
        // the list of them under the top bar's bell is open (`view.tasks`)
        "activityOpen": app.activity_open,
        // the recent notifications that list keeps under the tasks, newest first, and how many came since it was open
        "notifications": app.notifications.items.iter().map(|n| json!({"id": n.id, "text": n.text, "warning": n.warning, "details": n.report.is_some()})).collect::<Vec<_>>(),
        "unread": app.notifications.unread,
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

/// Parse a control-protocol key name into an egui key. `egui::Key::from_name` is tried first;
/// then lowercase aliases cover `return`/`enter`, `esc`/`escape`, `delete`/`backspace` (both
/// Backspace), arrow names, space/tab, and backslash, slash and square brackets.
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

/// Most clicks one `ui.click`/`ui.clickWidget` request may inject. The control server is a
/// local automation surface, but a request must not be able to fill memory with synthetic events.
pub(crate) const MAX_CLICKS: u64 = 16;
/// Most pointer-move steps one `ui.drag`/`ui.dragWidget` request may inject.
pub(crate) const MAX_DRAG_STEPS: u64 = 1000;

/// What a widget gesture's synthetic events were aimed at when they were resolved. Delivery
/// rechecks this against the latest completed layout before the first event and again before every
/// press: a moved widget cancels the request instead of being retargeted.
#[derive(Clone, Debug)]
pub(crate) struct GestureTarget {
    pub id: String,
    /// The rect the request resolved against. An explicit `fx`/`fy` may place `at` outside it;
    /// delivery accepts that only while egui's actual clipped interaction rectangle still
    /// contains the point.
    pub rect: egui::Rect,
    /// The point the pointer events aim at.
    pub at: egui::Pos2,
    /// Whether the release must still find this widget. egui attributes a click on release while a
    /// drag may legitimately end outside the widget it started on, so only `ui.clickWidget` sets
    /// this.
    pub check_release: bool,
    /// Whether the first event must find the widget visible and topmost. A hover is not harmless
    /// when its point is clipped out of the widget or covered by a non-modal overlay: the
    /// documented contract refuses it rather than moving the pointer at a stale place.
    pub check_visible: bool,
}

/// The events queued for a resolved gesture, plus the widget target whose geometry must still hold
/// when the first event and each press are injected.
#[derive(Debug)]
pub(crate) struct GestureOutcome {
    pub result: Value,
    pub target: Option<GestureTarget>,
}

impl GestureOutcome {
    /// A gesture outcome that injected no widget-targeted events, so delivery has no geometry to
    /// re-check.
    fn done(result: Value) -> Self {
        Self { result, target: None }
    }
}

fn push_drag(app: &mut LightcraftApp, a: egui::Pos2, b: egui::Pos2, steps: u64, m: egui::Modifiers) {
    app.synthetic.push(egui::Event::PointerMoved(a));
    app.synthetic.push(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: m });
    for i in 1..=steps.max(1) {
        let t = i as f32 / steps.max(1) as f32;
        app.synthetic.push(egui::Event::PointerMoved(a + (b - a) * t));
    }
    app.synthetic.push(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers: m });
}

/// The rectangle registered for automation id `id` in the latest completed pass. The last
/// registration wins, so a redrawn widget's current geometry is returned.
pub(crate) fn widget_rect(app: &LightcraftApp, id: &str) -> Option<egui::Rect> {
    app.widgets.iter().rev().find(|(w, _)| w == id).map(|(_, r)| *r)
}

/// The real egui widget that owns automation id `id` in the latest completed pass (the last
/// registration wins, as [`widget_rect`] does).
pub(crate) fn widget_hit(app: &LightcraftApp, id: &str) -> Option<crate::widgets::WidgetHit> {
    app.widget_hits.iter().rev().find(|(w, _)| w == id).map(|(_, hit)| *hit)
}

/// Whether the registered automation rectangle still owns egui's pointer hit at `at`.
///
/// A registration records the egui id and layer of a hover probe (or of the widget's own
/// response), plus that response's clipped `interact_rect` in the same completed pass. This asks
/// egui for the current areas at the point and checks:
///
/// - the recorded clipped rectangle still contains the point (a widget scrolled out of its panel
///   has an empty interact rect even though its unclipped rect still contains the old point);
/// - the top-most interactable area at the point is the recorded layer; or
/// - the top-most area is the current modal layer and the recorded layer is below it. egui routes
///   every point below a modal to that modal, even outside the dialog window, so leaving the press
///   to egui is safe: the dialog consumes it or `clicked_backdrop` closes the dialog, and a
///   background widget cannot receive it. A non-modal overlay is not the modal layer, so it must
///   still match the target and is refused otherwise. The one exception is the explicitly
///   blocking library-problem/launch-list window: its dim also owns the modal layer, but a
///   background target is refused here rather than reported `ok:true` after the dim swallows the
///   press; only controls drawn in that window's own layer pass. Same-layer overlaps are invisible
///   to this area check; egui still delivers the press to whichever widget is top-most in that
///   layer.
///
/// `interact_rect` comes from the latest completed registry, not `Context::read_response`:
/// `raw_input_hook` runs before `begin_pass`, where `read_response` checks `this_pass` before
/// `prev_pass` and can therefore return the pass before the one that just published the registry.
/// This matters when clipping, scrolling or a viewport change alters the target without changing
/// its layer.
///
/// In pinned egui `6d518fc`, `Response::hovered` is deliberately false while any button is down
/// unless this is the clicked widget (`context.rs:1884-1886`, `interaction.rs:257-300`). A
/// hover-only probe cannot use it for a click release. `Response::contains_pointer` would survive
/// the press, but its hit-test snapshot is one frame behind the pointer move that precedes the
/// press (`context.rs:517-548` reads `prev_pass.widgets` at `begin_pass`). The recorded clipped rect
/// plus the current layer has no such lag.
///
/// This also avoids the old centre-of-an-interactive-rect heuristic: a registration may be a
/// sub-rect of its widget's interactive response (rating stars, date cells, a checkbox's box, a
/// profile card's star), so the interactive rect's centre need not lie in the registered rect.
/// Disabled controls keep working as a no-op: their own probe is enabled and the injected click
/// is ignored by egui, which is the documented `ui.clickWidget` semantics.
pub(crate) fn widget_target_visible(ctx: &egui::Context, hit: Option<crate::widgets::WidgetHit>, at: egui::Pos2) -> bool {
    let Some(hit) = hit else {
        // A hand-built registry entry (tests) or painted geometry has no egui widget to own a
        // press. Production widget registrations always record a probe or a real response.
        return false;
    };
    let interact_rect = ctx.layer_transform_to_global(hit.layer_id).map_or(hit.interact_rect, |to_global| to_global * hit.interact_rect);
    if !interact_rect.is_finite() || !interact_rect.contains(at) {
        return false;
    }
    let (top_modal, allows_interaction, above_modal) =
        ctx.memory(|memory| (memory.top_modal_layer(), memory.allows_interaction(hit.layer_id), memory.is_above_modal_layer(hit.layer_id)));
    if crate::panels::library_problem::refuses_control_target(top_modal, hit.layer_id) {
        return false;
    }
    let at_layer = ctx.layer_id_at(at);
    if at_layer == Some(hit.layer_id) {
        return allows_interaction;
    }
    top_modal == at_layer && !above_modal
}

/// Dispatch one control request: run engine commands, list menus/widgets, inspect state, queue
/// file drops and synthetic input, capture screenshots, or return an error for an unknown or
/// invalid request. Requests that need fresh widget geometry are queued as `Outcome` values.
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
        "ui.clickWidget" | "ui.dragWidget" | "ui.hoverWidget" | "ui.move" | "ui.click" | "ui.drag" | "ui.pointer" | "ui.key" | "ui.text"
        | "ui.clipboard" | "ui.scroll" => {
            // Geometry and input events are resolved only after a fresh completed layout; the
            // reply waits until the UI has consumed the last event (plan §4.5).
            Outcome::Gesture { method: req.method.clone(), params: req.params.clone() }
        }
        "ui.zoom" => Outcome::Gesture { method: req.method.clone(), params: req.params.clone() },
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
            // Never over a photo's original, and pin the destination identity now: the write
            // revalidates it after the (possibly lengthy) capture and never replaces a path that
            // was absent at admission but appeared meanwhile.
            let target = match s("path") {
                Some(path) => match app.session.pin_write_target(path) {
                    Ok(t) => Some(t),
                    Err(e) => return err(e),
                },
                None => None,
            };
            ctx.request_repaint();
            Outcome::Screenshot { target, headless: p.get("headless").and_then(Value::as_bool).unwrap_or(false) }
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

/// Resolve one queued synthetic-input control request against the **current** layout.
///
/// Called by the app only after a completed UI pass newer than admission (plan §4.5). Widget rects
/// and the image rect therefore come from that pass; nothing is retargeted. A successful return
/// has pushed the events onto `app.synthetic`; the app answers the request once the UI consumed
/// them.
pub(crate) fn gesture_events(app: &mut LightcraftApp, ctx: &egui::Context, method: &str, p: &Value) -> Result<GestureOutcome, String> {
    let s = |k: &str| p.get(k).and_then(Value::as_str);
    let f = |k: &str| p.get(k).and_then(Value::as_f64);
    match method {
        "ui.clickWidget" | "ui.dragWidget" | "ui.hoverWidget" => {
            let Some(id) = s("id") else { return Err("missing `id`".into()) };
            let Some(r) = widget_rect(app, id) else { return Err(format!("no widget `{id}` on screen (see ui.widgets)")) };
            let m = modifiers(p);
            let (fx, fy) = (f("fx").unwrap_or(0.5), f("fy").unwrap_or(0.5));
            let at = egui::pos2(r.left() + r.width() * fx as f32, r.top() + r.height() * fy as f32);
            if !fx.is_finite() || !fy.is_finite() || !at.is_finite() {
                return Err(format!("{method}: fx and fy must resolve to a finite point"));
            }
            let hover = method == "ui.hoverWidget";
            if hover && !widget_target_visible(ctx, widget_hit(app, id), at) {
                return Err(format!("ui.hoverWidget: widget `{id}` has no visible interaction area at the requested point"));
            }
            if hover {
                app.synthetic.push(egui::Event::PointerMoved(at));
            } else if method == "ui.clickWidget" {
                let n = p.get("count").and_then(Value::as_u64).unwrap_or(1).min(MAX_CLICKS);
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
                push_drag(app, at, to, p.get("steps").and_then(Value::as_u64).unwrap_or(10).min(MAX_DRAG_STEPS), m);
            }
            ctx.request_repaint();
            Ok(GestureOutcome {
                result: json!({"rect": rect_json(r)}),
                target: Some(GestureTarget { id: id.to_string(), rect: r, at, check_release: method == "ui.clickWidget", check_visible: hover }),
            })
        }
        "ui.move" => {
            app.synthetic.push(egui::Event::PointerMoved(egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32)));
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.click" | "ui.drag" => {
            let a = egui::pos2(f("x").unwrap_or(0.0) as f32, f("y").unwrap_or(0.0) as f32);
            let m = modifiers(p);
            let button = if s("button") == Some("right") { egui::PointerButton::Secondary } else { egui::PointerButton::Primary };
            if method == "ui.drag" {
                let b = egui::pos2(f("toX").unwrap_or(0.0) as f32, f("toY").unwrap_or(0.0) as f32);
                push_drag(app, a, b, p.get("steps").and_then(Value::as_u64).unwrap_or(10).min(MAX_DRAG_STEPS), m);
            } else {
                app.synthetic.push(egui::Event::PointerMoved(a));
                for _ in 0..p.get("count").and_then(Value::as_u64).unwrap_or(1).min(MAX_CLICKS) {
                    app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: true, modifiers: m });
                    app.synthetic.push(egui::Event::PointerButton { pos: a, button, pressed: false, modifiers: m });
                }
            }
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.pointer" => {
            let Some(img) = app.image_rect else { return Err("no image on screen (switch to Detail view)".into()) };
            let Some(events) = p.get("events").and_then(Value::as_array) else { return Err("missing `events`".into()) };
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
            Ok(GestureOutcome::done(json!({"events": events.len()})))
        }
        "ui.key" => {
            let Some(k) = s("key").and_then(key_from) else { return Err("unknown or missing `key`".into()) };
            let m = modifiers(p);
            app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m });
            app.synthetic.push(egui::Event::Key { key: k, physical_key: None, pressed: false, repeat: false, modifiers: m });
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.text" => {
            app.synthetic.push(egui::Event::Text(s("text").unwrap_or("").to_string()));
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.clipboard" => {
            // RequestPaste is a viewport command, not an event: the prompt/library input barrier
            // cannot inspect it after the fact. Refuse the command while either barrier owns input,
            // including a window created after this pass began resolving the gesture. Without
            // `text` this only queues RequestPaste in egui; it is not a host-dispatch or delivery
            // acknowledgement. egui 0.37 documents RequestPaste as asking the host to paste into
            // the focused text edit, with no completion or correlation id. egui-winit emits a later
            // Event::Paste only for non-empty text (or a PasteImage fallback; Wayland reads finish
            // later), so a barrier created after the request can drop that later event; the reply
            // below cannot promise dispatch or delivery. With `text`, the synthetic
            // Event::Paste is filtered and canceled like other injected input.
            let barrier = if crate::panels::notices::blocks_input(app) {
                Some("a modal prompt")
            } else if crate::panels::library_problem::blocks_dialog_input(app) {
                Some("the blocking library window")
            } else {
                None
            };
            if let Some(barrier) = barrier {
                return Err(format!("ui.clipboard: {barrier} is blocking background input"));
            }
            if !ctx.text_edit_focused() {
                return Err("ui.clipboard: no text field has the focus (click one first)".into());
            }
            match s("action") {
                Some("cut") => app.synthetic.push(egui::Event::Cut),
                Some("copy") => app.synthetic.push(egui::Event::Copy),
                Some("paste") => match s("text") {
                    Some(text) => app.synthetic.push(egui::Event::Paste(text.to_string())),
                    None => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
                },
                _ => return Err("ui.clipboard: `action` is cut, copy or paste".into()),
            }
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.scroll" => {
            // Omitted keys keep the pre-0.37 request meaning; all other values are validated
            // before anything is queued.
            let source = match s("source") {
                None | Some("unknown") => egui::MouseWheelSource::Unknown,
                Some("wheel") => egui::MouseWheelSource::Wheel,
                Some("trackpad") => egui::MouseWheelSource::Trackpad,
                Some("momentum") => egui::MouseWheelSource::Momentum,
                Some(other) => return Err(format!("ui.scroll: `source` is wheel, trackpad, momentum or unknown, not {other:?}")),
            };
            let unit = match s("unit") {
                None | Some("point") => egui::MouseWheelUnit::Point,
                Some("line") => egui::MouseWheelUnit::Line,
                Some("page") => egui::MouseWheelUnit::Page,
                Some(other) => return Err(format!("ui.scroll: `unit` is point, line or page, not {other:?}")),
            };
            let phase = match s("phase") {
                None | Some("move") => egui::TouchPhase::Move,
                Some("start") => egui::TouchPhase::Start,
                Some("end") => egui::TouchPhase::End,
                Some("cancel") => egui::TouchPhase::Cancel,
                Some(other) => return Err(format!("ui.scroll: `phase` is start, move, end or cancel, not {other:?}")),
            };
            app.synthetic.push(egui::Event::MouseWheel {
                unit,
                delta: egui::vec2(f("dx").unwrap_or(0.0) as f32, f("dy").unwrap_or(0.0) as f32),
                phase,
                source,
                modifiers: modifiers(p),
            });
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        "ui.zoom" => {
            let Some(factor) = f("factor").map(|v| v as f32).filter(|v| v.is_finite() && *v > 0.0) else {
                return Err("ui.zoom: factor must be a finite positive number (1 = unchanged)".into());
            };
            app.synthetic.push(egui::Event::Zoom(factor));
            ctx.request_repaint();
            Ok(GestureOutcome::done(Value::Null))
        }
        other => Err(format!("not a synthetic-input gesture: `{other}`")),
    }
}

/// Encode `image` as PNG and write it to the capture's pinned target, returning the control
/// protocol's JSON result. The destination is revalidated before encoding; a new path is written
/// with create-new semantics (never replacing a file that appeared), and a missing encoder or
/// writer is an error. `None` only reports the image size without writing.
pub fn save_screenshot(app: &mut LightcraftApp, image: &egui::ColorImage, target: Option<&lightcraft_engine::originals::WriteTarget>) -> Value {
    let [w, h] = image.size;
    let Some(target) = target else {
        return json!({"ok": true, "result": {"width": w, "height": h}});
    };
    // Re-check the pinned destination immediately before encoding/writing. This is the point the
    // asynchronous renderer callback and the CPU fallback share.
    if let Err(error) = app.session.revalidate_write_target(target) {
        return json!({"ok": false, "error": error});
    }
    // Rgba8's buffer is the only extra full-image allocation while encoding (the PNG bytes are
    // the second transient; see MAX_SHOT_WORKING_SET_BYTES in `lib.rs`). Building it straight from
    // the `Color32` pixels avoids the former flat `Vec<u8>` copy of the same image.
    let Some(img) = lightcraft_raster::Rgba8::from_pixels(w, h, image.pixels.iter().map(|c| c.to_array())) else {
        return json!({"ok": false, "error": "bad screenshot buffer"});
    };
    let Some(png) = app.services.png.as_ref() else { return json!({"ok": false, "error": "no encoder"}) };
    let bytes = png(&img);
    let path = target.path();
    if target.was_absent() {
        // Publish a complete temp file with create-new semantics: a file that appeared after the
        // revalidation above is never replaced. Without this writer the capture fails closed.
        let Some(writer) = app.services.write_new.as_mut() else {
            return json!({"ok": false, "error": format!("{path}: no create-new writer configured for a new screenshot path")});
        };
        match writer(path, &bytes) {
            Ok(()) => json!({"ok": true, "result": {"path": path, "width": w, "height": h}}),
            Err(e) => json!({"ok": false, "error": e}),
        }
    } else {
        match app.services.write.as_mut() {
            Some(wr) => match wr(path, &bytes) {
                Ok(()) => json!({"ok": true, "result": {"path": path, "width": w, "height": h}}),
                Err(e) => json!({"ok": false, "error": e}),
            },
            None => json!({"ok": false, "error": "no writer configured"}),
        }
    }
}
