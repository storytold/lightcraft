//! The Print dialog (File ▸ Print…): settings on the left, the pages drawn to scale on the right.
//!
//! Every change goes through a `print.*` command ([`lightcraft_engine::cmd`]); this file only draws
//! the session's print ([`lightcraft_engine::Session::print`]) and turns pointer input into
//! commands: dragging a cell is `print.cell.move`, its corner handle `print.cell.resize`, with
//! the guides' snapping done by the command. Widgets register under `print:*`, `button:print*`
//! and `drag:print*` ids for the control channel and the headless tests.
use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_engine::export::ExportOptions;
use lightcraft_engine::print::compose::{self, place};
use lightcraft_engine::print::layout::{self, Snap, Style, Unit};
use lightcraft_engine::print::template::PrintSettings;
use lightcraft_engine::print_color::{ManagedSpace, PrintColorSettings, PrintIntent, PrintMedia, PrintProfile, PrintSharpen};
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::theme::Tokens;
use crate::widgets::{register, text_button};

/// Width of the settings column.
const SETTINGS_W: f32 = 290.0;
/// Handle for resizing the selected cell, screen points.
const HANDLE: f32 = 12.0;
/// Snap distance while dragging, screen points.
const SNAP_PX: f32 = 8.0;
/// Cell sizes offered as buttons, inches (width, height).
const CELL_SIZES: &[(f64, f64)] = &[(2.0, 2.0), (2.5, 3.5), (3.5, 5.0), (4.0, 5.0), (4.0, 6.0), (5.0, 7.0), (8.0, 10.0), (3.0, 3.0)];

/// A drag of a cell in progress: the cell's rectangle when it began and how far the pointer went.
#[derive(Clone, Debug)]
pub struct Drag {
    cell: u32,
    origin: layout::Rect,
    resize: bool,
    /// Where the pointer was pressed; the distance dragged is measured from it, so the first few
    /// points egui spends deciding between a click and a drag are not lost.
    press: Pos2,
    total: Vec2,
}

/// What the Print dialog remembers between frames (not saved).
#[derive(Clone, Debug, Default)]
pub struct PrintUi {
    /// The page being shown, zero-based.
    pub page: usize,
    /// The selected cell.
    pub selected: Option<u32>,
    drag: Option<Drag>,
    /// The name typed for Save Template.
    pub template_name: String,
    /// Size of the next cell added, inches.
    pub new_cell: Option<(f64, f64)>,
}

fn run(app: &mut LightcraftApp, ctx: &egui::Context, id: &str, params: Value) -> Option<Value> {
    match app.run(id, params) {
        Ok(v) => Some(v),
        Err(e) => {
            app.toast(ctx, e);
            None
        }
    }
}

fn patch(app: &mut LightcraftApp, ctx: &egui::Context, params: Value) {
    run(app, ctx, "print.set", params);
}

fn heading(ui: &mut Ui, t: &Tokens, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(crate::i18n::tr(text)).font(t.semibold(12.0)).color(t.text));
    ui.add_space(2.0);
}

fn row<R>(ui: &mut Ui, t: &Tokens, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(vec2(86.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr(label)).color(t.text_label));
        });
        add(ui)
    })
    .inner
}

fn decimals(unit: Unit) -> usize {
    match unit {
        Unit::Inches | Unit::Centimeters => 2,
        Unit::Millimeters => 1,
        Unit::Points => 0,
    }
}

fn fmt_len(inches: f64, unit: Unit) -> String {
    format!("{:.*} {}", decimals(unit), unit.from_inches(inches), unit.label())
}

/// A length field in the ruler's unit; the new value in inches when it changed.
fn length(ui: &mut Ui, id: &str, inches: f64, unit: Unit, min_in: f64, max_in: f64) -> Option<f64> {
    let mut v = unit.from_inches(inches);
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .range(unit.from_inches(min_in)..=unit.from_inches(max_in))
            .speed(unit.per_inch() * 0.01)
            .max_decimals(decimals(unit))
            .suffix(format!(" {}", unit.label())),
    );
    register(ui.ctx(), format!("drag:{id}"), r.rect);
    r.changed().then(|| unit.to_inches(v))
}

fn check(ui: &mut Ui, id: &str, label: &str, value: bool) -> Option<bool> {
    let mut v = value;
    let r = ui.checkbox(&mut v, crate::i18n::tr(label));
    register(ui.ctx(), format!("check:{id}"), r.rect);
    r.changed().then_some(v)
}

fn choice<V: PartialEq + Copy>(ui: &mut Ui, id: &str, options: &[(V, &str)], current: V) -> Option<V> {
    let mut out = None;
    ui.spacing_mut().item_spacing.x = 3.0;
    for (i, (v, l)) in options.iter().enumerate() {
        if text_button(ui, &format!("{id}-{i}"), l, current == *v).clicked() && current != *v {
            out = Some(*v);
        }
    }
    out
}

fn colour(ui: &mut Ui, id: &str, rgb: [u8; 3]) -> Option<[u8; 3]> {
    let mut c = rgb;
    let r = ui.color_edit_button_srgb(&mut c);
    register(ui.ctx(), format!("color:{id}"), r.rect);
    (c != rgb).then_some(c)
}

fn paper_label(name: &str) -> String {
    match name {
        "a3" | "a4" | "a5" => name.to_uppercase(),
        "letter" => "Letter".into(),
        "legal" => "Legal".into(),
        "tabloid" => "Tabloid".into(),
        "custom" => "Custom".into(),
        other => other.replace('x', " × ") + " in",
    }
}

/// The dialog's body: the settings column and the page preview side by side.
pub fn body(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens) {
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    let width = (screen.width() - 80.0).clamp(640.0, 1180.0);
    let height = (screen.height() - 190.0).clamp(280.0, 780.0);
    let mut pu = std::mem::take(&mut app.ui.print);
    let st = app.session.print.clone();
    // (no `id`/`ids` given: the selection, which never fails to parse)
    let ids = app.session.targets(&json!({})).unwrap_or_default();
    // the page shown stays inside the print
    let pages = st.layout.page_count(ids.len());
    pu.page = pu.page.min(pages.saturating_sub(1));
    if pu.selected.is_some_and(|c| st.layout.cell(c).is_none_or(|c| c.page as usize != pu.page)) && pu.drag.is_none() {
        pu.selected = pu.selected.filter(|c| st.layout.cell(*c).is_some());
    }
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(vec2(SETTINGS_W, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(vec2(SETTINGS_W, height));
            egui::ScrollArea::vertical().id_salt("print-settings").max_height(height).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(SETTINGS_W - 14.0);
                settings(app, ui, t, &mut pu, &st, ids.len());
            });
        });
        // (a `separator` would take the dialog's whole free height)
        let (line, _) = ui.allocate_exact_size(vec2(1.0, height), Sense::hover());
        ui.painter().line_segment([line.center_top(), line.center_bottom()], Stroke::new(1.0, t.divider));
        let w = (width - SETTINGS_W - 24.0).max(300.0);
        ui.allocate_ui_with_layout(vec2(w, height), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_min_size(vec2(w, height));
            preview(app, ui, t, &mut pu, &st, &ids, pages);
        });
    });
    app.ui.print = pu;
}

fn settings(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, pu: &mut PrintUi, st: &PrintSettings, photos: usize) {
    let ctx = ui.ctx().clone();
    let l = &st.layout;
    let unit = st.guides.units;
    ui.spacing_mut().item_spacing.y = 5.0;
    ui.label(egui::RichText::new(crate::i18n::tr_format!("{n} photo{}", if photos == 1 { "" } else { "s" }, n = photos)).color(t.text_dim));

    heading(ui, t, "Templates");
    ui.horizontal(|ui| {
        crate::text_field::TextField::singleline("field:printTemplateName", &mut pu.template_name)
            .hint(crate::i18n::tr("Template name"))
            .width(140.0)
            .show(ui);
        if text_button(ui, "printSaveTemplate", "Save", false).clicked() && !pu.template_name.trim().is_empty() {
            run(app, &ctx, "print.template.save", json!({"name": pu.template_name}));
        }
    });
    let names: Vec<String> = app.session.print_templates.iter().map(|t| t.name.clone()).collect();
    for (i, name) in names.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(name).color(t.text_label));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, &format!("printTemplateDelete-{i}"), "Delete", false).clicked() {
                    run(app, &ctx, "print.template.delete", json!({"name": name}));
                }
                if text_button(ui, &format!("printTemplateApply-{i}"), "Apply", false).clicked() {
                    run(app, &ctx, "print.template.apply", json!({"name": name}));
                    pu.page = 0;
                    pu.selected = None;
                }
            });
        });
    }

    heading(ui, t, "Layout Style");
    let styles =
        [(Style::SingleImage, "Single"), (Style::ContactSheet, "Contact"), (Style::PicturePackage, "Package"), (Style::CustomPackage, "Custom")];
    ui.horizontal_wrapped(|ui| {
        if let Some(s) = choice(ui, "printStyle", &styles, l.style) {
            patch(app, &ctx, json!({"layout": {"style": s}}));
            pu.page = 0;
        }
    });

    heading(ui, t, "Image Settings");
    let im = &st.image;
    if let Some(v) = check(ui, "printZoomFill", "Zoom to Fill Frame", im.zoom_to_fill) {
        patch(app, &ctx, json!({"image": {"zoomToFill": v}}));
    }
    if let Some(v) = check(ui, "printRotateFit", "Rotate to Fit", im.rotate_to_fit) {
        patch(app, &ctx, json!({"image": {"rotateToFit": v}}));
    }
    if l.style.is_package() {
        ui.horizontal(|ui| {
            if let Some(v) = check(ui, "printBorder", "Photo Border", im.border_enabled) {
                patch(app, &ctx, json!({"image": {"borderEnabled": v}}));
            }
            if let Some(w) = length(ui, "printBorderWidth", im.border_width, unit, 0.0, 2.0) {
                patch(app, &ctx, json!({"image": {"borderWidth": w}}));
            }
            if let Some(c) = colour(ui, "printBorderColor", im.border_color) {
                patch(app, &ctx, json!({"image": {"borderColor": c}}));
            }
        });
    }
    ui.horizontal(|ui| {
        if let Some(v) = check(ui, "printStroke", "Inner Stroke", im.stroke_enabled) {
            patch(app, &ctx, json!({"image": {"strokeEnabled": v}}));
        }
        let mut pt = im.stroke_width_pt;
        let r = ui.add(egui::DragValue::new(&mut pt).range(0.0..=36.0).speed(0.05).max_decimals(2).suffix(" pt"));
        register(ui.ctx(), "drag:printStrokeWidth", r.rect);
        if r.changed() {
            patch(app, &ctx, json!({"image": {"strokeWidthPt": pt}}));
        }
        if let Some(c) = colour(ui, "printStrokeColor", im.stroke_color) {
            patch(app, &ctx, json!({"image": {"strokeColor": c}}));
        }
    });

    heading(ui, t, "Layout");
    match l.style {
        Style::SingleImage => {
            ui.label(egui::RichText::new(crate::i18n::tr("One photo fills each page inside the margins.")).color(t.text_dim));
        }
        Style::ContactSheet => {
            let g = &l.grid;
            row(ui, t, "Grid", |ui| {
                let (mut c, mut r) = (g.columns, g.rows);
                let a = ui.add(egui::DragValue::new(&mut c).range(1..=layout::MAX_GRID).suffix(" cols"));
                register(ui.ctx(), "drag:printColumns", a.rect);
                let b = ui.add(egui::DragValue::new(&mut r).range(1..=layout::MAX_GRID).suffix(" rows"));
                register(ui.ctx(), "drag:printRows", b.rect);
                if a.changed() || b.changed() {
                    patch(app, &ctx, json!({"layout": {"grid": {"columns": c, "rows": r}}}));
                }
            });
            row(ui, t, "Spacing", |ui| {
                if let Some(v) = length(ui, "printSpacingX", g.spacing_x, unit, 0.0, 2.0) {
                    patch(app, &ctx, json!({"layout": {"grid": {"spacingX": v}}}));
                }
                if let Some(v) = length(ui, "printSpacingY", g.spacing_y, unit, 0.0, 2.0) {
                    patch(app, &ctx, json!({"layout": {"grid": {"spacingY": v}}}));
                }
            });
            if let Some(v) = check(ui, "printKeepSquare", "Keep Square", g.keep_square) {
                patch(app, &ctx, json!({"layout": {"grid": {"keepSquare": v}}}));
            }
        }
        Style::PicturePackage | Style::CustomPackage => {
            ui.label(egui::RichText::new(crate::i18n::tr("Add a cell")).color(t.text_label));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                for (i, (w, h)) in CELL_SIZES.iter().enumerate() {
                    if text_button(ui, &format!("printAddCell-{i}"), &format!("{} × {}", trim(*w), trim(*h)), false).clicked() {
                        add_cell(app, &ctx, pu, *w, *h);
                    }
                }
            });
            let (mut w, mut h) = pu.new_cell.unwrap_or((3.0, 2.0));
            row(ui, t, "Custom size", |ui| {
                if let Some(v) = length(ui, "printNewCellW", w, unit, layout::MIN_CELL_IN, 30.0) {
                    w = v;
                }
                if let Some(v) = length(ui, "printNewCellH", h, unit, layout::MIN_CELL_IN, 30.0) {
                    h = v;
                }
            });
            pu.new_cell = Some((w, h));
            ui.horizontal(|ui| {
                if text_button(ui, "printAddCustom", "Add Cell", false).clicked() {
                    add_cell(app, &ctx, pu, w, h);
                }
                if l.style == Style::PicturePackage && text_button(ui, "printAutoLayout", "Auto Layout", false).clicked() {
                    run(app, &ctx, "print.layout.autoLayout", json!({}));
                    pu.page = 0;
                }
            });
            ui.horizontal(|ui| {
                if text_button(ui, "printNewPage", "New Page", false).clicked()
                    && let Some(r) = run(app, &ctx, "print.layout.newPage", json!({}))
                {
                    pu.page = r["page"].as_u64().unwrap_or(0) as usize;
                }
                if text_button(ui, "printClear", "Clear Layout", false).clicked() {
                    run(app, &ctx, "print.layout.clear", json!({}));
                    pu.page = 0;
                    pu.selected = None;
                }
            });
            row(ui, t, "Gap", |ui| {
                if let Some(v) = length(ui, "printGap", l.spacing, unit, 0.0, 2.0) {
                    patch(app, &ctx, json!({"layout": {"spacing": v}}));
                }
            });
        }
    }

    heading(ui, t, "Rulers, Grid & Guides");
    let g = &st.guides;
    if let Some(v) = check(ui, "printRulers", "Show Rulers", g.show_rulers) {
        patch(app, &ctx, json!({"guides": {"showRulers": v}}));
    }
    row(ui, t, "Units", |ui| {
        let units = [(Unit::Inches, "in"), (Unit::Centimeters, "cm"), (Unit::Millimeters, "mm"), (Unit::Points, "pt")];
        if let Some(u) = choice(ui, "printUnits", &units, g.units) {
            patch(app, &ctx, json!({"guides": {"units": u}}));
        }
    });
    row(ui, t, "Snap", |ui| {
        if let Some(s) = choice(ui, "printSnap", &[(Snap::Off, "Off"), (Snap::Cells, "Cells"), (Snap::Grid, "Grid")], g.snap) {
            patch(app, &ctx, json!({"guides": {"snap": s}}));
        }
    });
    row(ui, t, "Grid size", |ui| {
        if let Some(v) = length(ui, "printGridSize", g.grid_size, unit, 0.05, 10.0) {
            patch(app, &ctx, json!({"guides": {"gridSize": v}}));
        }
    });
    for (id, label, on, key) in [
        ("printShowMargins", "Page Margins", g.show_margins, "showMargins"),
        ("printShowBleed", "Page Bleed", g.show_bleed, "showBleed"),
        ("printShowGrid", "Page Grid", g.show_page_grid, "showPageGrid"),
        ("printShowCells", "Image Cells", g.show_cells, "showCells"),
        ("printShowDims", "Dimensions", g.show_dimensions, "showDimensions"),
    ] {
        if let Some(v) = check(ui, id, label, on) {
            patch(app, &ctx, json!({"guides": {(key): v}}));
        }
    }

    heading(ui, t, "Page Setup");
    row(ui, t, "Paper", |ui| {
        let current = if l.page.paper.eq_ignore_ascii_case("custom") { "custom".to_string() } else { l.page.paper.to_ascii_lowercase() };
        let c = egui::ComboBox::from_id_salt("printPaper").selected_text(paper_label(&current)).show_ui(ui, |ui| {
            for name in layout::paper_names().chain(std::iter::once("custom")) {
                if ui.selectable_label(current == name, paper_label(name)).clicked() {
                    patch(app, &ctx, json!({"layout": {"page": {"paper": name}}}));
                }
            }
        });
        register(ui.ctx(), "combo:printPaper", c.response.rect);
    });
    if l.page.paper.eq_ignore_ascii_case("custom") {
        row(ui, t, "Size", |ui| {
            if let Some(v) = length(ui, "printCustomW", l.page.custom_width, unit, layout::MIN_PAGE_IN, layout::MAX_PAGE_IN) {
                patch(app, &ctx, json!({"layout": {"page": {"customWidth": v}}}));
            }
            if let Some(v) = length(ui, "printCustomH", l.page.custom_height, unit, layout::MIN_PAGE_IN, layout::MAX_PAGE_IN) {
                patch(app, &ctx, json!({"layout": {"page": {"customHeight": v}}}));
            }
        });
    }
    row(ui, t, "Orientation", |ui| {
        let o = [(layout::Orientation::Portrait, "Portrait"), (layout::Orientation::Landscape, "Landscape")];
        if let Some(v) = choice(ui, "printOrientation", &o, l.page.orientation) {
            patch(app, &ctx, json!({"layout": {"page": {"orientation": v}}}));
        }
    });
    let m = &l.page.margins;
    for (label, id, key, v) in [
        ("Left", "printMarginLeft", "left", m.left),
        ("Right", "printMarginRight", "right", m.right),
        ("Top", "printMarginTop", "top", m.top),
        ("Bottom", "printMarginBottom", "bottom", m.bottom),
    ] {
        row(ui, t, label, |ui| {
            if let Some(v) = length(ui, id, v, unit, 0.0, layout::MAX_MARGIN_IN) {
                patch(app, &ctx, json!({"layout": {"page": {"margins": {(key): v}}}}));
            }
        });
    }

    heading(ui, t, "Page");
    row(ui, t, "Background", |ui| {
        if let Some(c) = colour(ui, "printBackground", st.page.background) {
            patch(app, &ctx, json!({"page": {"background": c}}));
        }
    });
    if compose::has_page_options(l.style) {
        ui.label(egui::RichText::new(crate::i18n::tr("Page Options")).color(t.text_label));
        let p = &st.page;
        for (id, label, on, key) in [
            ("printPageNumbers", "Page Numbers", p.page_numbers, "pageNumbers"),
            ("printPageInfo", "Page Info", p.page_info, "pageInfo"),
            ("printCropMarks", "Crop Marks", p.crop_marks, "cropMarks"),
        ] {
            if let Some(v) = check(ui, id, label, on) {
                patch(app, &ctx, json!({"page": {(key): v}}));
            }
        }
    }

    heading(ui, t, "Print Job");
    row(ui, t, "Resolution", |ui| {
        let mut ppi = st.job.ppi;
        let r = ui.add(egui::DragValue::new(&mut ppi).range(72.0..=720.0).speed(1.0).max_decimals(0).suffix(" ppi"));
        register(ui.ctx(), "drag:printPpi", r.rect);
        if r.changed() {
            patch(app, &ctx, json!({"job": {"ppi": ppi}}));
        }
    });
    print_job_color(app, ui, t, &st.job.color);
}

/// Send one colour setting of the print job (the rest stay as they are).
fn patch_color(app: &mut LightcraftApp, ctx: &egui::Context, key: &str, value: Value) {
    patch(app, ctx, json!({"job": {"color": {(key): value}}}));
}

fn to_value<V: serde::Serialize>(v: V) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// The Print Job panel's colour management, print adjustment, print sharpening and depth.
fn print_job_color(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, color: &PrintColorSettings) {
    let ctx = ui.ctx().clone();
    let managed = |space| PrintProfile::Managed { space };
    let shown = match &color.profile {
        PrintProfile::Managed { space: ManagedSpace::Srgb } => crate::i18n::tr("Managed by Printer (sRGB)").to_string(),
        PrintProfile::Managed { space: ManagedSpace::AdobeRgb } => crate::i18n::tr("Managed by Printer (Adobe RGB)").to_string(),
        PrintProfile::Icc { path } => path.file_stem().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()),
    };
    let mut pick = false;
    row(ui, t, "Profile", |ui| {
        let c = egui::ComboBox::from_id_salt("print-profile").width(190.0).selected_text(shown.clone()).show_ui(ui, |ui| {
            for (space, label) in [(ManagedSpace::Srgb, "Managed by Printer (sRGB)"), (ManagedSpace::AdobeRgb, "Managed by Printer (Adobe RGB)")] {
                let r = ui.selectable_label(color.profile == managed(space), crate::i18n::tr(label));
                register(ui.ctx(), format!("item:printProfile-{}", to_value(space).as_str().unwrap_or_default()), r.rect);
                if r.clicked() && color.profile != managed(space) {
                    patch_color(app, &ctx, "profile", to_value(managed(space)));
                }
            }
            if let PrintProfile::Icc { .. } = &color.profile {
                let _ = ui.selectable_label(true, shown.as_str());
            }
            if app.services.pick_display_profile.is_some() {
                let r = ui.selectable_label(false, crate::i18n::tr("Other…"));
                register(ui.ctx(), "item:printProfile-other", r.rect);
                pick |= r.clicked();
            }
        });
        register(ui.ctx(), "combo:printProfile", c.response.rect);
    });
    if pick && let Some(path) = app.services.pick_display_profile.as_mut().and_then(|pick| pick().into_iter().next()) {
        patch_color(app, &ctx, "profile", json!({"kind": "icc", "path": path}));
    }
    row(ui, t, "Intent", |ui| {
        if let Some(v) = choice(ui, "printIntent", &[(PrintIntent::Perceptual, "Perceptual"), (PrintIntent::Relative, "Relative")], color.intent) {
            patch_color(app, &ctx, "intent", to_value(v));
        }
    });
    ui.add_enabled_ui(color.intent == PrintIntent::Relative, |ui| {
        if let Some(v) = check(ui, "printBpc", "Black Point Compensation", color.black_point_compensation) {
            patch_color(app, &ctx, "blackPointCompensation", json!(v));
        }
    });
    for (key, label, value) in [("brightness", "Brightness", color.brightness), ("contrast", "Contrast", color.contrast)] {
        row(ui, t, label, |ui| {
            let mut v = value;
            let r = ui.add(egui::DragValue::new(&mut v).range(-100.0..=100.0).speed(0.5).max_decimals(0));
            register(ui.ctx(), format!("drag:print{label}"), r.rect);
            if r.changed() {
                patch_color(app, &ctx, key, json!(v));
            }
        });
    }
    row(ui, t, "Sharpening", |ui| {
        let options = [(PrintSharpen::Off, "Off"), (PrintSharpen::Low, "Low"), (PrintSharpen::Standard, "Standard"), (PrintSharpen::High, "High")];
        if let Some(v) = choice(ui, "printSharpen", &options, color.sharpen) {
            patch_color(app, &ctx, "sharpen", to_value(v));
        }
    });
    ui.add_enabled_ui(color.sharpen != PrintSharpen::Off, |ui| {
        row(ui, t, "Media", |ui| {
            if let Some(v) = choice(ui, "printMedia", &[(PrintMedia::Matte, "Matte"), (PrintMedia::Glossy, "Glossy")], color.media) {
                patch_color(app, &ctx, "media", to_value(v));
            }
        });
    });
    if let Some(v) = check(ui, "print16bit", "16 bits per channel", color.sixteen_bit) {
        patch_color(app, &ctx, "sixteenBit", json!(v));
    }
    let bits = if color.sixteen_bit { "16-bit" } else { "8-bit" };
    let summary = format!("PDF · {shown} · {bits} · {}", crate::i18n::tr("current edits"));
    ui.label(egui::RichText::new(summary).color(t.text_dim));
}

fn trim(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn add_cell(app: &mut LightcraftApp, ctx: &egui::Context, pu: &mut PrintUi, w: f64, h: f64) {
    if let Some(r) = run(app, ctx, "print.cell.add", json!({"width": w, "height": h})) {
        pu.selected = r["id"].as_u64().and_then(|v| u32::try_from(v).ok());
        pu.page = r["cell"]["page"].as_u64().unwrap_or(0) as usize;
    }
}

// ---- preview -------------------------------------------------------------------------------

/// Screen geometry of the page on show.
struct Sheet {
    page: Rect,
    /// Screen points per inch.
    scale: f32,
}

impl Sheet {
    fn rect(&self, r: &layout::Rect) -> Rect {
        Rect::from_min_size(self.page.min + vec2(r.x as f32, r.y as f32) * self.scale, vec2(r.w as f32, r.h as f32) * self.scale)
    }
}

fn preview(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, pu: &mut PrintUi, st: &PrintSettings, ids: &[PhotoId], pages: usize) {
    let ctx = ui.ctx().clone();
    // room for the page buttons and, for a package, the selected cell's row (which may wrap)
    let footer = if st.layout.style.is_package() { 104.0 } else { 44.0 };
    let size = vec2(ui.available_width(), (ui.available_height() - footer).max(120.0));
    let (area, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 4.0, t.canvas);
    let Ok((pw, ph)) = st.layout.page_size() else {
        painter.text(area.center(), Align2::CENTER_CENTER, crate::i18n::tr("This page setup is not valid"), t.font(13.0), t.caution);
        return;
    };
    let g = &st.guides;
    let ruler = if g.show_rulers { 22.0 } else { 0.0 };
    let inner = Rect::from_min_max(area.min + vec2(ruler, ruler), area.max).shrink(14.0);
    let scale = ((inner.width() as f64 / pw).min(inner.height() as f64 / ph) as f32).max(1.0);
    let sheet = Sheet { page: Rect::from_center_size(inner.center(), vec2(pw as f32, ph as f32) * scale), scale };
    register(&ctx, "print:page", sheet.page);

    painter.rect_filled(sheet.page.translate(vec2(2.0, 3.0)), 0.0, Color32::from_black_alpha(70));
    let bg = st.page.background;
    painter.rect_filled(sheet.page, 0.0, Color32::from_rgb(bg[0], bg[1], bg[2]));
    let guide = Stroke::new(1.0, Color32::from_rgb(90, 160, 240));
    if g.show_rulers {
        rulers(&painter, t, &sheet, area, pw, ph, g.units);
    }
    let area_in = st.layout.printable().ok();
    if g.show_bleed {
        let b = 0.125;
        dashed(&painter, sheet.page.expand(b as f32 * scale), Stroke::new(1.0, t.caution));
    }
    if g.show_page_grid
        && let Some(a) = area_in
    {
        let faint = Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 120, 120, 70));
        let step = g.grid_size.max(0.05);
        let mut x = 0.0;
        while a.x + x <= a.right() + 1e-9 && x < 200.0 {
            let sx = sheet.page.left() + (a.x + x) as f32 * scale;
            painter.line_segment([pos2(sx, sheet.page.top()), pos2(sx, sheet.page.bottom())], faint);
            x += step;
        }
        let mut y = 0.0;
        while a.y + y <= a.bottom() + 1e-9 && y < 200.0 {
            let sy = sheet.page.top() + (a.y + y) as f32 * scale;
            painter.line_segment([pos2(sheet.page.left(), sy), pos2(sheet.page.right(), sy)], faint);
            y += step;
        }
    }
    if g.show_margins
        && let Some(a) = area_in
    {
        dashed(&painter, sheet.rect(&layout::Rect::new(a.x, a.y, a.w, a.h)), guide);
    }

    let package = st.layout.style.is_package();
    if package {
        package_cells(app, ui, &painter, t, pu, st, ids, &sheet);
    } else {
        let plan = compose::plan_cells(&st.layout, ids).ok();
        let cells: Vec<(layout::Rect, Option<PhotoId>)> = match plan.as_ref().and_then(|p| p.get(pu.page)) {
            Some(page) => page.iter().map(|(r, id)| (*r, Some(*id))).collect(),
            None => st.layout.grid_rects().unwrap_or_default().into_iter().map(|r| (r, None)).collect(),
        };
        let mut shown = Vec::new();
        for (i, (r, photo)) in cells.iter().enumerate() {
            let rect = sheet.rect(r);
            register(&ctx, format!("print:gridcell:{i}"), rect);
            shown.extend(paint_cell(app, &painter, t, st, rect, *photo, &sheet, r));
        }
        page_options(&painter, st, &sheet, &shown, pu.page + 1);
    }

    // footer: page navigation and the selected cell
    ui.horizontal(|ui| {
        if text_button(ui, "printPagePrev", "◀", false).clicked() && pu.page > 0 {
            pu.page -= 1;
            pu.selected = None;
        }
        let label = ui.label(egui::RichText::new(format!("{} {} / {}", crate::i18n::tr("Page"), pu.page + 1, pages)).color(t.text));
        register(ui.ctx(), "print:pageLabel", label.rect);
        if text_button(ui, "printPageNext", "▶", false).clicked() && pu.page + 1 < pages {
            pu.page += 1;
            pu.selected = None;
        }
        ui.label(egui::RichText::new(fmt_len(pw, g.units) + " × " + &fmt_len(ph, g.units)).color(t.text_dim));
        if package {
            ui.label(egui::RichText::new(format!("{} {}", st.layout.cells.len(), crate::i18n::tr("cells"))).color(t.text_dim));
            if st.layout.has_overlaps() {
                ui.label(egui::RichText::new(crate::i18n::tr("cells overlap")).color(t.caution));
            }
        }
    });
    if package {
        inspector(app, ui, t, pu, st);
    }
}

fn press_origin(ui: &Ui, resp: &egui::Response) -> Pos2 {
    ui.input(|i| i.pointer.press_origin()).or_else(|| resp.interact_pointer_pos()).unwrap_or_default()
}

/// Draws and handles every cell of a package page, back to front.
#[allow(clippy::too_many_arguments)]
fn package_cells(
    app: &mut LightcraftApp,
    ui: &mut Ui,
    painter: &egui::Painter,
    t: &Tokens,
    pu: &mut PrintUi,
    st: &PrintSettings,
    ids: &[PhotoId],
    sheet: &Sheet,
) {
    let ctx = ui.ctx().clone();
    let first = ids.first().copied();
    let cells: Vec<layout::Cell> = st.layout.cells_on_page(pu.page as u32).cloned().collect();
    for c in &cells {
        let photo = if st.layout.style == Style::CustomPackage { c.photo.map(PhotoId).or(first) } else { first };
        let rect = sheet.rect(&c.rect);
        paint_cell(app, painter, t, st, rect, photo, sheet, &c.rect);
        let resp = ui.interact(rect, egui::Id::new(("print-cell", c.id)), Sense::click_and_drag());
        register(&ctx, format!("print:cell:{}", c.id), rect);
        if resp.clicked() || resp.drag_started() {
            pu.selected = Some(c.id);
        }
        if resp.drag_started() {
            pu.drag = Some(Drag { cell: c.id, origin: c.rect, resize: false, press: press_origin(ui, &resp), total: Vec2::ZERO });
        }
        if resp.dragged()
            && let Some(d) = pu.drag.as_mut().filter(|d| d.cell == c.id && !d.resize)
        {
            d.total = resp.interact_pointer_pos().map_or(d.total, |p| p - d.press);
            let (x, y) = (d.origin.x + f64::from(d.total.x / sheet.scale), d.origin.y + f64::from(d.total.y / sheet.scale));
            let threshold = f64::from(SNAP_PX / sheet.scale);
            run(app, &ctx, "print.cell.move", json!({"id": c.id, "x": x, "y": y, "snap": true, "threshold": threshold}));
        }
        if resp.drag_stopped() {
            pu.drag = None;
        }
        let id = c.id;
        resp.context_menu(|ui| {
            for (how, label) in
                [("toFront", "Bring to Front"), ("forward", "Bring Forward"), ("backward", "Send Backward"), ("toBack", "Send to Back")]
            {
                if ui.button(crate::i18n::tr(label)).clicked() {
                    run(app, &ctx, "print.cell.stack", json!({"id": id, "order": how}));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button(crate::i18n::tr("Delete Cell")).clicked() {
                run(app, &ctx, "print.cell.delete", json!({"id": id}));
                ui.close();
            }
        });
    }
    // the selected cell's outline and resize handle go over everything
    let Some(sel) = pu.selected.and_then(|s| cells.iter().find(|c| c.id == s)) else { return };
    let rect = sheet.rect(&sel.rect);
    painter.rect_stroke(rect, 0.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
    let handle = Rect::from_center_size(rect.right_bottom(), Vec2::splat(HANDLE));
    painter.rect_filled(handle, 2.0, t.accent);
    let resp = ui.interact(handle, egui::Id::new(("print-resize", sel.id)), Sense::drag());
    register(&ctx, format!("print:resize:{}", sel.id), handle);
    if resp.drag_started() {
        pu.drag = Some(Drag { cell: sel.id, origin: sel.rect, resize: true, press: press_origin(ui, &resp), total: Vec2::ZERO });
    }
    if resp.dragged()
        && let Some(d) = pu.drag.as_mut().filter(|d| d.cell == sel.id && d.resize)
    {
        d.total = resp.interact_pointer_pos().map_or(d.total, |p| p - d.press);
        let (w, h) = (d.origin.w + f64::from(d.total.x / sheet.scale), d.origin.h + f64::from(d.total.y / sheet.scale));
        let threshold = f64::from(SNAP_PX / sheet.scale);
        run(
            app,
            &ctx,
            "print.cell.resize",
            json!({"id": sel.id, "width": w.max(layout::MIN_CELL_IN), "height": h.max(layout::MIN_CELL_IN), "snap": true, "threshold": threshold}),
        );
    }
    if resp.drag_stopped() {
        pu.drag = None;
    }
}

/// The selected cell's numbers, its place in the stack and its photo.
fn inspector(app: &mut LightcraftApp, ui: &mut Ui, t: &Tokens, pu: &mut PrintUi, st: &PrintSettings) {
    let ctx = ui.ctx().clone();
    let Some(cell) = pu.selected.and_then(|id| st.layout.cell(id)) else {
        ui.label(egui::RichText::new(crate::i18n::tr("Click a cell to move, resize or arrange it")).color(t.text_dim));
        return;
    };
    let (id, r, unit) = (cell.id, cell.rect, st.guides.units);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if let Some(v) = length(ui, "printCellX", r.x, unit, 0.0, 60.0) {
            run(app, &ctx, "print.cell.move", json!({"id": id, "x": v, "y": r.y}));
        }
        if let Some(v) = length(ui, "printCellY", r.y, unit, 0.0, 60.0) {
            run(app, &ctx, "print.cell.move", json!({"id": id, "x": r.x, "y": v}));
        }
        if let Some(v) = length(ui, "printCellW", r.w, unit, layout::MIN_CELL_IN, 60.0) {
            run(app, &ctx, "print.cell.resize", json!({"id": id, "width": v, "height": r.h}));
        }
        if let Some(v) = length(ui, "printCellH", r.h, unit, layout::MIN_CELL_IN, 60.0) {
            run(app, &ctx, "print.cell.resize", json!({"id": id, "width": r.w, "height": v}));
        }
        for (i, (how, label)) in [("toBack", "Back"), ("backward", "Backward"), ("forward", "Forward"), ("toFront", "Front")].into_iter().enumerate()
        {
            if text_button(ui, &format!("printStack-{i}"), label, false).clicked() {
                run(app, &ctx, "print.cell.stack", json!({"id": id, "order": how}));
            }
        }
        if st.layout.style == Style::CustomPackage {
            if text_button(ui, "printCellPhoto", "Use Active Photo", false).clicked() {
                let active = app.session.active().map(|p| p.0);
                run(app, &ctx, "print.cell.photo", json!({"id": id, "photo": active}));
            }
            if cell.photo.is_some() && text_button(ui, "printCellPhotoClear", "Clear Photo", false).clicked() {
                run(app, &ctx, "print.cell.photo", json!({"id": id, "photo": null}));
            }
        }
        if text_button(ui, "printCellDelete", "Delete", false).clicked() {
            run(app, &ctx, "print.cell.delete", json!({"id": id}));
            pu.selected = None;
        }
    });
}

/// One cell on screen: the photo as it will print (fit or fill, turned, border, stroke).
#[allow(clippy::too_many_arguments)]
fn paint_cell(
    app: &mut LightcraftApp,
    painter: &egui::Painter,
    t: &Tokens,
    st: &PrintSettings,
    cell: Rect,
    photo: Option<PhotoId>,
    sheet: &Sheet,
    inches: &layout::Rect,
) -> Option<Rect> {
    let im = &st.image;
    if st.guides.show_cells {
        painter.rect_stroke(cell, 0.0, Stroke::new(1.0, Color32::from_rgba_unmultiplied(120, 150, 190, 150)), StrokeKind::Inside);
    }
    let border = if st.layout.style.is_package() && im.border_enabled {
        (im.border_width as f32 * sheet.scale).min(cell.width().min(cell.height()).max(2.0) / 2.0 - 1.0).max(0.0)
    } else {
        0.0
    };
    let avail = cell.shrink(border);
    if avail.width() < 1.0 || avail.height() < 1.0 {
        return None;
    }
    // the photo's shape: its cropped size, or 3:2 for a placeholder
    let (nw, nh) = photo.and_then(|id| app.session.catalog.photo(id)).map_or((3.0, 2.0), |p| {
        let (w, h) = lightcraft_engine::export::output_size(p, &ExportOptions::default());
        (w as f64, h as f64)
    });
    let scale = 8.0; // sub-point resolution for the rounding in `place`
    let aw = (f64::from(avail.width()) * scale).round() as usize;
    let ah = (f64::from(avail.height()) * scale).round() as usize;
    let p = place(nw, nh, aw, ah, im.zoom_to_fill, im.rotate_to_fit)?;
    let shown = Rect::from_center_size(avail.center(), vec2(p.w as f32, p.h as f32) / scale as f32);
    let visible = shown.intersect(avail);
    if border > 0.0 {
        let frame = visible.expand(border);
        let c = im.border_color;
        painter.rect_filled(frame.intersect(cell), 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    }
    let clip = painter.with_clip_rect(avail.intersect(painter.clip_rect()));
    let tex = photo.and_then(|id| {
        // Use the library's thumbnail when there is one. A thumbnail is kept at one size, so asking
        // for another size here every frame made the preview and the grid behind the dialog
        // re-render each other's thumbnail for as long as the dialog stayed open.
        if app.renderer.thumb(id).is_none() {
            let px = (shown.width().max(shown.height()) * 2.0).clamp(64.0, 1024.0) as usize;
            crate::panels::grid::request_thumb(app, id, px, 10);
        }
        app.renderer.thumb(id).map(|tex| tex.tex.id())
    });
    match tex {
        Some(id) => {
            let mut mesh = egui::Mesh::with_texture(id);
            // an unturned quad maps the texture straight; a turned one (a quarter clockwise) takes
            // the left edge to the top
            let uv: [Pos2; 4] = if p.rotated {
                [pos2(0.0, 1.0), pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0)]
            } else {
                [pos2(0.0, 0.0), pos2(1.0, 0.0), pos2(1.0, 1.0), pos2(0.0, 1.0)]
            };
            let corners = [shown.left_top(), shown.right_top(), shown.right_bottom(), shown.left_bottom()];
            for (pos, uv) in corners.into_iter().zip(uv) {
                mesh.vertices.push(egui::epaint::Vertex { pos, uv, color: Color32::WHITE });
            }
            mesh.indices.extend([0, 1, 2, 0, 2, 3]);
            clip.add(egui::Shape::mesh(mesh));
        }
        None => {
            clip.rect_filled(shown, 0.0, if photo.is_some() { Color32::from_gray(52) } else { Color32::from_gray(200) });
        }
    }
    if im.stroke_enabled && im.stroke_width_pt > 0.0 {
        let w = (im.stroke_width_pt as f32 / 72.0 * sheet.scale).max(1.0).min(visible.width().min(visible.height()) / 2.0);
        let c = im.stroke_color;
        painter.rect_stroke(visible, 0.0, Stroke::new(w, Color32::from_rgb(c[0], c[1], c[2])), StrokeKind::Inside);
    }
    if st.guides.show_dimensions {
        let text = format!("{} × {}", fmt_len(inches.w, st.guides.units), fmt_len(inches.h, st.guides.units));
        let galley = painter.layout_no_wrap(text, t.font(10.0), Color32::WHITE);
        let at = pos2(cell.center().x - galley.size().x / 2.0, cell.bottom() - galley.size().y - 3.0);
        painter.rect_filled(Rect::from_min_size(at, galley.size()).expand(2.0), 2.0, Color32::from_black_alpha(160));
        painter.galley(at, galley, Color32::WHITE);
    }
    Some(visible)
}

/// The Page Options as the print draws them ([`compose`]): crop marks at the corners of each
/// photo, the page number at the bottom right and the print settings at the bottom left.
/// Packages draw none of them, as the print doesn't ([`compose::has_page_options`]).
fn page_options(painter: &egui::Painter, st: &PrintSettings, sheet: &Sheet, photos: &[Rect], number: usize) {
    if !compose::has_page_options(st.layout.style) {
        return;
    }
    let p = &st.page;
    let ink = compose::mark_ink(p.background);
    let ink = Color32::from_rgb(ink[0], ink[1], ink[2]);
    if p.crop_marks {
        let (len, gap) = (compose::CROP_MARK_IN as f32 * sheet.scale, compose::CROP_MARK_GAP_IN as f32 * sheet.scale);
        let stroke = Stroke::new(1.0, ink);
        for r in photos {
            for (corner, dx, dy) in
                [(r.left_top(), -1.0, -1.0), (r.right_top(), 1.0, -1.0), (r.left_bottom(), -1.0, 1.0), (r.right_bottom(), 1.0, 1.0)]
            {
                painter.line_segment([corner + vec2(dx * gap, 0.0), corner + vec2(dx * (gap + len), 0.0)], stroke);
                painter.line_segment([corner + vec2(0.0, dy * gap), corner + vec2(0.0, dy * (gap + len))], stroke);
            }
        }
    }
    let Ok(area) = st.layout.printable() else { return };
    let area = sheet.rect(&area);
    let font = egui::FontId::proportional((compose::PAGE_TEXT_PT as f32 / 72.0 * sheet.scale).max(4.0));
    let y = (area.bottom() + sheet.page.bottom()) / 2.0;
    if p.page_numbers {
        painter.text(pos2(area.right(), y), Align2::RIGHT_CENTER, number.to_string(), font.clone(), ink);
    }
    if p.page_info {
        painter.text(pos2(area.left(), y), Align2::LEFT_CENTER, compose::page_info_text(st), font, ink);
    }
}

fn dashed(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let pts = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    painter.extend(egui::Shape::dashed_line(&pts, stroke, 5.0, 4.0));
}

/// Rulers along the top and left of the preview, in `unit`, zero at the page's corner.
fn rulers(painter: &egui::Painter, t: &Tokens, sheet: &Sheet, area: Rect, pw: f64, ph: f64, unit: Unit) {
    let (major, minor) = match unit {
        Unit::Inches => (1.0, 0.25),
        Unit::Centimeters => (1.0, 0.5),
        Unit::Millimeters => (10.0, 5.0),
        Unit::Points => (72.0, 18.0),
    };
    let tick = Stroke::new(1.0, t.text_dim);
    let ink = t.text_dim;
    let top = Rect::from_min_max(area.min, pos2(area.max.x, area.min.y + 20.0));
    let left = Rect::from_min_max(area.min, pos2(area.min.x + 20.0, area.max.y));
    painter.rect_filled(top, 0.0, t.chrome);
    painter.rect_filled(left, 0.0, t.chrome);
    let ticks = |len_in: f64| (0..).map(move |k| f64::from(k) * minor).take_while(move |v| *v <= unit.from_inches(len_in) + 1e-9).take(2000);
    for v in ticks(pw) {
        let x = sheet.page.left() + unit.to_inches(v) as f32 * sheet.scale;
        let is_major = (v / major - (v / major).round()).abs() < 1e-9;
        painter.line_segment([pos2(x, top.bottom()), pos2(x, top.bottom() - if is_major { 10.0 } else { 5.0 })], tick);
        if is_major {
            painter.text(pos2(x + 2.0, top.top() + 1.0), Align2::LEFT_TOP, format!("{}", v.round() as i64), t.font(9.0), ink);
        }
    }
    for v in ticks(ph) {
        let y = sheet.page.top() + unit.to_inches(v) as f32 * sheet.scale;
        let is_major = (v / major - (v / major).round()).abs() < 1e-9;
        painter.line_segment([pos2(left.right(), y), pos2(left.right() - if is_major { 10.0 } else { 5.0 }, y)], tick);
        if is_major {
            painter.text(pos2(left.left() + 1.0, y + 1.0), Align2::LEFT_TOP, format!("{}", v.round() as i64), t.font(9.0), ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lengths_read_in_the_ruler_unit() {
        assert_eq!(fmt_len(1.0, Unit::Inches), "1.00 in");
        assert_eq!(fmt_len(1.0, Unit::Millimeters), "25.4 mm");
        assert_eq!(fmt_len(1.0, Unit::Points), "72 pt");
        assert_eq!(trim(4.0), "4");
        assert_eq!(trim(2.5), "2.5");
        assert_eq!(paper_label("a4"), "A4");
        assert_eq!(paper_label("4x6"), "4 × 6 in");
    }
}
