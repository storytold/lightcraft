//! Grid cell extras: the expanded Square Grid cell header, index numbers and thumbnail badges
//! (keywords, collections, cropped, rotated, metadata out of step with the XMP sidecar, remote /
//! Immich link), placed on the photo itself; and the View Options dialog.
//! Styles are set with `view.gridCellStyle` ([`crate::libtools`]).

use std::collections::HashMap;

use dac_catalog::{Photo, PhotoId, SidecarStat, XmpStatus};
use egui::{Align2, Color32, Rect, Sense, pos2, vec2};
use serde_json::json;

use crate::DacApp;
use crate::icons::{Icon, paint};
use crate::libtools::CellStyle;
use crate::theme::Tokens;
use crate::widgets::register;

/// Height of the expanded cell's header (two lines).
const EXPANDED_HEADER: f32 = 40.0;

/// The image area of a Square Grid cell.
pub fn image_rect(app: &DacApp, r: Rect) -> Rect {
    let top = if app.ui.lib.cell_style == CellStyle::Expanded { EXPANDED_HEADER } else { 24.0 };
    let min = r.min + vec2(10.0, top);
    let max = r.max - vec2(10.0, 10.0);
    // tiny cells: never an inverted rectangle
    Rect::from_min_max(min, pos2(max.x.max(min.x), max.y.max(min.y)))
}

/// Expanded header: index · file name · format, then dimensions and exposure.
pub fn expanded_header(app: &DacApp, ui: &egui::Ui, p: &Photo, r: Rect, index: usize) {
    let t = Tokens::get(ui.ctx());
    let painter = ui.painter().with_clip_rect(r);
    let mut x = r.left() + 8.0;
    if app.ui.lib.cell_index {
        let g = painter.text(pos2(x, r.top() + 12.0), Align2::LEFT_CENTER, (index + 1).to_string(), t.semibold(11.0), t.text_label);
        register(ui.ctx(), format!("cellIndex:{}", p.id.0), g);
        x = g.right() + 8.0;
    }
    painter.text(pos2(x, r.top() + 12.0), Align2::LEFT_CENTER, &p.file_name, t.font(10.5), t.text);
    let m = &p.meta;
    let mut line = format!("{} × {}", p.width, p.height);
    for part in [
        (!m.shutter.is_empty()).then(|| m.shutter.clone()),
        m.aperture.map(|a| format!("f/{a:.1}").replace(".0", "")),
        m.iso.map(|i| format!("ISO {i}")),
    ]
    .into_iter()
    .flatten()
    {
        line.push_str("  ");
        line.push_str(&part);
    }
    painter.text(pos2(r.left() + 8.0, r.top() + 28.0), Align2::LEFT_CENTER, line, t.font(10.0), t.text_dim);
}

/// Index number (compact cells: large and faint in the corner) and the thumbnail badges.
pub fn extras(app: &mut DacApp, ui: &egui::Ui, id: PhotoId, r: Rect, square: bool, index: usize, albums: Option<&HashMap<PhotoId, usize>>) {
    let t = Tokens::get(ui.ctx());
    let Some(p) = app.session.catalog.photo(id).cloned() else { return };
    let expanded = square && app.ui.lib.cell_style == CellStyle::Expanded;
    if app.ui.lib.cell_index && !expanded {
        let at = if square { pos2(r.right() - 8.0, r.bottom() - 2.0) } else { pos2(r.left() + 6.0, r.top() + 4.0) };
        let (align, color, size) = if square {
            (Align2::RIGHT_BOTTOM, Color32::from_white_alpha(28), 26.0)
        } else {
            (Align2::LEFT_TOP, Color32::from_white_alpha(200), 11.0)
        };
        let g = ui.painter().text(at, align, (index + 1).to_string(), t.semibold(size), color);
        register(ui.ctx(), format!("cellIndex:{}", id.0), g);
    }
    let Some(albums) = albums else { return };
    // on the photo itself, not the square around it (a portrait photo leaves bands at the sides)
    let img = if square { super::detail::fit_texture_rect(image_rect(app, r), shown_size(&p)) } else { r };
    let mut badges: Vec<(&str, Icon, String, Color32)> = Vec::new();
    if cropped(&p.develop) {
        badges.push(("cropped", Icon::Crop, crate::i18n::tr("Cropped").to_string(), Color32::WHITE));
    }
    if p.develop.orientation != dac_geom::Orientation::Normal {
        badges.push(("rotated", Icon::Rotate, crate::i18n::tr("Rotated or flipped").to_string(), Color32::WHITE));
    }
    if !p.meta.keywords.is_empty() {
        let names: Vec<&str> = p.meta.keywords.iter().map(|k| k.rsplit('|').next().unwrap_or(k)).collect();
        badges.push(("keywords", Icon::Tag, format!("{}: {}", crate::i18n::tr("Keywords"), names.join(", ")), Color32::WHITE));
    }
    if let Some(n) = albums.get(&id).copied().filter(|n| *n > 0) {
        badges.push(("collections", Icon::Album, format!("{} {n}", crate::i18n::tr("Collections:")), Color32::WHITE));
    }
    if let Some(why) = xmp_badge(app, ui, &p) {
        badges.push(("metadataConflict", Icon::Info, crate::i18n::tr(why).to_string(), t.caution));
    }
    let remote: Vec<String> = app.session.catalog.remote_of(id).map(|l| l.service.clone()).collect();
    if !remote.is_empty() {
        let immich = remote.iter().any(|s| s.eq_ignore_ascii_case("immich"));
        let tip =
            if immich { crate::i18n::tr("Linked to Immich").to_string() } else { format!("{} {}", crate::i18n::tr("Linked to"), remote.join(", ")) };
        badges.push(("remote", Icon::Cloud, tip, Color32::WHITE));
    }
    if badges.is_empty() {
        return;
    }
    // a row of small badges at the image's top right (below the virtual-copy tag)
    let size = 18.0;
    let top = img.top() + if p.copy_name.is_some() { 30.0 } else { 6.0 };
    let mut x = img.right() - 6.0;
    for (key, icon, tip, color) in badges {
        let br = Rect::from_min_size(pos2(x - size, top), vec2(size, size));
        if !img.contains_rect(br) {
            break;
        }
        ui.painter().rect_filled(br, 4.0, Color32::from_black_alpha(150));
        paint(ui.painter(), br.shrink(3.0), icon, color);
        register(ui.ctx(), format!("badge:{key}:{}", id.0), br);
        let resp = ui.interact(br, egui::Id::new(("cell-badge", key, id.0)), Sense::click()).on_hover_text(tip);
        if resp.clicked() {
            let _ = app.run("library.select", json!({"ids": [id.0]}));
            let panel = match key {
                "keywords" => Some("panel.keywords"),
                "metadataConflict" => Some("panel.info"),
                "cropped" => Some("panel.crop"),
                _ => None,
            };
            if let Some(cmd) = panel {
                let _ = app.run(cmd, json!({}));
            }
        }
        x -= size + 4.0;
    }
}

/// Whether the edit crops or straightens the photo.
pub fn cropped(d: &dac_develop::DevelopSettings) -> bool {
    let g = &d.crop.geometry;
    let r = &g.rect;
    let off = |a: f64, b: f64| (a - b).abs() > 1e-6;
    off(r.x0, 0.0) || off(r.y0, 0.0) || off(r.x1, 1.0) || off(r.y1, 1.0) || off(g.angle, 0.0)
}

/// The photo's size as shown: oriented and cropped (for placing badges on it).
fn shown_size(p: &Photo) -> [usize; 2] {
    let d = &p.develop;
    let (mut w, mut h) = (f64::from(p.width.max(1)), f64::from(p.height.max(1)));
    if d.orientation.swaps_axes() {
        std::mem::swap(&mut w, &mut h);
    }
    let r = &d.crop.geometry.rect;
    let (cw, ch) = ((r.x1 - r.x0).abs(), (r.y1 - r.y0).abs());
    if cw.is_finite() && ch.is_finite() && cw > 1e-3 && ch > 1e-3 {
        w *= cw;
        h *= ch;
    }
    [w.clamp(1.0, 1e6) as usize, h.clamp(1.0, 1e6) as usize]
}

/// Library ▸ View Options (⌘J): the grid cell style, index numbers and badges, the same settings
/// as `view.gridCellStyle` and the View ▸ Grid View Style menu.
pub fn view_options(app: &mut DacApp, ui: &mut egui::Ui) {
    ui.set_min_width(320.0);
    let lib = &app.ui.lib;
    let (mut expanded, mut index, mut badges) = (lib.cell_style == CellStyle::Expanded, lib.cell_index, lib.cell_badges);
    let mut names = app.ui.show_filenames;
    ui.label(egui::RichText::new(crate::i18n::tr("Grid View Style")).strong());
    ui.horizontal(|ui| {
        let r = ui.radio_value(&mut expanded, false, crate::i18n::tr("Compact Cells"));
        register(ui.ctx(), "radio:viewOptions.compact", r.rect);
        let r = ui.radio_value(&mut expanded, true, crate::i18n::tr("Expanded Cells"));
        register(ui.ctx(), "radio:viewOptions.expanded", r.rect);
    });
    ui.add_space(6.0);
    let r = ui.checkbox(&mut index, crate::i18n::tr("Show Index Numbers"));
    register(ui.ctx(), "check:viewOptions.index", r.rect);
    let r = ui.checkbox(&mut badges, crate::i18n::tr("Show Thumbnail Badges"));
    register(ui.ctx(), "check:viewOptions.badges", r.rect);
    ui.label(
        egui::RichText::new(crate::i18n::tr("Keywords, collections, crop, rotation, metadata status, remote link"))
            .size(11.0)
            .color(Tokens::get(ui.ctx()).text_dim),
    );
    let r = ui.checkbox(&mut names, crate::i18n::tr("Show File Names"));
    register(ui.ctx(), "check:viewOptions.names", r.rect);
    let lib = &app.ui.lib;
    if expanded != (lib.cell_style == CellStyle::Expanded) || index != lib.cell_index || badges != lib.cell_badges {
        let style = if expanded { "expanded" } else { "compact" };
        let _ = app.run("view.gridCellStyle", json!({"style": style, "index": index, "badges": badges}));
    }
    app.ui.show_filenames = names;
}

/// How often a sidecar is looked at again (seconds).
const STAT_TTL: f64 = 5.0;

/// The metadata-vs-XMP badge text, if the photo is out of step with its sidecar. Only photos that
/// were read from / written to a sidecar have a stamp; the sidecar is stat'ed at most every
/// [`STAT_TTL`] seconds per photo.
fn xmp_badge(app: &mut DacApp, ui: &egui::Ui, p: &Photo) -> Option<&'static str> {
    p.xmp?;
    let dac_catalog::Source::File { path } = &p.source else { return None };
    let now = ui.input(|i| i.time);
    let stats = &mut app.ui.lib.sidecar_stats;
    let stat = match stats.get(&p.id) {
        Some((at, s)) if now - at < STAT_TTL => *s,
        _ => {
            let naming = app.session.sidecar_naming(p.id);
            let s = dac_engine::sidecar::find_sidecar(path, naming)
                .and_then(|f| std::fs::metadata(f).ok())
                .map(|m| SidecarStat { mtime: mtime(&m), size: m.len() });
            if stats.len() > 4096 {
                stats.clear();
            }
            stats.insert(p.id, (now, s));
            s
        }
    };
    match p.xmp_status(stat) {
        XmpStatus::ChangedInCatalog => Some("Metadata changed in the catalog: not yet saved to the file"),
        XmpStatus::ChangedOnDisk => Some("Metadata file changed on disk: read it from the file"),
        XmpStatus::Conflict => Some("Metadata conflict: changed both in the catalog and on disk"),
        XmpStatus::InSync | XmpStatus::Unknown => None,
    }
}

fn mtime(m: &std::fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtime_of_a_real_file_is_positive() {
        let m = std::fs::metadata(file!()).or_else(|_| std::fs::metadata(".")).unwrap();
        assert!(mtime(&m) > 0);
    }
}
