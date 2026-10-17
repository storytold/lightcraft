//! What Settings says and marks can be read in every theme: its explanations, placeholders,
//! cautions and the accent used as text stand 4.5:1 against what they are on, and the lines that
//! say where a control or the tab bar is stand 3:1. No text in it uses a character the interface
//! font lacks.
//!
//! Before: explanations were 4.20:1 in Charcoal and 4.33:1 in Silver, "In Use" 2.9:1 in Charcoal,
//! placeholders under 4.3:1 in every theme, the borders of text boxes and checkboxes 1.6 to 1.7:1
//! and the line under the tabs 1.2 to 1.4:1; and the hint under External Editing drew two empty
//! boxes where its menu path had arrows.

use std::time::Duration;

use egui::Color32;
use serde_json::json;

use crate::headless::Headless;
use crate::panels::settings::TABS;
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
            ("the line under the tabs", t.divider, t.chrome, 3.0),
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
        // The filter's placeholder, on the box it is in
        let placeholder = text_colour(&h, "Filter settings");
        assert!(contrast(placeholder, t.field) >= 4.5, "{kind:?}: a placeholder is {:.2}:1", contrast(placeholder, t.field));
        // "In Use" on the card of the theme that is shown
        show(&mut h, "interface");
        let in_use = text_colour(&h, "In Use");
        assert!(contrast(in_use, t.inset) >= 4.5, "{kind:?}: In Use is {:.2}:1", contrast(in_use, t.inset));
    }
}

#[test]
fn what_the_filter_found_can_be_read_with_the_pointer_or_the_focus_on_it() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        let r = h.request("ui.clickWidget", json!({"id": "field:settingsFilter"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.request("ui.text", json!({"text": "clear cache"}), T);
        for _ in 0..4 {
            h.step();
        }
        // (the line under a result's title; a hovered or focused row has the hover fill behind it)
        let snippet = text_colour(&h, "Clear Cache");
        assert!(
            contrast(snippet, t.hover) >= 4.5 && contrast(snippet, t.chrome) >= 4.5,
            "{kind:?}: a result's text is {:.2}:1 on a hovered row",
            contrast(snippet, t.hover)
        );
    }
}

#[test]
fn the_lines_that_say_where_a_control_is_stand_out_in_every_theme() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        show(&mut h, "performance");
        let image = h.paint();
        // The line under the tabs: just under a tab that is not open, so its own line is not there
        let tab = rect(&h, "button:settingsTab-library");
        let under = egui::Rect::from_min_max(egui::pos2(tab.left(), tab.bottom().round()), egui::pos2(tab.right(), tab.bottom().round() + 1.0));
        assert!(strongest(&image, under, t.chrome) >= 3.0, "{kind:?}: the line under the tabs is {:.2}:1", strongest(&image, under, t.chrome));
        // A text box's border: the filter's top edge
        let filter = rect(&h, "field:settingsFilter");
        let edge = egui::Rect::from_min_max(
            egui::pos2(filter.center().x - 20.0, filter.top() - 1.0),
            egui::pos2(filter.center().x + 20.0, filter.top() + 2.0),
        );
        assert!(strongest(&image, edge, t.chrome) >= 3.0, "{kind:?}: a text box's border is {:.2}:1", strongest(&image, edge, t.chrome));
        // A checkbox's border: the left edge of its box (unticked: the GPU is off here)
        let check = rect(&h, "check:settings.gpu");
        let side =
            egui::Rect::from_min_max(egui::pos2(check.left() - 1.0, check.center().y - 2.0), egui::pos2(check.left() + 5.0, check.center().y + 2.0));
        assert!(strongest(&image, side, t.chrome) >= 3.0, "{kind:?}: a checkbox's border is {:.2}:1", strongest(&image, side, t.chrome));
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

/// What Settings says that no language had a word for: every catalog has each of these now.
const ONCE_UNTRANSLATED: [&str; 27] = [
    "Choose Profile…",
    "Delete and Change Folder",
    "Delete the smart previews in the folder used until now?",
    "Edit in Other Editor (⌥⌘E) opens the copy in this application (an app name on macOS, a program path elsewhere).",
    "External Editing",
    "Faces",
    "Found automatically",
    "Keep",
    "LUT-based",
    "Matrix/TRC",
    "Model installation failed",
    "Monitor profile",
    "None (the display is treated as sRGB)",
    "On, every change to a photo's edits or metadata is written to an XMP file beside the photo, for other apps to read. Off, that happens when you choose Save Metadata to File in the Photo menu.",
    "Other editor",
    "Photo > Edit In > Edit in PhotoCraft (⇧⌘E, or right-click a photo) renders a copy with your edits, stacks it with the original and opens it in PhotoCraft. Save there and the copy updates here. As PSD, PhotoCraft saves straight back to it. Leave the location empty unless you use a portable copy.",
    "Photos are shown through your monitor's ICC profile, so colours are right on wide-gamut and calibrated displays, and the Detail view uses the display's whole gamut. The histogram, the preview caches and exports are not affected.",
    "Photos whose originals are not connected are shown and edited from their smart previews; new ones can only be made from originals that are connected.",
    "Primaries (x, y)",
    "Profiles assigned in your desktop's colour settings are usually in ~/.local/share/icc.",
    "The download failed",
    "The model's file is deleted. To use it again you download or add it again.",
    "Thumbnails and previews are made again as you browse, which takes a while in a large library. Your photos and edits are not touched.",
    "Use Screen Profile",
    "Use sRGB",
    "starting…",
    "this screen's profile",
];

#[test]
fn every_language_has_the_sentences_settings_was_missing() {
    use crate::i18n::Locale;
    for language in Locale::ALL.iter().copied().filter(|language| *language != Locale::En) {
        for source in ONCE_UNTRANSLATED {
            let said = language.tr(source);
            // (German says Matrix as English does, and TRC is TRC everywhere)
            let same_word = source == "Matrix/TRC" && language == Locale::De;
            assert!(said != source || same_word, "{}: {source:?} is still in English", language.code());
            assert!(!said.trim().is_empty() && !said.contains('▸'), "{}: {source:?} -> {said:?}", language.code());
        }
    }
}

#[test]
fn the_sentences_settings_was_missing_are_the_ones_it_says() {
    // (a sentence reworded in the code and not here would leave its translations orphaned)
    let sources =
        [include_str!("panels/settings.rs"), include_str!("panels/denoise.rs"), include_str!("panels/faces.rs"), include_str!("widgets.rs")].concat();
    for source in ONCE_UNTRANSLATED {
        let literal = format!("{source:?}");
        assert!(sources.contains(&literal), "{source:?} is no longer said by Settings as written here");
    }
}

#[test]
fn a_choice_a_radio_and_a_question_have_borders_that_stand_out_too() {
    for kind in ThemeKind::ALL {
        let mut h = demo(kind);
        let t = Tokens::for_kind(kind);
        // A choice that is not the chosen one: its left edge (Performance, the second preview size)
        show(&mut h, "performance");
        let image = h.paint();
        let choice = rect(&h, "button:settingsPreview-1");
        let side = egui::Rect::from_min_max(
            egui::pos2(choice.left() - 1.0, choice.center().y - 3.0),
            egui::pos2(choice.left() + 2.0, choice.center().y + 3.0),
        );
        assert!(strongest(&image, side, t.chrome) >= 3.0, "{kind:?}: a choice's border is {:.2}:1", strongest(&image, side, t.chrome));
        // The question a button asks before it acts: the border of its Keep button, on the panel it is in
        let r = h.request("ui.clickWidget", json!({"id": "button:settingsClearCache"}), T);
        assert_eq!(r["ok"], true, "{r}");
        for _ in 0..4 {
            h.step();
        }
        h.request("ui.move", json!({"x": 3.0, "y": 3.0}), T);
        h.step();
        h.step();
        let image = h.paint();
        let confirm = rect(&h, "button:settingsClearCache:confirm");
        let top = egui::Rect::from_min_max(
            egui::pos2(confirm.center().x - 10.0, confirm.top() - 1.0),
            egui::pos2(confirm.center().x + 10.0, confirm.top() + 2.0),
        );
        assert!(strongest(&image, top, t.chrome) >= 3.0, "{kind:?}: a button in the question is {:.2}:1", strongest(&image, top, t.chrome));
        h.request("ui.key", json!({"key": "Escape"}), T);
        h.step();
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
