//! What dropping the files dragged over the window will do: the same sorting the drop applies,
//! shown on a veil over the whole window while the drag lasts.

use egui::{Align2, Color32, Context, Order, Rect, Stroke, StrokeKind, vec2};

use crate::LightcraftApp;
use crate::theme::Tokens;

/// Dropped paths by what becomes of them.
#[derive(Default, Debug, PartialEq)]
pub struct Sorted {
    /// Face models (`.onnx`): the first opens the licence dialog.
    pub models: Vec<String>,
    /// Preset files: imported as presets.
    pub presets: Vec<String>,
    /// Everything else: photos and folders, for the import.
    pub photos: Vec<String>,
}

/// Sort `paths` as a drop does.
pub fn classify(paths: Vec<String>) -> Sorted {
    let (models, rest): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| crate::is_model_file(p));
    let (presets, photos) = crate::split_presets(rest);
    Sorted { models, presets, photos }
}

/// The lines the card shows for files hovering over the window (`unknown`: how many have no path
/// to look at, as on some platforms and in browsers), for the current settings.
fn lines(app: &LightcraftApp, paths: Vec<String>, unknown: bool) -> Vec<String> {
    if app.ui.dialog.is_some() {
        return vec![crate::i18n::tr("Close the open dialog to drop files here").to_string()];
    }
    let drop = classify(paths);
    let mut out = Vec::new();
    if !drop.models.is_empty() {
        out.push(crate::i18n::tr("Drop to open the face model").to_string());
    }
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    // no library open yet (desktop: the web build's drops import into its session as before):
    // photos wait for the library chosen, presets are refused (the drop handler does the same)
    let waiting = crate::panels::library_problem::holds_drops(app) && !cfg!(target_arch = "wasm32");
    if !drop.presets.is_empty() && waiting {
        out.push(crate::i18n::tr("Open a library first, then drop the presets again").to_string());
    } else if !drop.presets.is_empty() {
        let n = drop.presets.len();
        out.push(crate::i18n::tr_format!("Drop to import {n} preset{}", plural(n), n = n));
    }
    if !drop.photos.is_empty() && waiting {
        out.push(crate::i18n::tr("The photos are imported once a library is open").to_string());
    } else if !drop.photos.is_empty() && app.lightroom.is_some() && !cfg!(target_arch = "wasm32") {
        // the drop is refused while a Lightroom catalog import owns the catalog
        out.push(crate::i18n::tr("Finish Lightroom catalog import before adding photos").to_string());
    } else if !drop.photos.is_empty() && crate::import::running(app) && !cfg!(target_arch = "wasm32") {
        out.push(crate::i18n::tr("An import is running. Drop photos when it has finished").to_string());
    } else if !drop.photos.is_empty() {
        let n = drop.photos.len();
        // the web build has no review to open: its drops import at once
        if app.ui.settings.import_dropped_directly || cfg!(target_arch = "wasm32") {
            out.push(crate::i18n::tr_format!("Drop to import {n} item{}", plural(n), n = n));
        } else {
            out.push(crate::i18n::tr_format!("Drop to review {n} item{} for import", plural(n), n = n));
        }
    }
    if out.is_empty() || unknown {
        out.push(crate::i18n::tr("Drop to import").to_string());
    }
    out
}

/// Veil and card while files are dragged over the window.
pub fn show(app: &LightcraftApp, ctx: &Context) {
    // This card is an `Order::Tooltip` layer, above the prompt's Foreground window. While a prompt
    // owns input the drag can only be background noise: do not paint over or register it.
    if crate::panels::notices::blocks_input(app) {
        return;
    }
    let count = ctx.input(|i| i.raw.hovered_files.len());
    if count == 0 {
        return;
    }
    let known: Vec<String> = ctx.input(|i| {
        i.raw.hovered_files.iter().filter_map(|f| f.path.as_ref()).map(|p| p.to_string_lossy().to_string()).filter(|p| !p.is_empty()).collect()
    });
    let lines = lines(app, known.clone(), known.len() < count);
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Tooltip, egui::Id::unique("drop-overlay")));
    painter.rect_filled(screen, 0.0, Color32::from_black_alpha(150));
    painter.rect_stroke(screen.shrink(10.0), 12.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
    let galleys: Vec<_> = lines.into_iter().map(|l| painter.layout(l, t.semibold(18.0), t.text, (screen.width() - 120.0).max(200.0))).collect();
    let (w, h) = galleys.iter().fold((0.0f32, 0.0f32), |(w, h), g| (w.max(g.size().x), h + g.size().y + 8.0));
    let card = Rect::from_center_size(screen.center(), vec2(w + 64.0, h + 40.0));
    painter.rect_filled(card, 10.0, t.chrome);
    painter.rect_stroke(card, 10.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
    let mut y = card.top() + 24.0;
    for g in galleys {
        let at = egui::pos2(card.center().x, y);
        let height = g.size().y;
        painter.galley(Align2::CENTER_TOP.anchor_size(at, g.size()).min, g, t.text);
        y += height + 8.0;
    }
    crate::widgets::register_geometry(ctx, "dropOverlay", card);
    ctx.request_repaint();
}
