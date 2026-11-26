//! Export settings over SDK controls; the engine still owns validation and starting work.

use craft::behavior::Proposal;
use craft::ui::buttons::Button;
use craft::ui::choice::{Checkbox, Radio, RadioGroup};
use craft::ui::contract::{Bind, Entry, Props as _, Show as _, ShowContainer as _};
use craft::ui::display::{Shade, Text};
use craft::ui::pickers::Select;
use craft::ui::text::{NumberField, TextField};
use craft::ui::value_drag::Slider;
use lightcraft_engine::export::{ExportOptions, Resize};
use serde_json::json;

use super::{LIMIT_KB, QUALITY, START_NUMBER, WM_IMAGE_WIDTH, WM_INSET_X, WM_INSET_Y, WM_OPACITY, WM_POSITIONS, WM_SIZE};
use super::{LightcraftApp, Tokens, export_dialog_params, export_size, preset_lzw_note, tiff_lzw_note, watermark_preview};
use crate::dialogs::frame::trailing_button_row;
use crate::state::Dialog;

/// Never publish automation hits from a speculative layout pass.
fn register(id: impl Into<String>, response: &egui::Response) {
    if !craft::ui::runtime::measure::active(&response.ctx) {
        crate::widgets::register_response(id, response.rect, response);
    }
}

pub(super) fn field<R>(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(super::LABEL_W, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(super::LABEL_W);
            Text::new(crate::i18n::tr(label).to_owned()).shade(Shade::Muted).show(ui);
        });
        add(ui)
    })
    .inner
}

/// Each format/option remains independently addressable, with one roving keyboard group.
pub(super) fn choices<V: PartialEq + Copy + Send + Sync + 'static>(ui: &mut egui::Ui, label: &str, id: &str, options: &[(V, &str)], value: &mut V) {
    let body = |ui: &mut egui::Ui, _: &mut craft::ui::contract::BodyCx<'_>| {
        ui.horizontal_wrapped(|ui| {
            for (i, (value, label)) in options.iter().enumerate() {
                let response = Radio::new(*value).id_salt((id, i)).label(crate::i18n::tr(label).to_owned()).show(ui);
                register(format!("button:{id}-{i}"), &response);
            }
        });
    };
    field(ui, label, |ui| {
        // Native labelled rows have already spent LABEL_W. A flow-mounted SDK container
        // still sees the enclosing Stack column; measure and mount in the actual remainder.
        let measure = |ui: &mut egui::Ui, width: f32| {
            let mut measuring = *value;
            let child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, f32::INFINITY)))
                    .sizing_pass()
                    .invisible(),
            );
            RadioGroup::new(crate::i18n::tr(label).to_owned(), Bind::bound(&mut measuring)).id_salt(id).measure_in(&child, Proposal::Ideal, body)
        };
        // A short group keeps to one line, as main's chips did (the nine formats, "Original files" too): the dialog
        // takes the few points more the radios need. A long one (the watermark's positions) wraps in the room.
        let one_line = measure(&mut *ui, 10_000.0).x.ceil();
        let width = if one_line <= 560.0 { ui.available_width().max(one_line) } else { ui.available_width() }.max(1.0);
        let size = measure(&mut *ui, width);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, size.y.max(24.0)), egui::Sense::hover());
        RadioGroup::new(crate::i18n::tr(label).to_owned(), Bind::bound(value)).id_salt(id).show_at(ui, rect, body);
    });
}

/// Keep the eight resize modes compact, as the existing dialog was: the pixel field must
/// remain visible without scrolling just because a dropdown became eight separate rows.
pub(super) fn resize_mode(
    ui: &mut egui::Ui,
    options: &[(lightcraft_engine::export::ResizeMode, &str)],
    value: &mut lightcraft_engine::export::ResizeMode,
) {
    field(ui, "Resize to", |ui| {
        let entries: Vec<_> = options.iter().map(|(value, label)| Entry::new(*value, crate::i18n::tr(label).to_owned())).collect();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(180.0_f32.min(ui.available_width().max(1.0)), 24.0), egui::Sense::hover());
        let shown = Select::new(crate::i18n::tr("Resize"), Bind::bound(value), &entries).id_salt("exportResizeMode").show_at(ui, rect);
        register("combo:exportResizeMode", &shown.response);
    });
}

pub(super) fn check(ui: &mut egui::Ui, id: &str, label: &str, value: &mut bool) -> egui::Response {
    let before = *value;
    let shown = Checkbox::new(Bind::bound(value)).id_salt(id).label(crate::i18n::tr(label).to_owned()).show(ui);
    let mut response = shown.response;
    if *value != before {
        response.mark_changed();
    }
    register(id, &response);
    response
}

/// A slider row as main's Export had it: the caption and the typed value on one line over a full-width track, with
/// exact typed entry (one-pixel sizes too) and the existing `slider:` / `sliderValue:` / `sliderField:` ids. Every part
/// is placed at a rect of the row, so the row is as wide as the dialog's room (a flow-mounted framework row measured to
/// its content and came out a short track with its caption cut).
pub(super) fn num(ui: &mut egui::Ui, spec: &lightcraft_develop::ControlSpec, value: &mut f64) -> bool {
    const INDENT: f32 = 24.0;
    const END: f32 = 22.0;
    const VALUE_W: f32 = 72.0;
    let before = *value;
    let room = ui.available_width();
    let width = if room.is_finite() { room } else { 400.0 }.max(INDENT + END + VALUE_W + 40.0);
    // one allocation for the caption line and the track, as compact as main's rows (about 50 points with the gap)
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, 22.0 + 18.0), egui::Sense::hover());
    let line = egui::Rect::from_min_size(row.min, egui::vec2(width, 22.0));
    let number_rect = egui::Rect::from_min_size(egui::pos2(line.right() - END - VALUE_W, line.top()), egui::vec2(VALUE_W, 22.0));
    let caption = egui::Rect::from_min_max(egui::pos2(line.left() + INDENT, line.top()), egui::pos2(number_rect.left() - 8.0, line.bottom()));
    Text::new(crate::i18n::tr(spec.label).to_owned()).shade(Shade::Muted).show_at(ui, caption);
    let mut typed = Some(*value);
    let field = NumberField::new(crate::i18n::tr(spec.label), Bind::bound(&mut typed))
        .id_salt(("export-number", spec.id))
        .range(spec.min..=spec.max)
        .step(spec.step)
        .snap_typed(true)
        .commit_decimals(spec.decimals as usize)
        .fixed_decimals(spec.decimals as usize)
        .show_at(ui, number_rect);
    register(format!("sliderValue:{}", spec.id), &field.response);
    register(format!("sliderField:{}", spec.id), &field.response);
    if let Some(number) = typed.filter(|v| v.is_finite()) {
        *value = number;
    }
    let track = egui::Rect::from_min_max(egui::pos2(row.left(), line.bottom()), row.max);
    let slider_rect = egui::Rect::from_min_max(egui::pos2(track.left() + INDENT, track.top()), egui::pos2(track.right() - END, track.bottom()));
    let slider = Slider::new(Bind::bound(value), spec.min..=spec.max)
        .id_salt(("export-slider", spec.id))
        .label(crate::i18n::tr(spec.label))
        .step(spec.step)
        .show_at(ui, slider_rect);
    register(format!("slider:{}", spec.id), &slider.response);
    *value != before
}

fn text(ui: &mut egui::Ui, id: &str, label: &str, value: &mut String, hint: &str) -> egui::Response {
    // This row's label/trailing buttons are placed by egui, not the SDK Stack. FillWidth
    // would fill the enclosing SDK column again and widen the auto-sized dialog by LABEL_W
    // every frame. Mount at the actual remaining native-row rect instead.
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), 24.0), egui::Sense::hover());
    let shown = TextField::new(crate::i18n::tr(label).to_owned(), Bind::bound(value))
        .id_salt(id)
        .placeholder(crate::i18n::tr(hint).to_owned())
        .limit(None)
        .submits()
        .show_at(ui, rect);
    register(id, &shown.response);
    shown.response
}

fn button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    let shown = Button::new(crate::i18n::tr(label).to_owned()).id_salt(id).enabled(enabled).show(ui);
    register(format!("button:{id}"), &shown.response);
    !craft::ui::runtime::measure::active(ui.ctx()) && shown.clicked()
}

/// The template editor keeps its shared cursor-based insertion helper, but its toggle is SDK-owned.
fn naming_tags(ui: &mut egui::Ui) -> bool {
    let key = egui::Id::unique(("exportNaming", "tags-open"));
    let mut open = ui.ctx().data(|data| data.get_temp::<bool>(key)).unwrap_or(false);
    let shown = Button::new(crate::i18n::tr("Tags")).id_salt("export-naming-tags").selected(open).show(ui);
    register("button:exportNamingTags", &shown.response);
    if !craft::ui::runtime::measure::active(ui.ctx()) && shown.clicked() {
        open = !open;
        ui.ctx().data_mut(|data| data.insert_temp(key, open));
    }
    open
}

/// The body of the Export dialog.
pub(super) fn body(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, dlg: &mut Dialog) {
    if craft::ui::runtime::measure::active(ui.ctx()) {
        ui.disable();
    }
    let Dialog::Export { opts, full_size, resize, preset_name, limit_kb, dir } = dlg else { return };
    let n = app.session.selection.ids.len().max(1);
    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if n == 1 { "" } else { "s" }, n = n)).color(t.text_dim));
    export_preset_row(app, ui, opts, full_size, resize, limit_kb);
    ui.add_space(4.0);
    export_format_rows(ui, t, opts);
    export_color_rows(ui, t, opts);
    export_quality_rows(ui, opts, limit_kb);
    if opts.format.is_rendered() {
        export_size(ui, full_size, resize, &mut opts.ppi);
        export_output_rows(app, ui, opts, *full_size, *resize);
    }
    ui.add_space(4.0);
    export_naming_rows(ui, opts);
    export_destination_rows(app, ui, t, opts, dir);
    export_save_preset_row(app, ui, opts, *full_size, resize, *limit_kb, preset_name);
}

/// Export dialog: load a built-in or saved set of options.
fn export_preset_row(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    opts: &mut ExportOptions,
    full_size: &mut bool,
    resize: &mut Resize,
    limit_kb: &mut u32,
) {
    // Preset: load a built-in or saved set of options into the dialog
    field(ui, "Preset", |ui| {
        if craft::ui::runtime::measure::active(ui.ctx()) {
            // Registering a disabled native combo would clear its real keyboard focus.
            ui.allocate_space(egui::vec2(220.0, 24.0));
            return;
        }
        let mut chosen = None;
        let presets = app.session.all_export_presets();
        // the closed box of a dropdown (the value at the start, the chevron at the end), as main's preset combo was
        let (rect, _) = ui.allocate_exact_size(egui::vec2(220.0_f32.min(ui.available_width().max(1.0)), 24.0), egui::Sense::hover());
        let face = crate::widgets::menu_field(ui, rect, "exportPreset", crate::i18n::tr("Preset"), crate::i18n::tr("Choose…"));
        register("combo:exportPreset", &face);
        let menu = craft::ui::menu::Menu::new(crate::i18n::tr("Preset").to_owned()).id_salt(("lc-dropdown-menu", "combo:exportPreset")).values();
        menu.opened_by(&face, craft::ui::contract::Trigger::Click).show(ui, |ui, _| {
            let mut builtin = true;
            for (at, (p, b)) in presets.iter().enumerate() {
                if builtin && !*b {
                    crate::widgets::menu_divider(ui);
                }
                builtin = *b;
                let note = preset_lzw_note(&p.params).map(crate::i18n::tr);
                if crate::widgets::menu_row(ui, ("exportPreset", at), None, crate::i18n::builtin_label(&p.name, *b), note, false, true) {
                    chosen = Some(p.name.clone());
                }
            }
        });
        if let Some(name) = chosen
            && let Ok(params) = app.session.export_params(&json!({"preset": name}))
        {
            let o = lightcraft_engine::export::ExportOptions::from_json(&params);
            *full_size = o.resize.is_none();
            *resize = o.resize.unwrap_or_default();
            *limit_kb = o.limit_kb.unwrap_or(0);
            *opts = o;
        }
    });
}

/// Export dialog: the format, its variants and the bit depth.
fn export_format_rows(ui: &mut egui::Ui, t: &Tokens, opts: &mut ExportOptions) {
    use lightcraft_engine::export::ExportFormat as F;
    let before = opts.format;
    // "Rendered DNG" is a DNG option (below), not a format of its own in this list
    let mut shown = if opts.format == F::RenderedDng { F::Dng } else { opts.format };
    choices(
        ui,
        "Format",
        "exportFormat",
        &[
            (F::Jpeg, "JPEG"),
            (F::Png, "PNG"),
            (F::Tiff, "TIFF"),
            (F::Psd, "PSD"),
            (F::Webp, "WebP"),
            (F::Avif, "AVIF"),
            (F::Jxl, "JPEG XL"),
            (F::Dng, "DNG"),
            (F::Original, "Original files"),
        ],
        &mut shown,
    );
    if shown != opts.format && !(shown == F::Dng && opts.format == F::RenderedDng) {
        opts.format = shown;
    }
    if matches!(opts.format, F::Dng | F::RenderedDng) {
        choices(ui, "DNG of", "exportDngKind", &[(F::Dng, "Raw data"), (F::RenderedDng, "Rendered photo")], &mut opts.format);
    }
    if !opts.format.is_rendered() {
        let note = if opts.format == F::Dng {
            "Raw photos as DNG, with the edits embedded. Size, color and output options don't apply."
        } else {
            "The original files, unchanged, each with an XMP sidecar holding its edits."
        };
        ui.label(egui::RichText::new(note).color(t.text_dim));
    }
    if opts.format != before {
        // each format starts at its own default depth (TIFF 16-bit, others 8-bit)
        opts.bit_depth = None;
    }
    let rendered = opts.format.is_rendered();
    let depths = lightcraft_engine::export::ExportOptions::bit_depths(opts.format);
    if rendered && depths.len() > 1 {
        let mut bd = opts.bit_depth.filter(|b| depths.iter().any(|d| d.0 == *b)).unwrap_or(depths[0].0);
        choices(ui, "Bit depth", "exportBitDepth", depths, &mut bd);
        opts.bit_depth = Some(bd);
    }
}

/// Export dialog: HDR output and the color space it implies (or the choice of one).
fn export_color_rows(ui: &mut egui::Ui, t: &Tokens, opts: &mut ExportOptions) {
    use lightcraft_engine::export::ExportFormat as F;
    let rendered = opts.format.is_rendered();
    if matches!(opts.format, F::Jpeg | F::Tiff | F::Avif | F::Png | F::Jxl) {
        let tip = crate::i18n::tr(
            "Photos edited in HDR: JPEG with an HDR gain map (looks right everywhere, brighter highlights on HDR displays), AVIF as 10-bit, PNG and JPEG XL as 16-bit Rec. 2020 PQ; TIFF needs 32-bit float",
        );
        check(ui, "check:exportHdr", "HDR output", &mut opts.hdr).on_hover_text(tip);
        if opts.hdr && !opts.hdr_output() {
            ui.label(egui::RichText::new(crate::i18n::tr("HDR TIFF needs 32-bit float.")).color(t.text_dim));
        }
        if opts.hdr && opts.format == F::Avif {
            use lightcraft_engine::export::HdrCurve as H;
            choices(ui, "HDR curve", "exportHdrCurve", &[(H::Pq, "PQ"), (H::Hlg, "HLG")], &mut opts.hdr_transfer);
        }
    }
    if !rendered {
    } else if opts.format == F::Avif && opts.hdr {
        let label = if opts.hdr_transfer == lightcraft_engine::export::HdrCurve::Hlg {
            crate::i18n::tr("Color space: Rec. 2020 HLG (HDR AVIF)")
        } else {
            crate::i18n::tr("Color space: Rec. 2020 PQ (HDR AVIF)")
        };
        ui.label(egui::RichText::new(label).color(t.text_dim));
    } else if opts.format == F::Png && opts.hdr {
        ui.label(egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ, 16-bit (HDR PNG)")).color(t.text_dim))
            .on_hover_text(crate::i18n::tr("Viewers that don't read the PNG cICP chunk show PQ values as sRGB: flat and washed out. For a picture that looks right everywhere, use JPEG (gain map)."));
    } else if opts.format == F::Jxl && opts.hdr {
        ui.label(egui::RichText::new(crate::i18n::tr("Color space: Rec. 2020 PQ (HDR JPEG XL)")).color(t.text_dim));
    } else if opts.format == F::Avif {
        ui.label(egui::RichText::new(crate::i18n::tr("Color space: sRGB (AVIF)")).color(t.text_dim));
    } else {
        use lightcraft_engine::export::OutputSpace as C;
        choices(
            ui,
            "Color space",
            "exportColorSpace",
            &[(C::Srgb, "sRGB"), (C::DisplayP3, "P3"), (C::AdobeRgb, "Adobe RGB"), (C::ProPhoto, "ProPhoto"), (C::Rec2020, "Rec.2020")],
            &mut opts.color_space,
        );
        if opts.effective_space() != opts.color_space {
            let note = "Lossy JPEG XL writes Adobe RGB and ProPhoto as Rec. 2020; choose Lossless to keep them.";
            ui.label(egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
        }
    }
}

/// Export dialog: quality and the other options of a lossy or JPEG XL file.
fn export_quality_rows(ui: &mut egui::Ui, opts: &mut ExportOptions, limit_kb: &mut u32) {
    use lightcraft_engine::export::ExportFormat as F;
    if opts.format == F::Jxl {
        let tip = "Mathematically lossless: every pixel is kept exactly (larger files)";
        check(ui, "check:exportJxlLossless", "Lossless", &mut opts.jxl_lossless).on_hover_text(crate::i18n::tr(tip));
    }
    if matches!(opts.format, F::Jpeg | F::Avif) || (opts.format == F::Jxl && !opts.jxl_lossless) {
        let mut q = opts.quality as f64;
        if num(ui, &QUALITY, &mut q) {
            opts.quality = q as u8;
        }
    }
    if opts.format == F::Jxl {
        use lightcraft_engine::export::JxlEffort as E;
        choices(ui, "Effort", "exportJxlEffort", &[(E::Fast, "Fast"), (E::Normal, "Normal")], &mut opts.jxl_effort);
        let tip = "Shows a coarse version first and refines it as the file loads";
        check(ui, "check:exportJxlProgressive", "Progressive", &mut opts.jxl_progressive).on_hover_text(crate::i18n::tr(tip));
    }
    if opts.format == F::Jpeg {
        let mut k = *limit_kb as f64;
        if num(ui, &LIMIT_KB, &mut k) {
            *limit_kb = k as u32;
        }
    }
}

/// Export dialog: sharpening, metadata and the watermark of a rendered photo.
fn export_output_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, opts: &mut ExportOptions, full_size: bool, resize: Resize) {
    use lightcraft_engine::export::{MetadataPolicy as M, SharpenAmount as A, SharpenFor as S};
    choices(
        ui,
        "Sharpen",
        "exportSharpen",
        &[(S::None, "None"), (S::Screen, "Screen"), (S::Matte, "Matte"), (S::Glossy, "Glossy")],
        &mut opts.sharpen,
    );
    if opts.sharpen != S::None {
        choices(ui, "Amount", "exportSharpenAmount", &[(A::Low, "Low"), (A::Standard, "Standard"), (A::High, "High")], &mut opts.sharpen_amount);
    }
    choices(
        ui,
        "Metadata",
        "exportMetadata",
        &[(M::All, "All"), (M::AllExceptCamera, "No camera"), (M::Copyright, "Copyright"), (M::None, "None")],
        &mut opts.metadata,
    );
    if !matches!(opts.metadata, M::None | M::Copyright) {
        check(ui, "check:exportRemoveLocation", "Remove location info", &mut opts.remove_location);
    }
    export_watermark(app, ui, opts, full_size, resize);
}

/// Export dialog: the watermark switch, its preview and its settings.
fn export_watermark(app: &mut LightcraftApp, ui: &mut egui::Ui, opts: &mut ExportOptions, full_size: bool, resize: Resize) {
    let mut wm_on = opts.watermark.is_some();
    if check(ui, "check:exportWatermark", "Watermark", &mut wm_on).changed() {
        opts.watermark = wm_on.then(|| lightcraft_engine::export::Watermark { text: "© ".into(), ..Default::default() });
    }
    let wm_shown = opts.watermark.clone();
    if !craft::ui::runtime::measure::active(ui.ctx())
        && let Some(wm) = &wm_shown
    {
        watermark_preview(app, ui, wm, (!full_size).then_some(resize));
    }
    if let Some(wm) = &mut opts.watermark {
        // text, or a graphic (a logo with transparency)
        let mut graphic = !wm.image.is_empty() || ui.data(|d| d.get_temp::<bool>(egui::Id::unique("wm-graphic"))).unwrap_or(false);
        let before = graphic;
        choices(ui, "Style", "exportWmStyle", &[(false, "Text"), (true, "Graphic")], &mut graphic);
        if graphic != before {
            ui.data_mut(|d| d.insert_temp(egui::Id::unique("wm-graphic"), graphic));
            if !graphic {
                wm.image.clear();
            }
        }
        if graphic {
            field(ui, "Graphic", |ui| {
                trailing_button_row(ui, |ui| {
                    if app.services.pick_files.is_some()
                        && button(ui, "exportWmChoose", "Choose…", true)
                        && let Some(f) = app.services.pick_files.as_mut().and_then(|pick| pick().into_iter().next())
                    {
                        wm.image = f;
                    }
                    text(ui, "field:exportWmImage", "Image", &mut wm.image, "logo.png");
                });
            });
            let mut width = wm.image_width as f64 * 100.0;
            if num(ui, &WM_IMAGE_WIDTH, &mut width) {
                wm.image_width = (width / 100.0) as f32;
            }
        } else {
            choices(
                ui,
                app.ui.language.tr("Text direction"),
                "exportWmOrientation",
                &[(false, app.ui.language.tr("Horizontal text")), (true, app.ui.language.tr("Vertical text"))],
                &mut wm.vertical,
            );
            field(ui, "Text", |ui| {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(1.0), 48.0), egui::Sense::hover());
                let shown = TextField::new(crate::i18n::tr("Text"), Bind::bound(&mut wm.text))
                    .id_salt("export-watermark-text")
                    .lines(2)
                    .limit(None)
                    .placeholder(crate::i18n::tr("© Your Name"))
                    .show_at(ui, rect);
                register("field:exportWmText", &shown.response);
            });
        }
        choices(ui, "Position", "exportWmAnchor", &WM_POSITIONS, &mut wm.anchor);
        choices(ui, "Rotate", "exportWmRotation", &[(0u16, "0°"), (90, "90°"), (180, "180°"), (270, "270°")], &mut wm.rotation);
        let mut size = wm.size as f64 * 100.0;
        if !graphic && num(ui, &WM_SIZE, &mut size) {
            wm.size = (size / 100.0) as f32;
        }
        let mut op = wm.opacity as f64 * 100.0;
        if num(ui, &WM_OPACITY, &mut op) {
            wm.opacity = (op / 100.0) as f32;
        }
        // like Lightroom Classic: a margin from the side edges and one from the top and bottom
        // edges, each unused (greyed) when the position is centred along that axis
        use lightcraft_engine::export::Anchor as P;
        let across = !matches!(wm.anchor, P::Top | P::Center | P::Bottom);
        let mut x = wm.inset_x as f64 * 100.0;
        if ui.add_enabled_ui(across, |ui| num(ui, &WM_INSET_X, &mut x)).inner {
            wm.inset_x = (x / 100.0) as f32;
        }
        let down = !matches!(wm.anchor, P::Left | P::Center | P::Right);
        let mut y = wm.inset_y as f64 * 100.0;
        if ui.add_enabled_ui(down, |ui| num(ui, &WM_INSET_Y, &mut y)).inner {
            wm.inset_y = (y / 100.0) as f32;
        }
        if !graphic {
            check(ui, "check:exportWmShadow", "Shadow", &mut wm.shadow);
        }
    }
    if opts.watermark != wm_shown {
        // the preview above was drawn with the old settings
        ui.ctx().request_repaint();
    }
}

/// Export dialog: compression and the file name.
fn export_naming_rows(ui: &mut egui::Ui, opts: &mut ExportOptions) {
    use lightcraft_engine::export::ExportFormat as F;
    if opts.format == F::Tiff {
        use lightcraft_engine::export::TiffCompression as Z;
        choices(ui, "Compression", "exportTiffCompression", &[(Z::None, "None"), (Z::Lzw, "LZW"), (Z::Deflate, "ZIP")], &mut opts.tiff_compression);
        // issue #2882: LZW stays as picked, with a note on what it costs
        if let Some(note) = tiff_lzw_note(opts) {
            let t = Tokens::get(ui.ctx());
            let r = ui.label(egui::RichText::new(crate::i18n::tr(note)).color(t.text_dim));
            register("note:exportTiffLzw", &r);
        }
    }
    if opts.format == F::Dng {
        use lightcraft_engine::export::DngCompression as Z;
        choices(
            ui,
            "Compression",
            "exportDngCompression",
            &[(Z::Lossless, "Lossless"), (Z::Deflate, "ZIP"), (Z::Uncompressed, "None"), (Z::Lossy, "Lossy")],
            &mut opts.dng_compression,
        );
    }
    // The shared template/tag helper owns this egui TextEdit cursor id. Retain it until that
    // editor is ported as one unit; replacing only its field would break insertion at the caret.
    let naming_id = egui::Id::unique("export-naming");
    let tags_open = field(ui, "File name", |ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // the room the SDK Tags button actually takes (the old reserve was for a smaller native button, which cut
        // the SDK one down to an ellipsis)
        let tags = Button::new(crate::i18n::tr("Tags")).measure_in(ui, Proposal::Ideal).x.ceil();
        let w = (ui.available_width() - tags - 2.0 * ui.spacing().item_spacing.x).floor().max(80.0);
        if craft::ui::runtime::measure::active(ui.ctx()) {
            // A disabled native TextEdit calls ignore_focus. Measure only its allocation so
            // the real pass keeps the active caret, selection, and multi-frame typing stream.
            ui.allocate_space(egui::vec2(w, 24.0));
        } else {
            let response = crate::widgets::named(
                ui.add(
                    egui::TextEdit::singleline(&mut opts.naming)
                        .id(naming_id)
                        .hint_text("{name}-{seq}  ·  {date}  ·  {title}  ·  {folder}")
                        .desired_width(w),
                ),
                crate::i18n::tr("File name"),
            );
            register("field:exportNaming", &response);
        }
        naming_tags(ui)
    });
    if tags_open {
        // The shared helper is an egui editor: never run its interactive body speculatively.
        let key = egui::Id::unique("export-tag-help-height");
        if craft::ui::runtime::measure::active(ui.ctx()) {
            let height = ui.ctx().data(|data| data.get_temp::<f32>(key)).unwrap_or(170.0);
            ui.allocate_space(egui::vec2(ui.available_width(), height));
        } else {
            let top = ui.cursor().top();
            crate::import::tag_help(ui, "exportNaming", &mut opts.naming, naming_id);
            let height = (ui.cursor().top() - top).max(0.0);
            ui.ctx().data_mut(|data| data.insert_temp(key, height));
        }
    }
    crate::import::unknown_tags_warning(ui, &opts.naming);
    if opts.naming.contains("{seq") {
        let mut v = opts.start_number as f64;
        if num(ui, &START_NUMBER, &mut v) {
            opts.start_number = v as u32;
        }
    }
}

/// Export dialog: where the files go and what happens when one exists.
fn export_destination_rows(app: &mut LightcraftApp, ui: &mut egui::Ui, t: &Tokens, opts: &mut ExportOptions, dir: &mut String) {
    // like Lightroom Classic's Export To: one folder, or each photo's own folder (not on
    // the web, where originals live in the browser's storage, not in folders)
    use lightcraft_engine::export::DestinationMode as D;
    if !cfg!(target_arch = "wasm32") || opts.destination_mode != D::SpecificFolder {
        choices(
            ui,
            "Export to",
            "exportDestinationMode",
            &[(D::SpecificFolder, "Specific folder"), (D::SameAsOriginal, "Same folder as original photo")],
            &mut opts.destination_mode,
        );
    }
    if opts.destination_mode == D::SpecificFolder {
        field(ui, "Folder", |ui| {
            trailing_button_row(ui, |ui| {
                if app.services.pick_folder.is_some()
                    && button(ui, "exportChooseFolder", "Choose…", true)
                    && let Some(pick) = app.services.pick_folder.as_mut()
                    && let Some(d) = pick()
                {
                    *dir = d;
                }
                text(ui, "field:exportFolder", "Folder", dir, "");
            });
        });
    }
    field(ui, "Subfolder", |ui| {
        text(ui, "field:exportSubfolder", "Subfolder", &mut opts.subfolder, "none");
    });
    use lightcraft_engine::export::Conflict as K;
    choices(ui, "If file exists", "exportConflict", &[(K::Unique, "Add number"), (K::Overwrite, "Overwrite"), (K::Skip, "Skip")], &mut opts.conflict);
    export_apple_photos_rows(ui, t, opts);
}

/// Export dialog, after export: Apple Photos imports the files written (macOS only).
#[cfg(target_os = "macos")]
fn export_apple_photos_rows(ui: &mut egui::Ui, t: &Tokens, opts: &mut ExportOptions) {
    check(ui, "exportAddToPhotos", "Add to Apple Photos", &mut opts.add_to_photos);
    if opts.add_to_photos {
        field(ui, "Album", |ui| {
            text(ui, "field:exportPhotosAlbum", "Album", &mut opts.photos_album, "none");
        });
        ui.label(
            egui::RichText::new(crate::i18n::tr("The first time, macOS asks whether LightCraft may control Photos.")).size(11.0).color(t.text_dim),
        );
    }
}

/// Nowhere to add them: a setting carried over from a Mac is dropped, not refused.
#[cfg(not(target_os = "macos"))]
fn export_apple_photos_rows(_ui: &mut egui::Ui, _t: &Tokens, opts: &mut ExportOptions) {
    opts.add_to_photos = false;
}

/// Export dialog: save the options as a preset.
fn export_save_preset_row(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    opts: &ExportOptions,
    full_size: bool,
    resize: &Resize,
    limit_kb: u32,
    preset_name: &mut String,
) {
    ui.add_space(4.0);
    field(ui, "Save preset", |ui| {
        trailing_button_row(ui, |ui| {
            let named = !preset_name.trim().is_empty();
            if button(ui, "exportSavePreset", "Save", named) {
                let params = export_dialog_params(opts, full_size, resize, limit_kb);
                match app.run("export.savePreset", json!({"name": preset_name.trim(), "params": params})) {
                    Ok(_) => {
                        app.toast(ui.ctx(), crate::i18n::tr_format!("Saved export preset “{}”", preset_name.trim()));
                        preset_name.clear();
                    }
                    Err(e) => app.toast(ui.ctx(), e),
                }
            }
            text(ui, "field:exportPresetName", "Preset name", preset_name, "Preset name");
        });
    });
}

#[cfg(test)]
mod sdk_export_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use lightcraft_engine::export::{DestinationMode, ExportFormat, Resize};
    use serde_json::{Value, json};

    use crate::headless::{Headless, HeadlessView};
    use crate::state::Dialog;
    use crate::{LightcraftApp, Services};

    const T: Duration = Duration::from_secs(20);
    type Writes = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    fn fixture() -> (Headless, Writes) {
        let writes: Writes = Arc::default();
        let output = writes.clone();
        let services = Services {
            png: None,
            // In-memory sink only. No file, directory or real host picker is ever opened.
            write_shared: Some(Arc::new(move |path, bytes| {
                output.lock().unwrap().push((path.to_owned(), bytes.to_vec()));
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = Headless::new(app, [1600.0, 1500.0], 1.0);
        h.settle(T);
        ask(&mut h, "engine.execute", json!({"command": "dialog.export", "params": {}}));
        if let Some(Dialog::Export { full_size, resize, dir, .. }) = &mut h.app.ui.dialog {
            *full_size = false;
            *resize = Resize::long_edge(16);
            *dir = "/lc-sdk-export-fixture".into();
        }
        h.settle(T);
        (h, writes)
    }

    fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
        let target = params.get("id").and_then(Value::as_str).map(str::to_owned);
        let rects =
            |h: &Headless| h.app.widgets.iter().filter(|(id, _)| Some(id) == target.as_ref() || id == "dialog:window").cloned().collect::<Vec<_>>();
        let before = rects(h);
        let response = h.request(method, params, T);
        assert_eq!(response["ok"], true, "{method}: {response}; before={before:?}; after={:?}", rects(h));
        h.step();
        h.step();
        response
    }

    fn click(h: &mut Headless, id: &str) {
        ask(h, "ui.clickWidget", json!({"id": id}));
    }

    fn edit(h: &mut Headless, id: &str, text: &str) {
        click(h, id);
        ask(h, "ui.key", json!({"key": "A", "cmd": true, "ctrl": true}));
        if text.is_empty() {
            // An empty text event inserts nothing; a real user clears the selected contents.
            ask(h, "ui.key", json!({"key": "Backspace"}));
        } else {
            ask(h, "ui.text", json!({"text": text}));
        }
        ask(h, "ui.key", json!({"key": "Enter"}));
    }

    fn opts(h: &Headless) -> &lightcraft_engine::export::ExportOptions {
        let Some(Dialog::Export { opts, .. }) = &h.app.ui.dialog else { panic!("export draft closed") };
        opts
    }

    #[test]
    fn sdk_export_format_and_cancel_are_draft_only() {
        let (mut h, writes) = fixture();
        let ids = h.app.session.selected();
        let undo = h.app.session.undo.len();
        let revision = h.app.session.catalog.revision;
        let last = h.app.session.last_export.clone();
        click(&mut h, "button:exportFormat-2");
        assert_eq!(opts(&h).format, ExportFormat::Tiff);
        click(&mut h, "button:exportBitDepth-2");
        assert_eq!(opts(&h).bit_depth, Some(32));
        click(&mut h, "button:exportFormat-1");
        assert_eq!(opts(&h).format, ExportFormat::Png);
        assert_eq!(opts(&h).bit_depth, Some(8), "new format starts at its own default");
        edit(&mut h, "field:exportSubfolder", "Proofs");
        assert_eq!(opts(&h).subfolder, "Proofs");
        click(&mut h, "button:dialogCancel");
        assert!(h.app.ui.dialog.is_none());
        assert!(h.app.export.is_none());
        assert!(writes.lock().unwrap().is_empty());
        assert_eq!(h.app.session.last_export, last);
        assert_eq!(h.app.session.selected(), ids);
        assert_eq!(h.app.session.catalog.revision, revision);
        assert_eq!(h.app.session.undo.len(), undo);
        h.settle(T);
    }

    #[test]
    fn sdk_export_folder_picker_cancel_then_choice_and_mode_roundtrip() {
        let (mut h, writes) = fixture();
        let calls = Arc::new(AtomicUsize::new(0));
        let picked = calls.clone();
        h.app.services.pick_folder =
            Some(Box::new(move || (picked.fetch_add(1, Ordering::SeqCst) > 0).then(|| "/lc-sdk-export-fixture/chosen".into())));
        h.settle(T);
        click(&mut h, "button:exportChooseFolder");
        let dir = |h: &Headless| match &h.app.ui.dialog {
            Some(Dialog::Export { dir, .. }) => dir.clone(),
            _ => panic!("no draft"),
        };
        assert_eq!(dir(&h), "/lc-sdk-export-fixture");
        click(&mut h, "button:exportChooseFolder");
        assert_eq!(dir(&h), "/lc-sdk-export-fixture/chosen");
        click(&mut h, "button:exportDestinationMode-1");
        assert_eq!(opts(&h).destination_mode, DestinationMode::SameAsOriginal);
        assert!(!h.app.widgets.iter().any(|(id, _)| id == "field:exportFolder"));
        click(&mut h, "button:exportDestinationMode-0");
        assert_eq!(opts(&h).destination_mode, DestinationMode::SpecificFolder);
        assert_eq!(dir(&h), "/lc-sdk-export-fixture/chosen");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(writes.lock().unwrap().is_empty());
        click(&mut h, "button:dialogCancel");
        h.settle(T);
    }

    #[test]
    fn sdk_export_keyboard_settings_and_exact_pixels_never_start() {
        let (mut h, writes) = fixture();
        let undo = h.app.session.undo.len();
        click(&mut h, "button:exportFormat-0");
        // Mouse selection does not promise keyboard focus in the existing modal. Reach the
        // real selected radio through Tab, rather than testing arrows with no focused stop.
        let jpeg = crate::control::widget_hit(&h.app, "button:exportFormat-0").expect("real JPEG radio hit");
        let mut visited = Vec::new();
        let mut reached = false;
        for _ in 0..48 {
            ask(&mut h, "ui.key", json!({"key": "Tab"}));
            let focused = craft::ui::runtime::focus::focused_rec(&h.view.ctx).map(|record| record.stop.id);
            visited.push(focused);
            if focused == Some(jpeg.id) {
                reached = true;
                break;
            }
        }
        assert!(reached, "Tab must reach the real selected format radio, wanted {:?}, visited {visited:?}", jpeg.id);
        let focused_before = format!("{:?}", craft::ui::runtime::focus::focused_rec(&h.view.ctx));
        ask(&mut h, "ui.key", json!({"key": "ArrowRight"}));
        let focused_after_move = format!("{:?}", craft::ui::runtime::focus::focused_rec(&h.view.ctx));
        ask(&mut h, "ui.key", json!({"key": "Space"}));
        assert_eq!(
            opts(&h).format,
            ExportFormat::Png,
            "roving radio keyboard selection: before={focused_before}, moved={focused_after_move}, after={:?}",
            craft::ui::runtime::focus::focused_rec(&h.view.ctx)
        );
        edit(&mut h, "sliderValue:export.sizePx", "3000");
        let Some(Dialog::Export { resize, .. }) = &h.app.ui.dialog else { panic!("no draft") };
        assert_eq!(resize.value, 3000.0, "no snapping to multiples of sixteen");
        click(&mut h, "combo:exportResizeMode");
        ask(&mut h, "ui.key", json!({"key": "Home"}));
        for _ in 0..4 {
            ask(&mut h, "ui.key", json!({"key": "ArrowDown"}));
        }
        ask(&mut h, "ui.key", json!({"key": "Enter"}));
        edit(&mut h, "sliderValue:export.sizeW", "1080");
        edit(&mut h, "sliderValue:export.sizeH", "1350");
        let Some(Dialog::Export { resize, .. }) = &h.app.ui.dialog else { panic!("no draft") };
        assert_eq!((resize.value, resize.height), (1080.0, 1350));
        assert!(h.app.export.is_none(), "Enter in settings is not Start");
        assert!(writes.lock().unwrap().is_empty());
        assert_eq!(h.app.session.undo.len(), undo);
        ask(&mut h, "ui.key", json!({"key": "Escape"}));
        assert!(h.app.ui.dialog.is_none(), "Escape keeps existing cancel semantics");
        h.settle(T);
    }

    #[test]
    fn sdk_export_start_uses_selected_batch_format_and_destination() {
        let (mut h, writes) = fixture();
        let ids: Vec<_> = h.app.session.visible_cloned().iter().take(2).map(|id| id.0).collect();
        assert_eq!(ids.len(), 2);
        ask(&mut h, "engine.execute", json!({"command": "library.select", "params": {"ids": ids}}));
        let undo = h.app.session.undo.len();
        let revision = h.app.session.catalog.revision;
        click(&mut h, "button:exportFormat-1");
        edit(&mut h, "field:exportSubfolder", "Proofs");
        click(&mut h, "button:dialogOk");
        assert!(h.app.ui.dialog.is_none(), "Start accepted the draft");
        assert!(h.app.export.is_some() || h.app.last_export_result.is_some());
        assert!(h.step_until(T, |h| h.app.export.is_none()), "batch finishes");
        let written = writes.lock().unwrap();
        assert_eq!(written.len(), 2);
        for (path, bytes) in written.iter() {
            assert!(path.starts_with("/lc-sdk-export-fixture/Proofs/"), "{path}");
            assert!(path.ends_with(".png"), "{path}");
            assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"), "real PNG bytes, not a mocked render");
        }
        assert_eq!(h.app.session.undo.len(), undo);
        assert_eq!(h.app.session.catalog.revision, revision);
        assert_eq!(h.app.session.selected().iter().map(|id| id.0).collect::<Vec<_>>(), ids);
        assert_eq!(h.app.session.last_export.as_ref().unwrap()["format"], "png");
        drop(written);
        h.settle(T);
    }

    #[test]
    fn sdk_export_engine_refusal_keeps_empty_destination_and_writes_nothing() {
        let (mut h, writes) = fixture();
        edit(&mut h, "field:exportFolder", "");
        let Some(Dialog::Export { dir, .. }) = &h.app.ui.dialog else { panic!("draft closed while editing") };
        assert!(dir.is_empty(), "clear must actually remove the destination before checking engine refusal");
        click(&mut h, "button:dialogOk");
        assert!(h.app.ui.dialog.is_some(), "missing destination remains correctable");
        assert!(h.app.export.is_none());
        assert!(writes.lock().unwrap().is_empty());
        assert!(h.app.session.last_export.is_none());
        edit(&mut h, "field:exportFolder", "/lc-sdk-export-fixture/corrected");
        click(&mut h, "button:dialogOk");
        assert!(h.app.ui.dialog.is_none());
        assert!(h.step_until(T, |h| h.app.export.is_none()));
        let written = writes.lock().unwrap();
        assert!(!written.is_empty(), "corrected draft starts normally");
        assert!(written.iter().all(|(path, _)| path.starts_with("/lc-sdk-export-fixture/corrected/")));
        drop(written);
        h.settle(T);
    }

    #[test]
    fn sdk_export_pointer_choice_then_single_escape_cancels_without_writes() {
        for id in ["button:exportFormat-1", "check:exportHdr"] {
            let (mut h, writes) = fixture();
            let undo = h.app.session.undo.len();
            let selected = h.app.session.selected();
            let last_export = h.app.session.last_export.clone();
            click(&mut h, id);
            assert!(h.app.ui.dialog.is_some());
            ask(&mut h, "ui.key", json!({"key": "Escape"}));
            assert!(h.app.ui.dialog.is_none(), "single Escape after pointer pick of {id}");
            assert!(h.app.export.is_none());
            assert!(writes.lock().unwrap().is_empty());
            assert_eq!(h.app.session.last_export, last_export);
            assert_eq!(h.app.session.selected(), selected);
            assert_eq!(h.app.session.undo.len(), undo);
            h.settle(T);
        }
    }

    #[test]
    fn sdk_export_filename_typing_keeps_focus_across_frames_and_measure() {
        let (mut h, writes) = fixture();
        if let Some(Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            opts.naming.clear();
        }
        h.settle(T);
        let naming_id = egui::Id::unique("export-naming");
        click(&mut h, "field:exportNaming");
        for (character, expected) in [("a", "a"), ("b", "ab"), ("c", "abc")] {
            ask(&mut h, "ui.text", json!({"text": character}));
            assert_eq!(opts(&h).naming, expected);
            assert!(h.view.ctx.memory(|memory| memory.has_focus(naming_id)), "lost focus after {expected}");
            h.step();
        }
        click(&mut h, "button:dialogCancel");
        assert!(h.app.export.is_none());
        assert!(writes.lock().unwrap().is_empty());
        h.settle(T);
    }

    #[test]
    fn sdk_export_measure_pass_preserves_draft_and_skips_picker_and_writer() {
        let writes: Writes = Arc::default();
        let picked = Arc::new(AtomicUsize::new(0));
        let count = picked.clone();
        let output = writes.clone();
        let services = Services {
            pick_folder: Some(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
                Some("/must-not-be-picked".into())
            })),
            write_shared: Some(Arc::new(move |path, bytes| {
                output.lock().unwrap().push((path.into(), bytes.to_vec()));
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
        let view = HeadlessView::new();
        app.install_framework(&view.ctx);
        let mut draft = Dialog::Export {
            opts: Default::default(),
            full_size: false,
            resize: Resize::long_edge(1080),
            preset_name: "Untouched".into(),
            limit_kb: 0,
            dir: "/fixture/unchanged".into(),
        };
        let before = draft.clone();
        let undo = app.session.undo.len();
        let input = HeadlessView::raw_input(egui::vec2(1000.0, 1400.0), 1.0, 0.0, vec![]);
        let mut rendered = view.ctx.run_ui(input, |ui| {
            let ctx = ui.ctx().clone();
            let tokens = crate::theme::Tokens::get(&ctx);
            let naming_id = egui::Id::unique("export-naming");
            ctx.memory_mut(|memory| memory.request_focus(naming_id));
            let mut sizing = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).sizing_pass().invisible());
            craft::ui::runtime::measure::run(&ctx, || super::super::export_body(&mut app, &mut sizing, &tokens, &mut draft));
            assert!(ctx.memory(|memory| memory.has_focus(naming_id)), "measure must not ignore the real template field's focus");
        });
        rendered.textures_delta.clear();
        assert_eq!(draft, before);
        assert_eq!(picked.load(Ordering::SeqCst), 0);
        assert!(writes.lock().unwrap().is_empty());
        assert!(app.export.is_none());
        assert!(app.session.last_export.is_none());
        assert_eq!(app.session.undo.len(), undo);
        assert!(crate::widgets::take_registry(&view.ctx).is_empty(), "measure must not publish interactive automation hits");
        // A real interaction after the speculative run still reaches the existing picker once.
        // Keep the same egui context so a measurement that consumed input/state cannot hide.
        let mut h = Headless::new(app, [1600.0, 1500.0], 1.0);
        h.view = view;
        h.app.ui.dialog = Some(before);
        h.settle(T);
        click(&mut h, "button:exportChooseFolder");
        assert_eq!(picked.load(Ordering::SeqCst), 1, "the first real picker click is not consumed by measure");
        let Some(Dialog::Export { dir, .. }) = &h.app.ui.dialog else { panic!("real draft closed") };
        assert_eq!(dir, "/must-not-be-picked");
        click(&mut h, "button:dialogCancel");
        assert!(writes.lock().unwrap().is_empty());
        h.settle(T);
    }
}
