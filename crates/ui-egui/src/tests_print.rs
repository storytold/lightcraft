//! The Print dialog, driven through the control channel: styles, cells dragged and resized with
//! real pointer input, Auto Layout, pages, templates and the PDF.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(30);

fn demo(services: Services) -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    h.settle(T);
    h
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(T);
}

/// Clicks `id` in the dialog's settings column after scrolling that column so the widget sits mid-window:
/// the column scrolls, and a widget below its visible edge is registered where it would be drawn, where a
/// click hits nothing.
fn click_in_view(h: &mut Headless, id: &str) {
    let r = rect(h, id);
    // (mid-height of the 1400x900 test window)
    let middle = 450.0;
    let r2 = h.request("ui.move", json!({"x": r.center().x, "y": middle}), T);
    assert_eq!(r2["ok"], true, "{id}: {r2}");
    let r2 = h.request("ui.scroll", json!({"dy": middle - r.center().y}), T);
    assert_eq!(r2["ok"], true, "{id}: {r2}");
    h.settle(T);
    click(h, id);
}

fn drag(h: &mut Headless, id: &str, dx: f32, dy: f32) {
    let r = h.request("ui.dragWidget", json!({"id": id, "dx": dx, "dy": dy, "steps": 8}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(T);
}

fn cmd(h: &mut Headless, id: &str, params: Value) -> Value {
    let r = h.request("engine.execute", json!({"command": id, "params": params}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.settle(T);
    r["result"].clone()
}

/// The first page's MediaBox width and height in points.
fn media_box(pdf: &[u8]) -> (f64, f64) {
    let text = String::from_utf8_lossy(pdf);
    let at = text.find("/MediaBox [").expect("a MediaBox") + "/MediaBox [".len();
    let nums: Vec<f64> = text[at..].split(']').next().unwrap().split_whitespace().map(|v| v.parse().unwrap()).collect();
    (nums[2], nums[3])
}

fn open_print(h: &mut Headless) {
    cmd(h, "dialog.print", json!({}));
    assert!(matches!(h.app.ui.dialog, Some(Dialog::Print)));
    assert!(has(h, "print:page"), "the page is on screen");
}

/// Screen points per inch of the page shown.
fn scale(h: &Headless) -> f32 {
    rect(h, "print:page").width() / h.app.session.print.layout.page_size().unwrap().0 as f32
}

fn cell_rect(h: &Headless, id: u32) -> lcc_engine::print::layout::Rect {
    h.app.session.print.layout.cell(id).unwrap().rect
}

#[test]
fn style_buttons_and_checkboxes_run_print_set() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    assert!(has(&h, "button:printStyle-0") && has(&h, "check:printZoomFill") && has(&h, "button:dialogOk"));
    click(&mut h, "button:printStyle-1");
    assert_eq!(h.app.session.print.layout.style, lcc_engine::print::layout::Style::ContactSheet);
    assert!(has(&h, "drag:printColumns") && has(&h, "print:gridcell:0"));
    click(&mut h, "check:printZoomFill");
    click(&mut h, "check:printStroke");
    assert!(h.app.session.print.image.zoom_to_fill && h.app.session.print.image.stroke_enabled);
    click_in_view(&mut h, "button:printOrientation-1");
    assert_eq!(h.app.session.print.layout.page.orientation, lcc_engine::print::layout::Orientation::Landscape);
    let p = rect(&h, "print:page");
    assert!(p.width() > p.height(), "the page is drawn landscape");
    click_in_view(&mut h, "button:printUnits-2");
    assert_eq!(h.app.session.print.guides.units, lcc_engine::print::layout::Unit::Millimeters);
    click_in_view(&mut h, "check:printShowGrid");
    assert!(h.app.session.print.guides.show_page_grid);
    // Cancel closes the dialog
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
}

#[test]
fn cells_are_added_dragged_resized_and_arranged() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    click(&mut h, "button:printStyle-3");
    // snapping off, so the pointer maths is exact
    cmd(&mut h, "print.set", json!({"guides": {"snap": "off"}}));
    click(&mut h, "button:printAddCell-4");
    click(&mut h, "button:printAddCell-1");
    let cells: Vec<u32> = h.app.session.print.layout.cells.iter().map(|c| c.id).collect();
    assert_eq!(cells.len(), 2);
    assert_eq!(h.app.session.print.layout.cells[0].rect.w, 4.0);
    assert!(has(&h, &format!("print:cell:{}", cells[0])));

    // drag the first cell with the pointer: the move is in inches, through print.cell.move
    let before = cell_rect(&h, cells[0]);
    let s = scale(&h);
    drag(&mut h, &format!("print:cell:{}", cells[0]), 0.0, 100.0);
    let after = cell_rect(&h, cells[0]);
    assert!((after.y - before.y - f64::from(100.0 / s)).abs() < 0.05, "moved {} in, expected {}", after.y - before.y, 100.0 / s);
    assert!((after.x - before.x).abs() < 0.05);
    assert_eq!(h.app.ui.print.selected, Some(cells[0]));

    // resize with its corner handle
    let r0 = cell_rect(&h, cells[0]);
    drag(&mut h, &format!("print:resize:{}", cells[0]), 30.0, 20.0);
    let r1 = cell_rect(&h, cells[0]);
    assert!((r1.w - r0.w - f64::from(30.0 / s)).abs() < 0.05 && (r1.h - r0.h - f64::from(20.0 / s)).abs() < 0.05, "{r0:?} → {r1:?}");
    assert_eq!((r1.x, r1.y), (r0.x, r0.y), "the top-left corner stays");

    // arrange: Front moves it last
    click(&mut h, "button:printStack-3");
    assert_eq!(h.app.session.print.layout.cells.last().unwrap().id, cells[0]);
    click(&mut h, "button:printStack-0");
    assert_eq!(h.app.session.print.layout.cells.first().unwrap().id, cells[0]);

    // the inspector's delete
    click(&mut h, "button:printCellDelete");
    assert_eq!(h.app.session.print.layout.cells.len(), 1);
    assert_eq!(h.app.ui.print.selected, None);
}

#[test]
fn dragging_a_cell_snaps_to_its_neighbour_when_snap_is_on() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    click(&mut h, "button:printStyle-3");
    cmd(&mut h, "print.set", json!({"layout": {"spacing": 0.0}}));
    click(&mut h, "button:printAddCell-0");
    click(&mut h, "button:printAddCell-0");
    let ids: Vec<u32> = h.app.session.print.layout.cells.iter().map(|c| c.id).collect();
    assert_eq!(h.app.session.print.guides.snap, lcc_engine::print::layout::Snap::Cells);
    let a = cell_rect(&h, ids[0]);
    // park the second cell away from the first, then drag it back to 0.06 in from the first's right edge
    cmd(&mut h, "print.cell.move", json!({"id": ids[1], "x": a.right() + 0.8, "y": a.y}));
    let s = scale(&h);
    drag(&mut h, &format!("print:cell:{}", ids[1]), -(0.74 * s), 0.0);
    let r = cell_rect(&h, ids[1]);
    assert!((r.x - a.right()).abs() < 1e-6, "snapped to the neighbour's edge: {} vs {}", r.x, a.right());
    // with snapping off the same drag lands where the pointer put it
    cmd(&mut h, "print.set", json!({"guides": {"snap": "off"}}));
    cmd(&mut h, "print.cell.move", json!({"id": ids[1], "x": a.right() + 0.8, "y": a.y}));
    drag(&mut h, &format!("print:cell:{}", ids[1]), -(0.74 * s), 0.0);
    let r = cell_rect(&h, ids[1]);
    assert!((r.x - (a.right() + 0.06)).abs() < 0.05 && r.x > a.right() + 0.01, "unsnapped: {}", r.x - a.right());
}

#[test]
fn auto_layout_new_page_and_clear_buttons() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    click(&mut h, "button:printStyle-2");
    for _ in 0..3 {
        click(&mut h, "button:printAddCell-4");
    }
    for _ in 0..3 {
        click(&mut h, "button:printAddCell-5");
    }
    let before_pages = h.app.session.print.layout.pages;
    // scramble everything onto one spot, then pack
    cmd(&mut h, "print.layout.autoLayout", json!({}));
    click(&mut h, "button:printAutoLayout");
    assert!(!h.app.session.print.layout.has_overlaps());
    assert!(h.app.session.print.layout.pages >= before_pages.min(2));
    let pages = h.app.session.print.layout.pages;
    click(&mut h, "button:printNewPage");
    assert_eq!(h.app.session.print.layout.pages, pages + 1);
    assert_eq!(h.app.ui.print.page as u32, pages, "the new page is shown");
    // the next-page button stays on the last page
    click(&mut h, "button:printPageNext");
    assert_eq!(h.app.ui.print.page as u32, pages);
    click(&mut h, "button:printPagePrev");
    assert_eq!(h.app.ui.print.page as u32, pages - 1);
    click(&mut h, "button:printClear");
    assert_eq!((h.app.session.print.layout.cells.len(), h.app.session.print.layout.pages), (0, 1));
    assert_eq!(h.app.ui.print.page, 0);
}

#[test]
fn templates_save_apply_and_delete_from_the_dialog() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    click(&mut h, "button:printStyle-1");
    cmd(&mut h, "print.set", json!({"layout": {"grid": {"columns": 5, "rows": 6}}}));
    h.app.ui.print.template_name = "Sheet 5x6".into();
    click(&mut h, "button:printSaveTemplate");
    assert_eq!(h.app.session.print_templates.len(), 1);
    // template files need the host's file dialogs: without them there is no Export… or Import…
    assert!(!has(&h, "button:printTemplateExport-0"), "no save dialog: no Export…");
    assert!(!has(&h, "button:printTemplateImport"), "no open dialog: no Import…");
    cmd(&mut h, "print.reset", json!({}));
    assert!(has(&h, "button:printTemplateApply-0"));
    click(&mut h, "button:printTemplateApply-0");
    assert_eq!(h.app.session.print.layout.grid.columns, 5);
    click(&mut h, "button:printTemplateDelete-0");
    assert!(h.app.session.print_templates.is_empty());
}

#[test]
fn templates_export_and_import_from_the_dialog() {
    let dir = lcc_testkit::TempDir::new("lcc-ui-print-templates");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("Sheet 5x6.lcprint").to_string_lossy().to_string();
    let suggested = Arc::new(Mutex::new(Vec::<String>::new()));
    let names = suggested.clone();
    let (save_to, pick) = (file.clone(), file.clone());
    let services = Services {
        save_print_template: Some(Box::new(move |name: &str| {
            names.lock().unwrap().push(name.to_string());
            Some(save_to.clone())
        })),
        pick_print_templates: Some(Box::new(move || vec![pick.clone()])),
        ..Default::default()
    };
    let mut h = demo(services);
    open_print(&mut h);
    click(&mut h, "button:printStyle-1");
    cmd(&mut h, "print.set", json!({"layout": {"grid": {"columns": 5, "rows": 6}}}));
    h.app.ui.print.template_name = "Sheet 5x6".into();
    click(&mut h, "button:printSaveTemplate");
    click(&mut h, "button:printTemplateExport-0");
    assert_eq!(*suggested.lock().unwrap(), vec!["Sheet 5x6.lcprint".to_string()]);
    assert!(std::path::Path::new(&file).is_file());
    click(&mut h, "button:printTemplateImport");
    let names: Vec<&str> = h.app.session.print_templates.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["Sheet 5x6", "Sheet 5x6 (2)"]);
    assert_eq!(h.app.session.print_templates[1].settings, h.app.session.print_templates[0].settings);
    assert!(has(&h, "button:printTemplateExport-1"), "the imported template is listed");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn print_to_pdf_picks_a_file_and_runs_in_the_background() {
    let asked = Arc::new(Mutex::new(Vec::<crate::pick::PickRequest>::new()));
    let requests = asked.clone();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let written = bytes.clone();
    let services = Services {
        picker: Some(Box::new(move |req| {
            requests.lock().unwrap().push(req);
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(Ok(vec!["Print.pdf".into()])).unwrap();
            Ok(rx)
        })),
        write_shared: Some(Arc::new(move |path, data| {
            assert_eq!(path, "Print.pdf");
            *written.lock().unwrap() = data.to_vec();
            Ok(())
        })),
        ..Default::default()
    };
    let mut h = demo(services);
    open_print(&mut h);
    cmd(&mut h, "print.set", json!({"layout": {"page": {"paper": "5x7", "orientation": "landscape"}}, "job": {"ppi": 100.0}}));
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "the dialog closes once the print has started");
    assert!(h.step_until(Duration::from_secs(60), |h| h.app.last_export_result.is_some()), "the print did not finish");
    let req = asked.lock().unwrap();
    assert_eq!(req[0].file_name.as_deref(), Some("Print.pdf"));
    let b = bytes.lock().unwrap();
    assert!(b.starts_with(b"%PDF-"));
    let (pw, ph) = media_box(&b);
    assert!((pw - 504.0).abs() < 1.0 && (ph - 360.0).abs() < 1.0, "7 x 5 in, got {pw} x {ph} pt");
    assert_eq!(h.app.last_export_result.as_ref().unwrap()["files"][0]["print"], true);
}

#[test]
fn printing_shows_a_row_in_the_activity_stack_and_stops() {
    let mut app = LccApp::new(
        lcc_engine::Session::with_demo(),
        Services { write_shared: Some(Arc::new(|_, _| panic!("a stopped print wrote a file"))), ..Default::default() },
    );
    app.session.execute("print.set", &json!({"layout": {"style": "contactSheet"}, "job": {"ppi": 300.0}})).unwrap();
    let ids: Vec<_> = app.session.catalog.photos().take(12).map(|p| p.id.0).collect();
    crate::export_task::start_print(&mut app, &json!({"path": "Stop.pdf", "ids": ids})).unwrap();
    let rows = app.session.activity.list();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!((rows[0].kind, rows[0].label.as_str()), ("export", "Printing to PDF"));
    assert!(rows[0].cancellable);
    app.session.activity.cancel(rows[0].id).unwrap();
    let task = app.export.take().unwrap();
    assert!(task.cancel.load(std::sync::atomic::Ordering::Relaxed));
    drop(task);
    assert!(app.session.activity.list().is_empty(), "the row goes with the print");
}

#[test]
fn the_print_dialog_fits_the_window_with_its_buttons_on_screen() {
    // (the smaller window: what fits it fits a bigger one)
    let size = [1100.0, 760.0];
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services::default());
    let mut h = Headless::new(app, size, 1.0);
    h.settle(T);
    for style in ["singleImage", "customPackage"] {
        cmd(&mut h, "print.set", json!({"layout": {"style": style}}));
        if style == "customPackage" {
            cmd(&mut h, "print.cell.add", json!({"width": 4, "height": 6}));
        }
        open_print(&mut h);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
        let dialog = rect(&h, "dialog:window");
        assert!(screen.contains_rect(dialog), "{style} at {size:?}: dialog {dialog:?} runs off the window");
        for b in ["button:dialogOk", "button:dialogCancel"] {
            assert!(dialog.contains_rect(rect(&h, b)), "{style} at {size:?}: {b} is outside the dialog");
        }
        click(&mut h, "button:dialogCancel");
        assert!(h.app.ui.dialog.is_none());
    }
}

#[test]
fn print_job_colour_controls_reach_the_job_and_the_pdf() {
    use lcc_engine::print_color::{PrintIntent, PrintMedia, PrintProfile, PrintSharpen};
    let dir = lcc_testkit::TempDir::new("app-print-ui");
    std::fs::create_dir_all(&dir).unwrap();
    let icc = dir.join("Baryta Paper.icc");
    std::fs::write(&icc, lcc_codecs::icc::write_named(lcc_codecs::NamedSpace::AdobeRgb)).unwrap();
    let picked = icc.to_string_lossy().into_owned();
    let services = Services { png: None, pick_display_profile: Some(Box::new(move || vec![picked.clone()])), ..Default::default() };
    let mut h = demo(services);
    open_print(&mut h);
    // the Print Job panel is at the bottom of the settings column: scroll down to it, as a person would
    let r = h.request("ui.hoverWidget", json!({"id": "combo:printPaper"}), T);
    assert_eq!(r["ok"], true, "{r}");
    for _ in 0..4 {
        h.request("ui.scroll", json!({"dy": -600.0}), T);
        h.settle(T);
    }
    let job = |h: &Headless| h.app.session.print.job.color.clone();
    assert_eq!(job(&h).intent, PrintIntent::Perceptual);
    assert!(!has(&h, "item:printProfile-other"), "the menu is closed");

    click(&mut h, "button:printIntent-1");
    assert_eq!(job(&h).intent, PrintIntent::Relative);
    click(&mut h, "check:printBpc");
    assert!(!job(&h).black_point_compensation);
    click(&mut h, "button:printSharpen-3");
    click(&mut h, "button:printMedia-0");
    assert_eq!((job(&h).sharpen, job(&h).media), (PrintSharpen::High, PrintMedia::Matte));
    click(&mut h, "check:print16bit");
    assert!(!job(&h).sixteen_bit);
    drag(&mut h, "drag:printBrightness", 30.0, 0.0);
    assert!(job(&h).brightness > 0.0, "{}", job(&h).brightness);

    // a printer profile from the picker
    click(&mut h, "combo:printProfile");
    click(&mut h, "item:printProfile-other");
    assert_eq!(job(&h).profile, PrintProfile::Icc { path: icc.clone() });

    // the PDF uses it: converted pages, the profile named as the output intent, 8 bits
    cmd(
        &mut h,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 1, "rows": 1}, "page": {"paper": "4x6"}}, "job": {"ppi": 72.0}}),
    );
    let ids: Vec<u64> = h.app.session.catalog.photos().take(1).map(|p| p.id.0).collect();
    let out = dir.join("colour.pdf");
    cmd(&mut h, "print.exportPdf", json!({"path": out, "ids": ids}));
    let text = String::from_utf8_lossy(&std::fs::read(&out).unwrap()).into_owned();
    assert!(text.contains("/OutputIntents") && text.contains("/BitsPerComponent 8") && text.contains("/ICCBased"));

    // back to Managed by Printer
    click(&mut h, "combo:printProfile");
    click(&mut h, "item:printProfile-adobeRgb");
    assert_eq!(job(&h).profile, PrintProfile::Managed { space: lcc_engine::print_color::ManagedSpace::AdobeRgb });
}

#[test]
fn the_print_preview_does_not_rebuild_thumbnails_every_frame() {
    let mut h = demo(Services { png: None, ..Default::default() });
    let before = h.app.renderer.thumb_jobs_built;
    cmd(&mut h, "dialog.print", json!({}));
    for style in ["contactSheet", "picturePackage", "singleImage"] {
        cmd(&mut h, "print.set", json!({"layout": {"style": style}}));
        if style == "picturePackage" {
            // the same photo in cells of different sizes
            cmd(&mut h, "print.cell.add", json!({"width": 2.0, "height": 2.5}));
            cmd(&mut h, "print.cell.add", json!({"width": 5.0, "height": 7.0}));
        }
        assert!(h.settle(Duration::from_secs(5)), "{style}: the dialog settles");
        let built = h.app.renderer.thumb_jobs_built;
        for _ in 0..60 {
            h.step();
        }
        assert_eq!(h.app.renderer.thumb_jobs_built, built, "{style}: no thumbnail is rebuilt while nothing changes");
        // (a job still running on a slow machine is the one already counted in `built`, finishing;
        // wait for it instead of reading the worker's progress mid-flight)
        assert!(h.settle(Duration::from_secs(5)), "{style}: nothing is left running");
        assert_eq!(h.app.renderer.in_flight(), 0, "{style}");
    }
    assert!(h.app.renderer.thumb_jobs_built - before < 50, "{} jobs", h.app.renderer.thumb_jobs_built - before);
}

#[test]
fn page_options_set_the_page_and_packages_have_cut_guides_instead() {
    let mut h = demo(Services::default());
    open_print(&mut h);
    let scroll = |h: &mut Headless, dy: f32| {
        let r = h.request("ui.hoverWidget", json!({"id": "combo:printPaper"}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..4 {
            h.request("ui.scroll", json!({"dy": dy}), T);
            h.settle(T);
        }
    };
    // the Page panel is below Page Setup: scroll down to it
    scroll(&mut h, -600.0);
    for (id, on) in [("check:printPageNumbers", 0), ("check:printPageInfo", 1), ("check:printCropMarks", 2)] {
        click(&mut h, id);
        let p = &h.app.session.print.page;
        assert_eq!([p.page_numbers, p.page_info, p.crop_marks].iter().filter(|v| **v).count(), on + 1, "{id}");
    }
    let p = &h.app.session.print.page;
    assert!(p.page_numbers && p.page_info && p.crop_marks);
    scroll(&mut h, 600.0);
    click(&mut h, "button:printStyle-1");
    assert!(has(&h, "check:printCropMarks"), "a contact sheet has Page Options");
    // Lightroom's packages have no page numbers or page info (they have Cut Guides, tested below)
    click(&mut h, "button:printStyle-2");
    assert!(!has(&h, "check:printPageNumbers") && !has(&h, "check:printPageInfo"));
    h.app.session.print.page.crop_marks = false;
    // (the pointer stays over the settings column from the last widget interaction, so the wheel
    // keeps scrolling it after combo:printPaper has left the view)
    let wheel = |h: &mut Headless, dy: f32| {
        for _ in 0..4 {
            h.request("ui.scroll", json!({"dy": dy}), T);
            h.settle(T);
        }
    };
    // picture package (style 2) and custom package (style 3) both offer Cut Guides, not the
    // Page Options: only crop marks, reusing page.cropMarks, labelled for packages
    for style in ["button:printStyle-2", "button:printStyle-3"] {
        click(&mut h, style);
        wheel(&mut h, -600.0);
        assert!(has(&h, "check:printCropMarks"), "{style}: a package has Cut Guides");
        assert!(!has(&h, "check:printPageNumbers") && !has(&h, "check:printPageInfo"), "{style}: packages have no page numbers or page info");
        assert!(h.painted_text().iter().any(|t| t == "Cut Guides"), "{style}: the checkbox is labelled Cut Guides: {:?}", h.painted_text());
        assert!(!h.painted_text().iter().any(|t| t == "Crop Marks"), "{style}: packages do not label it Crop Marks: {:?}", h.painted_text());
        assert!(!h.app.session.print.page.crop_marks);
        click(&mut h, "check:printCropMarks");
        assert!(h.app.session.print.page.crop_marks, "{style}: Cut Guides sets crop_marks");
        click(&mut h, "check:printCropMarks");
        assert!(!h.app.session.print.page.crop_marks, "{style}: the reuse keeps reading page.cropMarks");
        wheel(&mut h, 600.0);
    }
}

/// The Print module (issue #853): cell sizes in centimetres, typed sizes, a cell resized with the
/// pointer by its corner, and cells added, moved and removed freely in Custom Package.
#[test]
fn print_module_cells_in_cm_typed_and_resized_with_the_pointer() {
    let mut h = demo(Services::default());
    cmd(&mut h, "module.print", json!({}));
    click(&mut h, "print:style:2");
    cmd(&mut h, "printui.guides", json!({"units": "cm"}));
    let st = cmd(&mut h, "printui.state", json!({}));
    assert_eq!(st["guides"]["units"], "cm");
    let custom = |h: &Headless| match &h.app.print.settings.layout {
        lcc_print::LayoutStyle::CustomPackage { pages } => pages[0].cells.iter().map(|c| c.rect).collect::<Vec<_>>(),
        _ => panic!("not a custom package"),
    };
    // a typed size, in cm by default
    let r = cmd(&mut h, "printui.cell", json!({"index": 0, "w": 10, "h": 15, "x": 2, "y": 2}));
    assert_eq!(r["index"], 0);
    let c = custom(&h)[0];
    assert!((c.w - 10.0 / 2.54 * 72.0).abs() < 0.1 && (c.h - 15.0 / 2.54 * 72.0).abs() < 0.1, "{c:?}");
    // selecting the cell shows its corner handle; dragging it makes the cell larger
    click(&mut h, "print:cell:0");
    assert_eq!(h.app.print.selected, Some(0));
    assert!(has(&h, "print:resize:0"));
    let before = custom(&h)[0];
    drag(&mut h, "print:resize:0", 40.0, 30.0);
    let after = custom(&h)[0];
    assert!(after.w > before.w + 5.0 && after.h > before.h + 5.0, "{before:?} → {after:?}");
    assert_eq!((after.x, after.y), (before.x, before.y), "resizing keeps the corner it grows from");
    // add a second cell, then remove the first
    cmd(&mut h, "printui.cell", json!({"w": 5, "h": 5, "unit": "in"}));
    assert_eq!(custom(&h).len(), 2);
    assert!((custom(&h)[1].w - 360.0).abs() < 0.1);
    cmd(&mut h, "printui.cell", json!({"index": 0, "remove": true}));
    assert_eq!(custom(&h).len(), 1);
}

/// Bad `printui.cell` arguments are errors, never a panic or a broken layout.
#[test]
fn print_cell_rejects_hostile_arguments() {
    let mut h = demo(Services::default());
    cmd(&mut h, "printui.template", json!({"name": "2×2 Cells"}));
    let bad = |h: &mut Headless, p: Value| {
        let r = h.request("engine.execute", json!({"command": "printui.cell", "params": p}), T);
        assert_eq!(r["ok"], false, "{p}: {r}");
    };
    bad(&mut h, json!({"w": 2, "h": 2})); // a grid has no free cells
    cmd(&mut h, "module.print", json!({}));
    click(&mut h, "print:style:1");
    assert!(matches!(h.app.print.settings.layout, lcc_print::LayoutStyle::PicturePackage(_)));
    cmd(&mut h, "printui.cell", json!({"index": 0, "remove": true}));
    for p in [
        json!({"w": 2}),
        json!({"w": -1, "h": 2}),
        json!({"w": 1e30, "h": 2}),
        json!({"w": "x", "h": 2}),
        json!({"w": 2, "h": 2, "unit": "furlong"}),
        json!({"w": 2, "h": 2, "x": 1}),
        json!({"index": 99, "w": 2}),
        json!({"index": -1, "remove": true}),
        json!({"index": 99, "remove": true}),
    ] {
        bad(&mut h, p);
    }
    // the package panel lists the cells with typed sizes
    cmd(&mut h, "printui.cell", json!({"w": 10, "h": 15, "unit": "cm"}));
    assert!(has(&h, "field:print:cellW:0") || has(&h, "print:cellW:0") || has(&h, "print:removeCell:0"));
    cmd(&mut h, "printui.cell", json!({"index": 0, "remove": true}));
    for _ in 0..64 {
        cmd(&mut h, "printui.cell", json!({"w": 1, "h": 1, "unit": "in"}));
    }
    bad(&mut h, json!({"w": 1, "h": 1}));
}

/// The Print module prints the filmstrip's selection: the one photo chosen there is what the
/// preview shows and the job takes, and only an empty selection falls back to the whole filmstrip.
#[test]
fn print_module_follows_the_filmstrip_selection() {
    let mut h = demo(Services::default());
    let ids: Vec<u64> = h.app.session.visible_cloned().iter().take(3).map(|p| p.0).collect();
    cmd(&mut h, "library.select", json!({"ids": [ids[0]]}));
    cmd(&mut h, "module.print", json!({}));
    assert!(h.settle(T));
    assert_eq!(crate::print_ui::job_photos(&mut h.app, false).iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[0]]);
    // choosing another photo in the filmstrip moves the print to it
    cmd(&mut h, "library.select", json!({"ids": [ids[1]]}));
    assert!(h.settle(T));
    assert_eq!(crate::print_ui::job_photos(&mut h.app, false).iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[1]]);
    // several selected print those, in the selection's order
    cmd(&mut h, "library.select", json!({"ids": [ids[0], ids[2]]}));
    assert_eq!(crate::print_ui::job_photos(&mut h.app, false).iter().map(|p| p.0).collect::<Vec<_>>(), vec![ids[0], ids[2]]);
    // nothing selected: the whole filmstrip
    cmd(&mut h, "library.select", json!({"ids": []}));
    assert!(crate::print_ui::job_photos(&mut h.app, false).len() >= ids.len());
}
