//! Help ▸ Debug ▸ Show Frame Rate (`view.frameRate`; `LIGHTCRAFT_FPS=1` starts with it on): a
//! frame-rate display in the window's top-right corner, to watch how a slider drag or a zoom holds
//! up. It shows where a frame's time goes: the UI
//! thread's CPU time (layout, painting, texture uploads) and the rest of the frame interval, which
//! is waiting — for vsync, or for the GPU when render work is ahead of the window's frame. A
//! tick over the graph marks each frame in which a render arrived, so spikes can be put next to
//! renders. While it shows, the window repaints continuously, so the rate is the one a drag gets.

use std::collections::VecDeque;

use crate::LightcraftApp;
use crate::render::Slot;

/// Frames kept for the graph (about two seconds at 120 Hz).
const KEEP: usize = 240;

/// A gap between frames longer than this is a pause (window hidden or occluded, the machine
/// asleep), not a frame: the display starts counting again instead of showing it as a spike.
const PAUSE: f64 = 2.0;

#[derive(Clone, Copy)]
pub(crate) struct Frame {
    at: f64,
    /// Seconds since the previous frame.
    pub(crate) dt: f64,
    /// The previous frame's CPU time on the UI thread, seconds.
    cpu: f64,
    /// Renders that arrived this frame.
    renders: u64,
    /// Of those, thrown away (superseded by a newer request).
    discarded: u64,
}

/// The display's state, kept in the context's temp data (one per window or headless view) and
/// dropped while the display is off, so turning it back on starts a fresh count.
#[derive(Clone, Default)]
struct Hud {
    last: Option<f64>,
    completed: u64,
    discarded: u64,
    frames: VecDeque<Frame>,
    /// When the numbers were last logged.
    logged: Option<f64>,
}

fn hud_id() -> egui::Id {
    egui::Id::new("fps-hud-state")
}

/// The display is off: drop its state, so the time it was off never shows as one long frame.
pub(crate) fn forget(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<Hud>(hud_id()));
}

/// Record a frame at `now` (`completed`/`discarded`: the renderer's running totals; `cpu`: the
/// previous frame's UI CPU time, seconds). Returns the frames kept for the display, and whether
/// the numbers are due in the log (once a second; A/B runs read them).
fn record(ctx: &egui::Context, now: f64, completed: u64, discarded: u64, cpu: f64) -> (Vec<Frame>, bool) {
    ctx.data_mut(|d| {
        let hud = d.get_temp_mut_or_default::<Hud>(hud_id());
        match hud.last {
            Some(prev) if now - prev > PAUSE || now < prev => hud.frames.clear(),
            Some(prev) => {
                let renders = completed.saturating_sub(hud.completed);
                let thrown = discarded.saturating_sub(hud.discarded);
                hud.frames.push_back(Frame { at: now, dt: (now - prev).max(1e-6), cpu, renders, discarded: thrown });
                while hud.frames.len() > KEEP {
                    hud.frames.pop_front();
                }
            }
            None => {}
        }
        hud.last = Some(now);
        hud.completed = completed;
        hud.discarded = discarded;
        let log = hud.logged.is_none_or(|t| now - t >= 1.0 || now < t);
        if log {
            hud.logged = Some(now);
        }
        (hud.frames.iter().copied().collect(), log)
    })
}

/// The frames the display holds now (tests).
#[cfg(test)]
pub(crate) fn frames(ctx: &egui::Context) -> Vec<Frame> {
    ctx.data_mut(|d| d.get_temp::<Hud>(hud_id())).map(|h| h.frames.into_iter().collect()).unwrap_or_default()
}

fn mb(bytes: u64) -> f64 {
    bytes as f64 / (1 << 20) as f64
}

/// Count this frame and draw the display over everything.
pub fn show(ctx: &egui::Context, app: &LightcraftApp) {
    let now = ctx.input(|i| i.time);
    let cpu = app.perf.host_frame_ms.unwrap_or(0.0) / 1e3;
    let (frames, log) = record(ctx, now, app.renderer.completed, app.renderer.discarded, cpu);

    let recent: Vec<&Frame> = frames.iter().filter(|f| now - f.at <= 1.0).collect();
    let fps_now = frames.last().map_or(0.0, |f| 1.0 / f.dt);
    let worst = recent.iter().copied().max_by(|a, b| a.dt.total_cmp(&b.dt));
    let avg_fps = if recent.is_empty() { 0.0 } else { recent.len() as f64 / recent.iter().map(|f| f.dt).sum::<f64>().max(1e-6) };
    let slow = recent.iter().filter(|f| f.dt > 1.0 / 50.0).count();
    let renders_s: u64 = recent.iter().map(|f| f.renders).sum();
    let discarded_s: u64 = recent.iter().map(|f| f.discarded).sum();
    let window = app.renderer.textures.get(&Slot::Region);
    let mem = lightcraft_engine::gpu::memory();
    // (`ready` first: `available` would wait for a device still being created)
    let device = match lightcraft_engine::gpu::unavailable_reason() {
        Some(why) => format!("off ({why})"),
        None if lightcraft_engine::gpu::ready() && lightcraft_engine::gpu::available() => {
            lightcraft_engine::gpu::adapter_name().unwrap_or_else(|| "on".into())
        }
        None => "starting…".into(),
    };
    if log {
        let (wdt, wcpu) = worst.map_or((0.0, 0.0), |w| (w.dt * 1e3, w.cpu * 1e3));
        let p95 = {
            let mut d: Vec<f64> = recent.iter().map(|f| f.dt * 1e3).collect();
            d.sort_by(f64::total_cmp);
            d.get(d.len().saturating_sub(1) * 95 / 100).copied().unwrap_or(0.0)
        };
        log::info!(
            "fps-hud: frames {} slow>20ms {slow} p95 {p95:.1} worst {wdt:.1} (cpu {wcpu:.1}) renders {renders_s} discarded {discarded_s} window {:.1} gpu {device}",
            recent.len(),
            window.map_or(0.0, |t| t.ms)
        );
    }
    let lines = [
        format!("{fps_now:>4.0} fps  avg {avg_fps:.0}  {slow} frames > 20 ms in 1 s"),
        match worst {
            Some(w) => format!("worst {:.1} ms = CPU {:.1} + wait {:.1}", w.dt * 1e3, w.cpu * 1e3, (w.dt - w.cpu).max(0.0) * 1e3),
            None => "worst –".to_string(),
        },
        format!("UI CPU {:.1} ms (update {:.1})", app.perf.host_frame_ms.unwrap_or(0.0), app.perf.update_ms),
        format!("render frame {:.1} ms", app.renderer.last_main_ms),
        match window {
            Some(t) => format!("render window {:.1} ms", t.ms),
            None => "render window –".to_string(),
        },
        format!("renders: {} queued, {} running, {renders_s}/s ({discarded_s} thrown away)", app.renderer.queued(), app.renderer.in_flight()),
        format!("GPU: {device}"),
        format!("GPU mem {:.0} MB (+{:.0} pooled)", mb(mem.allocated), mb(mem.pooled)),
    ];

    let screen = ctx.content_rect();
    let width = 300.0;
    egui::Area::new(egui::Id::new("fps-hud"))
        .order(egui::Order::Tooltip)
        .fixed_pos(egui::pos2(screen.right() - width - 24.0, screen.top() + 56.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new().fill(egui::Color32::from_black_alpha(210)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 1.0;
                for l in &lines {
                    ui.label(egui::RichText::new(l).monospace().size(11.0).color(egui::Color32::from_gray(230)));
                }
                ui.add_space(4.0);
                graph(ui, &frames, width);
                ui.label(
                    egui::RichText::new("bars: blue = UI CPU, orange/red = waiting · ticks = a render arrived · line = 60 fps")
                        .size(9.0)
                        .color(egui::Color32::from_gray(160)),
                );
            });
        });
    // a rate the drag gets, not the idle one
    ctx.request_repaint();
}

/// Frame intervals, 0 at the bottom and 50 ms at the top: the UI thread's CPU part in blue and
/// the waiting part above it (green within 60 fps, orange within 30, red beyond), with a tick
/// over each frame in which a render arrived.
fn graph(ui: &mut egui::Ui, frames: &[Frame], width: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 70.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let plot = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 6.0), rect.max);
    let y = |ms: f64| plot.bottom() - (ms / 50.0).clamp(0.0, 1.0) as f32 * plot.height();
    let w = rect.width() / KEEP as f32;
    let start = KEEP.saturating_sub(frames.len());
    for (i, f) in frames.iter().enumerate() {
        let (ms, cpu) = (f.dt * 1e3, (f.cpu * 1e3).min(f.dt * 1e3));
        let x = rect.left() + (start + i) as f32 * w;
        let bar = |from: f64, to: f64, c: egui::Color32| {
            p.rect_filled(egui::Rect::from_min_max(egui::pos2(x, y(to)), egui::pos2(x + w.max(1.0), y(from))), 0.0, c);
        };
        let wait = if ms > 33.4 {
            egui::Color32::from_rgb(230, 80, 70)
        } else if ms > 17.5 {
            egui::Color32::from_rgb(235, 160, 60)
        } else {
            egui::Color32::from_rgb(110, 190, 110)
        };
        bar(cpu, ms, wait);
        bar(0.0, cpu, egui::Color32::from_rgb(90, 150, 240));
        if f.renders > 0 {
            p.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x, rect.top()), egui::pos2(x + w.max(1.0), rect.top() + 4.0)),
                0.0,
                egui::Color32::WHITE,
            );
        }
    }
    p.hline(rect.x_range(), y(1000.0 / 60.0), egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 200, 90)));
    p.hline(rect.x_range(), y(1000.0 / 30.0), egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 90, 80).gamma_multiply(0.6)));
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use crate::headless::Headless;
    use crate::menubar::{MenuNode, menu_bar};
    use crate::{LightcraftApp, Services};

    const T: Duration = Duration::from_secs(20);

    fn set(h: &mut Headless, params: serde_json::Value) -> Option<bool> {
        let r = h.request("engine.execute", json!({"command": "view.frameRate", "params": params}), T);
        assert_eq!(r["ok"], true, "{r}");
        r["result"]["show"].as_bool()
    }

    /// The Help ▸ Debug item for it, and its checkmark.
    fn debug_item(app: &LightcraftApp) -> Option<Option<bool>> {
        let help = menu_bar(app).into_iter().find(|(t, _)| t == "Help").map(|(_, items)| items).unwrap_or_default();
        help.into_iter().find_map(|n| match n {
            MenuNode::Submenu { label, children } if label == "Debug" => children.into_iter().find_map(|c| match c {
                MenuNode::Item { id, checked, .. } if id == "view.frameRate" => Some(checked),
                _ => None,
            }),
            _ => None,
        })
    }

    /// Help ▸ Debug ▸ Show Frame Rate toggles the display (a checked menu item), `{show}` sets it,
    /// and frames draw with it on.
    #[test]
    fn show_frame_rate_is_a_debug_toggle() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        let was = h.app.ui.frame_rate;
        assert_eq!(debug_item(&h.app), Some(Some(was)), "Help ▸ Debug lists it, checked as it is");
        assert_eq!(set(&mut h, json!({})), Some(!was));
        assert_eq!(debug_item(&h.app), Some(Some(!was)));
        // frames with the display on draw (and keep the window repainting)
        assert_eq!(set(&mut h, json!({"show": true})), Some(true));
        for _ in 0..3 {
            h.step();
        }
        assert_eq!(set(&mut h, json!({"show": false})), Some(false));
        assert!(!h.app.ui.frame_rate);
    }

    fn worst_ms(h: &Headless) -> f64 {
        super::frames(&h.view.ctx).iter().map(|f| f.dt * 1e3).fold(0.0, f64::max)
    }

    /// Turned off and on again later, the display starts a fresh count: the time it was off is not
    /// one long frame (a red spike and a "worst" of the whole time off).
    #[test]
    fn turning_it_back_on_shows_no_spike_for_the_time_it_was_off() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        set(&mut h, json!({"show": true}));
        for _ in 0..3 {
            h.step();
        }
        assert!(!super::frames(&h.view.ctx).is_empty(), "frames are counted while it shows");
        set(&mut h, json!({"show": false}));
        h.step();
        h.time += 1.5;
        set(&mut h, json!({"show": true}));
        for _ in 0..3 {
            h.step();
        }
        assert!(worst_ms(&h) < 1000.0, "worst frame {} ms after turning it back on", worst_ms(&h));
    }

    /// A pause while it shows (window hidden, machine asleep) starts a fresh count too.
    #[test]
    fn a_long_pause_while_it_shows_is_not_a_frame() {
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
        let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
        set(&mut h, json!({"show": true}));
        for _ in 0..3 {
            h.step();
        }
        h.time += 30.0;
        for _ in 0..3 {
            h.step();
        }
        assert!(worst_ms(&h) < 1000.0, "worst frame {} ms after a pause", worst_ms(&h));
    }

    /// Each window (egui context) keeps its own count.
    #[test]
    fn each_context_keeps_its_own_count() {
        let (a, b) = (egui::Context::default(), egui::Context::default());
        for t in [0.0, 0.016, 0.033] {
            super::record(&a, t, 0, 0, 0.0);
        }
        assert_eq!(super::frames(&a).len(), 2);
        assert!(super::frames(&b).is_empty());
        let (frames, _) = super::record(&b, 100.0, 0, 0, 0.0);
        assert!(frames.is_empty(), "a new context's first frame has no interval: {}", frames.len());
    }
}
