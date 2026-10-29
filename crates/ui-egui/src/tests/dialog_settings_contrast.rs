//! What Settings says and marks can be read in every theme: its explanations, placeholders,
//! cautions and the accent used as text stand 4.5:1 against what they are on, and the lines that
//! say where a control is stand 3:1. No text in it uses a character the interface
//! font lacks.
//!
//! Before: explanations were 4.20:1 in Charcoal and 4.33:1 in Silver, "In Use" 2.9:1 in Charcoal,
//! placeholders under 4.3:1 in every theme, the borders of text boxes and checkboxes 1.6 to 1.7:1;
//! and the hint under External Editing drew two empty
//! boxes where its menu path had arrows.

use std::time::Duration;

use egui::Color32;
use serde_json::json;

use crate::dialogs::settings::TABS;
use crate::headless::Headless;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo(kind: ThemeKind) -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 1000.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    let r = h.request("engine.execute", json!({"command": "view.theme", "params": {"theme": kind.id()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h
}

fn show(h: &mut Headless, tab: &str) {
    let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": tab}}), T);
    assert_eq!(r["ok"], true, "app.settings {tab}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
    // (the pointer away from the window: nothing is hovered)
    h.request("ui.move", json!({"x": 3.0, "y": 3.0}), T);
    h.step();
    h.step();
}

/// Scroll the open Settings page to its end (General's External Editing is far below the top).
fn scroll_to_end(h: &mut Headless) {
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "settings:pageRoom"}), T)["ok"], true);
    h.request("ui.scroll", json!({"dy": -3000.0}), T);
    for _ in 0..4 {
        h.step();
    }
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// WCAG contrast of two opaque colours.
fn contrast(a: Color32, b: Color32) -> f32 {
    let lum = |c: Color32| {
        let lin = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b())
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The colour a piece of text is drawn in: the first text shape that has `part`.
fn text_colour(h: &Headless, part: &str) -> Color32 {
    fn find(shape: &egui::Shape, part: &str) -> Option<Color32> {
        match shape {
            egui::Shape::Text(text) if text.galley.job.text.contains(part) => {
                let own = text.galley.job.sections.first().map(|s| s.format.color).filter(|c| *c != Color32::PLACEHOLDER);
                Some(text.override_text_color.or(own).unwrap_or(text.fallback_color))
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| find(shape, part)),
            _ => None,
        }
    }
    h.view.painted_shapes().iter().find_map(|shape| find(shape, part)).unwrap_or_else(|| panic!("no text with {part:?} is drawn"))
}

/// How much of an edge stands `ratio` against `ground`: the largest share, over the rows of `rect` (or its
/// columns, when `horizontal` is false), of that line's pixels that do. One bright antialiased pixel does not
/// make a border; a line that is mostly at the ratio does (one pixel per point here).
fn edge_share(image: &egui::ColorImage, rect: egui::Rect, ground: Color32, ratio: f32, horizontal: bool) -> f32 {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    let stands = |x: usize, y: usize| contrast(image.pixels[y * w + x], ground) >= ratio;
    if horizontal {
        (y0..y1).map(|y| (x0..x1).filter(|x| stands(*x, y)).count() as f32 / (x1 - x0).max(1) as f32).fold(0.0, f32::max)
    } else {
        (x0..x1).map(|x| (y0..y1).filter(|y| stands(x, *y)).count() as f32 / (y1 - y0).max(1) as f32).fold(0.0, f32::max)
    }
}

/// Share of a straight edge that must stand the ratio; a radio button's ring is a curve, so it gets half.
const STRAIGHT: f32 = 0.75;
const CURVED: f32 = 0.5;

#[test]
fn a_colour_is_made_legible_only_where_it_is_not() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        // Given the colours Settings writes and marks in, on the surfaces it puts them on
        for (what, colour, on, ratio) in [
            ("an explanation", t.text_dim, t.chrome, 4.5),
            ("a caution", t.caution, t.chrome, 4.5),
            ("a refusal", t.reject, t.chrome, 4.5),
            ("the accent as text on a card", t.accent, t.inset, 4.5),
            ("a placeholder", t.text_dim, t.field, 4.5),
            ("a control's border", t.button_border, t.chrome, 3.0),
        ] {
            // Then the legible one stands the ratio asked for
            let legible = t.legible(colour, on, ratio);
            assert!(contrast(legible, on) >= ratio, "{kind:?}: {what} is {:.2}:1, wants {ratio}", contrast(legible, on));
            // And a colour that stood it already is left as it is
            if contrast(colour, on) >= ratio {
                assert_eq!(legible, colour, "{kind:?}: {what} was legible and is unchanged");
            }
        }
        // And even the surface's own colour is brought to stand on it
        assert!(contrast(t.legible(t.chrome, t.chrome, 4.5), t.chrome) >= 4.5);
    }
    // (a ratio nothing can reach, or one that is not a number, ends at the text colour too)
    let t = Tokens::for_kind(ThemeKind::Charcoal);
    assert_eq!(t.legible(t.text_dim, t.chrome, 99.0), t.text);
    assert_eq!(t.legible(t.text_dim, t.chrome, f32::NAN), t.text);
}

#[test]
fn explanations_placeholders_and_the_accent_as_text_can_be_read_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        // An explanation under a setting
        show(&mut h, "performance");
        let hint = text_colour(&h, "Choose a size to cap");
        assert!(contrast(hint, t.chrome) >= 4.5, "{kind:?}: an explanation is {:.2}:1", contrast(hint, t.chrome));
        // A text box's placeholder, on the box it is in (General, PhotoCraft's location)
        show(&mut h, "general");
        let placeholder = text_colour(&h, "Found automatically");
        assert!(contrast(placeholder, t.field) >= 4.5, "{kind:?}: a placeholder is {:.2}:1", contrast(placeholder, t.field));
        // The line that says which GPU renders, under the Rendering setting
        show(&mut h, "performance");
        let gpu = h.painted_text().into_iter().find(|text| text.starts_with("GPU:") || text.contains("CPU")).expect("a GPU status line");
        let gpu = text_colour(&h, &gpu);
        assert!(contrast(gpu, t.chrome) >= 4.5, "{kind:?}: the GPU line is {:.2}:1", contrast(gpu, t.chrome));
        // "In Use" on the card of the theme that is shown
        show(&mut h, "interface");
        let in_use = text_colour(&h, "In Use");
        assert!(contrast(in_use, t.inset) >= 4.5, "{kind:?}: In Use is {:.2}:1", contrast(in_use, t.inset));
        // The Faces tab (with a folder for the models, or it only says it has none): the hint beside "Add a model
        // file…", and the marks on a model's row
        let models = std::env::temp_dir().join(format!("lc-settings-contrast-faces-{}", std::process::id()));
        std::fs::create_dir_all(&models).unwrap();
        h.app.session.face_models_dir = Some(models.clone());
        // (an installed model is the one in use, and has a size)
        let file = models.join("m.onnx");
        std::fs::write(&file, lightcraft_faces::synthetic::tiny_embedder_model(64)).unwrap();
        h.app.session.execute("faces.models.install", &json!({"path": file.to_string_lossy(), "acknowledged": true})).unwrap();
        h.app.caches.faces_epoch += 1;
        show(&mut h, "faces");
        let drop = text_colour(&h, "or drop a .onnx file");
        assert!(contrast(drop, t.chrome) >= 4.5, "{kind:?}: the Faces hint is {:.2}:1", contrast(drop, t.chrome));
        // the Processor section's labels and the license line of the model (its terms are unclear: the warning color)
        for part in ["Detection", "Recognition", "terms unclear"] {
            let colour = text_colour(&h, part);
            assert!(contrast(colour, t.chrome) >= 4.5, "{kind:?}: \"{part}\" is {:.2}:1", contrast(colour, t.chrome));
        }
        let painted = h.painted_text();
        let marks: Vec<&str> = ["in use", "installed"].into_iter().filter(|m| painted.iter().any(|text| text.as_str() == *m)).collect();
        assert!(!marks.is_empty(), "{kind:?}: no model is marked in use or installed");
        for mark in marks {
            let colour = text_colour(&h, mark);
            assert!(contrast(colour, t.chrome) >= 4.5, "{kind:?}: \"{mark}\" is {:.2}:1", contrast(colour, t.chrome));
        }
        let _ = std::fs::remove_dir_all(&models);
    }
}

#[test]
fn the_lines_that_say_where_a_control_is_stand_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        // A text box's border: the top edge of General's "Other editor"
        show(&mut h, "general");
        scroll_to_end(&mut h);
        let image = h.paint();
        let field = rect(&h, "field:externalEditor");
        let edge =
            egui::Rect::from_min_max(egui::pos2(field.center().x - 20.0, field.top() - 1.0), egui::pos2(field.center().x + 20.0, field.top() + 2.0));
        let share = edge_share(&image, edge, t.chrome, 3.0, true);
        assert!(share >= STRAIGHT, "{kind:?}: only {share:.2} of a text box's border stands 3:1");
        // A button's border: the top edge of Performance's "Clear Cache"
        show(&mut h, "performance");
        let image = h.paint();
        let button = rect(&h, "button:settingsClearCache");
        let edge = egui::Rect::from_min_max(
            egui::pos2(button.center().x - 20.0, button.top() - 1.0),
            egui::pos2(button.center().x + 20.0, button.top() + 2.0),
        );
        let share = edge_share(&image, edge, t.chrome, 3.0, true);
        assert!(share >= STRAIGHT, "{kind:?}: only {share:.2} of a button's border stands 3:1");
        // A text button's border (the tab bar's, and a row of choices: Preview size), which are drawn by
        // `widgets::text_button`, not by egui's button: the top edge of one that is not chosen
        for (what, id) in [("a tab", "button:settingsTab-general"), ("a choice", "button:settingsPreview-1")] {
            show(&mut h, "performance");
            let image = h.paint();
            let button = rect(&h, id);
            let edge = egui::Rect::from_min_max(
                egui::pos2(button.center().x - 12.0, button.top() - 1.0),
                egui::pos2(button.center().x + 12.0, button.top() + 2.0),
            );
            let share = edge_share(&image, edge, t.chrome, 3.0, true);
            assert!(share >= STRAIGHT, "{kind:?}: only {share:.2} of {what}'s border stands 3:1");
        }
        // A checkbox's border: the left edge of its box (unticked: the GPU is off here)
        show(&mut h, "performance");
        let image = h.paint();
        let check = rect(&h, "check:settings.gpu");
        let side =
            egui::Rect::from_min_max(egui::pos2(check.left() - 1.0, check.center().y - 2.0), egui::pos2(check.left() + 5.0, check.center().y + 2.0));
        let share = edge_share(&image, side, t.chrome, 3.0, false);
        assert!(share >= STRAIGHT, "{kind:?}: only {share:.2} of a checkbox's border stands 3:1");
        // A radio button on a theme's card, which is on the card's own fill: the one not chosen
        show(&mut h, "interface");
        let image = h.paint();
        let other = if kind == ThemeKind::Midnight { "radio:theme-charcoal" } else { "radio:theme-midnight" };
        let radio = rect(&h, other);
        let ring =
            egui::Rect::from_min_max(egui::pos2(radio.left() - 1.0, radio.center().y - 2.0), egui::pos2(radio.left() + 4.0, radio.center().y + 2.0));
        let share = edge_share(&image, ring, t.inset, 3.0, false);
        assert!(share >= CURVED, "{kind:?}: only {share:.2} of a radio button's ring on a card stands 3:1");
    }
}

#[test]
fn a_text_box_in_use_is_outlined_so_it_stands_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        scroll_to_end(&mut h);
        // Given the text box as it is without the keyboard, then with it (a click in it), the pointer away
        let unfocused = h.paint();
        let r = h.request("ui.clickWidget", json!({"id": "field:externalEditor"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.request("ui.move", json!({"x": 3.0, "y": 3.0}), T);
        for _ in 0..4 {
            h.step();
        }
        // Then what the keyboard focus adds to the edge stands 3:1 against the window: the border
        // a text box has without it (also 3:1) must not be what passes
        let focused = h.paint();
        let field = rect(&h, "field:externalEditor");
        let edge =
            egui::Rect::from_min_max(egui::pos2(field.center().x - 20.0, field.top() - 1.0), egui::pos2(field.center().x + 20.0, field.top() + 2.0));
        let outline = added_by_focus(&unfocused, &focused, edge);
        assert!(!outline.is_empty(), "{kind:?}: the text box in use looks the same as when it is not");
        // (along the edge's busiest row, most of its pixels are ones focus added at 3:1)
        let columns = (edge.right().ceil() - edge.left().floor()).max(1.0);
        let share = outline.iter().filter(|(_, c)| contrast(*c, t.chrome) >= 3.0).fold(
            std::collections::HashMap::<usize, usize>::new(),
            |mut rows, (y, _)| {
                *rows.entry(*y).or_default() += 1;
                rows
            },
        );
        let best = share.values().copied().max().unwrap_or(0) as f32 / columns;
        assert!(best >= STRAIGHT, "{kind:?}: only {best:.2} of a text box's edge in use is outlined at 3:1");
    }
}

/// The row and colour of the pixels of `rect` that differ between `before` and `after`.
fn added_by_focus(before: &egui::ColorImage, after: &egui::ColorImage, rect: egui::Rect) -> Vec<(usize, Color32)> {
    let [w, h] = after.size;
    assert_eq!(before.size, after.size);
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| y * w + x))
        .filter(|i| before.pixels[*i] != after.pixels[*i])
        .map(|i| (i / w, after.pixels[i]))
        .collect()
}

#[test]
fn no_text_in_settings_uses_a_character_the_font_lacks() {
    let mut h = demo(ThemeKind::Charcoal);
    for (tab, _) in TABS {
        show(&mut h, tab);
        // (▸ has no glyph in the interface's fonts: it draws as an empty box; a page is read at its
        // top and again scrolled to its foot, since what is below the fold is not painted)
        for scrolled in [false, true] {
            if scrolled {
                scroll_to_end(&mut h);
            }
            for text in h.painted_text() {
                assert!(!text.contains('▸'), "{tab}: {text:?} has an arrow the font cannot draw");
            }
        }
    }
}

#[test]
fn a_models_licence_line_and_a_favourite_star_can_be_read_in_every_theme() {
    use crate::dialogs::settings::favorite_star_colour;
    use crate::panels::denoise::licence_colour;
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        // A licence that forbids commercial use, or says nothing, is cautioned; one that allows it is plain
        for (commercial, plain) in [("no", t.text_dim), ("unknown", t.text_dim), ("yes", t.text_dim), ("yes", t.text)] {
            let colour = licence_colour(&t, &json!({"licence": {"commercial": commercial}}), plain);
            assert!(contrast(colour, t.chrome) >= 4.5, "{kind:?}: a {commercial} licence line is {:.2}:1", contrast(colour, t.chrome));
        }
        // The star on a theme card, filled or offered, is a mark that stands 3:1 against the card
        for favorite in [true, false] {
            let star = favorite_star_colour(&t, favorite);
            assert!(contrast(star, t.inset) >= 3.0, "{kind:?}: a star (favourite: {favorite}) is {:.2}:1", contrast(star, t.inset));
        }
    }
}

#[test]
fn the_gpu_line_reads_as_it_did_in_english_and_in_the_language_shown() {
    use crate::dialogs::settings::gpu_line;

    assert_eq!(gpu_line(Some("Apple M2".into())), "GPU: Apple M2");
    // (no adapter known yet: the word is the language's, not English)
    crate::i18n::set_language(crate::i18n::Locale::De);
    let line = gpu_line(None);
    crate::i18n::set_language(crate::i18n::Locale::En);
    assert_eq!(line, format!("GPU: {}", crate::i18n::Locale::De.tr("starting…")));
    assert_eq!(gpu_line(None), "GPU: starting…");
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn the_smart_previews_folder_reads_as_it_did_in_english() {
    assert_eq!(crate::dialogs::settings::smart_folder_default(), "(library default)");
}
