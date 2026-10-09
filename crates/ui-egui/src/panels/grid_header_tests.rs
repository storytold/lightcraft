use egui::{Rect, vec2};

use crate::{DacApp, Services, headless::HeadlessView, i18n::Locale};

fn text_bounds(shape: &egui::Shape, out: &mut Vec<Rect>) {
    match shape {
        egui::Shape::Text(text) => out.push(text.visual_bounding_rect()),
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| text_bounds(s, out)),
        _ => {}
    }
}

#[test]
fn local_folder_header_text_and_controls_do_not_overlap() {
    for locale in [Locale::En, Locale::De, Locale::PtBr] {
        for width in [136.0, 200.0, 320.0, 590.0, 1100.0] {
            for scale in [1.0, 2.0] {
                for (path, local_n) in [
                    ("/Volumes/External drive/Photography archive/Trips and holidays/A very long folder name with many photographs to import", 2408),
                    ("/A very long folder name with many photographs to import", 2408),
                    (r"C:\Users\Photographer\Pictures\A very long folder name with many photographs to import", 2408),
                    ("/Pictures", 0),
                ] {
                    let mut app = DacApp::new(dac_engine::Session::new(), Services::default());
                    let browse = dac_engine::Browse { path: path.into(), subfolders: false };
                    let ctx = egui::Context::default();
                    crate::i18n::set_language(locale);
                    crate::theme::install_fonts(&ctx);
                    crate::theme::apply(&ctx);
                    let counted = crate::i18n::tr_format!("{sel_n} selected · {counted}", sel_n = 2408, counted = "2408 photos");
                    let mut texts = Vec::new();
                    let mut widgets = Vec::new();
                    let mut header = Rect::NOTHING;
                    for frame in 0..3 {
                        let raw = HeadlessView::raw_input(vec2(width, 300.0), scale, frame as f64 / 60.0, vec![]);
                        let mut out = ctx.run_ui(raw, |ui| {
                            egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                                header = super::folder_header(&mut app, ui, &browse, &[], local_n, &counted);
                                let (grid, _) = ui.allocate_exact_size(vec2(ui.available_width(), 100.0), egui::Sense::hover());
                                assert!(grid.top() >= header.bottom(), "wrapped controls must reserve space above the grid");
                            });
                        });
                        out.textures_delta.clear();
                        texts.clear();
                        for shape in &out.shapes {
                            text_bounds(&shape.shape, &mut texts);
                        }
                        widgets = crate::widgets::take_registry(&ctx);
                    }
                    crate::i18n::set_language(Locale::En);
                    assert!(header.left() >= 0.0 && header.right() <= width, "header must not expand past the centre pane: {header:?}");
                    let check = widgets.iter().find(|(id, _)| id == "check:includeSubfolders").unwrap().1;
                    assert!(widgets.iter().any(|(id, _)| id.starts_with("crumb:")));
                    assert_eq!(widgets.iter().any(|(id, _)| id == "button:addToLibrary"), local_n > 0);
                    for (id, rect) in &widgets {
                        assert!(header.contains_rect(*rect), "{locale:?} {width} {scale}: {id} outside header: {rect:?}");
                        if id.starts_with("crumb:") {
                            assert!(!rect.intersects(check), "breadcrumb overlaps Include subfolders");
                        }
                    }
                    for (i, a) in texts.iter().enumerate() {
                        assert!(header.expand(1.0).contains_rect(*a), "{locale:?} {width} {scale}: text outside header: {a:?}");
                        for b in texts.iter().skip(i + 1) {
                            assert!(!a.intersects(*b), "{locale:?} {width} {scale}: painted text overlaps: {a:?} / {b:?}");
                        }
                    }
                }
            }
        }
    }
}
