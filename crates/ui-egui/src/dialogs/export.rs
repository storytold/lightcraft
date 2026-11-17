use super::frame::{field, num};
use super::{ControlSpec, LightcraftApp, Section, SettingsGroup, Tokens, Track};

/// The Export dialog's watermark preview: the first exported photo, as wide as the dialog whatever
/// its shape, with the watermark drawn on it as the file will have it at size `resize` (`None` =
/// full size). On a worker thread, once per photo, catalog change and size step, the cached
/// thumbnail comes first (at once, a little soft) and then a render at the preview's own pixel size
/// replaces it. The watermark is redrawn on a copy only when it, the output size or the picture
/// changes, which is quick at this size, so the preview follows the sliders.
pub(super) fn watermark_preview(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    wm: &lightcraft_engine::export::Watermark,
    resize: Option<lightcraft_engine::export::Resize>,
) {
    use std::hash::{Hash, Hasher};
    // (pictures received so far: 1 the thumbnail, 2 the sharp render; the latest, or why none came)
    type Base = std::sync::Arc<std::sync::Mutex<(u8, Option<Result<lightcraft_raster::Rgba8, String>>)>>;
    type Shown = (u64, u8, lightcraft_engine::export::Watermark, usize, egui::TextureHandle);
    let t = Tokens::get(ui.ctx());
    let Some(&id) = app.session.selected().first() else { return };
    let Some(photo) = app.session.catalog.photo(id).cloned() else { return };
    let (out_w, out_h) = lightcraft_engine::export::output_size(&photo, &lightcraft_engine::export::ExportOptions { resize, ..Default::default() });
    let export_short = out_w.min(out_h);
    // pixels across the dialog, in steps so a scroll bar or a point of layout doesn't re-render
    let step = 128.0;
    let px = ((ui.available_width() * ui.ctx().pixels_per_point() / step).ceil() * step).clamp(step, 2048.0) as usize;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (id, app.session.catalog.revision, px).hash(&mut h);
    let key = h.finish();
    let ctx = ui.ctx().clone();
    let (base_id, shown_id) = (egui::Id::unique("export-wm-preview-base"), egui::Id::unique("export-wm-preview-shown"));
    let base = match ctx.data(|d| d.get_temp::<(u64, Base)>(base_id)) {
        Some((k, base)) if k == key => base,
        _ => {
            let base: Base = Default::default();
            // the thumbnail, then as wide as the dialog (a tall photo up to three times that high)
            let jobs: Vec<_> = [app.session.thumb_job(id, 512), app.session.render_job(id, px, px * 3, false, true)].into_iter().flatten().collect();
            // the web build has no worker thread: a full render there would hold up the page
            #[cfg(target_arch = "wasm32")]
            let jobs: Vec<_> = jobs.into_iter().take(1).collect();
            if jobs.is_empty() {
                *base.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (1, Some(Err("no such photo".into())));
            } else {
                let (out, repaint) = (base.clone(), ctx.clone());
                let work = move || {
                    for job in jobs {
                        // (a panic in the render is its failure: `RenderJob::run`)
                        let r = job.run().rendered.map(|r| r.image);
                        let mut got = out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        // a failed sharp render keeps the thumbnail
                        if r.is_ok() || !matches!(got.1, Some(Ok(_))) {
                            *got = (got.0.saturating_add(1), Some(r));
                            repaint.request_repaint();
                        }
                    }
                };
                #[cfg(not(target_arch = "wasm32"))]
                if let Err(e) = std::thread::Builder::new().name("lc-watermark-preview".into()).spawn(work) {
                    log::warn!("watermark preview: {e}");
                }
                #[cfg(target_arch = "wasm32")]
                work();
            }
            ctx.data_mut(|d| d.insert_temp(base_id, (key, base.clone())));
            base
        }
    };
    let shown = ctx.data(|d| d.get_temp::<Shown>(shown_id));
    let stage = base.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0;
    let tex = match shown {
        Some((k, g, w, s, tex)) if k == key && g == stage && &w == wm && s == export_short => tex,
        _ => {
            let photo_px = match &base.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 {
                Some(Ok(img)) => Ok(img.clone()),
                Some(Err(e)) => Err(Some(e.clone())),
                None => Err(None),
            };
            let mut img = match photo_px {
                Ok(img) => img,
                Err(e) => {
                    let note = if e.is_some() { "Preview unavailable" } else { "Loading preview…" };
                    let r = ui
                        .horizontal(|ui| {
                            if e.is_none() {
                                crate::progress::spinner(ui, 12.0);
                            }
                            ui.label(egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim))
                        })
                        .inner;
                    if let Some(e) = e {
                        r.on_hover_text(e);
                    }
                    return;
                }
            };
            lightcraft_engine::export::draw_watermark_preview(&mut img, wm, export_short);
            // shown through the monitor profile like every other preview
            if let Some(d) = app.renderer.display()
                && let Err(e) = d.profile.from_srgb(&mut img)
            {
                log::warn!("display profile: {e}");
            }
            let color = std::sync::Arc::new(egui::ColorImage::from_rgba_unmultiplied([img.width, img.height], &img.as_bytes()));
            let tex = match shown {
                Some((.., mut tex)) => {
                    tex.set(color, egui::TextureOptions::LINEAR);
                    tex
                }
                None => ctx.load_texture("export-wm-preview", color, egui::TextureOptions::LINEAR),
            };
            ctx.data_mut(|d| d.insert_temp(shown_id, (key, stage, wm.clone(), export_short, tex.clone())));
            tex
        }
    };
    let size = tex.size_vec2();
    let scale = ui.available_width() / size.x.max(1.0);
    let r = ui.vertical_centered(|ui| ui.add(egui::Image::new((tex.id(), size * scale)))).inner;
    crate::widgets::register(ui, "image:exportWatermarkPreview", r.rect);
}

/// The 16-bit note: LZW's byte dictionary does worse on 16-bit samples than storing them, so
/// 16-bit LZW TIFFs came out 10-33 % larger than uncompressed ones (issue #2882).
pub(crate) const TIFF_LZW_NOTE_16: &str =
    "At 16 bit, LZW is often larger than no compression, and larger and slower than ZIP. Use it only if other software needs LZW.";
/// The note at 8 bit and 32-bit float: one 8-bit file in #2882 was larger with LZW too.
pub(crate) const TIFF_LZW_NOTE: &str =
    "LZW is usually larger and slower than ZIP, and can be larger than no compression. Use it only if other software needs LZW.";

/// The note shown under Compression when a TIFF export uses LZW (none otherwise). The export
/// keeps LZW: some software reads only LZW TIFFs.
pub(crate) fn tiff_lzw_note(o: &lightcraft_engine::export::ExportOptions) -> Option<&'static str> {
    use lightcraft_engine::export::{ExportFormat, OutputDepth, TiffCompression};
    if o.format != ExportFormat::Tiff || o.tiff_compression != TiffCompression::Lzw {
        return None;
    }
    Some(if o.effective_depth() == OutputDepth::U16 { TIFF_LZW_NOTE_16 } else { TIFF_LZW_NOTE })
}

/// [`tiff_lzw_note`] for an export preset's params: the hint on presets that write LZW TIFFs.
pub(crate) fn preset_lzw_note(params: &serde_json::Value) -> Option<&'static str> {
    tiff_lzw_note(&lightcraft_engine::export::ExportOptions::from_json(params))
}

/// The Export dialog's choices as `app.export` params (without the folder).
pub(super) fn export_dialog_params(
    opts: &lightcraft_engine::export::ExportOptions,
    full_size: bool,
    resize: &lightcraft_engine::export::Resize,
    limit_kb: u32,
) -> serde_json::Value {
    let o = lightcraft_engine::export::ExportOptions {
        resize: (!full_size).then_some(*resize),
        limit_kb: (limit_kb > 0).then_some(limit_kb),
        ..opts.clone()
    };
    o.to_json()
}

// ------------------------------------------------------------------------------------------ dialog widgets

/// Two columns of settings-group checkboxes (Copy Settings, Create Preset) plus All / None.
pub(super) fn group_checklist(ui: &mut egui::Ui, tag: &str, groups: &mut Vec<String>) {
    let key_of = |g: &SettingsGroup| serde_json::to_value(g).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    ui.columns(2, |cols| {
        for (i, g) in SettingsGroup::ALL.iter().enumerate() {
            let key = key_of(g);
            let mut on = groups.contains(&key);
            let r = cols[i % 2].checkbox(&mut on, crate::i18n::tr(g.label()));
            crate::widgets::register_response(format!("{tag}:{key}"), r.rect, &r);
            if r.changed() {
                if on {
                    groups.push(key);
                } else {
                    groups.retain(|x| *x != key);
                }
            }
        }
    });
    ui.horizontal(|ui| {
        let all = ui.small_button(crate::i18n::tr("All"));
        crate::widgets::register(ui, format!("{tag}:all"), all.rect);
        if all.clicked() {
            *groups = SettingsGroup::ALL.iter().map(key_of).collect();
        }
        let none = ui.small_button(crate::i18n::tr("None"));
        crate::widgets::register(ui, format!("{tag}:none"), none.rect);
        if none.clicked() {
            groups.clear();
        }
    });
}

/// Width of the label column in dialogs.
pub(super) const LABEL_W: f32 = 78.0;

const fn spec(id: &'static str, label: &'static str, min: f64, max: f64, default: f64, step: f64) -> ControlSpec {
    ControlSpec { id, label, section: Section::Light, min, max, default, step, decimals: 0, track: Track::Plain }
}

pub(super) const QUALITY: ControlSpec = spec("export.quality", "Quality", 1.0, 100.0, 90.0, 1.0);

pub(super) const LIMIT_KB: ControlSpec = spec("export.limitKb", "Limit file size (KB, 0 = off)", 0.0, 20_000.0, 0.0, 10.0);

// whole pixels: a typed size is snapped to the step, and 1080 or 1350 must stay what was typed
const SIZE_PX: ControlSpec = spec("export.sizePx", "Pixels", 16.0, 20_000.0, 2048.0, 1.0);

const SIZE_W: ControlSpec = spec("export.sizeW", "Width (px)", 16.0, 20_000.0, 2048.0, 1.0);

const SIZE_H: ControlSpec = spec("export.sizeH", "Height (px)", 16.0, 20_000.0, 2048.0, 1.0);

const SIZE_MP: ControlSpec = ControlSpec { decimals: 1, ..spec("export.sizeMp", "Megapixels", 0.1, 100.0, 12.0, 0.1) };

const SIZE_PCT: ControlSpec = spec("export.sizePercent", "Percent", 1.0, 400.0, 50.0, 1.0);

pub(super) const START_NUMBER: ControlSpec = spec("export.startNumber", "Start number", 1.0, 9999.0, 1.0, 1.0);

const PPI: ControlSpec = spec("export.ppi", "Resolution (ppi)", 1.0, 1200.0, 240.0, 1.0);

/// Image Sizing: full size, or a resize mode and its value(s), don't enlarge, ppi.
pub(super) fn export_size(ui: &mut egui::Ui, full: &mut bool, r: &mut lightcraft_engine::export::Resize, ppi: &mut u16) {
    use lightcraft_engine::export::ResizeMode as R;
    const MODES: [(R, &str); 8] = [
        (R::LongEdge, "Long Edge"),
        (R::ShortEdge, "Short Edge"),
        (R::Width, "Width"),
        (R::Height, "Height"),
        (R::WidthHeight, "Width & Height"),
        (R::Dimensions, "Dimensions"),
        (R::Megapixels, "Megapixels"),
        (R::Percent, "Percentage"),
    ];
    let r_full = ui.checkbox(full, crate::i18n::tr("Full size"));
    crate::widgets::register(ui, "check:exportFullSize", r_full.rect);
    if !*full {
        field(ui, "Resize to", |ui| {
            let cur = MODES.iter().find(|m| m.0 == r.mode).map_or("Long Edge", |m| m.1);
            let before = r.mode;
            let c = crate::widgets::combo_box("exportResizeMode").width(150.0).selected_text(crate::i18n::tr(cur)).show_ui(ui, |ui| {
                for (m, l) in MODES {
                    ui.selectable_value(&mut r.mode, m, crate::i18n::tr(l));
                }
            });
            crate::widgets::name_combo(&c.response, crate::i18n::tr("Resize"));
            crate::widgets::register(ui, "combo:exportResizeMode", c.response.rect);
            if r.mode != before {
                // a sensible value for the new unit
                r.value = match r.mode {
                    R::Megapixels => 12.0,
                    R::Percent => 50.0,
                    _ if matches!(before, R::Megapixels | R::Percent) => 2048.0,
                    _ => r.value,
                };
                if matches!(r.mode, R::Dimensions | R::WidthHeight) && r.height == 0 {
                    r.height = r.value as u32;
                }
            }
        });
        let mut v = r.value as f64;
        let spec = match r.mode {
            R::Megapixels => &SIZE_MP,
            R::Percent => &SIZE_PCT,
            R::Dimensions | R::WidthHeight => &SIZE_W,
            _ => &SIZE_PX,
        };
        if num(ui, spec, &mut v) {
            r.value = v as f32;
        }
        if matches!(r.mode, R::Dimensions | R::WidthHeight) {
            let mut h = r.height as f64;
            if num(ui, &SIZE_H, &mut h) {
                r.height = h as u32;
            }
        }
        let c = ui.checkbox(&mut r.dont_enlarge, crate::i18n::tr("Don't enlarge"));
        crate::widgets::register(ui, "check:exportDontEnlarge", c.rect);
    }
    let mut p = *ppi as f64;
    if num(ui, &PPI, &mut p) {
        *ppi = p as u16;
    }
}

/// All nine positions supported by both the text and graphic watermark renderer.
/// Keep them in grid order so bottom-centre and edge-centre anchors are available in Export.
pub(super) const WM_POSITIONS: [(lightcraft_engine::export::Anchor, &str); 9] = {
    use lightcraft_engine::export::Anchor::*;
    [(TopLeft, "↖"), (Top, "↑"), (TopRight, "↗"), (Left, "←"), (Center, "•"), (Right, "→"), (BottomLeft, "↙"), (Bottom, "↓"), (BottomRight, "↘")]
};

pub(super) const WM_IMAGE_WIDTH: ControlSpec = spec("export.watermarkImageWidth", "Width (% of photo)", 2.0, 100.0, 20.0, 1.0);

pub(super) const WM_SIZE: ControlSpec = spec("export.watermarkSize", "Size (% of short edge)", 1.0, 15.0, 3.5, 0.5);

pub(super) const WM_OPACITY: ControlSpec = spec("export.watermarkOpacity", "Opacity (%)", 5.0, 100.0, 70.0, 1.0);

pub(super) const WM_INSET_X: ControlSpec =
    ControlSpec { decimals: 1, ..spec("export.watermarkInsetX", "Horizontal inset (% of short edge)", 0.0, 40.0, 2.5, 0.5) };

pub(super) const WM_INSET_Y: ControlSpec =
    ControlSpec { decimals: 1, ..spec("export.watermarkInsetY", "Vertical inset (% of short edge)", 0.0, 40.0, 2.5, 0.5) };

#[cfg(test)]
mod watermark_position_tests {
    use super::WM_POSITIONS;
    use lightcraft_engine::export::Anchor;

    #[test]
    fn export_dialog_offers_every_rendered_anchor_exactly_once() {
        let anchors = [
            Anchor::TopLeft,
            Anchor::Top,
            Anchor::TopRight,
            Anchor::Left,
            Anchor::Center,
            Anchor::Right,
            Anchor::BottomLeft,
            Anchor::Bottom,
            Anchor::BottomRight,
        ];
        assert_eq!(WM_POSITIONS.len(), anchors.len());
        for anchor in anchors {
            assert_eq!(WM_POSITIONS.iter().filter(|(position, _)| *position == anchor).count(), 1);
        }
    }
}

#[cfg(test)]
mod tiff_lzw_note_tests {
    use std::time::Duration;

    use lightcraft_engine::export::{ExportFormat, ExportOptions, TiffCompression};
    use serde_json::json;

    use super::{TIFF_LZW_NOTE, TIFF_LZW_NOTE_16, export_dialog_params, preset_lzw_note, tiff_lzw_note};
    use crate::headless::Headless;
    use crate::state::Dialog;
    use crate::{LightcraftApp, Services};

    const T: Duration = Duration::from_secs(20);

    fn tiff(compression: TiffCompression, bit_depth: Option<u8>) -> ExportOptions {
        ExportOptions { format: ExportFormat::Tiff, tiff_compression: compression, bit_depth, ..Default::default() }
    }

    // Issue #2882: the note shows for LZW only, worded more strongly at 16 bit (TIFF's default depth)
    #[test]
    fn lzw_gets_a_note_stronger_at_16_bit() {
        assert_eq!(tiff_lzw_note(&tiff(TiffCompression::Lzw, None)), Some(TIFF_LZW_NOTE_16), "TIFF defaults to 16-bit");
        assert_eq!(tiff_lzw_note(&tiff(TiffCompression::Lzw, Some(16))), Some(TIFF_LZW_NOTE_16));
        assert_eq!(tiff_lzw_note(&tiff(TiffCompression::Lzw, Some(8))), Some(TIFF_LZW_NOTE));
        assert_eq!(tiff_lzw_note(&tiff(TiffCompression::Lzw, Some(32))), Some(TIFF_LZW_NOTE));
        for compression in [TiffCompression::None, TiffCompression::Deflate, TiffCompression::PackBits] {
            for depth in [None, Some(8), Some(16), Some(32)] {
                assert_eq!(tiff_lzw_note(&tiff(compression, depth)), None, "{compression:?} {depth:?}");
            }
        }
        // the setting only matters for TIFF: a JPEG preset that carries it gets no note
        let jpeg = ExportOptions { format: ExportFormat::Jpeg, tiff_compression: TiffCompression::Lzw, ..Default::default() };
        assert_eq!(tiff_lzw_note(&jpeg), None);
        assert!(TIFF_LZW_NOTE_16.contains("16 bit") && TIFF_LZW_NOTE.contains("other software needs LZW"));
    }

    // Presets that write LZW TIFFs carry the same hint; the built-in ones don't write LZW
    #[test]
    fn presets_with_lzw_get_the_same_hint() {
        assert_eq!(preset_lzw_note(&json!({"format": "tiff", "tiffCompression": "lzw"})), Some(TIFF_LZW_NOTE_16));
        assert_eq!(preset_lzw_note(&json!({"format": "tiff", "tiffCompression": "LZW", "bitDepth": 8})), Some(TIFF_LZW_NOTE));
        assert_eq!(preset_lzw_note(&json!({"format": "tiff", "tiffCompression": "zip"})), None);
        assert_eq!(preset_lzw_note(&json!({"format": "tiff"})), None, "ZIP is the default");
        for p in lightcraft_engine::export::builtin_presets() {
            assert_eq!(preset_lzw_note(&p.params), None, "{}", p.name);
        }
    }

    // The note never changes the choice: the dialog exports exactly the compression picked
    #[test]
    fn the_dialog_keeps_lzw_as_picked() {
        for depth in [None, Some(8), Some(16), Some(32)] {
            let o = tiff(TiffCompression::Lzw, depth);
            let params = export_dialog_params(&o, true, &Default::default(), 0);
            assert_eq!(ExportOptions::from_json(&params).tiff_compression, TiffCompression::Lzw, "{depth:?}: {params}");
        }
    }

    fn set(h: &mut Headless, compression: TiffCompression, bit_depth: Option<u8>) {
        if let Some(Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            opts.format = ExportFormat::Tiff;
            opts.tiff_compression = compression;
            opts.bit_depth = bit_depth;
        } else {
            panic!("no Export dialog");
        }
        h.settle(T);
    }

    fn note_rect(h: &Headless) -> Option<egui::Rect> {
        h.app.widgets.iter().find(|(w, _)| w == "note:exportTiffLzw").map(|(_, r)| *r)
    }

    fn widget(h: &Headless, id: &str) -> egui::Rect {
        h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
    }

    // Scenario: in the Export dialog, picking LZW for a TIFF shows the note under Compression; ZIP
    // and None show none, and the choice stays LZW
    #[test]
    fn export_dialog_shows_the_lzw_note_under_compression() {
        let services = Services { png: None, write_shared: Some(std::sync::Arc::new(|_: &str, _: &[u8]| Ok(()))), ..Default::default() };
        let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        let mut h = Headless::new(app, [1600.0, 1000.0], 1.0);
        h.settle(T);
        let r = h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(T);
        for compression in [TiffCompression::Deflate, TiffCompression::None] {
            set(&mut h, compression, None);
            assert!(note_rect(&h).is_none(), "{compression:?} has no note");
        }
        for depth in [None, Some(8), Some(32)] {
            set(&mut h, TiffCompression::Lzw, depth);
            let note = note_rect(&h).unwrap_or_else(|| panic!("LZW at {depth:?} shows the note"));
            // under the Compression choice, inside the dialog
            assert!(note.top() >= widget(&h, "button:exportTiffCompression-1").bottom() - 1.0, "{note:?}");
            assert!(widget(&h, "dialog:window").contains_rect(note), "{note:?}");
            let Some(Dialog::Export { opts, .. }) = &h.app.ui.dialog else { panic!("dialog closed") };
            assert_eq!(opts.tiff_compression, TiffCompression::Lzw, "the note leaves the choice alone");
        }
        set(&mut h, TiffCompression::Deflate, None);
        assert!(note_rect(&h).is_none(), "back to ZIP: the note goes");
    }
}
