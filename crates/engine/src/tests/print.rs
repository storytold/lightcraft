//! The `print.*` commands: state, cells, templates and the PDF.

use lightcraft_catalog::MemStore;
use serde_json::{Value, json};

use crate::Session;
use crate::library::LibraryStores;

fn run(s: &mut Session, id: &str, p: Value) -> Value {
    s.execute(id, &p).unwrap_or_else(|e| panic!("{id} {p}: {e}"))
}

fn fails(s: &mut Session, id: &str, p: Value) -> String {
    s.execute(id, &p).expect_err(&format!("{id} {p} should fail")).to_string()
}

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lightcraft-print-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn package(s: &mut Session) {
    run(s, "print.set", json!({"layout": {"style": "customPackage", "page": {"paper": "letter"}}}));
}

#[test]
fn get_set_and_reset() {
    let mut s = Session::with_demo();
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!(g["settings"]["layout"]["style"], "singleImage");
    assert_eq!(g["pageWidth"], 8.5);
    assert_eq!(g["printable"]["x"], 0.25);
    assert_eq!(g["settings"]["job"]["ppi"], 240.0);
    let papers = g["papers"].as_array().unwrap();
    assert!(papers.iter().any(|p| p["name"] == "a4"));
    // The Japanese photo and postcard papers are listed with their metric sizes in inches.
    let paper = |name: &str| papers.iter().find(|p| p["name"] == name).cloned().unwrap_or_else(|| panic!("{name}"));
    let l = paper("l");
    assert!((l["width"].as_f64().unwrap() - 89.0 / 25.4).abs() < 1e-9 && (l["height"].as_f64().unwrap() - 127.0 / 25.4).abs() < 1e-9);
    assert!(papers.iter().any(|p| p["name"] == "postcard") && papers.iter().any(|p| p["name"] == "6cut"));
    assert_eq!(g["gridCells"].as_array().unwrap().len(), 1);

    let g = run(
        &mut s,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 2, "rows": 3}, "page": {"paper": "a4", "orientation": "landscape", "margins": {"left": 1.0}}}, "image": {"zoomToFill": true}, "job": {"ppi": 300.0}}),
    );
    assert_eq!(g["settings"]["layout"]["page"]["margins"]["left"], 1.0);
    assert_eq!(g["settings"]["layout"]["page"]["margins"]["top"], 0.25, "a patch merges");
    assert_eq!(g["gridCells"].as_array().unwrap().len(), 6);
    assert!((g["pageWidth"].as_f64().unwrap() - 11.693).abs() < 1e-3);
    assert_eq!(g["settings"]["image"]["zoomToFill"], true);
    // the page count follows the photos: 6 cells per page
    let g = run(&mut s, "print.get", json!({"ids": vec![1; 7]}));
    assert_eq!(g["pageCount"], 2);

    let g = run(&mut s, "print.reset", json!({}));
    assert_eq!(g["settings"]["job"]["ppi"], 240.0);
}

#[test]
fn bad_settings_are_refused_and_change_nothing() {
    let mut s = Session::with_demo();
    let before = run(&mut s, "print.get", json!({}));
    for p in [
        json!({"typo": 1}),
        json!({"layout": {"typo": 1}}),
        json!({"layout": {"style": "poster"}}),
        json!({"layout": {"cells": []}}),
        json!({"layout": {"pages": 3}}),
        json!({"layout": {"page": {"paper": "poster"}}}),
        json!({"layout": {"page": {"paper": "custom", "customWidth": -1}}}),
        json!({"layout": {"page": {"margins": {"top": 1e9}}}}),
        json!({"layout": {"page": {"margins": {"top": 6.0, "bottom": 5.0}}}}),
        json!({"layout": {"grid": {"columns": 0}}}),
        json!({"layout": {"grid": {"columns": 9999}}}),
        json!({"layout": {"spacing": -1}}),
        json!({"image": {"borderWidth": 99}}),
        json!({"image": {"strokeWidthPt": -2}}),
        json!({"image": {"borderColor": [1, 2]}}),
        json!({"image": {"zoomToFill": "yes"}}),
        json!({"guides": {"gridSize": 0}}),
        json!({"guides": {"units": "furlong"}}),
        json!({"job": {"ppi": 1e30}}),
        json!({"job": {"ppi": 10}}),
        json!({"job": null}),
    ] {
        fails(&mut s, "print.set", p);
    }
    assert_eq!(run(&mut s, "print.get", json!({})), before);
    assert!(s.execute("print.set", &json!([1])).is_err());
}

#[test]
fn cells_are_commands() {
    let mut s = Session::with_demo();
    // grid styles have no cells
    fails(&mut s, "print.cell.add", json!({"width": 2, "height": 2}));
    package(&mut s);
    let a = run(&mut s, "print.cell.add", json!({"width": 3.0, "height": 2.0}))["id"].as_u64().unwrap();
    let b = run(&mut s, "print.cell.add", json!({"width": 3.0, "height": 2.0, "x": 5.0, "y": 5.0}))["id"].as_u64().unwrap();
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!(g["settings"]["layout"]["cells"].as_array().unwrap().len(), 2);
    assert_eq!(g["overlaps"], false);
    // move, clamped inside the margins
    let r = run(&mut s, "print.cell.move", json!({"id": a, "x": 99.0, "y": 0.0}));
    assert_eq!(r["rect"]["x"], 8.25 - 3.0);
    assert_eq!(r["rect"]["y"], 0.25);
    // resize keeps what is not given
    let r = run(&mut s, "print.cell.resize", json!({"id": b, "width": 4.0}));
    assert_eq!((r["rect"]["w"].as_f64().unwrap(), r["rect"]["h"].as_f64().unwrap()), (4.0, 2.0));
    // snapping to the other cell's edge
    let ra = run(&mut s, "print.get", json!({}))["settings"]["layout"]["cells"][0]["rect"].clone();
    let left = ra["x"].as_f64().unwrap();
    let r = run(&mut s, "print.cell.move", json!({"id": b, "x": left - 4.0 - 0.04, "y": 5.0, "snap": true}));
    assert!((r["rect"]["x"].as_f64().unwrap() - (left - 4.0)).abs() < 1e-9, "{r}");
    let r = run(&mut s, "print.cell.move", json!({"id": b, "x": left - 4.0 - 0.04, "y": 5.0}));
    assert!((r["rect"]["x"].as_f64().unwrap() - (left - 4.04)).abs() < 1e-9, "no snap unless asked: {r}");
    run(&mut s, "print.set", json!({"guides": {"snap": "off"}}));
    let r = run(&mut s, "print.cell.move", json!({"id": b, "x": left - 4.0 - 0.04, "y": 5.0, "snap": true}));
    assert!((r["rect"]["x"].as_f64().unwrap() - (left - 4.04)).abs() < 1e-9, "snap off");
    // stacking
    let r = run(&mut s, "print.cell.stack", json!({"id": a, "order": "toFront"}));
    assert_eq!(r["order"], json!([b, a]));
    fails(&mut s, "print.cell.stack", json!({"id": a, "order": "sideways"}));
    // photos and pages
    let photo = s.catalog.photos().next().unwrap().id.0;
    run(&mut s, "print.cell.photo", json!({"id": a, "photo": photo}));
    fails(&mut s, "print.cell.photo", json!({"id": a, "photo": 999_999}));
    run(&mut s, "print.layout.newPage", json!({}));
    run(&mut s, "print.cell.page", json!({"id": b, "page": 1}));
    fails(&mut s, "print.cell.page", json!({"id": b, "page": 7}));
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!(g["pageCount"], 2);
    // delete, clear
    run(&mut s, "print.cell.delete", json!({"id": b}));
    fails(&mut s, "print.cell.delete", json!({"id": b}));
    fails(&mut s, "print.cell.move", json!({"id": 99, "x": 0, "y": 0}));
    fails(&mut s, "print.cell.move", json!({"x": 0, "y": 0}));
    run(&mut s, "print.layout.clear", json!({}));
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!((g["settings"]["layout"]["cells"].as_array().unwrap().len(), g["pageCount"].as_u64()), (0, Some(1)));
}

#[test]
fn hostile_cell_numbers_are_refused() {
    let mut s = Session::with_demo();
    package(&mut s);
    for (w, h) in [(0.0, 2.0), (-1.0, 2.0), (1e12, 1e12), (2.0, 1e-9), (50.0, 50.0)] {
        fails(&mut s, "print.cell.add", json!({"width": w, "height": h}));
    }
    fails(&mut s, "print.cell.add", json!({"width": "wide", "height": 2}));
    fails(&mut s, "print.cell.add", json!({"width": 2}));
    let id = run(&mut s, "print.cell.add", json!({"width": 2, "height": 2}))["id"].as_u64().unwrap();
    fails(&mut s, "print.cell.move", json!({"id": id, "x": "left", "y": 0}));
    fails(&mut s, "print.cell.resize", json!({"id": id, "width": -4}));
    fails(&mut s, "print.cell.resize", json!({"id": id, "width": "x"}));
    fails(&mut s, "print.cell.move", json!({"id": id, "x": 1, "y": 1, "snap": "yes"}));
    fails(&mut s, "print.cell.move", json!({"id": u64::MAX, "x": 1, "y": 1}));
    let r = run(&mut s, "print.cell.move", json!({"id": id, "x": -1e300, "y": 1e300}));
    assert_eq!(r["rect"]["x"], 0.25);
}

#[test]
fn auto_layout_fills_pages_without_overlap() {
    let mut s = Session::with_demo();
    run(&mut s, "print.set", json!({"layout": {"style": "picturePackage"}}));
    for (w, h) in [(4.0, 6.0), (4.0, 6.0), (5.0, 7.0), (2.5, 3.5), (2.5, 3.5), (2.5, 3.5), (2.5, 3.5), (8.0, 10.0)] {
        run(&mut s, "print.cell.add", json!({"width": w, "height": h}));
    }
    let r = run(&mut s, "print.layout.autoLayout", json!({}));
    assert!(r["pages"].as_u64().unwrap() >= 2);
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!(g["overlaps"], false);
    let again = run(&mut s, "print.layout.autoLayout", json!({}));
    assert_eq!(again["cells"], r["cells"], "deterministic");
    // an oversize cell that arrived by a paper change is pulled in, not left outside the page
    run(&mut s, "print.set", json!({"layout": {"page": {"paper": "4x6"}}}));
    let g = run(&mut s, "print.get", json!({}));
    for c in g["settings"]["layout"]["cells"].as_array().unwrap() {
        assert!(c["rect"]["x"].as_f64().unwrap() + c["rect"]["w"].as_f64().unwrap() <= 4.0 - 0.25 + 1e-9);
    }
}

/// The first page's MediaBox width and height in points.
fn media_box(pdf: &[u8]) -> (f64, f64) {
    let text = String::from_utf8_lossy(pdf);
    let at = text.find("/MediaBox [").expect("a MediaBox") + "/MediaBox [".len();
    let nums: Vec<f64> = text[at..].split(']').next().unwrap().split_whitespace().map(|v| v.parse().unwrap()).collect();
    (nums[2], nums[3])
}

fn temp_pdf(name: &str) -> String {
    std::env::temp_dir().join(format!("lightcraft-print-{name}-{}.pdf", std::process::id())).to_string_lossy().to_string()
}

#[test]
fn export_pdf_prints_the_current_settings() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(3).map(|p| p.id.0).collect();
    run(
        &mut s,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 1, "rows": 2}, "page": {"paper": "a5"}}, "job": {"ppi": 100.0}}),
    );
    let path = temp_pdf("export");
    let r = run(&mut s, "print.exportPdf", json!({"path": path, "ids": ids}));
    assert_eq!((r["pages"].as_u64(), r["photos"].as_u64()), (Some(2), Some(3)));
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, r["bytes"].as_u64().unwrap());
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("%PDF-") && text.contains("/Count 2"));
    // A5 is 148 x 210 mm = 419.53 x 595.28 pt, rounded to whole pixels at 100 ppi (583 x 827)
    let (w, h) = media_box(&bytes);
    assert!((w - 419.53).abs() < 1.0 && (h - 595.28).abs() < 1.0, "A5, got {w} x {h} pt");
    // the row in the activity stack is gone when the print is done
    assert!(s.activity.list().is_empty());
    fails(&mut s, "print.exportPdf", json!({}));
    fails(&mut s, "print.exportPdf", json!({"path": path, "typo": 1}));
    fails(&mut s, "print.exportPdf", json!({"path": path, "ids": ["x"]}));
    fails(&mut s, "print.exportPdf", json!({"path": path, "ids": [999_999]}));
    // a package with no cells has nothing to print
    run(&mut s, "print.set", json!({"layout": {"style": "picturePackage"}}));
    fails(&mut s, "print.exportPdf", json!({"path": path, "ids": ids}));
}

/// The Japanese photo and postcard papers reach the PDF page at their metric sizes: はがき is
/// 100 × 148 mm (283.46 × 419.53 pt) and L is 89 × 127 mm (252.28 × 359.99 pt). 254 ppi is exactly
/// ten pixels per millimetre, so the page pixels are whole millimetres and the MediaBox is exact.
#[test]
fn export_pdf_uses_the_japanese_paper_sizes() {
    let mut s = Session::with_demo();
    let id = s.catalog.photos().next().map(|p| p.id.0).unwrap();
    for (paper, w_pt, h_pt) in [("postcard", 283.46, 419.53), ("はがき", 283.46, 419.53), ("l", 252.28, 359.99)] {
        run(&mut s, "print.set", json!({"layout": {"style": "singleImage", "page": {"paper": paper}}, "job": {"ppi": 254.0}}));
        let path = temp_pdf(paper);
        run(&mut s, "print.exportPdf", json!({"path": path, "ids": [id]}));
        let bytes = std::fs::read(&path).unwrap();
        let (w, h) = media_box(&bytes);
        assert!((w - w_pt).abs() < 0.1 && (h - h_pt).abs() < 0.1, "{paper}: {w} x {h} pt, wanted {w_pt} x {h_pt}");
    }
    // The customary names are accepted and the page size follows them; an unknown name is refused.
    let g = run(&mut s, "print.set", json!({"layout": {"page": {"paper": "はがき"}}}));
    assert!((g["pageWidth"].as_f64().unwrap() - 100.0 / 25.4).abs() < 1e-9);
    assert!((g["pageHeight"].as_f64().unwrap() - 148.0 / 25.4).abs() < 1e-9);
    fails(&mut s, "print.set", json!({"layout": {"page": {"paper": "poster"}}}));
}

#[test]
fn export_pdf_with_a_printer_profile() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(1).map(|p| p.id.0).collect();
    let dir = std::env::temp_dir().join(format!("lightcraft-print-icc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let icc = dir.join("Fine Art Matte.icc");
    std::fs::write(&icc, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::AdobeRgb)).unwrap();
    let color = json!({"profile": {"kind": "icc", "path": icc}, "intent": "relative", "sharpen": "high", "media": "matte", "brightness": 10});
    run(
        &mut s,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 1, "rows": 1}, "page": {"paper": "4x6"}}, "job": {"ppi": 72.0, "color": color}}),
    );
    assert_eq!(run(&mut s, "print.get", json!({}))["settings"]["job"]["color"]["media"], "matte");
    let path = temp_pdf("icc");
    run(&mut s, "print.exportPdf", json!({"path": path, "ids": ids}));
    let text = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
    assert!(text.contains("/OutputIntents") && text.contains("/ICCBased") && text.contains("/BitsPerComponent 16"));
    // out-of-range adjustments are refused when set; a missing profile fails before rendering
    fails(&mut s, "print.set", json!({"job": {"color": {"contrast": 150}}}));
    run(&mut s, "print.set", json!({"job": {"color": {"profile": {"kind": "icc", "path": dir.join("gone.icc")}}}}));
    let err = fails(&mut s, "print.exportPdf", json!({"path": path, "ids": ids}));
    assert!(err.contains("gone.icc"), "{err}");
    assert!(s.activity.list().is_empty(), "nothing started");
}

#[test]
fn custom_package_prints_each_cells_photo() {
    let mut s = Session::with_demo();
    let ids: Vec<u64> = s.catalog.photos().take(2).map(|p| p.id.0).collect();
    run(&mut s, "print.set", json!({"layout": {"style": "customPackage", "page": {"paper": "4x6"}}, "job": {"ppi": 72.0}}));
    let a = run(&mut s, "print.cell.add", json!({"width": 3.0, "height": 2.0}))["id"].as_u64().unwrap();
    let b = run(&mut s, "print.cell.add", json!({"width": 3.0, "height": 2.0}))["id"].as_u64().unwrap();
    run(&mut s, "print.cell.photo", json!({"id": a, "photo": ids[0]}));
    run(&mut s, "print.cell.photo", json!({"id": b, "photo": ids[1]}));
    let r = run(&mut s, "print.exportPdf", json!({"path": temp_pdf("custom"), "ids": [ids[0]]}));
    assert_eq!(r["pages"], 1);
}

#[test]
fn templates_save_apply_delete_and_persist() {
    let files = MemStore::new();
    let open = |files: &MemStore| {
        let mut s = Session::with_demo();
        let stores = LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
        s.open_library_in(stores, false).unwrap();
        s
    };
    let mut s = open(&files);
    run(
        &mut s,
        "print.set",
        json!({"layout": {"style": "contactSheet", "grid": {"columns": 4, "rows": 5}}, "page": {"background": [10, 20, 30]}, "guides": {"gridSize": 1.0}}),
    );
    let r = run(&mut s, "print.template.save", json!({"name": "Proofs"}));
    assert_eq!(r["templates"][0]["name"], "Proofs");
    assert!(r["templates"][0]["settings"].get("guides").is_none());
    run(&mut s, "print.template.save", json!({"name": "proofs"}));
    assert_eq!(run(&mut s, "print.template.list", json!({}))["templates"].as_array().unwrap().len(), 1, "the same name in another case replaces");

    run(&mut s, "print.reset", json!({}));
    run(&mut s, "print.set", json!({"guides": {"gridSize": 0.5}}));
    let g = run(&mut s, "print.template.apply", json!({"name": "PROOFS"}));
    assert_eq!(g["settings"]["layout"]["grid"]["columns"], 4);
    assert_eq!(g["settings"]["page"]["background"], json!([10, 20, 30]));
    assert_eq!(g["settings"]["guides"]["gridSize"], 0.5, "guides stay as they were");

    // it survives reopening the library
    let mut again = open(&files);
    assert_eq!(run(&mut again, "print.template.list", json!({}))["templates"][0]["name"], "proofs");
    run(&mut again, "print.template.apply", json!({"name": "Proofs"}));

    fails(&mut s, "print.template.save", json!({"name": "  "}));
    fails(&mut s, "print.template.save", json!({}));
    fails(&mut s, "print.template.apply", json!({"name": "Nope"}));
    fails(&mut s, "print.template.delete", json!({"name": "Nope"}));
    run(&mut s, "print.template.delete", json!({"name": "proofs"}));
    assert!(run(&mut s, "print.template.list", json!({}))["templates"].as_array().unwrap().is_empty());
    assert!(run(&mut open(&files), "print.template.list", json!({}))["templates"].as_array().unwrap().is_empty());
}

#[test]
fn templates_export_to_a_file_and_import_from_one() {
    let files = MemStore::new();
    let open = |files: &MemStore| {
        let mut s = Session::with_demo();
        let stores = LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
        s.open_library_in(stores, false).unwrap();
        s
    };
    let scratch = Scratch::new("templates");
    let dir = &scratch.0;
    let mut s = open(&files);
    run(&mut s, "print.set", json!({"layout": {"style": "contactSheet", "grid": {"columns": 3, "rows": 4}}, "page": {"background": [5, 6, 7]}}));
    run(&mut s, "print.template.save", json!({"name": "Proofs"}));

    // the extension is added when the path has none
    let r = run(&mut s, "print.template.export", json!({"name": "proofs", "path": dir.join("proofs").to_string_lossy()}));
    assert_eq!(r["name"], "Proofs");
    let file = dir.join("proofs.lcprint");
    assert_eq!(r["path"], file.to_string_lossy().as_ref());
    let doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!((doc["format"].as_str(), doc["version"].as_u64(), doc["name"].as_str()), (Some("lightcraft.printTemplate"), Some(1), Some("Proofs")));

    // into the same library: the name is taken, so the copy is renamed
    let r = run(&mut s, "print.template.import", json!({"path": file.to_string_lossy()}));
    assert_eq!((r["name"].as_str(), r["renamed"].as_bool()), (Some("Proofs (2)"), Some(true)), "{r}");
    assert_eq!(r["templates"].as_array().unwrap().len(), 2);
    assert_eq!(r["templates"][1]["settings"], r["templates"][0]["settings"]);

    // into another library, under its own name, and it is kept there
    let other = MemStore::new();
    let mut b = open(&other);
    let r = run(&mut b, "print.template.import", json!({"path": file.to_string_lossy()}));
    assert_eq!((r["name"].as_str(), r["renamed"].as_bool()), (Some("Proofs"), Some(false)));
    let g = run(&mut open(&other), "print.template.apply", json!({"name": "Proofs"}));
    assert_eq!(g["settings"]["layout"]["grid"]["columns"], 3);
    assert_eq!(g["settings"]["page"]["background"], json!([5, 6, 7]));

    // refusals leave the templates as they were
    let bad_file = dir.join("bad.lcprint");
    std::fs::write(&bad_file, br#"{"format": "lightcraft.printTemplate", "version": 1, "name": "Bad", "settings": {"layout": {"pages": 0}}}"#)
        .unwrap();
    let huge = dir.join("huge.lcprint");
    std::fs::write(&huge, vec![b' '; crate::print::template::MAX_FILE as usize + 1]).unwrap();
    for (p, why) in [
        (json!({}), "missing `path`"),
        (json!({"path": "  "}), "missing `path`"),
        (json!({"path": dir.join("absent.lcprint").to_string_lossy()}), "absent.lcprint"),
        (json!({"path": dir.to_string_lossy()}), "not a file"),
        (json!({"path": huge.to_string_lossy()}), "too large"),
        (json!({"path": bad_file.to_string_lossy()}), "template `Bad`"),
    ] {
        let e = fails(&mut s, "print.template.import", p.clone());
        assert!(e.contains(why), "{p}: {e}");
    }
    assert_eq!(run(&mut s, "print.template.list", json!({}))["templates"].as_array().unwrap().len(), 2);
    assert!(fails(&mut s, "print.template.export", json!({"name": "Nope", "path": dir.join("x").to_string_lossy()})).contains("no print template"));
    assert!(fails(&mut s, "print.template.export", json!({"name": "Proofs"})).contains("missing `path`"));
    assert!(fails(&mut s, "print.template.export", json!({"path": dir.join("x").to_string_lossy()})).contains("missing `name`"));
    assert!(!dir.join("x.lcprint").exists());

    // a full list takes no more
    while s.print_templates.len() < crate::print::template::MAX_TEMPLATES {
        let n = s.print_templates.len();
        run(&mut s, "print.template.save", json!({"name": format!("T{n}")}));
    }
    assert!(fails(&mut s, "print.template.import", json!({"path": file.to_string_lossy()})).contains("at most"));
}

/// The names of the folders `print.template.list` gives, User Templates first.
fn folder_names(r: &Value) -> Vec<String> {
    r["folders"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap().to_string()).collect()
}

/// Each template's name and folder, in the order `print.template.list` gives them.
fn template_folders(r: &Value) -> Vec<(String, String)> {
    r["templates"].as_array().unwrap().iter().map(|t| (t["name"].as_str().unwrap().into(), t["folder"].as_str().unwrap().into())).collect()
}

fn pair(name: &str, folder: &str) -> (String, String) {
    (name.into(), folder.into())
}

#[test]
fn template_folders_hold_templates_and_survive_reopening() {
    let files = MemStore::new();
    let open = |files: &MemStore| {
        let mut s = Session::with_demo();
        let stores = LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
        s.open_library_in(stores, false).unwrap();
        s
    };
    let mut s = open(&files);
    let r = run(&mut s, "print.template.list", json!({}));
    assert_eq!(r["folders"], json!([{"name": "User Templates", "default": true, "templates": 0}]));

    // a folder is kept while it is empty, and the folders are listed by name after User Templates
    let r = run(&mut s, "print.template.folder.create", json!({"name": " Weddings "}));
    assert_eq!(r["name"], "Weddings");
    run(&mut s, "print.template.folder.create", json!({"name": "Proofs"}));
    let r = run(&mut open(&files), "print.template.list", json!({}));
    assert_eq!(folder_names(&r), ["User Templates", "Proofs", "Weddings"], "{r}");

    // a template is saved into a folder, or into User Templates when none is named
    run(&mut s, "print.set", json!({"layout": {"style": "contactSheet", "grid": {"columns": 4, "rows": 5}}}));
    run(&mut s, "print.template.save", json!({"name": "Sheet", "folder": "weddings"}));
    let r = run(&mut s, "print.template.save", json!({"name": "Plain"}));
    assert_eq!(template_folders(&r), [pair("Sheet", "Weddings"), pair("Plain", "User Templates")]);
    assert_eq!(r["folders"][2], json!({"name": "Weddings", "default": false, "templates": 1}));
    // saving over a name keeps its folder unless the call names another
    run(&mut s, "print.set", json!({"layout": {"grid": {"columns": 6}}}));
    let r = run(&mut s, "print.template.save", json!({"name": "sheet"}));
    assert_eq!(template_folders(&r), [pair("sheet", "Weddings"), pair("Plain", "User Templates")]);
    let r = run(&mut s, "print.template.save", json!({"name": "Plain", "folder": "Proofs"}));
    assert_eq!(template_folders(&r)[1], pair("Plain", "Proofs"));

    // a move changes only the folder; a move to where it is already does nothing
    let settings = r["templates"][0]["settings"].clone();
    let r = run(&mut s, "print.template.move", json!({"name": "SHEET", "folder": "Proofs"}));
    assert_eq!((r["moved"].as_bool(), template_folders(&r)[0].clone()), (Some(true), pair("sheet", "Proofs")));
    assert_eq!(r["templates"][0]["settings"], settings);
    let r = run(&mut s, "print.template.move", json!({"name": "Sheet", "folder": "proofs"}));
    assert_eq!(r["moved"], false);
    let r = run(&mut s, "print.template.move", json!({"name": "Plain", "folder": "user templates"}));
    assert_eq!(template_folders(&r)[1], pair("Plain", "User Templates"));
    run(&mut s, "print.template.move", json!({"name": "Plain", "folder": "Weddings"}));

    // a rename takes the folder's templates along; a change of case is a rename too
    let r = run(&mut s, "print.template.folder.rename", json!({"name": "proofs", "to": "Contact Sheets"}));
    assert_eq!(folder_names(&r), ["User Templates", "Contact Sheets", "Weddings"]);
    assert_eq!(template_folders(&r)[0], pair("sheet", "Contact Sheets"));
    let r = run(&mut s, "print.template.folder.rename", json!({"name": "contact sheets", "to": "contact sheets"}));
    assert_eq!(template_folders(&r)[0], pair("sheet", "contact sheets"));

    // the folders and where each template is survive reopening the library
    let mut again = open(&files);
    let r = run(&mut again, "print.template.list", json!({}));
    assert_eq!(folder_names(&r), ["User Templates", "contact sheets", "Weddings"]);
    assert_eq!(template_folders(&r), [pair("sheet", "contact sheets"), pair("Plain", "Weddings")]);
    run(&mut again, "print.template.apply", json!({"name": "Sheet"}));
    assert_eq!(again.print.layout.grid.columns, 6);

    // deleting a folder keeps its templates: they go to User Templates
    let r = run(&mut s, "print.template.folder.delete", json!({"name": "WEDDINGS"}));
    assert_eq!((r["deleted"].as_str(), r["moved"].as_u64()), (Some("Weddings"), Some(1)));
    assert_eq!(folder_names(&r), ["User Templates", "contact sheets"]);
    assert_eq!(template_folders(&r), [pair("sheet", "contact sheets"), pair("Plain", "User Templates")]);
    let r = run(&mut open(&files), "print.template.list", json!({}));
    assert_eq!(template_folders(&r)[1], pair("Plain", "User Templates"));

    // refusals change nothing
    let before = run(&mut s, "print.template.list", json!({}));
    for (id, p, why) in [
        ("print.template.folder.create", json!({}), "missing `name`"),
        ("print.template.folder.create", json!({"name": "  "}), "folder name"),
        ("print.template.folder.create", json!({"name": "a\tb"}), "folder name"),
        ("print.template.folder.create", json!({"name": "x".repeat(81)}), "folder name"),
        ("print.template.folder.create", json!({"name": "user templates"}), "always there"),
        ("print.template.folder.create", json!({"name": "Contact Sheets"}), "already a folder"),
        ("print.template.folder.rename", json!({"name": "User Templates", "to": "Mine"}), "can't be renamed"),
        ("print.template.folder.rename", json!({"name": "Nope", "to": "Mine"}), "no print template folder"),
        ("print.template.folder.rename", json!({"name": "contact sheets"}), "missing `to`"),
        ("print.template.folder.rename", json!({"name": "contact sheets", "to": "User Templates"}), "always there"),
        ("print.template.folder.delete", json!({"name": "User Templates"}), "can't be renamed or deleted"),
        ("print.template.folder.delete", json!({"name": "Nope"}), "no print template folder"),
        ("print.template.move", json!({"name": "Sheet"}), "missing `folder`"),
        ("print.template.move", json!({"name": "Sheet", "folder": "Nope"}), "no print template folder"),
        ("print.template.move", json!({"name": "Sheet", "folder": 3}), "`folder` must be"),
        ("print.template.move", json!({"name": "Nope", "folder": "User Templates"}), "no print template `Nope`"),
        ("print.template.save", json!({"name": "New", "folder": "Nope"}), "no print template folder"),
        ("print.template.import", json!({"path": "x.lcprint", "folder": "Nope"}), "no print template folder"),
    ] {
        let e = fails(&mut s, id, p.clone());
        assert!(e.contains(why), "{id} {p}: {e}");
    }
    run(&mut s, "print.template.folder.create", json!({"name": "Second"}));
    assert!(fails(&mut s, "print.template.folder.rename", json!({"name": "second", "to": "CONTACT SHEETS"})).contains("already a folder"));
    run(&mut s, "print.template.folder.delete", json!({"name": "Second"}));
    assert_eq!(run(&mut s, "print.template.list", json!({})), before);

    // a full list of folders takes no more
    while s.print_template_folders.len() < crate::print::template::MAX_FOLDERS {
        let n = s.print_template_folders.len();
        run(&mut s, "print.template.folder.create", json!({"name": format!("F{n}")}));
    }
    assert!(fails(&mut s, "print.template.folder.create", json!({"name": "One more"})).contains("at most"));
}

#[test]
fn templates_import_into_a_folder_and_export_without_it() {
    let mut s = Session::with_demo();
    let scratch = Scratch::new("folders");
    let file = scratch.0.join("proofs.lcprint");
    run(&mut s, "print.template.folder.create", json!({"name": "Proofs"}));
    run(&mut s, "print.template.save", json!({"name": "Sheet", "folder": "Proofs"}));
    run(&mut s, "print.template.export", json!({"name": "Sheet", "path": file.to_string_lossy()}));
    let doc: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let mut keys: Vec<&String> = doc.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(keys, ["format", "name", "settings", "version"], "a .lcprint file stays version 1, without the folder");
    let r = run(&mut s, "print.template.import", json!({"path": file.to_string_lossy()}));
    assert_eq!(template_folders(&r)[1], pair("Sheet (2)", "User Templates"));
    let r = run(&mut s, "print.template.import", json!({"path": file.to_string_lossy(), "folder": "proofs"}));
    assert_eq!(template_folders(&r)[2], pair("Sheet (3)", "Proofs"));
}

#[test]
fn templates_from_before_folders_are_in_user_templates() {
    let files = MemStore::new();
    let old = json!({"printTemplates": [
        {"name": "Old", "settings": {"layout": {"style": "contactSheet"}}},
        {"name": "Kept", "folder": "Family", "settings": {}},
        {"name": "Spelled", "folder": " family ", "settings": {}},
        {"name": "Bad", "folder": "a\nb", "settings": {}},
        {"name": "Reserved", "folder": "USER TEMPLATES", "settings": {}}
    ], "printTemplateFolders": ["Empty", "", "empty", "User Templates", "Family"]});
    files.set("prefs.json", serde_json::to_vec(&old).unwrap());
    let mut s = Session::with_demo();
    let stores = LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
    s.open_library_in(stores, false).unwrap();
    assert!(s.take_library_warnings().is_empty());
    let r = run(&mut s, "print.template.list", json!({}));
    assert_eq!(folder_names(&r), ["User Templates", "Empty", "Family"]);
    assert_eq!(
        template_folders(&r),
        [
            pair("Old", "User Templates"),
            pair("Kept", "Family"),
            pair("Spelled", "Family"),
            pair("Bad", "User Templates"),
            pair("Reserved", "User Templates")
        ]
    );
    run(&mut s, "print.template.apply", json!({"name": "Old"}));
}

/// A library whose `prefs.json` can't be read, so saving the preferences fails.
#[derive(Clone, Default)]
struct PrefsUnreadable(MemStore);

impl lightcraft_catalog::Store for PrefsUnreadable {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        if name == "prefs.json" {
            return Err(std::io::Error::other("locked by another process"));
        }
        self.0.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.0.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.0.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.0.truncate(name, len)
    }
    fn describe(&self) -> String {
        "prefs unreadable".into()
    }
}

#[test]
fn template_changes_that_cant_be_saved_are_undone() {
    let mut s = Session::with_demo();
    let stores =
        LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(PrefsUnreadable::default()), on_disk: false };
    s.open_library_in(stores, false).unwrap();
    s.print_templates.push(crate::print::template::PrintTemplate::new("Proofs", &s.print).unwrap());
    s.print_template_folders.push("Family".into());
    let names = |s: &Session| {
        (
            s.print_templates.iter().map(|t| (t.name.clone(), t.folder.clone(), t.settings.clone())).collect::<Vec<_>>(),
            s.print_template_folders.clone(),
        )
    };
    let kept = names(&s);
    let scratch = Scratch::new("unsaved");
    let file = scratch.0.join("proofs.lcprint");
    run(&mut s, "print.template.export", json!({"name": "Proofs", "path": file.to_string_lossy()}));

    run(&mut s, "print.set", json!({"layout": {"style": "contactSheet"}}));
    for (id, p) in [
        ("print.template.save", json!({"name": "Other"})),
        ("print.template.save", json!({"name": "proofs"})),
        ("print.template.delete", json!({"name": "Proofs"})),
        ("print.template.import", json!({"path": file.to_string_lossy()})),
        ("print.template.move", json!({"name": "Proofs", "folder": "Family"})),
        ("print.template.folder.create", json!({"name": "Work"})),
        ("print.template.folder.rename", json!({"name": "Family", "to": "Home"})),
        ("print.template.folder.delete", json!({"name": "Family"})),
    ] {
        assert!(fails(&mut s, id, p.clone()).contains("prefs.json"), "{id} {p}");
        assert_eq!(names(&s), kept, "{id} {p}");
    }
}

#[test]
fn a_hostile_template_in_the_preferences_does_not_break_the_library() {
    let files = MemStore::new();
    let hostile = json!({"printTemplates": [
        {"name": "Bad", "settings": {"layout": {"pages": 0}, "extra": true}},
        {"name": "Huge", "settings": {"job": {"ppi": 1e300}}},
        {"name": "Fine", "settings": {"layout": {"style": "contactSheet"}}}
    ], "cacheMb": 123});
    files.set("prefs.json", serde_json::to_vec(&hostile).unwrap());
    let mut s = Session::with_demo();
    let stores = LibraryStores { dir: "print".into(), catalog: Box::new(MemStore::new()), files: Box::new(files.clone()), on_disk: false };
    s.open_library_in(stores, false).unwrap();
    assert!(s.take_library_warnings().is_empty(), "the preferences are not damaged");
    assert_eq!(s.cache_mb, 123);
    assert_eq!(run(&mut s, "print.template.list", json!({}))["templates"].as_array().unwrap().len(), 3);
    fails(&mut s, "print.template.apply", json!({"name": "Bad"}));
    fails(&mut s, "print.template.apply", json!({"name": "Huge"}));
    run(&mut s, "print.template.apply", json!({"name": "Fine"}));
}

#[test]
fn every_print_command_is_registered_with_a_description() {
    for id in [
        "print.get",
        "print.set",
        "print.reset",
        "print.cell.add",
        "print.cell.move",
        "print.cell.resize",
        "print.cell.delete",
        "print.cell.stack",
        "print.cell.photo",
        "print.cell.page",
        "print.layout.autoLayout",
        "print.layout.newPage",
        "print.layout.clear",
        "print.exportPdf",
        "print.template.list",
        "print.template.save",
        "print.template.apply",
        "print.template.delete",
        "print.template.export",
        "print.template.import",
        "print.template.move",
        "print.template.folder.create",
        "print.template.folder.rename",
        "print.template.folder.delete",
    ] {
        let c = crate::find_command(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(c.params.len() > 3, "{id}");
    }
}

#[test]
fn page_options_are_print_settings_and_template_fields() {
    let mut s = Session::with_demo();
    let g = run(&mut s, "print.get", json!({}));
    assert_eq!(g["settings"]["page"], json!({"background": [255, 255, 255], "pageNumbers": false, "pageInfo": false, "cropMarks": false}));
    let g = run(&mut s, "print.set", json!({"page": {"pageNumbers": true, "cropMarks": true}}));
    assert_eq!((g["settings"]["page"]["pageNumbers"].as_bool(), g["settings"]["page"]["pageInfo"].as_bool()), (Some(true), Some(false)));
    assert_eq!(g["settings"]["page"]["background"], json!([255, 255, 255]), "a patch merges");
    fails(&mut s, "print.set", json!({"page": {"pageInfo": "yes"}}));
    fails(&mut s, "print.set", json!({"page": {"pageNumber": true}}));
    // a template keeps them
    run(&mut s, "print.template.save", json!({"name": "Marked"}));
    run(&mut s, "print.reset", json!({}));
    assert!(!s.print.page.crop_marks);
    run(&mut s, "print.template.apply", json!({"name": "Marked"}));
    assert!(s.print.page.page_numbers && s.print.page.crop_marks && !s.print.page.page_info);
    let set = crate::find_command("print.set").unwrap();
    assert!(set.params.contains("pageNumbers") && set.params.contains("pageInfo") && set.params.contains("cropMarks"), "{}", set.params);
}
