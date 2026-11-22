//! Photo ▸ Enhance… (`dialog.enhance`): the Enhance Preview dialog. Raw Details or Super Resolution (and, when an
//! upscaler is installed, AI Super Resolution and AI Sharpen, adapted from upstream #3493), a preview of
//! the middle of the first selected photo before and after (click to zoom in, drag to pan), the estimated time and
//! Create Stack; Enhance runs `enhance.rawDetails` / `enhance.superResolution` on the selection.
//!
//! The preview is made on a worker thread ([`lcc_engine::enhance::EnhanceWork::preview`]); changing the method
//! starts a new one.

use crate::json_ext::JsonExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};

use lcc_catalog::PhotoId;
use lcc_engine::enhance::{EnhanceKind, EnhancePreview};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::LccApp;

/// The dialog's options.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct EnhanceDialog {
    /// Super Resolution (else Raw Details).
    pub super_resolution: bool,
    /// AI Super Resolution with the installed upscaler (2×; overrides the two above).
    pub ai_super_resolution: bool,
    /// AI Sharpen with the installed upscaler (overrides the others).
    pub ai_sharpen: bool,
    /// The colour space of an AI result (`space` of the commands: srgb, displayP3, adobeRgb).
    pub space: String,
    pub stack: bool,
}

impl Default for EnhanceDialog {
    fn default() -> Self {
        EnhanceDialog { super_resolution: false, ai_super_resolution: false, ai_sharpen: false, space: "srgb".into(), stack: true }
    }
}

impl EnhanceDialog {
    fn kind(&self) -> EnhanceKind {
        if self.ai_sharpen {
            EnhanceKind::AiSharpen
        } else if self.ai_super_resolution {
            EnhanceKind::AiSuperResolution(2)
        } else if self.super_resolution {
            EnhanceKind::SuperResolution
        } else {
            EnhanceKind::RawDetails
        }
    }
    fn command(&self) -> &'static str {
        match self.kind() {
            EnhanceKind::AiSharpen => "enhance.aiSharpen",
            EnhanceKind::SuperResolution | EnhanceKind::AiSuperResolution(_) => "enhance.superResolution",
            EnhanceKind::RawDetails => "enhance.rawDetails",
        }
    }
    /// Pick one method (the radio buttons).
    fn pick(&mut self, kind: EnhanceKind) {
        self.super_resolution = kind == EnhanceKind::SuperResolution;
        self.ai_super_resolution = matches!(kind, EnhanceKind::AiSuperResolution(_));
        self.ai_sharpen = kind == EnhanceKind::AiSharpen;
    }
}

struct PreviewTask {
    kind: EnhanceKind,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<EnhancePreview, String>>,
}

/// A finished preview: textures and what the dialog says about it.
pub struct Shown {
    pub kind: EnhanceKind,
    before: egui::TextureHandle,
    after: egui::TextureHandle,
    pub after_size: [usize; 2],
    pub out_size: (usize, usize),
    pub estimate_secs: f64,
}

/// The dialog's state in the app.
#[derive(Default)]
pub struct EnhanceUi {
    /// Photos Enhance runs on (the selection when the dialog opened).
    pub ids: Vec<PhotoId>,
    task: Option<PreviewTask>,
    pub preview: Option<Shown>,
    /// (method, error) of the last preview that failed (not retried until the method changes).
    pub error: Option<(EnhanceKind, String)>,
    /// Zoomed in by a click: the point at the middle of the box (0..1). `None` fits the preview.
    pub zoom: Option<(f32, f32)>,
    /// Showing the crop before enhancing.
    pub before: bool,
}

impl EnhanceUi {
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
    fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.cancel.store(true, Ordering::Relaxed);
        }
    }
}

/// `dialog.enhance`: open the dialog for the selection (or the active photo).
pub fn open(app: &mut LccApp) -> Result<Value, String> {
    let mut ids = app.session.selected();
    if ids.is_empty() {
        ids.extend(app.session.active());
    }
    if ids.is_empty() {
        return Err("select a photo to enhance".into());
    }
    app.enhance.stop();
    app.enhance = EnhanceUi { ids, ..Default::default() };
    app.ui.dialog = Some(crate::state::Dialog::Enhance { opts: EnhanceDialog::default() });
    Ok(json!({"photos": app.enhance.ids.len()}))
}

/// The dialog's Enhance button: run the command on the photos.
pub fn confirm(app: &mut LccApp, opts: &EnhanceDialog) -> Result<Value, String> {
    app.enhance.stop();
    let ids: Vec<u64> = app.enhance.ids.iter().map(|i| i.0).collect();
    let method = if opts.ai_super_resolution { "ai" } else { "classical" };
    let mut p = json!({"ids": ids, "stack": opts.stack, "method": method});
    if opts.kind().ai_factor().is_some() {
        *p.field_mut("space") = json!(opts.space);
    }
    app.run(opts.command(), p)
}

fn spawn(app: &mut LccApp, kind: EnhanceKind) -> Result<PreviewTask, String> {
    let first = app.enhance.ids.first().copied().ok_or("select a photo to enhance")?;
    let work = app
        .session
        .plan_enhance(kind, &[first], lcc_pipeline::OutputSpace::Srgb)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or("nothing to enhance")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let flag = cancel.clone();
    let job = move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work.preview(&|| flag.load(Ordering::Relaxed))))
            .unwrap_or_else(|_| Err("the preview failed unexpectedly".into()));
        let _ = tx.send(r);
    };
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::Builder::new().name("enhance-preview".into()).spawn(job).map_err(|e| e.to_string())?;
    #[cfg(target_arch = "wasm32")]
    job();
    Ok(PreviewTask { kind, cancel, rx })
}

fn texture(app: &LccApp, ctx: &egui::Context, name: &str, mut img: lcc_raster::Rgba8) -> egui::TextureHandle {
    // shown through the monitor profile like every other preview
    if let Some(d) = app.renderer.display()
        && let Err(e) = d.profile.from_srgb(&mut img)
    {
        log::warn!("display profile: {e}");
    }
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes());
    ctx.load_texture(name, color, egui::TextureOptions::NEAREST)
}

/// Per frame: keep the preview in step with the dialog's method.
pub fn poll(app: &mut LccApp, ctx: &egui::Context) {
    let kind = match &app.ui.dialog {
        Some(crate::state::Dialog::Enhance { opts }) => opts.kind(),
        _ => {
            app.enhance.stop();
            return;
        }
    };
    let current = app.enhance.task.as_ref().map(|t| t.kind).or(app.enhance.preview.as_ref().map(|p| p.kind));
    let failed = app.enhance.error.as_ref().is_some_and(|(k, _)| *k == kind);
    if current != Some(kind) && !failed {
        app.enhance.stop();
        match spawn(app, kind) {
            Ok(t) => app.enhance.task = Some(t),
            Err(e) => app.enhance.error = Some((kind, e)),
        }
    }
    let Some(t) = &app.enhance.task else { return };
    match t.rx.try_recv() {
        Ok(r) => {
            let Some(t) = app.enhance.task.take() else { return };
            match r {
                Ok(p) => {
                    let after_size = [p.after.width, p.after.height];
                    let before = texture(app, ctx, "enhance-before", p.before);
                    let after = texture(app, ctx, "enhance-after", p.after);
                    app.enhance.error = None;
                    app.enhance.preview =
                        Some(Shown { kind: t.kind, before, after, after_size, out_size: p.out_size, estimate_secs: p.estimate_secs });
                }
                Err(e) if e == "cancelled" => {}
                Err(e) => {
                    app.enhance.preview = None;
                    app.enhance.error = Some((t.kind, e));
                }
            }
        }
        Err(_) => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
    }
}

/// "about 2 minutes" and the like.
fn duration_text(secs: f64) -> String {
    let s = secs.max(1.0).round();
    if s < 60.0 {
        crate::i18n::tr_format!("about {} seconds", s as u64)
    } else if s < 3600.0 {
        crate::i18n::tr_format!("about {} minutes", (s / 60.0).round().max(1.0) as u64)
    } else {
        crate::i18n::tr_format!("about {} hours", (s / 3600.0).round().max(1.0) as u64)
    }
}

/// Click on the preview: zoom in on the point clicked, or back out; drag pans while zoomed in.
fn preview_input(app: &mut LccApp, ui: &egui::Ui, resp: &egui::Response, img: egui::Rect) {
    let zoomed = app.enhance.zoom.is_some();
    if resp.hovered() {
        ui.ctx().set_cursor_icon(if !zoomed {
            egui::CursorIcon::ZoomIn
        } else if resp.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::Grab
        });
    }
    if resp.clicked() {
        app.enhance.zoom = if zoomed {
            None
        } else {
            resp.interact_pointer_pos()
                .map(|q| (((q.x - img.left()) / img.width()).clamp(0.0, 1.0), ((q.y - img.top()) / img.height()).clamp(0.0, 1.0)))
        };
    } else if resp.dragged()
        && let Some((u, v)) = app.enhance.zoom
    {
        let d = resp.drag_delta();
        app.enhance.zoom = Some((u - d.x / img.width(), v - d.y / img.height()));
    }
}

/// The dialog body: the preview on the left, the method and options on the right.
pub fn body(app: &mut LccApp, ui: &mut egui::Ui, opts: &mut EnhanceDialog) {
    let t = crate::theme::Tokens::get(ui.ctx());
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(300.0, 300.0), egui::Sense::click_and_drag());
            ui.painter().rect_filled(rect, 4.0, t.canvas);
            crate::widgets::register(ui.ctx(), "enhance.preview", rect);
            if let Some(p) = &app.enhance.preview {
                let tex = if app.enhance.before { p.before.id() } else { p.after.id() };
                let (r, zoom) = crate::merge::preview_rect(rect, p.after_size, ui.ctx().pixels_per_point(), app.enhance.zoom);
                app.enhance.zoom = zoom;
                let full = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                ui.painter_at(rect).image(tex, r, full, egui::Color32::WHITE);
                preview_input(app, ui, &resp, r);
            }
            if app.enhance.busy() {
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, crate::i18n::tr("Making the preview…"), t.font(12.0), t.text);
            } else if let Some((_, e)) = &app.enhance.error {
                let galley = ui.painter().layout(e.clone(), t.font(12.0), t.text, rect.width() - 24.0);
                ui.painter().galley(rect.center() - galley.size() / 2.0, galley, t.text);
            }
            ui.horizontal(|ui| {
                if crate::widgets::text_button(ui, "enhanceAfter", "After", !app.enhance.before).clicked() {
                    app.enhance.before = false;
                }
                if crate::widgets::text_button(ui, "enhanceBefore", "Before", app.enhance.before).clicked() {
                    app.enhance.before = true;
                }
                ui.label(egui::RichText::new(crate::i18n::tr("Click to zoom")).color(t.text_dim));
            });
        });
        ui.add_space(12.0);
        ui.vertical(|ui| {
            ui.set_width(240.0);
            ui.label(egui::RichText::new(crate::i18n::tr("Enhancements")).color(t.text_label));
            let kind = opts.kind();
            if ui.radio(kind == EnhanceKind::RawDetails, crate::i18n::tr("Raw Details")).clicked() {
                opts.pick(EnhanceKind::RawDetails);
            }
            crate::dialog_frame::note(
                ui,
                egui::RichText::new(crate::i18n::tr("Finer detail and edges, less false colour, at the same size.")).color(t.text_dim),
            );
            if ui.radio(kind == EnhanceKind::SuperResolution, crate::i18n::tr("Super Resolution")).clicked() {
                opts.pick(EnhanceKind::SuperResolution);
            }
            crate::dialog_frame::note(
                ui,
                egui::RichText::new(crate::i18n::tr("Raw Details, then twice the width and height (four times the pixels).")).color(t.text_dim),
            );
            // the installed upscaler (Settings ▸ AI Models): offered only when there is one
            if app.session.enhance_upscaler_ready() || kind.ai_factor().is_some() {
                if ui.radio(matches!(kind, EnhanceKind::AiSuperResolution(_)), crate::i18n::tr("AI Super Resolution")).clicked() {
                    opts.pick(EnhanceKind::AiSuperResolution(2));
                }
                crate::dialog_frame::note(
                    ui,
                    egui::RichText::new(crate::i18n::tr("Twice the width and height, filled in by the AI upscaler, as a new TIFF."))
                        .color(t.text_dim),
                );
                if ui.radio(kind == EnhanceKind::AiSharpen, crate::i18n::tr("AI Sharpen")).clicked() {
                    opts.pick(EnhanceKind::AiSharpen);
                }
                crate::dialog_frame::note(
                    ui,
                    egui::RichText::new(crate::i18n::tr("Finer detail from the AI upscaler at the same size, as a new TIFF.")).color(t.text_dim),
                );
                if kind.ai_factor().is_some() {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("Colour space of the TIFF")).color(t.text_label));
                    for (key, space) in lcc_engine::enhance::AI_SPACES {
                        if ui.radio(opts.space == key, space.label()).clicked() {
                            opts.space = key.to_string();
                        }
                    }
                    crate::dialog_frame::note(
                        ui,
                        egui::RichText::new(crate::i18n::tr(
                            "Display P3 and Adobe RGB keep more saturated colours than sRGB. The preview is shown in sRGB.",
                        ))
                        .color(t.text_dim),
                    );
                }
            } else if app.session.keeps_ai_models() {
                // where the AI methods come from, in this app's own settings
                crate::dialog_frame::note(
                    ui,
                    egui::RichText::new(crate::i18n::tr("AI Super Resolution and AI Sharpen need an AI upscaler, under Settings ▸ AI Models."))
                        .color(t.text_dim),
                );
                if crate::dialog_frame::link(ui, Some("enhance:openAiModels"), "Open AI Models").clicked() {
                    let _ = app.run("app.settings", json!({"tab": "ai"}));
                }
            }
            ui.add_space(6.0);
            ui.checkbox(&mut opts.stack, crate::i18n::tr("Create Stack"));
            ui.add_space(6.0);
            let n = app.enhance.ids.len();
            match app.enhance.preview.as_ref().filter(|p| p.kind == opts.kind()) {
                Some(p) => {
                    let (w, h) = p.out_size;
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Output: {} × {} px", w, h)).color(t.text_dim));
                    let est = duration_text(p.estimate_secs * n.max(1) as f64);
                    ui.label(egui::RichText::new(crate::i18n::tr_format!("Estimated time: {est}", est = est)).color(t.text_dim));
                }
                None => {
                    ui.label(egui::RichText::new(crate::i18n::tr("Estimating time…")).color(t.text_dim));
                }
            }
            ui.label(egui::RichText::new(crate::i18n::tr_format!("{} photos", n)).color(t.text_dim));
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_estimate_reads_in_the_largest_whole_unit() {
        assert!(duration_text(0.2).contains('1'));
        assert!(duration_text(150.0).contains("minutes"));
        assert!(duration_text(f64::NAN).contains("second"), "a NaN estimate still reads as a time");
    }
}
