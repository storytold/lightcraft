//! Lightroom Classic saved prints → Saved Prints.
//!
//! A saved print is a collection with `creationId = "com.adobe.ag.print"`; its layout is a Lua table
//! in `AgLibraryCollectionContent.content` (`owningModule = "com.adobe.ag.print"`):
//!
//! * `_engineIdentifier` names the layout style: `com.adobe.lightroom.print.engine.grid` (Single
//!   Image / Contact Sheet), `….package.quick` (Picture Package) or `….package.custom` (Custom
//!   Package);
//! * `_engineSettings[<style>]` holds that style's settings; a package's `pages` list their `cells`,
//!   each with `bounds = AgMutableRect( left, top, right, bottom )` in points from the page's top
//!   left corner, its 1-based `page` and, in a Custom Package, the `imageID` (`Adobe_images.id_local`)
//!   of the photo placed in it;
//! * `_pageDimensions_x/_y` are the page size in points, already turned to `_orientation`, and
//!   `_margin_*` its margins in points;
//! * `_globalSettings` is the Print Job panel: destination, resolution, sharpening, media, colour
//!   management, Print Adjustment and 16-bit output.
//!
//! Colours are `AgColor( r, g, b, a )` with components from 0 to 1. Cells are kept in the order the
//! catalog lists them, which this module takes as back to front.
//!
//! What has no equivalent (identity plate, watermark, a package's photo border, a printer profile
//! named by the printer driver, a contact sheet's fixed cell size) is reported as a note; the
//! original stays in the import's recovery archive.
//!
//! Sources: own design, read from the Lua literals of saved prints in a user's own catalog (rows
//! dumped with `sqlite3` and shared by that user); no Adobe code or data.

use lcc_catalog::PhotoId;
use lcc_layout::grid::Grid;
use lcc_layout::{Cell, CellKind, Color, Fit, Insets, MAX_CELLS, MAX_PAGE_PT, MAX_PAGES, Page, PageLayout, Rect, Size, Stroke};
use lcc_print::{Destination, LayoutStyle, PrintIntent, PrintMedia, PrintSettings, PrintSharpening};

use crate::preset_import::{Lua, parse_lua};

const GRID: &str = "com.adobe.lightroom.print.engine.grid";
const PICTURE_PACKAGE: &str = "com.adobe.lightroom.print.engine.package.quick";
const CUSTOM_PACKAGE: &str = "com.adobe.lightroom.print.engine.package.custom";

/// A saved print as a Saved Print: the layout document (JSON, with its print settings) and notes
/// on what didn't carry over.
#[derive(Debug)]
pub(crate) struct SavedPrint {
    pub document: String,
    pub notes: Vec<String>,
}

/// The Saved Print a saved print's `content` describes, or why it can't be read. `photo` maps a
/// catalog `imageID` to the photo the import made of it; `members` are the collection's photos in
/// collection order, which fill the cells of a grid or picture package as Lightroom does.
pub(crate) fn saved_print(content: &str, photo: &dyn Fn(i64) -> Option<PhotoId>, members: &[PhotoId]) -> Result<SavedPrint, String> {
    let root = parse_lua(content)?;
    if !matches!(root, Lua::Table(..)) {
        return Err("the print layout is not a table".into());
    }
    let mut notes = Vec::new();
    let mut s = PrintSettings::default();
    let size = Size::new(num(&root, "_pageDimensions_x").unwrap_or(0.0) as f32, num(&root, "_pageDimensions_y").unwrap_or(0.0) as f32);
    if !(size.w.is_finite() && size.h.is_finite() && size.w >= 1.0 && size.h >= 1.0 && size.w <= MAX_PAGE_PT && size.h <= MAX_PAGE_PT) {
        return Err("its page size is missing or out of range".into());
    }
    s.page = Page::new(size);
    set_margins(&mut s.page, margins(&root, "_margin_"), &mut notes);
    let engine = root.get("_engineIdentifier").and_then(Lua::str).unwrap_or_default();
    let es = root.get("_engineSettings").and_then(|e| e.get(engine)).ok_or_else(|| format!("no settings for its layout style {engine:?}"))?;
    let mut photos: Vec<String> = members.iter().map(|p| p.0.to_string()).collect();
    match engine {
        CUSTOM_PACKAGE | PICTURE_PACKAGE => {
            let custom = engine == CUSTOM_PACKAGE;
            s.image.zoom_to_fill = flag(es, "zoomToFillFrame");
            s.image.rotate_to_fit = flag(es, "autorotate");
            if flag(es, "innerStrokeBorder") {
                s.image.stroke =
                    Some(Stroke { width: width(es, "innerStrokeWidth", &mut notes), color: color(es, "innerStrokeColor", [0, 0, 0, 255]) });
            }
            if flag(es, "photoBorder") {
                notes.push("its photo border has no equivalent and is left out".into());
            }
            s.options.crop_marks = flag(es, "cutLinesOn");
            let fit = if s.image.zoom_to_fill { Fit::Fill } else { Fit::Fit };
            let pages = package_pages(es, fit, custom.then_some(photo), &mut notes)?;
            if custom {
                // a Custom Package names the photo of each cell; one Lightroom couldn't place stays empty
                photos.clear();
            } else {
                // a Picture Package repeats each photo in every cell, one package per photo
                s.image.repeat_one = true;
            }
            s.layout = LayoutStyle::CustomPackage { pages };
        }
        GRID => {
            s.image.zoom_to_fill = flag(es, "autofillLetterboxing");
            s.image.rotate_to_fit = flag(es, "autorotate");
            s.image.repeat_one = flag(es, "oneImagePerPage");
            if flag(es, "printImageBorder") {
                s.image.stroke = Some(Stroke { width: width(es, "borderWidth", &mut notes), color: color(es, "imageBorderColor", [0, 0, 0, 255]) });
            }
            set_margins(&mut s.page, margins(es, "margin_e_"), &mut notes);
            let count = |k: &str| num(es, k).filter(|n| (1.0..=100.0).contains(n)).map(|n| n.round() as u32);
            let (Some(cols), Some(rows)) = (count("template_cells_h"), count("template_cells_v")) else {
                return Err("its grid needs 1 to 100 rows and columns".into());
            };
            let gap = |k: &str| num(es, k).filter(|n| *n >= 0.0 && *n <= f64::from(MAX_PAGE_PT)).unwrap_or(0.0) as f32;
            let grid = Grid { gutter_x: gap("template_spacing_h"), gutter_y: gap("template_spacing_v"), ..Grid::new(rows, cols) };
            if let (Some(w), Some(h), Ok(cells)) = (num(es, "template_size_h"), num(es, "template_size_v"), grid.rects(s.page.content()))
                && let Some(first) = cells.first()
                && ((f64::from(first.w) - w).abs() > 1.0 || (f64::from(first.h) - h).abs() > 1.0)
            {
                notes.push(format!(
                    "its cells are {:.0} × {:.0} pt in Lightroom and fill the margins here ({:.0} × {:.0} pt)",
                    w, h, first.w, first.h
                ));
            }
            s.layout = LayoutStyle::SingleImage { grid };
            s.options.crop_marks = flag(es, "printCropMarks");
        }
        other => return Err(format!("its layout style {other:?} is unknown")),
    }
    s.options.page_numbers = flag(es, "printPageNumbers");
    s.options.page_info = flag(es, "printPageInfo");
    if flag(es, "pageBackgroundColorActive") {
        s.options.background = color(es, "pageBackgroundColor", lcc_layout::WHITE);
    }
    if flag(es, "identityPlateActive") {
        notes.push("its identity plate is left out (set it again in the Page panel)".into());
    }
    if flag(es, "watermarkingEnabled") {
        notes.push("its watermark is left out (prints have no watermark here)".into());
    }
    if let Some(g) = root.get("_globalSettings") {
        job(&mut s, g, &mut notes);
    }
    s.validate().map_err(|e| e.to_string())?;
    let mut doc = s.build(&photos).map_err(|e| e.to_string())?;
    doc.settings = Some(serde_json::to_value(&s).map_err(|e| e.to_string())?);
    let document = doc.to_json().map_err(|e| e.to_string())?;
    if document.len() > lcc_catalog::MAX_CREATION_BYTES {
        return Err("its layout is too large".into());
    }
    Ok(SavedPrint { document, notes })
}

/// The pages of a package: each cell where `bounds` puts it, on the page it names. `photo` (a
/// Custom Package) resolves the photo each cell names.
fn package_pages(es: &Lua, fit: Fit, photo: Option<&dyn Fn(i64) -> Option<PhotoId>>, notes: &mut Vec<String>) -> Result<Vec<PageLayout>, String> {
    let listed = match es.get("pages") {
        Some(Lua::Table(pages, _)) => pages.as_slice(),
        _ => &[],
    };
    let wanted = num(es, "numLayoutPages").filter(|n| n.is_finite() && *n >= 1.0).map_or(1.0, |n| n.min(MAX_PAGES as f64)) as usize;
    let mut pages: Vec<PageLayout> = vec![PageLayout::default(); wanted.max(listed.len()).clamp(1, MAX_PAGES)];
    let (mut bad, mut unplaced, mut dropped) = (0usize, 0usize, 0usize);
    for (at, page) in listed.iter().enumerate() {
        let cells = match page.get("cells") {
            Some(Lua::Table(cells, _)) => cells.as_slice(),
            _ => &[],
        };
        for cell in cells {
            let Some(rect) = bounds(cell) else {
                bad += 1;
                continue;
            };
            // the cell's own 1-based page wins over the list it is in
            let index = num(cell, "page").filter(|n| *n >= 1.0 && *n <= MAX_PAGES as f64).map_or(at, |n| n as usize - 1);
            if index >= pages.len() {
                pages.resize(index + 1, PageLayout::default());
            }
            let mut c = Cell::photo(rect);
            if let CellKind::Photo(p) = &mut c.kind {
                p.fit = fit;
                if let (Some(resolve), Some(id)) = (photo, num(cell, "imageID")) {
                    p.photo = resolve(id as i64).map(|id| id.0.to_string());
                    unplaced += usize::from(p.photo.is_none());
                }
            }
            match pages.get_mut(index) {
                Some(p) if p.cells.len() < MAX_CELLS => p.cells.push(c),
                _ => dropped += 1,
            }
        }
    }
    if pages.iter().all(|p| p.cells.is_empty()) {
        return Err("it has no cells".into());
    }
    if bad > 0 {
        notes.push(format!("{bad} of its cells have no readable position and are left out"));
    }
    if unplaced > 0 {
        notes.push(format!("{unplaced} of its cells hold a photo that wasn't imported and are left empty"));
    }
    if dropped > 0 {
        notes.push(format!("{dropped} of its cells are past {MAX_CELLS} on a page and are left out"));
    }
    Ok(pages)
}

/// Print Job: destination, resolution, sharpening, media, colour management and adjustment.
fn job(s: &mut PrintSettings, g: &Lua, notes: &mut Vec<String>) {
    s.destination = match g.get("printDestination").and_then(Lua::str) {
        Some("jpeg") => Destination::Jpeg,
        _ => Destination::Printer,
    };
    let dpi = if s.destination == Destination::Jpeg {
        num(g, "printingFileResolution")
    } else {
        num(g, "printingResolution").filter(|_| flag(g, "usePrintingResolution"))
    };
    if let Some(dpi) = dpi {
        s.job.dpi = dpi.clamp(72.0, 720.0) as f32;
    }
    if let Some(q) = num(g, "jpegFileQuality") {
        s.job.jpeg_quality = q.clamp(1.0, 100.0).round() as u8;
    }
    s.job.draft = flag(g, "draftModePrinting");
    // the strength counts from 1: Low, Standard, High (a saved print showing Low stores 1)
    s.job.sharpening = match num(g, "imageSharpening").map(f64::round) {
        _ if !flag(g, "imageSharpeningOn") => PrintSharpening::Off,
        Some(n) if n <= 1.0 => PrintSharpening::Low,
        Some(n) if n >= 3.0 => PrintSharpening::High,
        _ => PrintSharpening::Standard,
    };
    s.job.media = if g.get("mediaType").and_then(Lua::str) == Some("matte") { PrintMedia::Matte } else { PrintMedia::Glossy };
    let c = &mut s.job.color;
    c.intent = match g.get("renderingIntent").and_then(Lua::str) {
        Some(i) if i.to_ascii_lowercase().starts_with("relative") => PrintIntent::Relative,
        _ => PrintIntent::Perceptual,
    };
    c.adjust = flag(g, "outputAdjustmentsOn");
    c.brightness = num(g, "outputBrightness").map_or(0.0, |v| v.clamp(-100.0, 100.0) as f32);
    c.contrast = num(g, "outputClarity").map_or(0.0, |v| v.clamp(-100.0, 100.0) as f32);
    c.bit16 = flag(g, "sixteenBitPrinting");
    let profile = if s.destination == Destination::Jpeg { "printingFileProfile" } else { "printingProfile" };
    if let Some(p) = g.get(profile).and_then(Lua::str).map(str::trim).filter(|p| !p.is_empty() && *p != "(none)" && *p != "sRGB") {
        notes.push(format!("its colour profile “{p}” is not chosen here (choose it again in Print Job)"));
    }
}

fn num(t: &Lua, key: &str) -> Option<f64> {
    match t.get(key) {
        Some(Lua::Num(n)) if n.is_finite() => Some(*n),
        _ => None,
    }
}

fn flag(t: &Lua, key: &str) -> bool {
    matches!(t.get(key), Some(Lua::Bool(true)))
}

/// A stroke width in points, 0 to 72.
fn width(t: &Lua, key: &str, notes: &mut Vec<String>) -> f32 {
    match num(t, key) {
        Some(w) if (0.0..=72.0).contains(&w) => w as f32,
        _ => {
            notes.push(format!("its {key} is out of range; 1 pt is used"));
            1.0
        }
    }
}

/// `AgColor( r, g, b, a )` → 8-bit sRGB with alpha; `default` when it isn't one.
fn color(t: &Lua, key: &str, default: Color) -> Color {
    let Some(Lua::Table(parts, _)) = t.get(key) else { return default };
    let mut out = default;
    for (slot, part) in out.iter_mut().zip(parts) {
        if let Lua::Num(v) = part
            && v.is_finite()
        {
            *slot = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// Margins `<prefix>top/right/bottom/left` in points (missing or negative: 0).
fn margins(t: &Lua, prefix: &str) -> Insets {
    let m = |side: &str| num(t, &format!("{prefix}{side}")).filter(|v| *v >= 0.0).unwrap_or(0.0) as f32;
    Insets { top: m("top"), right: m("right"), bottom: m("bottom"), left: m("left") }
}

fn set_margins(page: &mut Page, m: Insets, notes: &mut Vec<String>) {
    let before = page.margins;
    page.margins = m;
    if page.validate().is_err() {
        page.margins = before;
        notes.push("its margins leave no room on the page and are left out".into());
    }
}

/// `bounds = AgMutableRect( left, top, right, bottom )` → a rectangle on the page.
fn bounds(cell: &Lua) -> Option<Rect> {
    let Some(Lua::Table(parts, _)) = cell.get("bounds") else { return None };
    let v: Vec<f64> = parts.iter().filter_map(|p| if let Lua::Num(n) = p { Some(*n) } else { None }).collect();
    let [x1, y1, x2, y2] = v.as_slice() else { return None };
    let limit = f64::from(MAX_PAGE_PT) * 2.0;
    if ![x1, y1, x2, y2].iter().all(|v| v.is_finite() && v.abs() <= limit) {
        return None;
    }
    let (x, y) = (x1.min(*x2), y1.min(*y2));
    let (w, h) = ((x2 - x1).abs(), (y2 - y1).abs());
    (w >= 1.0 && h >= 1.0).then(|| Rect::new(x as f32, y as f32, w as f32, h as f32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_layout::Document;

    /// A saved print as the catalog stores it (shape of a real catalog's row; numbers made up).
    fn custom(cells: &str) -> String {
        format!(
            r#"s = {{
	_contentFilter = "selectedPhotos",
	_engineIdentifier = "com.adobe.lightroom.print.engine.package.custom",
	_engineSettings = {{
		["com.adobe.lightroom.print.engine.package.custom"] = {{
			autorotate = true,
			innerStrokeBorder = true,
			innerStrokeColor = AgColor( 0, 0, 0, 1 ),
			innerStrokeWidth = 0.2,
			numLayoutPages = 1,
			pageBackgroundColor = AgColor( 0, 0, 0, 1 ),
			pageBackgroundColorActive = false,
			pages = {{
				{{
					cells = {{
{cells}
					}},
				}},
			}},
			photoBorder = false,
			zoomToFillFrame = true,
		}},
	}},
	_globalSettings = {{
		imageSharpening = 1,
		imageSharpeningOn = true,
		jpegFileQuality = 100,
		mediaType = "matte",
		printDestination = "jpeg",
		printingFileProfile = "sRGB",
		printingFileResolution = 300,
		printingProfile = "(none)",
		renderingIntent = "Perceptual",
	}},
	_margin_bottom = 0,
	_margin_left = 0,
	_margin_right = 0,
	_margin_top = 0,
	_orientation = "portrait",
	_pageDimensions_x = 1984.251953125,
	_pageDimensions_y = 2976.3779296875,
	_paperID = "Canvas 70x105",
	_printInfoBase64 = "AAAA",
}}
"#
        )
    }

    fn cell(x1: f64, y1: f64, x2: f64, y2: f64, image: Option<i64>, page: u32) -> String {
        let image = image.map(|i| format!("imageID = {i},")).unwrap_or_default();
        format!(
            "{{ bounds = AgMutableRect( {x1}, {y1}, {x2}, {y2} ), cellScreenBounds = AgMutableRect( 0, 0, 1, 1 ), imageHeight = {}, {image} imageWidth = {}, page = {page}, }},",
            y2 - y1,
            x2 - x1
        )
    }

    fn photos(id: i64) -> Option<PhotoId> {
        (id >= 100).then(|| PhotoId(id as u64 - 99))
    }

    fn doc(p: &SavedPrint) -> Document {
        Document::from_json(&p.document).unwrap()
    }

    fn settings(d: &Document) -> PrintSettings {
        serde_json::from_value(d.settings.clone().unwrap()).unwrap()
    }

    #[test]
    fn a_custom_package_keeps_its_cells_photos_and_job() {
        let cells = [
            cell(653.6, 1396.1, 1984.25, 2279.9, Some(100), 1),
            cell(0.0, 0.0, 799.3, 419.0, Some(101), 1),
            cell(2.2737e-13, 333.2, 1364.2, 1239.3, Some(102), 1),
        ];
        let p = saved_print(&custom(&cells.concat()), &photos, &[PhotoId(7)]).unwrap();
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        let d = doc(&p);
        assert_eq!(d.pages.len(), 1);
        assert!((d.page.size.w - 1984.25).abs() < 0.01 && (d.page.size.h - 2976.38).abs() < 0.01);
        let cells: Vec<_> = d.pages[0].cells.iter().filter_map(|c| Some((c.rect, c.as_photo()?.clone()))).collect();
        assert_eq!(cells.len(), 3);
        // in catalog order (back to front), where the catalog puts them, each with its photo
        let (r, first) = &cells[0];
        assert!((r.x - 653.6).abs() < 0.01 && (r.y - 1396.1).abs() < 0.01 && (r.w - 1330.65).abs() < 0.01 && (r.h - 883.8).abs() < 0.01);
        assert_eq!(first.photo.as_deref(), Some("1"));
        assert_eq!(cells[1].1.photo.as_deref(), Some("2"));
        assert_eq!(cells[2].1.photo.as_deref(), Some("3"));
        assert!(cells.iter().all(|(_, c)| c.fit == Fit::Fill && c.rotate_to_fit && c.stroke.is_some_and(|s| (s.width - 0.2).abs() < 1e-6)));
        // a collection member the layout doesn't place stays out of the cells
        assert!(!d.photos().contains(&"7"));
        let s = settings(&d);
        assert_eq!(s.destination, Destination::Jpeg);
        assert_eq!((s.job.dpi, s.job.jpeg_quality, s.job.sharpening, s.job.media), (300.0, 100, PrintSharpening::Low, PrintMedia::Matte));
        assert_eq!(s.options.background, lcc_layout::WHITE, "an inactive background colour stays white");
        assert!(matches!(s.layout, LayoutStyle::CustomPackage { .. }));
    }

    /// Print sharpening counts from 1 (Low) to 3 (High); off wins over the strength.
    #[test]
    fn print_sharpening_strength_counts_from_one() {
        let cells = cell(0.0, 0.0, 100.0, 100.0, Some(100), 1);
        for (text, want) in [
            ("imageSharpening = 1,\n\t\timageSharpeningOn = true", PrintSharpening::Low),
            ("imageSharpening = 2,\n\t\timageSharpeningOn = true", PrintSharpening::Standard),
            ("imageSharpening = 3,\n\t\timageSharpeningOn = true", PrintSharpening::High),
            ("imageSharpening = 3,\n\t\timageSharpeningOn = false", PrintSharpening::Off),
        ] {
            let content = custom(&cells).replace("imageSharpening = 1,\n\t\timageSharpeningOn = true", text);
            assert!(content.contains(text));
            let p = saved_print(&content, &photos, &[]).unwrap();
            assert_eq!(settings(&doc(&p)).job.sharpening, want, "{text}");
        }
    }

    #[test]
    fn cells_go_to_their_pages_and_unknown_photos_leave_them_empty() {
        let cells = [cell(0.0, 0.0, 100.0, 100.0, Some(100), 1), cell(10.0, 10.0, 200.0, 300.0, Some(5), 2), cell(0.0, 0.0, 50.0, 50.0, None, 2)];
        let p = saved_print(&custom(&cells.concat()), &photos, &[]).unwrap();
        let d = doc(&p);
        assert_eq!(d.pages.len(), 2);
        assert_eq!(d.pages[0].photo_cells().count(), 1);
        assert_eq!(d.pages[1].photo_cells().filter(|c| c.photo.is_none()).count(), 2);
        assert!(p.notes.iter().any(|n| n.contains("1 of its cells hold a photo that wasn't imported")), "{:?}", p.notes);
    }

    #[test]
    fn a_grid_reads_rows_columns_margins_and_border() {
        let text = r#"s = {
	_engineIdentifier = "com.adobe.lightroom.print.engine.grid",
	_engineSettings = { ["com.adobe.lightroom.print.engine.grid"] = {
		autofillLetterboxing = true, oneImagePerPage = false,
		margin_e_bottom = 41, margin_e_left = 18, margin_e_right = 18, margin_e_top = 18,
		printImageBorder = true, borderWidth = 2, imageBorderColor = AgColor( 1, 0, 0, 1 ),
		pageBackgroundColorActive = true, pageBackgroundColor = AgColor( 0.5, 0.5, 0.5, 1 ),
		template_cells_h = 3, template_cells_v = 2, template_spacing_h = 9, template_spacing_v = 12,
		identityPlateActive = true,
	} },
	_pageDimensions_x = 612, _pageDimensions_y = 792,
}"#;
        let p = saved_print(text, &photos, &[PhotoId(1), PhotoId(2)]).unwrap();
        let s = settings(&doc(&p));
        let LayoutStyle::SingleImage { grid } = &s.layout else { panic!("{:?}", s.layout) };
        assert_eq!((grid.rows, grid.cols, grid.gutter_x, grid.gutter_y), (2, 3, 9.0, 12.0));
        assert_eq!((s.page.margins.top, s.page.margins.bottom, s.page.margins.left), (18.0, 41.0, 18.0));
        assert!(s.image.zoom_to_fill);
        assert_eq!(s.image.stroke.map(|s| (s.width, s.color)), Some((2.0, [255, 0, 0, 255])));
        assert_eq!(s.options.background, [128, 128, 128, 255]);
        assert_eq!(doc(&p).photos(), vec!["1", "2"]);
        assert!(p.notes.iter().any(|n| n.contains("identity plate")), "{:?}", p.notes);
    }

    #[test]
    fn a_picture_package_repeats_each_photo() {
        let text = r#"s = {
	_engineIdentifier = "com.adobe.lightroom.print.engine.package.quick",
	_engineSettings = { ["com.adobe.lightroom.print.engine.package.quick"] = {
		pages = { { cells = {
			{ bounds = AgMutableRect( 0, 0, 360, 504 ), page = 1 },
			{ bounds = AgMutableRect( 360, 0, 540, 252 ), page = 1 },
		} } },
	} },
	_pageDimensions_x = 612, _pageDimensions_y = 792,
}"#;
        let p = saved_print(text, &photos, &[PhotoId(1), PhotoId(2)]).unwrap();
        let d = doc(&p);
        assert_eq!(d.pages.len(), 2, "one package per photo");
        assert!(d.pages[1].photo_cells().all(|c| c.photo.as_deref() == Some("2")));
    }

    /// Damaged, hostile or foreign layouts are refused with a reason (the collection then imports
    /// as a plain one), or lose only the cells that can't be read; never a panic.
    #[test]
    fn hostile_layouts_never_panic() {
        let huge = format!("{}", f64::MAX);
        let cases: Vec<String> = vec![
            String::new(),
            "s = 3".into(),
            "s = {".into(),
            "s = { _pageDimensions_x = 612, _pageDimensions_y = 792 }".into(),
            custom("").replace("1984.251953125", "-5"),
            custom("").replace("1984.251953125", &huge),
            custom("").replace("1984.251953125", "0/0"),
            custom(""),
            custom("{ bounds = AgMutableRect( 1, 2 ), page = 1 },"),
            custom("{ bounds = AgMutableRect( 0, 0, 1e308, 5 ), page = 1 },"),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(i64::MAX), 1)),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(-1), 99_999_999)),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 3)).replace("numLayoutPages = 1", "numLayoutPages = 1e300"),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 1)).replace("innerStrokeWidth = 0.2", "innerStrokeWidth = -4"),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 1)).replace("AgColor( 0, 0, 0, 1 )", "AgColor( 7, -3 )"),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 1)).replace("_margin_top = 0", "_margin_top = 5000"),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 1)).replace("package.custom\",", "package.unknown\","),
            custom(&cell(0.0, 0.0, 10.0, 10.0, Some(100), 1)).replace("printingFileResolution = 300", "printingFileResolution = 1e9"),
            "s = { _engineIdentifier = \"com.adobe.lightroom.print.engine.grid\", _engineSettings = { [\"com.adobe.lightroom.print.engine.grid\"] = { template_cells_h = 1e9, template_cells_v = 0 } }, _pageDimensions_x = 612, _pageDimensions_y = 792 }".into(),
            "s = { AgMutableRect( AgColor( AgColor( (((( ".into(),
        ];
        for (i, text) in cases.iter().enumerate() {
            match saved_print(text, &photos, &[PhotoId(1)]) {
                Ok(p) => {
                    let d = doc(&p);
                    assert!(d.validate().is_ok(), "case {i}");
                    let s = settings(&d);
                    assert!(s.validate().is_ok(), "case {i}");
                }
                Err(why) => assert!(!why.is_empty(), "case {i}"),
            }
        }
        // many cells on one page: the ones past the limit are dropped with a note
        let many = cell(0.0, 0.0, 10.0, 10.0, None, 1).repeat(MAX_CELLS + 3);
        let p = saved_print(&custom(&many), &photos, &[]).unwrap();
        assert_eq!(doc(&p).pages[0].cells.len(), MAX_CELLS);
        assert!(p.notes.iter().any(|n| n.contains("3 of its cells are past")), "{:?}", p.notes);
    }
}
