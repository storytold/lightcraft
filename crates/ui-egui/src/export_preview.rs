//! The Export screen's preview: the first photo to be exported, as its file will be. It is made
//! by the export's own rendering ([`lcc_engine::export::prepare_preview`],
//! [`lcc_engine::export::PreparedExport::preview_until`]): the photo at the export's size, with
//! its output sharpening and watermark. So it shows the SDR rendition for an SDR file, and for an
//! HDR file either the HDR rendition (on an HDR display: the same upload the loupe uses) or its
//! SDR rendition with a line saying why HDR isn't shown.
//!
//! The render runs on a thread of its own, under a row in the activity stack, once per photo,
//! edit and setting that changes the picture (size, sharpening, watermark, format and colour);
//! where the files go, their names and their metadata don't re-render it. A newer request
//! stops the one before. What is drawn is fitted to its place on a second, short thread.

use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use egui::{Id, TextureHandle, TextureId};
use lcc_engine::activity::Cancel;
use lcc_engine::export::ExportOptions;

use crate::LccApp;
use crate::panels::dialogs::ExportForm;
use crate::theme::Tokens;

/// The height the line of the file's size takes under the picture.
const CAPTION_H: f32 = 22.0;
/// The preview's picture.
pub(crate) const WIDGET: &str = "image:exportPreview";
/// The line under the picture (why HDR isn't shown, or that no picture could be made).
pub(crate) const NOTE: &str = "label:exportPreviewNote";
/// The file's size under the picture.
pub(crate) const CAPTION: &str = "label:exportPreviewSize";
/// The task row's kind.
pub(crate) const KIND: &str = "exportPreview";
/// The Export screen's preview column, in points.
pub(crate) const WIDTH: f32 = 380.0;

/// The HDR output slot the preview's texture lives in: one of its own, so it never takes a
/// view's ([`crate::render::HDR_LOUPE`]…; the Compare views start at 8).
pub(crate) const HDR_SLOT: u8 = 3;

const BASE: &str = "export-preview-base";
const SHOWN: &str = "export-preview-shown";
const FIT: &str = "export-preview-fit";
const FIT_ERROR: &str = "export-preview-fit-error";
const STATE: &str = "export-preview-state";
const RENDERS: &str = "export-preview-renders";
const SEEN: &str = "export-preview-seen";
/// How long the settings must rest before a render starts (typing a size changes them at every
/// key, and each would be a render of its own; the screen opens without one in the way).
const DEBOUNCE: f64 = 0.15;

const OFF_IN_FILE: &str = "HDR output is off: turn it on under File Settings to write HDR files.";
const FORMAT_IS_SDR: &str = "This format can't carry HDR: JPEG, AVIF, PNG, JPEG XL or 32-bit TIFF can.";
const PHOTO_IS_SDR: &str = "This photo has no HDR edit.";
const DISPLAY_OFF: &str = "HDR display is off: turn it on under Settings ▸ Display (it applies at the next launch).";
const PREVIEW_OFF: &str = "HDR Preview is off: turn it on under View ▸ Loupe Info ▸ HDR Preview.";
const SDR_OF_HDR: &str = "Showing the SDR rendition, as SDR displays show this file.";
const FILE_COPIED: &str = "The file is exported unchanged: the preview shows the photo as developed.";
const VIDEO: &str = "A video clip is exported as a movie: the preview shows a still of the frame.";
const TASK: &str = "Rendering export preview";

type Shared<T> = Arc<Mutex<T>>;

/// A render, as a screen takes it: the SDR rendition as sRGB, the HDR one as linear-light sRGB.
struct Frame {
    sdr: lcc_raster::Rgba8,
    hdr: Option<lcc_raster::Rgb32f>,
}

enum Stage {
    Running,
    Done(Result<Arc<Frame>, String>),
}

/// What a render asked for is doing.
#[derive(Clone)]
enum Phase {
    /// The settings are still changing: it starts once they rest.
    Waiting,
    /// On its way.
    Running(Base),
}

/// The render asked for: its shared state and the flag a newer request stops it with.
#[derive(Clone)]
struct Base {
    state: Shared<Stage>,
    cancel: Arc<AtomicBool>,
}

/// A render fitted to the preview's place (`of`: the render's own size).
pub(crate) struct Fitted {
    pub(crate) sdr: lcc_raster::Rgba8,
    hdr: Option<lcc_raster::Rgb32f>,
    pub(crate) of: [usize; 2],
}

/// (the render's key, the place's size, its result once made)
type Fit = (u64, [usize; 2], Shared<Option<Result<Arc<Fitted>, String>>>);

/// The texture shown: the render `key` fitted to `fit`. Its HDR texture was uploaded for
/// `headroom` (it is uploaded again when that changes).
pub(crate) struct Shown {
    pub(crate) key: u64,
    fit: [usize; 2],
    sdr: TextureHandle,
    pub(crate) fitted: Arc<Fitted>,
    hdr: Option<TextureId>,
    headroom: Option<f32>,
}

/// Whether the preview shows the HDR rendition, or why not (a line for under the picture).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hdr {
    Shown,
    Not(&'static str),
}

fn lock<T>(m: &Shared<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn edited_in_hdr(app: &LccApp, id: lcc_catalog::PhotoId) -> bool {
    app.session.catalog.photo(id).is_some_and(|p| p.develop.hdr.enabled)
}

/// The preview's HDR state for photo `id` exported under `opts`: the HDR rendition when the file
/// has one and the display shows it, else a line saying why.
pub(crate) fn hdr_state(app: &LccApp, id: lcc_catalog::PhotoId, opts: &ExportOptions) -> Hdr {
    if !opts.hdr {
        return Hdr::Not(OFF_IN_FILE);
    }
    let carries = ExportOptions { format: opts.format, bit_depth: opts.bit_depth, hdr: true, ..Default::default() }.hdr_output();
    if !carries {
        return Hdr::Not(FORMAT_IS_SDR);
    }
    if !edited_in_hdr(app, id) {
        return Hdr::Not(PHOTO_IS_SDR);
    }
    // (the HDR display setting applies at the next launch: a window that shows HDR now, shows it)
    if app.hdr_headroom.is_none() {
        return Hdr::Not(if app.ui.settings.hdr_display { crate::panels::edit::HDR_SDR_WINDOW_NOTE } else { DISPLAY_OFF });
    }
    if !app.ui.hdr_preview {
        return Hdr::Not(PREVIEW_OFF);
    }
    Hdr::Shown
}

/// The options whose changes re-render the preview (everything that changes its picture).
fn picture_opts(o: &ExportOptions) -> ExportOptions {
    ExportOptions {
        format: o.format,
        resize: o.resize,
        sharpen: o.sharpen,
        sharpen_amount: o.sharpen_amount,
        watermark: o.watermark.clone(),
        color_space: o.color_space,
        bit_depth: o.bit_depth,
        hdr: o.hdr,
        jxl_lossless: o.jxl_lossless,
        ..Default::default()
    }
}

/// The render's key: the photo, its edits (the catalog's revision) and what changes its picture.
fn key_of(app: &LccApp, id: lcc_catalog::PhotoId, o: &ExportOptions, want_hdr: bool) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (id, app.session.catalog.revision, want_hdr, picture_opts(o).to_json().to_string()).hash(&mut h);
    h.finish()
}

/// Ask for the render of photo `id` as the file under `o` (its HDR rendition too when `want_hdr`):
/// once per key, on a thread; a render asked for before is stopped. `None` while an export runs.
/// The row it holds can't be cancelled from the activity stack: the screen's closing stops it.
fn request(app: &mut LccApp, ctx: &egui::Context, id: lcc_catalog::PhotoId, o: &ExportOptions, want_hdr: bool) -> Option<(u64, Phase)> {
    // (never in an export's way: while one runs the picture before stays, and the render waits)
    if app.session.activity.list().iter().any(|t| t.batch && t.kind == "export") {
        // (asked again once it has ended)
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        return None;
    }
    let key = key_of(app, id, o, want_hdr);
    let had = ctx.data(|d| d.get_temp::<(u64, Base)>(Id::new(BASE)));
    if let Some((_, base)) = had.as_ref().filter(|(k, _)| *k == key) {
        return Some((key, Phase::Running(base.clone())));
    }
    // the settings rest a moment before their render starts
    let now = ctx.input(|i| i.time);
    let since = ctx.data_mut(|d| {
        let seen = d.get_temp_mut_or_default::<(u64, f64)>(Id::new(SEEN));
        if seen.0 != key || seen.1 > now {
            *seen = (key, now);
        }
        seen.1
    });
    if now - since < DEBOUNCE {
        return Some((key, Phase::Waiting));
    }
    if let Some((_, old)) = &had {
        old.cancel.store(true, Ordering::Relaxed);
    }
    let base = Base { state: Arc::new(Mutex::new(Stage::Running)), cancel: Arc::new(AtomicBool::new(false)) };
    ctx.data_mut(|d| {
        let n = d.get_temp_mut_or_default::<u64>(Id::new(RENDERS));
        *n = n.saturating_add(1);
    });
    ctx.data_mut(|d| d.insert_temp(Id::new(BASE), (key, base.clone())));
    let guard = app.session.activity.start(KIND, TASK, Cancel::No);
    let prepared = lcc_engine::export::prepare_preview(&mut app.session, id, o);
    let started = std::thread::Builder::new().name("lc-export-preview".into()).spawn({
        let (state, cancel, ctx) = (Arc::clone(&base.state), Arc::clone(&base.cancel), ctx.clone());
        move || {
            let _guard = guard;
            let stopped = || cancel.load(Ordering::Relaxed);
            let r = match prepared {
                Err(e) => Err(e),
                // (a panic in the render is its failure: `RenderJob::run`)
                Ok(p) => p.keep_hdr(want_hdr).preview_until(&stopped).map(|p| {
                    let (sdr, hdr) = p.to_display();
                    Arc::new(Frame { sdr, hdr: hdr.map(|(w, h, data)| lcc_raster::Rgb32f { width: w, height: h, data }) })
                }),
            };
            // (a render stopped for a newer one leaves no result: nothing waits for it)
            if !stopped() {
                *lock(&state) = Stage::Done(r);
                ctx.request_repaint();
            }
        }
    });
    if let Err(e) = started {
        *lock(&base.state) = Stage::Done(Err(format!("the preview could not be started: {e}")));
    }
    Some((key, Phase::Running(base)))
}

/// The place's size in physical pixels (`box_pts`: points).
fn fit_of(box_pts: egui::Vec2, ppp: f32) -> [usize; 2] {
    let side = |v: f32| if v.is_finite() { ((v.max(1.0) * ppp).floor() as usize).clamp(1, 8192) } else { 1 };
    [side(box_pts.x), side(box_pts.y)]
}

/// `img` shrunk or grown to `w × h`, its pixels' places kept (a triangle filter; 8-bit in, 8-bit
/// out, in two passes with a 16-bit step between so the first one's rounding isn't doubled).
fn resize8(img: &lcc_raster::Rgba8, w: usize, h: usize) -> lcc_raster::Rgba8 {
    use lcc_raster::resample::{Filter, weights};
    let (w, h) = (w.max(1), h.max(1));
    // (the filter's weights, in 16-bit fixed point)
    let taps = |len: usize, out: usize| -> Vec<Vec<(usize, u64)>> {
        weights(len, out, Filter::Bilinear)
            .into_iter()
            .map(|(lo, ws)| ws.into_iter().enumerate().map(|(j, wt)| (lo.saturating_add(j), ((wt.max(0.0) * 65536.0) as u64).min(65535))).collect())
            .collect()
    };
    let (wx, wy) = (taps(img.width, w), taps(img.height, h));
    let row_len = img.width.max(1);
    // pass 1 (across): 8-bit samples as 16-bit ones (× 257), row-major as the source is
    let mut mid: Vec<[u16; 4]> = vec![[0; 4]; w.saturating_mul(img.height)];
    for (src, dst) in img.data.chunks(row_len).zip(mid.chunks_mut(w)) {
        for (x, taps) in wx.iter().enumerate() {
            let mut acc = [0u64; 4];
            for (at, wt) in taps {
                if let Some(p) = src.get(*at) {
                    for (c, v) in p.iter().take(4).enumerate() {
                        if let Some(a) = acc.get_mut(c) {
                            *a = a.saturating_add(u64::from(*v).saturating_mul(257).saturating_mul(*wt));
                        }
                    }
                }
            }
            // (the weights of a column add up to 1, less a little to rounding: never past white)
            if let Some(out) = dst.get_mut(x) {
                *out = acc.map(|v| u16::try_from(v >> 16).unwrap_or(u16::MAX));
            }
        }
    }
    // pass 2 (down)
    let mut out = lcc_raster::Rgba8::new(w, h);
    for (dst, taps) in out.data.chunks_mut(w).zip(wy.iter()) {
        for (x, px) in dst.iter_mut().enumerate() {
            let mut acc = [0u64; 4];
            for (row, wt) in taps {
                if let Some(p) = mid.get(row.saturating_mul(w).saturating_add(x)) {
                    for (c, v) in p.iter().enumerate() {
                        if let Some(a) = acc.get_mut(c) {
                            *a = a.saturating_add(u64::from(*v).saturating_mul(*wt));
                        }
                    }
                }
            }
            let v = acc.map(|v| u8::try_from(v >> 24).unwrap_or(255));
            *px = v;
        }
    }
    out
}

/// `frame` fitted into `fit` (its own shape kept).
fn fit_frame(frame: &Frame, fit: [usize; 2]) -> Result<Arc<Fitted>, String> {
    let (iw, ih) = (frame.sdr.width.max(1) as f64, frame.sdr.height.max(1) as f64);
    let k = (fit[0] as f64 / iw).min(fit[1] as f64 / ih).max(f64::MIN_POSITIVE);
    let (w, h) = (((iw * k).round() as usize).max(1).min(fit[0]), ((ih * k).round() as usize).max(1).min(fit[1]));
    let sdr = if (w, h) == (frame.sdr.width, frame.sdr.height) { frame.sdr.clone() } else { resize8(&frame.sdr, w, h) };
    let hdr = match &frame.hdr {
        Some(hh) if (w, h) != (hh.width, hh.height) => Some(lcc_raster::resample::resize(hh, w, h, lcc_raster::resample::Filter::Lanczos3)),
        Some(hh) => Some(hh.clone()),
        None => None,
    };
    Ok(Arc::new(Fitted { sdr, hdr, of: [frame.sdr.width, frame.sdr.height] }))
}

/// Ask for `frame` (render `key`) fitted to `fit`; a thread makes it. The fit asked for before,
/// for another size, is dropped.
fn request_fit(app: &LccApp, ctx: &egui::Context, key: u64, fit: [usize; 2], frame: &Arc<Frame>) {
    if ctx.data(|d| d.get_temp::<Fit>(Id::new(FIT))).is_some_and(|(k, f, _)| k == key && f == fit) {
        return;
    }
    ctx.data_mut(|d| d.remove::<String>(Id::new(FIT_ERROR)));
    let out: Shared<Option<Result<Arc<Fitted>, String>>> = Arc::new(Mutex::new(None));
    ctx.data_mut(|d| d.insert_temp(Id::new(FIT), (key, fit, Arc::clone(&out))));
    let guard = app.session.activity.start(KIND, TASK, Cancel::No);
    let repaint = ctx.clone();
    let frame = Arc::clone(frame);
    let shared = Arc::clone(&out);
    let started = std::thread::Builder::new().name("lc-export-preview-fit".into()).spawn(move || {
        let _guard = guard;
        *lock(&shared) = Some(fit_frame(&frame, fit));
        repaint.request_repaint();
    });
    if let Err(e) = started {
        *lock(&out) = Some(Err(format!("the preview could not be fitted: {e}")));
    }
}

/// The HDR texture of `fitted`, uploaded for the display's headroom now.
fn upload_hdr(app: &mut LccApp, fitted: &Fitted) -> Option<TextureId> {
    let h = fitted.hdr.as_ref()?;
    let out = app.services.hdr_output.as_mut()?;
    match out.upload(HDR_SLOT, h.width, h.height, h.data.as_flattened()) {
        Ok(tex) => Some(tex),
        Err(e) => {
            log::warn!("export preview HDR texture: {e}");
            None
        }
    }
}

/// Make the texture of a finished fit (the SDR one through the monitor profile, like every
/// preview), replacing what was shown.
fn show_fit(app: &mut LccApp, ctx: &egui::Context, key: u64, fit: [usize; 2], fitted: Arc<Fitted>, want_hdr: bool) {
    let mut img = fitted.sdr.clone();
    if let Some(d) = app.renderer.display()
        && let Err(e) = d.profile.from_srgb(&mut img)
    {
        log::warn!("display profile: {e}");
    }
    let color = Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
    let old = ctx.data(|d| d.get_temp::<Arc<Shown>>(Id::new(SHOWN)));
    ctx.data_mut(|d| d.remove::<Arc<Shown>>(Id::new(SHOWN)));
    let sdr = match old {
        Some(h) => {
            let mut tex = h.sdr.clone();
            tex.set(color, egui::TextureOptions::LINEAR);
            tex
        }
        None => ctx.load_texture("export-preview", color, egui::TextureOptions::LINEAR),
    };
    let hdr = if want_hdr { upload_hdr(app, &fitted) } else { None };
    app.export_preview_hdr = hdr;
    if hdr.is_none()
        && let Some(o) = app.services.hdr_output.as_mut()
    {
        o.release(HDR_SLOT);
    }
    ctx.data_mut(|d| d.insert_temp(Id::new(SHOWN), Arc::new(Shown { key, fit, sdr, fitted, hdr, headroom: app.hdr_headroom })));
}

/// What the preview did on its last frame (its tests read it).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Info {
    /// HDR shown, or why not.
    pub hdr: Hdr,
    /// Its line (why HDR isn't shown, or what went wrong).
    pub line: Option<String>,
    /// A render is on its way.
    pub running: bool,
    /// Renders asked for since the app started.
    pub renders: u64,
    /// What is on screen.
    pub shown: Option<ShownInfo>,
}

/// What the preview shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ShownInfo {
    pub photo: lcc_catalog::PhotoId,
    /// The render's own size (the file's).
    pub of: [usize; 2],
    /// The size it is shown at.
    pub at: [usize; 2],
    /// In HDR.
    pub hdr: bool,
    /// It is the picture asked for now (not the one before, while that renders).
    pub current: bool,
}

/// Whether a render of the preview is on its way or waiting to start (so a settled screen has it).
pub(crate) fn busy(ctx: &egui::Context) -> bool {
    info(ctx).is_some_and(|i| i.running)
}

/// The preview's last frame ([`Info`]).
pub(crate) fn info(ctx: &egui::Context) -> Option<Info> {
    ctx.data(|d| d.get_temp::<Info>(Id::new(STATE)))
}

/// What is shown now: the SDR texture and, when the preview shows HDR, the HDR texture uploaded
/// for the display's headroom now (uploaded again when it changed).
fn shown(app: &mut LccApp, ctx: &egui::Context, want_hdr: bool) -> Option<(egui::TextureId, [usize; 2], Option<TextureId>)> {
    let shown = ctx.data(|d| d.get_temp::<Arc<Shown>>(Id::new(SHOWN)))?;
    let mut hdr = shown.hdr.filter(|_| want_hdr);
    if want_hdr && shown.hdr.is_some() && shown.headroom.map(|h| h.to_bits()) != app.hdr_headroom.map(|h| h.to_bits()) {
        hdr = upload_hdr(app, &shown.fitted);
        app.export_preview_hdr = hdr;
    }
    Some((shown.sdr.id(), [shown.sdr.size_vec2().x as usize, shown.sdr.size_vec2().y as usize], hdr))
}

/// Forgets the preview (its renders and textures) once the Export screen has closed: its render
/// is stopped and its HDR texture freed. Nothing to do when there is none.
pub(crate) fn forget(app: &mut LccApp, ctx: &egui::Context) {
    let base = ctx.data(|d| d.get_temp::<(u64, Base)>(Id::new(BASE)));
    let shown = ctx.data(|d| d.get_temp::<Arc<Shown>>(Id::new(SHOWN)).is_some());
    if base.is_none() && app.export_preview_hdr.is_none() && !shown {
        return;
    }
    ctx.data_mut(|d| d.remove::<(u64, Base)>(Id::new(BASE)));
    if let Some((_, base)) = base {
        base.cancel.store(true, Ordering::Relaxed);
    }
    if app.export_preview_hdr.take().is_some()
        && let Some(o) = app.services.hdr_output.as_mut()
    {
        o.release(HDR_SLOT);
    }
    ctx.data_mut(|d| d.remove::<Arc<Shown>>(Id::new(SHOWN)));
    ctx.data_mut(|d| d.remove::<Fit>(Id::new(FIT)));
    ctx.data_mut(|d| d.remove::<String>(Id::new(FIT_ERROR)));
    ctx.data_mut(|d| d.remove::<Info>(Id::new(STATE)));
}

/// The preview for the first photo to be exported under `f`'s settings. `title`: it is a card of
/// its own (it sits beside the settings); else the settings' form has drawn its section heading.
///
/// Registered: [`WIDGET`] (the picture), [`NOTE`] (its line) and the choice's buttons
/// (`button:exportPreviewMode-0` SDR, `-1` HDR).
pub(crate) fn draw(app: &mut LccApp, ui: &mut egui::Ui, f: ExportForm<'_>, title: bool, max_h: Option<f32>) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let ExportForm { opts, full_size, resize, .. } = f;
    let Some(&id) = app.session.selected().first() else { return };

    // ---- the SDR | HDR choice
    ui.horizontal(|ui| {
        if title {
            ui.label(egui::RichText::new(crate::i18n::tr("Preview")).font(t.semibold(12.5)).color(t.text));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(i) = crate::widgets::segments(ui, "exportPreviewMode", &["SDR", "HDR"], Some(usize::from(app.ui.export_preview_hdr))) {
                app.ui.export_preview_hdr = i == 1;
            }
        });
    });

    // the picture of the file (no size limit: the quality it settles on doesn't change the look)
    let mut o = ExportOptions { resize: (!*full_size).then_some(*resize), limit_kb: None, ..opts.clone() };
    let hdr = hdr_state(app, id, &o);
    let mut line: Option<String> = None;
    if !o.format.is_rendered() {
        // a file copied unchanged, a DNG, a clip: the photo as developed, as a JPEG would be
        let video = app.session.catalog.photo(id).is_some_and(|p| p.video.is_some());
        line = Some(crate::i18n::tr(if video { VIDEO } else { FILE_COPIED }).to_string());
        o.format = lcc_engine::export::ExportFormat::Jpeg;
        o.hdr = false;
    }

    // ---- the render. `hdr` says whether HDR could be shown; the SDR | HDR choice says whether
    // it is.
    let want_hdr = hdr == Hdr::Shown && app.ui.export_preview_hdr;
    let (key, running, frame) = match request(app, &ctx, id, &o, want_hdr) {
        Some((key, Phase::Waiting)) => (key, true, None),
        Some((key, Phase::Running(base))) => {
            let b = lock(&base.state);
            match &*b {
                Stage::Running => (key, true, None),
                Stage::Done(Ok(f)) => (key, false, Some(Arc::clone(f))),
                Stage::Done(Err(e)) => {
                    // the error says what to do (a size, a missing file): it is the picture's line
                    line = Some(e.clone());
                    (key, false, None)
                }
            }
        }
        None => (0, false, None),
    };

    // ---- the picture, its size and its line. The picture sits at the top of what is left, its
    // line at the bottom; the line is laid out first, so the picture knows the room it has.
    let mut all = ui.available_rect_before_wrap();
    // (in the settings' page it is this tall: the page scrolls, and its room is not the screen's)
    if let Some(h) = max_h.filter(|h| h.is_finite() && *h > 0.0) {
        all.max.y = all.top() + h;
    }
    // the room the line takes depends on what is shown, so the picture of the frame before is read first
    let before = ctx.data(|d| d.get_temp::<Arc<Shown>>(Id::new(SHOWN)));
    let text = line.clone().unwrap_or_else(|| match hdr {
        Hdr::Not(why) if app.ui.export_preview_hdr => crate::i18n::tr(why).to_string(),
        // HDR asked for, and no HDR texture yet (it is being uploaded, or the upload failed)
        Hdr::Shown if want_hdr && before.as_ref().is_none_or(|s| s.hdr.is_none()) => crate::i18n::tr(SDR_OF_HDR).to_string(),
        _ => String::new(),
    });
    let line = (!text.is_empty()).then_some(text);
    let with_line = match &line {
        Some(l) => {
            let mut job = egui::text::LayoutJob::simple_singleline(l.clone(), t.font(12.5), t.text_dim);
            job.wrap = egui::text::TextWrapping { max_width: all.width().max(60.0), max_rows: 8, break_anywhere: true, overflow_character: None };
            let h = ui.painter().layout_job(job).size().y + ui.spacing().item_spacing.y;
            egui::Rect::from_min_max(all.min, egui::pos2(all.right(), (all.bottom() - h).max(all.top())))
        }
        None => all,
    };

    // what is shown now: the HDR texture too when the preview shows HDR
    let shown = shown(app, &ctx, want_hdr);
    // the picture's place: the room's width, as tall as the picture is (the one before until the
    // new one is fitted; a landscape one meanwhile)
    let shape = shown.as_ref().map_or(0.67, |(_, size, _)| size[1] as f32 / size[0].max(1) as f32);
    // (the line of the file's size comes under the picture, inside the room above the line)
    let caption = if shown.is_some() { CAPTION_H } else { 0.0 };
    let tall = (with_line.width() * shape.clamp(0.05, 20.0) + ui.spacing().item_spacing.y).min((with_line.height() - caption).max(1.0));
    let (rect, _) = ui.allocate_exact_size(egui::vec2(with_line.width(), tall.max(1.0)), egui::Sense::hover());
    let fit = fit_of(rect.size(), ctx.pixels_per_point());

    // this render, fitted to its place (a thread makes it; meanwhile the picture before stays)
    if let Some(frame) = &frame {
        request_fit(app, &ctx, key, fit, frame);
        let ready = ctx.data(|d| d.get_temp::<Fit>(Id::new(FIT))).filter(|(k, f, _)| *k == key && *f == fit);
        if let Some((_, _, out)) = ready {
            match &*lock(&out) {
                Some(Ok(fitted)) => {
                    if before.as_ref().is_none_or(|s| s.key != key || s.fit != fit) {
                        show_fit(app, &ctx, key, fit, Arc::clone(fitted), want_hdr);
                    }
                }
                // (said once per fit)
                Some(Err(e)) if ctx.data(|d| d.get_temp::<String>(Id::new(FIT_ERROR))).is_none() => {
                    ctx.data_mut(|d| d.insert_temp(Id::new(FIT_ERROR), e.clone()));
                    log::warn!("export preview: {e}");
                }
                Some(Err(_)) => {}
                None => {}
            }
        }
    }
    match &shown {
        Some((sdr, size, hdr_tex)) => {
            let ppp = ctx.pixels_per_point();
            let shown_size = egui::vec2(size[0] as f32 / ppp, size[1] as f32 / ppp);
            // (a place that grew keeps the picture at its own size until it is fitted again)
            let k = (rect.width() / shown_size.x.max(1.0)).min(rect.height() / shown_size.y.max(1.0)).min(1.0);
            let at =
                egui::Rect::from_min_size(egui::pos2(rect.left() + ((rect.width() - shown_size.x * k) / 2.0).max(0.0), rect.top()), shown_size * k);
            let painter = ui.painter().with_clip_rect(rect);
            painter.image(hdr_tex.unwrap_or(*sdr), at, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            crate::widgets::register(&ctx, WIDGET, at);
            // the file's size, as the picture was rendered
            if let Some(s) = before.as_ref() {
                // (a row of the room's own, so the line below keeps its place)
                let (row, _) = ui.allocate_exact_size(egui::vec2(with_line.width(), CAPTION_H), egui::Sense::hover());
                let mut caption = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(egui::Rect::from_min_max(egui::pos2(at.left(), row.top()), egui::pos2(at.right(), row.bottom())))
                        .layout(egui::Layout::top_down(egui::Align::Center)),
                );
                let r = caption.add(egui::Label::new(
                    egui::RichText::new(format!("{} × {} px", s.fitted.of[0], s.fitted.of[1])).font(t.font(12.0)).color(t.text_dim),
                ));
                crate::widgets::register(&ctx, CAPTION, r.rect);
            }
        }
        None if rect.height() >= 28.0 && line.is_none() => {
            let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            row.spacing_mut().item_spacing.x = 8.0;
            row.vertical_centered(|ui| {
                ui.horizontal(|ui| {
                    crate::progress::spinner(ui, 14.0);
                    ui.label(egui::RichText::new(crate::i18n::tr("Loading preview…")).font(t.font(12.5)).color(t.text_dim));
                });
            });
        }
        None => {}
    }
    // the line, at the bottom of the room
    if let Some(text) = &line {
        // (under the picture and its size, which in the settings' page were fitted to the room above it)
        let r = ui.add(egui::Label::new(egui::RichText::new(text.clone()).font(t.font(12.5)).color(t.text_dim)).wrap());
        crate::widgets::register(&ctx, NOTE, r.rect);
    }
    ctx.data_mut(|d| {
        d.insert_temp(
            Id::new(STATE),
            Info {
                hdr,
                line,
                running,
                renders: d.get_temp::<u64>(Id::new(RENDERS)).unwrap_or(0),
                shown: shown.as_ref().map(|(_, size, hdr_tex)| ShownInfo {
                    photo: id,
                    of: before.as_ref().map_or(*size, |s| s.fitted.of),
                    at: *size,
                    hdr: hdr_tex.is_some(),
                    current: before.as_ref().is_some_and(|s| s.key == key && s.fit == fit),
                }),
            },
        );
    });
    if running {
        ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::resize8;

    /// Each pixel keeps its place through a resize: a red left half and a blue right half stay
    /// left and right, shrunk or grown; a solid colour stays solid.
    #[test]
    fn a_resize_keeps_each_pixel_in_its_place() {
        let mut img = lcc_raster::Rgba8::new(64, 8);
        for (i, p) in img.data.iter_mut().enumerate() {
            *p = if i % 64 < 32 { [200, 20, 20, 255] } else { [20, 20, 200, 255] };
        }
        for (w, h) in [(16, 4), (8, 24), (70, 9)] {
            let out = resize8(&img, w, h);
            assert_eq!((out.width, out.height), (w, h), "its size");
            for y in 0..h {
                for x in 0..w {
                    let p = out.data[y * w + x];
                    let red = p[0] > 170 && p[2] < 60;
                    let blue = p[2] > 170 && p[0] < 60;
                    assert!(red || blue, "{w}×{h} at {x},{y}: {p:?}");
                    assert_eq!(x * 2 < w, red, "{w}×{h} at {x},{y}: {p:?} is on the wrong side");
                }
            }
        }
        let mut solid = lcc_raster::Rgba8::new(9, 9);
        for p in solid.data.iter_mut() {
            *p = [37, 99, 201, 255];
        }
        for (w, h) in [(3, 3), (40, 40)] {
            let out = resize8(&solid, w, h);
            assert!(out.data.iter().all(|p| p[0] == 37 && p[1] == 99 && p[2] == 201 && p[3] == 255), "{w}×{h}");
        }
    }
}
