//! `--hdr-probe [SECONDS]`: a small window that reports what HDR display output finds on this
//! machine (adapter, the surface formats and colour spaces offered, the configuration chosen,
//! the display's HDR facts and headroom) and shows a ramp from 0 to 4× SDR white beside an SDR
//! white reference patch, then closes. The report goes to stderr and to `hdr-probe.log` in the
//! log folder (else the temp folder).

use eframe::egui_wgpu::RenderState;
use lcc_ui_egui::HdrOutput;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::hdr_display::{self, WindowHdr};

/// The ramp's brightest value, × SDR white.
const RAMP_MAX: f32 = 4.0;
const RAMP_W: usize = 1024;
const RAMP_H: usize = 8;

/// The ramp texture's pixels: linear, 0 at the left to [`RAMP_MAX`] at the right.
pub fn ramp() -> Vec<f32> {
    let mut v = Vec::with_capacity(RAMP_W * RAMP_H * 3);
    for _ in 0..RAMP_H {
        for x in 0..RAMP_W {
            let l = x as f32 / (RAMP_W - 1) as f32 * RAMP_MAX;
            v.extend([l, l, l]);
        }
    }
    v
}

struct Log {
    path: Option<std::path::PathBuf>,
    lines: Vec<String>,
}

impl Log {
    fn line(&mut self, s: String) {
        eprintln!("hdr-probe: {s}");
        self.lines.push(s);
        if let Some(p) = &self.path {
            let written = std::fs::File::create(p).and_then(|mut f| f.write_all(self.lines.join("\n").as_bytes()).and_then(|_| f.write_all(b"\n")));
            if let Err(e) = written {
                eprintln!("hdr-probe: can't write {}: {e}", p.display());
                self.path = None;
            }
        }
    }
}

struct Probe {
    log: Log,
    hdr: Option<WindowHdr>,
    ramp: Option<egui::TextureId>,
    until: Instant,
    last: Option<Option<f32>>,
    closing: bool,
}

fn describe(state: &RenderState, log: &mut Log) {
    let i = state.adapter.get_info();
    log.line(format!("adapter: {} ({:?}, {:?}, driver {} {})", i.name, i.backend, i.device_type, i.driver, i.driver_info));
    for f in &state.surface_formats {
        log.line(format!("surface offers: {:?} in {:?}", f.format, f.color_spaces));
    }
    log.line(format!("configured: {:?} in {:?}", state.target_format, state.target_color_space));
    let info = state.display_hdr_info.read().clone();
    log.line(format!("display HDR facts: {info:?}"));
    log.line(format!("tone-map headroom: {:?}", info.as_ref().and_then(|i| i.tone_map_headroom())));
    log.line(format!("decision: {:?}", hdr_display::report(Some(state))));
}

impl eframe::App for Probe {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let headroom = self.hdr.as_mut().map(|h| h.headroom()).unwrap_or(None);
        if self.last != Some(headroom) {
            let levels = self.hdr.as_ref().and_then(WindowHdr::levels);
            self.log.line(format!("showing: headroom {headroom:?} (levels {levels:?})"));
            self.last = Some(headroom);
        }
        if self.ramp.is_none()
            && let Some(h) = self.hdr.as_mut()
        {
            h.set_upload_headroom(RAMP_MAX);
            match h.upload(RAMP_W, RAMP_H, &ramp()) {
                Ok(id) => self.ramp = Some(id),
                Err(e) => self.log.line(format!("ramp texture: {e}")),
            }
        }
        let left = self.until.saturating_duration_since(Instant::now());
        egui::Frame::new().fill(egui::Color32::from_gray(40)).inner_margin(16.0).show(ui, |ui| {
            ui.heading(format!("HDR probe — closes in {} s", left.as_secs() + 1));
            ui.label(match headroom {
                Some(h) => format!("HDR: the display has {h:.2}× SDR white; the ramp should keep brightening up to that point."),
                None => "SDR: the ramp clips at the reference patch's white (1×).".to_string(),
            });
            ui.add_space(12.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 120.0), egui::Sense::hover());
            let p = ui.painter();
            let patch = egui::Rect::from_min_size(rect.min, egui::vec2(100.0, 100.0));
            p.rect_filled(patch, 0.0, egui::Color32::WHITE);
            p.text(
                patch.center_bottom() + egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                "SDR white",
                egui::FontId::proportional(12.0),
                egui::Color32::LIGHT_GRAY,
            );
            let ramp = egui::Rect::from_min_max(egui::pos2(patch.right() + 16.0, rect.top()), egui::pos2(rect.right(), rect.top() + 100.0));
            match self.ramp {
                Some(id) => {
                    p.image(id, ramp, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
                }
                None => {
                    p.text(
                        ramp.center(),
                        egui::Align2::CENTER_CENTER,
                        "no wgpu renderer",
                        egui::FontId::proportional(14.0),
                        egui::Color32::LIGHT_GRAY,
                    );
                }
            }
            for k in 0..=RAMP_MAX as usize {
                let x = ramp.left() + ramp.width() * k as f32 / RAMP_MAX;
                p.line_segment([egui::pos2(x, ramp.bottom()), egui::pos2(x, ramp.bottom() + 6.0)], egui::Stroke::new(1.0, egui::Color32::LIGHT_GRAY));
                p.text(
                    egui::pos2(x, ramp.bottom() + 14.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{k}×"),
                    egui::FontId::proportional(12.0),
                    egui::Color32::LIGHT_GRAY,
                );
            }
        });
        if left.is_zero() && !self.closing {
            self.closing = true;
            self.log.line("done".into());
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !self.closing {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }
}

/// Run the probe for `seconds`; `log_dir`: where `hdr-probe.log` goes (else the temp folder).
pub fn run(seconds: u64, log_dir: Option<std::path::PathBuf>) -> eframe::Result {
    let dir = log_dir.filter(|d| std::fs::create_dir_all(d).is_ok()).unwrap_or_else(std::env::temp_dir);
    let mut log = Log { path: Some(dir.join("hdr-probe.log")), lines: Vec::new() };
    log.line(format!("{} {} HDR probe, {} {}", lcc_brand::DISPLAY_NAME, env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH));
    log.line(format!("platform: {:?}; HDR wanted: {}", hdr_display::Platform::current(), hdr_display::wanted()));
    if let Some(p) = &log.path {
        eprintln!("hdr-probe: writing {}", p.display());
    }
    let mut wgpu_options = crate::window_gpu::base_options();
    hdr_display::configure(&mut wgpu_options);
    wgpu_options.hdr_output = true;
    log.line(format!("compositor headroom: {:?}", hdr_display::read_compositor_headroom()));
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default().with_title("HDR probe").with_inner_size([960.0, 300.0]),
        wgpu_options,
        ..Default::default()
    };
    let until = Instant::now() + Duration::from_secs(seconds.clamp(1, 600));
    eframe::run_native(
        "HDR probe",
        options,
        Box::new(move |cc| {
            let hdr = match cc.wgpu_render_state.as_ref() {
                Some(state) => {
                    describe(state, &mut log);
                    Some(WindowHdr::any(state))
                }
                None => {
                    log.line("no wgpu render state (glow?)".into());
                    None
                }
            };
            Ok(Box::new(Probe { log, hdr, ramp: None, until, last: None, closing: false }))
        }),
    )
}
