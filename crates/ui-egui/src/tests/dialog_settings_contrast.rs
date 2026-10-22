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

/// The most any pixel of `rect` stands out from `ground` (one pixel per point here).
fn strongest(image: &egui::ColorImage, rect: egui::Rect, ground: Color32) -> f32 {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().floor().max(0.0) as usize, (rect.right().ceil().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().floor().max(0.0) as usize, (rect.bottom().ceil().max(0.0) as usize).min(h));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| contrast(image.pixels[y * w + x], ground)).fold(1.0, f32::max)
}

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
    }
}

#[test]
fn the_lines_that_say_where_a_control_is_stand_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        // A text box's border: the top edge of General's "Other editor"
        show(&mut h, "general");
        let image = h.paint();
        let field = rect(&h, "field:externalEditor");
        let edge =
            egui::Rect::from_min_max(egui::pos2(field.center().x - 20.0, field.top() - 1.0), egui::pos2(field.center().x + 20.0, field.top() + 2.0));
        assert!(strongest(&image, edge, t.chrome) >= 3.0, "{kind:?}: a text box's border is {:.2}:1", strongest(&image, edge, t.chrome));
        // A button's border: the top edge of Performance's "Clear Cache"
        show(&mut h, "performance");
        let image = h.paint();
        let button = rect(&h, "button:settingsClearCache");
        let edge = egui::Rect::from_min_max(
            egui::pos2(button.center().x - 20.0, button.top() - 1.0),
            egui::pos2(button.center().x + 20.0, button.top() + 2.0),
        );
        assert!(strongest(&image, edge, t.chrome) >= 3.0, "{kind:?}: a button's border is {:.2}:1", strongest(&image, edge, t.chrome));
        // A checkbox's border: the left edge of its box (unticked: the GPU is off here)
        show(&mut h, "performance");
        let image = h.paint();
        let check = rect(&h, "check:settings.gpu");
        let side =
            egui::Rect::from_min_max(egui::pos2(check.left() - 1.0, check.center().y - 2.0), egui::pos2(check.left() + 5.0, check.center().y + 2.0));
        assert!(strongest(&image, side, t.chrome) >= 3.0, "{kind:?}: a checkbox's border is {:.2}:1", strongest(&image, side, t.chrome));
        // A radio button on a theme's card, which is on the card's own fill: the one not chosen
        show(&mut h, "interface");
        let image = h.paint();
        let other = if kind == ThemeKind::Midnight { "radio:theme-charcoal" } else { "radio:theme-midnight" };
        let radio = rect(&h, other);
        let ring =
            egui::Rect::from_min_max(egui::pos2(radio.left() - 1.0, radio.center().y - 2.0), egui::pos2(radio.left() + 4.0, radio.center().y + 2.0));
        assert!(strongest(&image, ring, t.inset) >= 3.0, "{kind:?}: a radio button on a card is {:.2}:1", strongest(&image, ring, t.inset));
    }
}

#[test]
fn a_text_box_in_use_is_outlined_so_it_stands_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        // Given the text box has the keyboard (a click in it), and the pointer has gone away
        let r = h.request("ui.clickWidget", json!({"id": "field:externalEditor"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.request("ui.move", json!({"x": 3.0, "y": 3.0}), T);
        for _ in 0..4 {
            h.step();
        }
        // Then its top edge stands 3:1 against the window
        let image = h.paint();
        let field = rect(&h, "field:externalEditor");
        let edge =
            egui::Rect::from_min_max(egui::pos2(field.center().x - 20.0, field.top() - 1.0), egui::pos2(field.center().x + 20.0, field.top() + 2.0));
        assert!(strongest(&image, edge, t.chrome) >= 3.0, "{kind:?}: a text box in use is outlined {:.2}:1", strongest(&image, edge, t.chrome));
    }
}

#[test]
fn no_text_in_settings_uses_a_character_the_font_lacks() {
    let mut h = demo(ThemeKind::Charcoal);
    for (tab, _) in TABS {
        show(&mut h, tab);
        // (▸ has no glyph in the interface's fonts: it draws as an empty box)
        for text in h.painted_text() {
            assert!(!text.contains('▸'), "{tab}: {text:?} has an arrow the font cannot draw");
        }
    }
}

/// What Settings says that no language had a word for: every catalog has each of these now (these
/// 20, and the three menu items the hint names, which the last test below checks).
const ONCE_UNTRANSLATED: [&str; 20] = [
    "Choose Profile…",
    "Edit in Other Editor (⌥⌘E) opens the copy in this application (an app name on macOS, a program path elsewhere).",
    "External Editing",
    "Found automatically",
    "LUT-based",
    "Matrix/TRC",
    "Model installation failed",
    "Monitor profile",
    "None (the display is treated as sRGB)",
    "Other editor",
    "Photo > Edit In > Edit in PhotoCraft (⇧⌘E, or right-click a photo) renders a copy with your edits, stacks it with the original and opens it in PhotoCraft. Save there and the copy updates here. As PSD, PhotoCraft saves straight back to it. Leave the location empty unless you use a portable copy.",
    "Photos are shown through your monitor's ICC profile, so colours are right on wide-gamut and calibrated displays, and the Detail view uses the display's whole gamut. The histogram, the preview caches and exports are not affected.",
    "Primaries (x, y)",
    "Profiles assigned in your desktop's colour settings are usually in ~/.local/share/icc.",
    "The download failed",
    "Use Screen Profile",
    "Use sRGB",
    "starting…",
    "this screen's profile",
    "Faces",
];

#[test]
fn every_language_has_the_sentences_settings_was_missing() {
    use crate::i18n::Locale;
    for language in Locale::ALL.iter().copied().filter(|language| *language != Locale::En) {
        for source in ONCE_UNTRANSLATED {
            let said = language.tr(source);
            // (German, Georgian, Indonesian and Vietnamese say Matrix as English does, and TRC is TRC everywhere)
            let same_word = source == "Matrix/TRC" && matches!(language, Locale::De | Locale::Id | Locale::Ka | Locale::Vi);
            assert!(said != source || same_word, "{}: {source:?} is still in English", language.code());
            assert!(!said.trim().is_empty() && !said.contains('▸'), "{}: {source:?} -> {said:?}", language.code());
        }
    }
}

#[test]
fn the_sentences_settings_was_missing_are_the_ones_it_says() {
    // (a sentence reworded in the code and not here would leave its translations orphaned)
    let sources = [include_str!("panels/settings.rs"), include_str!("panels/denoise.rs"), include_str!("panels/faces.rs")].concat();
    for source in ONCE_UNTRANSLATED {
        let literal = format!("{source:?}");
        assert!(sources.contains(&literal), "{source:?} is no longer said by Settings as written here");
    }
}

#[test]
fn the_menu_path_a_hint_gives_is_the_one_the_menus_show() {
    use crate::i18n::Locale;
    // (the hint under External Editing says where the command is: in each language it names the
    // menu items as that language's menus do)
    let hint = ONCE_UNTRANSLATED.iter().find(|source| source.starts_with("Photo > Edit In > Edit in PhotoCraft")).unwrap();
    for language in Locale::ALL.iter().copied().filter(|language| *language != Locale::En) {
        let said = language.tr(hint);
        for item in ["Edit In", "Edit in PhotoCraft"] {
            let name = language.tr(item);
            assert!(name != item, "{}: the menu item {item:?} has no translation", language.code());
            assert!(said.contains(name), "{}: the hint does not name {name:?}: {said:?}", language.code());
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
fn the_gpu_line_and_the_smart_previews_folder_go_through_translation() {
    use crate::dialogs::settings::{gpu_line, smart_folder_default};
    use crate::i18n::{Locale, set_language};
    for language in Locale::ALL.iter().copied().filter(|language| *language != Locale::En) {
        set_language(language);
        let starting = gpu_line(None);
        assert_eq!(starting, format!("GPU: {}", language.tr("starting…")), "{}", language.code());
        assert_ne!(starting, "GPU: starting…", "{}: still English", language.code());
        let shown = smart_folder_default();
        assert_eq!(shown, language.tr("  (library default)").trim(), "{}", language.code());
        assert_ne!(shown, "(library default)", "{}: still English", language.code());
    }
    set_language(Locale::En);
    assert_eq!(gpu_line(Some("Apple M2".into())), "GPU: Apple M2");
    assert_eq!(smart_folder_default(), "(library default)");
}
