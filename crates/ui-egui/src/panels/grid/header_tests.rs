use egui::{Rect, vec2};

use crate::{LightcraftApp, Services, headless::HeadlessView, i18n::Locale};

/// Issue #538: a breadcrumb opens its folder spelled as the path is, keeping a UNC share's leading
/// `\\` and giving a bare drive its root back.
#[test]
fn breadcrumbs_open_their_folder_as_spelled() {
    let to = |path: &str| super::crumbs(path).into_iter().map(|(name, to)| (name.to_string(), to)).collect::<Vec<_>>();
    let pairs = |v: &[(&str, &str)]| v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>();
    assert_eq!(to("/Users/me/Pictures"), pairs(&[("Users", "/Users"), ("me", "/Users/me"), ("Pictures", "/Users/me/Pictures")]));
    assert_eq!(to(r"C:\Users\me"), pairs(&[("C:", r"C:\"), ("Users", r"C:\Users"), ("me", r"C:\Users\me")]));
    assert_eq!(to("C:/Users"), pairs(&[("C:", "C:/"), ("Users", "C:/Users")]));
    assert_eq!(to(r"\\server\share\photos"), pairs(&[("server", r"\\server"), ("share", r"\\server\share"), ("photos", r"\\server\share\photos")]));
    assert_eq!(to("relative/dir/"), pairs(&[("relative", "relative"), ("dir", "relative/dir")]));
    assert_eq!(to("/Fotos/día 1/写真"), pairs(&[("Fotos", "/Fotos"), ("día 1", "/Fotos/día 1"), ("写真", "/Fotos/día 1/写真")]));
    assert!(to("").is_empty() && to("/").is_empty() && to(r"\\").is_empty());
}

/// Issue #3776: one selected photo is counted in the header as two or more are ("1 selected · 3
/// photos"); with nothing selected the header shows the photo count alone.
#[test]
fn the_header_counts_a_single_selected_photo() {
    use lightcraft_catalog::{Op, Photo, Source};
    for (selected, folder) in [(0, false), (1, false), (2, false), (0, true), (1, true)] {
        let mut session = lightcraft_engine::Session::new();
        for i in 0..3 {
            let id = session.catalog.alloc_photo_id();
            let p = Photo::new(id, Source::File { path: format!("/pics/{i}.jpg") }, "x.jpg", "JPEG", 60, 40, "2026-01-01T10:00:00");
            session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        }
        session.selection.ids = session.catalog.photos().take(selected).map(|p| p.id).collect();
        if folder {
            // a Local folder view: the same count, in the breadcrumb header
            session.source = lightcraft_engine::LibrarySource::Folder;
            session.browse = Some(lightcraft_engine::Browse { path: "/pics".into(), subfolders: false });
        }
        let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
        app.ui.view = crate::state::ViewMode::PhotoGrid;
        let mut h = crate::headless::Headless::new(app, [1400.0, 900.0], 1.0);
        h.step();
        h.step();
        assert_eq!(h.app.session.selection.ids.len(), selected, "the selection is kept");
        let has = |id: &str| h.app.widgets.iter().any(|(w, _)| w == id);
        assert!(has("grid:count") && has("grid:folderHeader") == folder, "folder {folder}: {:?}", h.app.widgets);
        let painted = h.painted_text();
        let photos = crate::panels::chips::count_text(h.app.session.visible_cloned().len(), h.app.session.source_total(), false);
        assert!(folder || photos == "3 photos", "{photos}");
        let expected = if selected == 0 { photos } else { format!("{selected} selected · {photos}") };
        assert!(painted.contains(&expected), "folder {folder}, {selected} selected: the header shows {expected:?}: {painted:?}");
        if selected == 0 {
            assert!(!painted.iter().any(|s| s.contains("selected ·")), "nothing selected, no count: {painted:?}");
        }
    }
    // a language whose word for "selected" agrees with the number reads "noun: number", right for one as for many
    for (locale, expected) in [(Locale::Es, "Seleccionadas: 1 · x"), (Locale::PtBr, "Selecionadas: 1 · x"), (Locale::Sv, "Markerade: 1 · x")] {
        crate::i18n::set_language(locale);
        let one = crate::i18n::tr_format!("{sel_n} selected · {counted}", sel_n = 1, counted = "x");
        crate::i18n::set_language(Locale::En);
        assert_eq!(one, expected, "{locale:?}");
    }
}

/// Every text the frame painted as it reads on screen (a cut one with its "…"), with where it was painted.
fn texts_painted(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
    match shape {
        egui::Shape::Text(text) => {
            let shown = text.galley.rows.iter().flat_map(|row| row.glyphs.iter().map(|g| g.chr)).collect();
            out.push((shown, text.visual_bounding_rect()));
        }
        egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| texts_painted(s, out)),
        _ => {}
    }
}

/// A library view's count too long for a narrow centre pane (German, at the narrowest the photo
/// area gets) is cut to stay inside the header, beside the title, and reads in full on hover.
#[test]
fn a_long_library_count_stays_inside_a_narrow_header() {
    for width in [crate::state::MIN_PHOTO_WIDTH, 590.0] {
        for scale in [1.0, 2.0] {
            let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
            let ctx = egui::Context::default();
            crate::i18n::set_language(Locale::De);
            crate::theme::install_fonts(&ctx);
            crate::theme::apply(&ctx);
            let counted = crate::i18n::tr_format!("{matching} of {t} photos", matching = 123_456_789, t = 987_654_321);
            let cnt = crate::i18n::tr_format!("{sel_n} selected · {counted}", sel_n = 123_456_789, counted = counted);
            let frame = |time: f64, events: Vec<egui::Event>| {
                let mut header = Rect::NOTHING;
                let raw = HeadlessView::raw_input(vec2(width, 300.0), scale, time, events);
                let mut out = ctx.run_ui(raw, |ui| {
                    egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                        header = super::library_header(&app, ui, &cnt);
                    });
                });
                out.textures_delta.clear();
                let mut texts = Vec::new();
                for shape in &out.shapes {
                    texts_painted(&shape.shape, &mut texts);
                }
                (texts, crate::widgets::take_registry(&ctx), header)
            };
            let (mut texts, mut widgets, mut header) = (Vec::new(), Vec::new(), Rect::NOTHING);
            for f in 0..3 {
                (texts, widgets, header) = frame(f as f64 / 60.0, vec![]);
            }
            let widget = |id: &str| widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r);
            let (Some(count), Some(title)) = (widget("grid:count"), widget("grid:title")) else { panic!("no count or title: {widgets:?}") };
            let at = format!("{width} {scale}");
            assert!(header.left() >= 0.0 && header.right() <= width, "{at}: header must not expand past the centre pane: {header:?}");
            assert!(header.contains_rect(count), "{at}: the count stays inside the header: {count:?} in {header:?}");
            assert!(title.right() <= count.left(), "{at}: the title stays left of the count: {title:?} {count:?}");
            for (i, (text, a)) in texts.iter().enumerate() {
                assert!(header.expand(1.0).contains_rect(*a), "{at}: {text:?} painted outside the header: {a:?}");
                for (other, b) in texts.iter().skip(i + 1) {
                    assert!(!a.intersects(*b), "{at}: {text:?} overlaps {other:?}");
                }
            }
            // cut where it doesn't fit, and in full on hover
            let fits = width > 500.0;
            assert_eq!(texts.iter().any(|(t, _)| *t == cnt), fits, "{at}: {texts:?}");
            let cut = cnt.split(" von ").next().unwrap_or_default();
            assert!(fits || texts.iter().any(|(t, _)| t.ends_with('…') && t.starts_with(cut)), "{at}: the count is cut at its end: {texts:?}");
            let mut shown = Vec::new();
            for f in 3..120 {
                let events = if f == 3 { vec![egui::Event::PointerMoved(count.center())] } else { vec![] };
                (shown, _, _) = frame(f as f64 / 60.0, events);
            }
            assert!(shown.iter().any(|(t, _)| *t == cnt), "{at}: the full count shows on hover: {shown:?}");
            crate::i18n::set_language(Locale::En);
        }
    }
}

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
                    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
                    let browse = lightcraft_engine::Browse { path: path.into(), subfolders: false };
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
