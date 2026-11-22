//! The Modern layout's full screens (fork-owned): Settings and Export.
//!
//! **Settings** shows the desktop Settings' own categories and rows ([`crate::panels::settings`]),
//! so both layouts read and write the same settings; only the presentation differs. On a phone it
//! is a full-screen page: the list of categories, and a tap opens one as its own page (Back
//! returns). On a tablet (or a folding phone's inner screen) it is a large sheet with the list
//! beside the open category. Rows are finger-sized, a label above its controls on a narrow screen,
//! and the page scrolls by dragging.
//!
//! **Export** is a sheet with a few presets (full-size JPEG, a 2048-pixel JPEG for sharing, 16-bit
//! TIFF, DNG for raw photos), size and quality as segmented choices, and the way the files leave:
//! on Android saved to Pictures/<app> (the gallery) or shared through the system share sheet,
//! elsewhere written to the export folder. It exports through `app.export` (the engine's one
//! encoder, `lcc_engine::export`) in the background; the activity stack shows the export, then the
//! save to Pictures; failures are toasts.
//!
//! Own design (patterns of touch settings and share sheets; no assets taken from any).

use crate::json_ext::JsonExt as _;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Align2, Id, Rect, Sense, Stroke, Vec2, pos2, vec2};
use lcc_engine::activity::{Cancel, TaskGuard};
use serde_json::{Value, json};

use crate::LccApp;
use crate::icons::Icon;
use crate::modern::round_button;
use crate::module::ModuleId;
use crate::state::Dialog;
use crate::theme::Tokens;
use crate::widgets::register;

/// Short side, in points, from which Settings is a sheet rather than a full screen.
const TABLET: f32 = 600.0;

/// What the touch screens remember between frames (`LccApp::modern_sheets`).
#[derive(Default)]
pub struct ModernSheets {
    /// Settings' rail and search ([`crate::settings_screen`]).
    pub settings: crate::settings_screen::SettingsScreen,
    /// The Export sheet, while it is open.
    pub export: Option<ExportSheet>,
    /// The sheet's choices last time (the next opening starts from them).
    last: Option<ExportChoice>,
    /// A background export the sheet started, and where its files go when it ends.
    handoff: Option<Then>,
    /// Files being saved to Pictures (and shared) by the host.
    gallery: Option<Gallery>,
}

struct Gallery {
    rx: Receiver<Result<Vec<String>, String>>,
    /// The save's row in the activity stack (it can't be stopped once the host has the files).
    _guard: TaskGuard,
    share: bool,
}

/// Where the exported files go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Then {
    /// Pictures/<app> (Android's gallery).
    Save,
    /// Pictures/<app>, then the share sheet.
    Share,
    /// The export folder only (no gallery on this platform).
    Folder,
}

impl Then {
    fn parse(s: &str) -> Option<Then> {
        match s.to_ascii_lowercase().as_str() {
            "save" => Some(Then::Save),
            "share" => Some(Then::Share),
            "folder" => Some(Then::Folder),
            _ => None,
        }
    }
}

/// The file format the sheet writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Tiff,
    Dng,
}

impl Format {
    fn parse(s: &str) -> Option<Format> {
        match s.to_ascii_lowercase().as_str() {
            "jpeg" | "jpg" => Some(Format::Jpeg),
            "tiff" | "tif" => Some(Format::Tiff),
            "dng" => Some(Format::Dng),
            _ => None,
        }
    }
}

/// The sheet's choices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExportChoice {
    pub format: Format,
    /// Long edge in pixels; 0 = full size.
    pub long_edge: u32,
    /// JPEG quality (1–100).
    pub quality: u8,
}

impl Default for ExportChoice {
    fn default() -> Self {
        ExportChoice { format: Format::Jpeg, long_edge: 0, quality: 90 }
    }
}

/// The open Export sheet: what it exports and how.
#[derive(Clone, Debug)]
pub struct ExportSheet {
    pub ids: Vec<u64>,
    pub choice: ExportChoice,
    /// Some of the photos are raw (DNG is offered).
    raw: bool,
}

/// The presets: (id, label, choice).
const PRESETS: [(&str, &str, ExportChoice); 4] = [
    ("full", "JPEG · Full Size", ExportChoice { format: Format::Jpeg, long_edge: 0, quality: 90 }),
    ("share", "JPEG · 2048 px, for Sharing", ExportChoice { format: Format::Jpeg, long_edge: 2048, quality: 85 }),
    ("tiff", "TIFF · 16-bit, Full Size", ExportChoice { format: Format::Tiff, long_edge: 0, quality: 90 }),
    ("dng", "DNG · Raw Data with Edits", ExportChoice { format: Format::Dng, long_edge: 0, quality: 90 }),
];
const SIZES: [(u32, &str); 4] = [(0, "Full"), (4096, "4096"), (2048, "2048"), (1080, "1080")];
const QUALITIES: [(u8, &str); 4] = [(60, "60"), (80, "80"), (90, "90"), (100, "100")];

/// Per frame, before the dialogs: the hand-off of finished exports, the Export sheet (Modern
/// layout), and Settings ([`crate::settings_screen`], every layout). True when Settings was drawn
/// here (the dialogs then skip it).
pub(crate) fn show(app: &mut LccApp, ctx: &egui::Context) -> bool {
    poll(app, ctx);
    if !crate::modern::on(app) {
        app.modern_sheets.export = None;
    } else if app.ui.dialog.is_none() && app.modern_sheets.export.is_some() {
        export_sheet(app, ctx);
    }
    if matches!(app.ui.dialog, Some(Dialog::Import { .. })) {
        crate::import_screen::show(app, ctx);
        return true;
    }
    if matches!(app.ui.dialog, Some(Dialog::Settings { .. })) {
        crate::settings_screen::show(app, ctx);
        return true;
    }
    // Export: a full-window screen in every layout
    if matches!(app.ui.dialog, Some(Dialog::Export { .. })) {
        crate::panels::dialogs::export_screen(app, ctx);
        return true;
    }
    app.modern_sheets.settings = Default::default();
    false
}

/// Back (`modern.back`) on these screens, before the dialogs: the Export sheet closes; on a
/// phone, Settings goes from a category's page to the category list (the next Back closes it, as
/// the dialog it is). `None`: neither was open, or Settings is on its list (Back closes it).
pub(crate) fn back(app: &mut LccApp) -> Option<&'static str> {
    // (a dialog over the sheet closes first, as Esc would)
    if app.ui.dialog.is_none() && app.modern_sheets.export.take().is_some() {
        return Some("export");
    }
    let ctx = app.tasks.repaint.clone()?;
    crate::settings_screen::back(app, &ctx)
}

fn dim(ctx: &egui::Context, screen: Rect) {
    egui::Area::new(Id::new("modern-sheet-dim")).order(egui::Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
        // the backdrop takes the taps under the sheet
        let (r, _) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(r, 0.0, egui::Color32::from_black_alpha(140));
    });
}

// ------------------------------------------------------------------------ Export

/// The photos an Export would take: Library's selection (in grid order), else the active photo;
/// in Develop the photo being edited.
fn targets(app: &mut LccApp) -> Vec<u64> {
    if app.ui.module == ModuleId::Library && !app.session.selection.ids.is_empty() {
        let sel: std::collections::HashSet<_> = app.session.selection.ids.iter().copied().collect();
        let mut v: Vec<_> = app.session.visible().iter().copied().filter(|id| sel.contains(id)).collect();
        let shown: std::collections::HashSet<_> = v.iter().copied().collect();
        v.extend(app.session.selection.ids.iter().filter(|id| !shown.contains(id)));
        return v.into_iter().map(|id| id.0).collect();
    }
    app.session.active().into_iter().map(|id| id.0).collect()
}

fn open_sheet(app: &mut LccApp) -> Result<Value, String> {
    let ids = targets(app);
    if ids.is_empty() {
        return Err("No photo selected".into());
    }
    let raw = ids.iter().any(|id| app.session.catalog.photo(lcc_catalog::PhotoId(*id)).is_some_and(|p| p.develops_raw()));
    let mut choice = app.modern_sheets.last.unwrap_or_default();
    if choice.format == Format::Dng && !raw {
        choice = ExportChoice::default();
    }
    let n = ids.len();
    app.modern_sheets.export = Some(ExportSheet { ids, choice, raw });
    Ok(json!({"photos": n}))
}

/// Where the sheet's files are written: the app's own storage on Android (they are then copied
/// to Pictures), else the usual export folder.
pub fn export_dir() -> String {
    if cfg!(target_os = "android")
        && let Some(d) = lcc_brand::platform_dirs()
    {
        return d.pictures.join("Exports").to_string_lossy().into_owned();
    }
    crate::control::default_export_dir()
}

fn can_hand_off(app: &LccApp) -> bool {
    app.services.save_to_gallery.is_some()
}

/// Exports `ids` with `choice`, then does `then` with the files. Params of `touch.export`.
fn start_export(app: &mut LccApp, ids: Vec<u64>, choice: ExportChoice, then: Then, dir: Option<String>) -> Result<Value, String> {
    if ids.is_empty() {
        return Err("no photo selected".into());
    }
    if then != Then::Folder && !can_hand_off(app) {
        return Err("saving to Pictures and sharing need the Android app here; use `then: folder`".into());
    }
    if app.export.is_some() {
        return Err("an export is already running".into());
    }
    let dir = dir.unwrap_or_else(export_dir);
    if dir.trim().is_empty() {
        return Err(crate::control::NO_EXPORT_FOLDER.into());
    }
    let mut p = json!({
        "ids": ids,
        "dir": dir,
        "destinationMode": "specificFolder",
        "conflict": "unique",
        "naming": "{name}",
        "background": true,
    });
    match choice.format {
        Format::Jpeg => {
            *p.field_mut("format") = json!("jpeg");
            *p.field_mut("quality") = json!(choice.quality.clamp(1, 100));
        }
        Format::Tiff => {
            *p.field_mut("format") = json!("tiff");
            *p.field_mut("bitDepth") = json!(16);
        }
        Format::Dng => *p.field_mut("format") = json!("dng"),
    }
    if choice.long_edge > 0 && choice.format != Format::Dng {
        *p.field_mut("longEdge") = json!(choice.long_edge);
        *p.field_mut("dontEnlarge") = json!(true);
    }
    app.modern_sheets.last = Some(choice);
    app.last_export_result = None;
    let r = app.run("app.export", p)?;
    if app.export.is_some() {
        app.modern_sheets.handoff = Some(then);
    } else {
        // no background writer (a window-less run): written already
        let files = paths(&r);
        hand_off(app, &files, then)?;
    }
    app.modern_sheets.export = None;
    Ok(r)
}

/// The files an export result lists as written.
fn paths(result: &Value) -> Vec<String> {
    result
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("path").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Gives the files to the host (Pictures, the share sheet); nothing to do for `Folder`.
fn hand_off(app: &mut LccApp, files: &[String], then: Then) -> Result<(), String> {
    if then == Then::Folder || files.is_empty() {
        return Ok(());
    }
    let share = then == Then::Share;
    let save = app.services.save_to_gallery.as_mut().ok_or("no gallery on this platform")?;
    let rx = save(files, share)?;
    let label = if share { "Sharing" } else { "Saving to Pictures" };
    let guard = app.session.activity.start("export", crate::i18n::tr(label), Cancel::No);
    app.modern_sheets.gallery = Some(Gallery { rx, _guard: guard, share });
    Ok(())
}

/// A finished background export goes on to Pictures / the share sheet; the host's answer is told.
fn poll(app: &mut LccApp, ctx: &egui::Context) {
    if app.modern_sheets.handoff.is_some() && app.export.is_none() {
        let then = app.modern_sheets.handoff.take().unwrap_or(Then::Folder);
        // (an export that failed outright left no result; its toast said why)
        let files = match &app.last_export_result {
            Some(r) if r.get("cancelled").and_then(Value::as_bool) != Some(true) => paths(r),
            _ => Vec::new(),
        };
        if let Err(e) = hand_off(app, &files, then) {
            app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Not saved to Pictures")));
        }
    }
    let Some(g) = &app.modern_sheets.gallery else { return };
    match g.rx.try_recv() {
        Ok(Ok(uris)) => {
            let share = g.share;
            app.modern_sheets.gallery = None;
            let n = uris.len();
            let msg = if share {
                format!("{} · {n}", crate::i18n::tr("Saved to Pictures and shared"))
            } else {
                format!("{} · {n}", crate::i18n::tr("Saved to Pictures"))
            };
            app.toast(ctx, msg);
        }
        Ok(Err(e)) => {
            app.modern_sheets.gallery = None;
            app.toast_error(ctx, format!("{}: {e}", crate::i18n::tr("Not saved to Pictures")));
        }
        Err(TryRecvError::Empty) => ctx.request_repaint_after(std::time::Duration::from_millis(250)),
        Err(TryRecvError::Disconnected) => {
            app.modern_sheets.gallery = None;
            app.toast_error(ctx, crate::i18n::tr("Saving to Pictures stopped unexpectedly"));
        }
    }
}

fn export_sheet(app: &mut LccApp, ctx: &egui::Context) {
    let Some(sheet) = app.modern_sheets.export.clone() else { return };
    let screen = ctx.content_rect();
    let phone = screen.width().min(screen.height()) < TABLET;
    let t = Tokens::get(ctx);
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.modern_sheets.export = None;
        return;
    }
    dim(ctx, screen);
    let w = if phone { screen.width() } else { 560.0f32.min(screen.width() - 32.0) };
    let h_id = Id::new("modern-export-height");
    let h = ctx.data(|d| d.get_temp::<f32>(h_id)).unwrap_or(520.0).min(screen.height() - 24.0);
    let min = if phone { pos2(screen.left(), screen.bottom() - h) } else { pos2(screen.center().x - w / 2.0, screen.center().y - h / 2.0) };
    let mut choice = sheet.choice;
    let mut go: Option<Then> = None;
    let mut close = false;
    egui::Area::new(Id::new("modern-export")).order(egui::Order::Foreground).fixed_pos(min).show(ctx, |ui| {
        let bg = Rect::from_min_size(min, vec2(w, h));
        let radius = if phone { egui::CornerRadius { nw: 16, ne: 16, sw: 0, se: 0 } } else { egui::CornerRadius::same(16) };
        ui.painter().rect_filled(bg, radius, t.chrome);
        ui.set_width(w);
        let was = crate::dialog_frame::set_finger_sized(ctx, true);
        let content = egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 10)).show(ui, |ui| {
            ui.set_width(w - 32.0);
            ui.spacing_mut().item_spacing = vec2(8.0, 10.0);
            // header: title, count, close
            let (head, _) = ui.allocate_exact_size(vec2(w - 32.0, 44.0), Sense::hover());
            ui.painter().text(pos2(head.left(), head.center().y - 8.0), Align2::LEFT_CENTER, crate::i18n::tr("Export"), t.semibold(19.0), t.text);
            let count = format!("{} · {}", crate::i18n::tr("Photos"), sheet.ids.len());
            ui.painter().text(pos2(head.left(), head.center().y + 12.0), Align2::LEFT_CENTER, count, t.font(13.0), t.text_dim);
            let shut = Rect::from_min_size(pos2(head.right() - 44.0, head.top()), Vec2::splat(44.0));
            if round_button(ui, shut, "modern:export:close", Icon::Close, "Close", false, true).clicked() {
                close = true;
            }
            // presets
            for (id, label, preset) in PRESETS {
                if preset.format == Format::Dng && !sheet.raw {
                    continue;
                }
                let on = choice == preset;
                let (r, resp) = ui.allocate_exact_size(vec2(w - 32.0, 48.0), Sense::click());
                let name = crate::i18n::tr(label);
                resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, on, name));
                register(ctx, format!("modern:export:preset:{id}"), r);
                let p = ui.painter();
                p.rect(
                    r,
                    10.0,
                    if on { t.tool_active } else { t.button },
                    Stroke::new(1.0, if on { t.accent } else { t.button_border }),
                    egui::StrokeKind::Inside,
                );
                let dot = pos2(r.left() + 22.0, r.center().y);
                p.circle_stroke(dot, 8.0, Stroke::new(1.5, if on { t.accent } else { t.text_dim }));
                if on {
                    p.circle_filled(dot, 4.5, t.accent);
                }
                p.text(pos2(r.left() + 42.0, r.center().y), Align2::LEFT_CENTER, name, t.font(15.0), t.text);
                if resp.clicked() {
                    choice = preset;
                }
            }
            if choice.format != Format::Dng {
                ui.label(egui::RichText::new(crate::i18n::tr("Long Edge (pixels)")).color(t.text_label));
                let items: Vec<(&str, &str)> = SIZES.iter().map(|(_, l)| (*l, *l)).collect();
                let active = SIZES.iter().position(|(v, _)| *v == choice.long_edge);
                if let Some(i) = crate::widgets::segmented(ui, "touchExportSize", &items, active, SIZES.len())
                    && let Some((v, _)) = SIZES.get(i)
                {
                    choice.long_edge = *v;
                }
            }
            if choice.format == Format::Jpeg {
                ui.label(egui::RichText::new(crate::i18n::tr("Quality")).color(t.text_label));
                let items: Vec<(&str, &str)> = QUALITIES.iter().map(|(_, l)| (*l, *l)).collect();
                let active = QUALITIES.iter().position(|(v, _)| *v == choice.quality);
                if let Some(i) = crate::widgets::segmented(ui, "touchExportQuality", &items, active, QUALITIES.len())
                    && let Some((v, _)) = QUALITIES.get(i)
                {
                    choice.quality = *v;
                }
            }
            ui.add_space(4.0);
            let busy = app.export.is_some();
            let gap = 10.0;
            if can_hand_off(app) {
                let bw = ((w - 32.0 - gap) / 2.0).max(60.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    if action_button(ui, "modern:export:share", Some(Icon::Share), "Share", false, !busy, bw).clicked() {
                        go = Some(Then::Share);
                    }
                    if action_button(ui, "modern:export:save", None, "Save to Pictures", true, !busy, bw).clicked() {
                        go = Some(Then::Save);
                    }
                });
                ui.label(egui::RichText::new(format!("{} ▸ {}", crate::i18n::tr("Pictures"), lcc_brand::DISPLAY_NAME)).size(12.0).color(t.text_dim));
            } else {
                if action_button(ui, "modern:export:go", None, "Export", true, !busy, w - 32.0).clicked() {
                    go = Some(Then::Folder);
                }
                ui.label(egui::RichText::new(export_dir()).size(12.0).color(t.text_dim));
            }
            if busy {
                ui.label(egui::RichText::new(crate::i18n::tr("An export is running; see the activity list.")).size(12.0).color(t.text_dim));
            }
            ui.add_space(8.0);
        });
        crate::dialog_frame::set_finger_sized(ctx, was);
        let measured = content.response.rect.height().ceil();
        if (measured - h).abs() > 0.5 {
            ctx.data_mut(|d| d.insert_temp(h_id, measured));
            ctx.request_repaint();
        }
    });
    if close {
        app.modern_sheets.export = None;
        return;
    }
    if let Some(s) = app.modern_sheets.export.as_mut() {
        s.choice = choice;
    }
    if let Some(then) = go {
        match start_export(app, sheet.ids.clone(), choice, then, None) {
            Ok(_) => app.modern_sheets.export = None,
            Err(e) => app.toast_error(ctx, e),
        }
    }
}

/// A wide button at the bottom of a sheet; `primary` is filled with the accent.
fn action_button(ui: &mut egui::Ui, id: &str, icon: Option<Icon>, label: &str, primary: bool, enabled: bool, width: f32) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let name = crate::i18n::tr(label);
    let (r, resp) = ui.allocate_exact_size(vec2(width, 50.0), if enabled { Sense::click() } else { Sense::hover() });
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, name));
    register(ui.ctx(), id, r);
    let p = ui.painter();
    let fill = if !enabled {
        t.button
    } else if primary {
        t.accent
    } else if resp.is_pointer_button_down_on() {
        t.pressed
    } else {
        t.button
    };
    p.rect(r, 12.0, fill, Stroke::new(1.0, if primary { fill } else { t.button_border }), egui::StrokeKind::Inside);
    let c = if !enabled {
        t.text_disabled
    } else if primary {
        egui::Color32::WHITE
    } else {
        t.text
    };
    let galley = p.layout_no_wrap(name.to_string(), t.semibold(15.0), c);
    let icon_w = if icon.is_some() { 28.0 } else { 0.0 };
    let start = r.center().x - (galley.size().x + icon_w) / 2.0;
    if let Some(icon) = icon {
        crate::icons::paint(p, Rect::from_center_size(pos2(start + 10.0, r.center().y), Vec2::splat(20.0)), icon, c);
    }
    p.galley(pos2(start + icon_w, r.center().y - galley.size().y / 2.0), galley, c);
    resp
}

// ------------------------------------------------------------------------ commands

/// The touch screens' commands (`menus_fork` lists them).
pub const COMMANDS: &[crate::menus::UiCommand] =
    &[("modern.exportSheet", "Export (Touch Sheet)…", None, ""), ("modern.export", "Export from the Touch Sheet", None, "")];

/// `touch.exportSheet {on?}` opens (or closes) the Export sheet; `touch.export {format?, longEdge?,
/// quality?, then?, dir?, ids?}` exports as the sheet's buttons do (`then`: save | share | folder).
pub(crate) fn run(app: &mut LccApp, id: &str, p: &Value) -> Option<Result<Value, String>> {
    Some(match id {
        "modern.exportSheet" => {
            if p.get("on").and_then(Value::as_bool) == Some(false) {
                app.modern_sheets.export = None;
                Ok(json!({"on": false}))
            } else {
                open_sheet(app)
            }
        }
        "modern.export" => export_command(app, p),
        _ => return None,
    })
}

fn export_command(app: &mut LccApp, p: &Value) -> Result<Value, String> {
    let base = app.modern_sheets.export.as_ref().map_or_else(|| app.modern_sheets.last.unwrap_or_default(), |s| s.choice);
    let format = match p.get("format") {
        None | Some(Value::Null) => base.format,
        Some(v) => v.as_str().and_then(Format::parse).ok_or("modern.export: `format` is jpeg, tiff or dng")?,
    };
    let long_edge = match p.get("longEdge") {
        None | Some(Value::Null) => base.long_edge,
        Some(v) => {
            v.as_u64().filter(|n| *n <= 65_535).ok_or("modern.export: `longEdge` is a whole number of pixels up to 65535 (0 = full size)")? as u32
        }
    };
    let quality = match p.get("quality") {
        None | Some(Value::Null) => base.quality,
        Some(v) => v.as_u64().filter(|q| (1..=100).contains(q)).ok_or("modern.export: `quality` is 1 to 100")? as u8,
    };
    let default_then = if can_hand_off(app) { Then::Save } else { Then::Folder };
    let then = match p.get("then") {
        None | Some(Value::Null) => default_then,
        Some(v) => v.as_str().and_then(Then::parse).ok_or("modern.export: `then` is save, share or folder")?,
    };
    let dir = match p.get("dir") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_str().ok_or("modern.export: `dir` is a folder path")?.to_string()),
    };
    let ids: Vec<u64> = match p.get("ids") {
        None | Some(Value::Null) => match app.modern_sheets.export.as_ref().map(|s| s.ids.clone()) {
            Some(ids) => ids,
            None => targets(app),
        },
        Some(v) => v
            .as_array()
            .ok_or("modern.export: `ids` is a list of photo ids")?
            .iter()
            .map(|x| x.as_u64().ok_or("modern.export: `ids` is a list of photo ids"))
            .collect::<Result<_, _>>()?,
    };
    start_export(app, ids, ExportChoice { format, long_edge, quality }, then, dir)
}
