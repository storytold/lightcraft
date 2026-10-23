//! The Settings tabs look like tabs and not like the choices under them, the open tab and a chosen
//! option are marked by more than a slightly different fill, and a tab or choice reached with the
//! keyboard shows that it has the focus.
//!
//! Before, one widget drew both: on General the languages read as a second tab bar, what was
//! chosen stood 1.35 to 1.56:1 against what was not, and Tab moved a focus nobody could see (the
//! picture with and without it was the same, pixel for pixel).

use std::time::Duration;

use egui::Color32;
use serde_json::json;

use crate::headless::Headless;
use crate::panels::settings::TABS;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 900.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(T);
    for _ in 0..4 {
        h.step();
    }
}

fn show(h: &mut Headless, tab: &str) {
    run(h, "app.settings", json!({"tab": tab}));
}

fn key(h: &mut Headless, key: &str) {
    assert_eq!(h.request("ui.key", json!({"key": key}), T)["ok"], true, "{key}");
    for _ in 0..3 {
        h.step();
    }
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn tab(h: &Headless, id: &str) -> egui::Rect {
    rect(h, &format!("button:settingsTab-{id}"))
}

/// How many pixels of `rect` are `colour` (one pixel per point here).
fn count(image: &egui::ColorImage, rect: egui::Rect, colour: Color32) -> usize {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().round().max(0.0) as usize, (rect.right().round().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().round().max(0.0) as usize, (rect.bottom().round().max(0.0) as usize).min(h));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|(x, y)| image.pixels[y * w + x] == colour).count()
}

/// How many pixels of the focus ring's colour run round `rect`: along its top and its two sides
/// (not its bottom, where the open tab has its line in the same colour).
fn ring(image: &egui::ColorImage, rect: egui::Rect, colour: Color32) -> usize {
    let [top, _, left, right] = edges(rect, 2.0);
    [top, left, right].iter().map(|e| count(image, *e, colour)).sum()
}

/// The outermost ring of `rect`, `width` points thick, as its four edges.
fn edges(rect: egui::Rect, width: f32) -> [egui::Rect; 4] {
    [
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.top() + width)),
        egui::Rect::from_min_max(egui::pos2(rect.left(), rect.bottom() - width), rect.max),
        egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + width, rect.bottom())),
        egui::Rect::from_min_max(egui::pos2(rect.right() - width, rect.top()), rect.max),
    ]
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

#[test]
fn the_open_tab_is_underlined_and_the_others_are_not() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        // Given Settings on Performance
        show(&mut h, "performance");
        let image = h.paint();
        for (id, _) in TABS {
            let r = tab(&h, id);
            // Then a line in the text colour runs under the open tab's whole name, and under no other tab
            let line = count(&image, egui::Rect::from_min_max(egui::pos2(r.left(), r.bottom() - 2.0), r.max), t.text);
            if *id == "performance" {
                assert!(line as f32 >= 2.0 * (r.width() - 2.0), "{kind:?}: the open tab {id} is underlined ({line} px of {r:?})");
            } else {
                assert_eq!(line, 0, "{kind:?}: {id} is not open and has no line");
            }
        }
        // And that line stands out from the window whatever the theme (3:1 for a mark that is not text)
        assert!(contrast(t.text, t.chrome) >= 3.0, "{kind:?}: {}", contrast(t.text, t.chrome));
    }
}

#[test]
fn tabs_are_not_drawn_as_the_choices_under_them_are() {
    let mut h = demo();
    let t = Tokens::get(&h.view.ctx);
    // Given General, where a row of language choices sits right under the tabs
    show(&mut h, "general");
    let image = h.paint();
    // Then a choice is a bordered button
    let choice = rect(&h, "button:settingsLanguage-4");
    let border: usize = edges(choice, 1.0).iter().map(|e| count(&image, *e, t.button_border)).sum();
    assert!(border as f32 > choice.width(), "a choice has a border ({border} px)");
    // And a tab that is not open is its name alone on the window: no border, no fill of its own
    let import = tab(&h, "import");
    let border: usize = edges(import, 1.0).iter().map(|e| count(&image, *e, t.button_border)).sum();
    assert_eq!(border, 0, "a tab has no border");
    assert_eq!(count(&image, import, t.button), 0, "a tab has no button fill");
    assert!(count(&image, import, t.chrome) as f32 > 0.6 * import.area(), "a tab is drawn on the window itself");
}

#[test]
fn a_tab_reached_with_the_keyboard_shows_the_focus() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        let import = tab(&h, "import");
        let ring = |h: &Headless| ring(&h.paint(), import, t.text);
        // Given no tab has the focus, there is no ring round Import
        assert!(ring(&h) < 8, "{kind:?}: no ring before ({})", ring(&h));
        // When Tab is pressed (the tab bar is one stop, at General) and then → (to Import)
        key(&mut h, "Tab");
        key(&mut h, "ArrowRight");
        // Then a ring in the text colour runs round Import
        let length = import.width() + 2.0 * import.height();
        assert!(ring(&h) as f32 > 1.5 * length, "{kind:?}: Import shows the focus ({} px of ring along {length} pt)", ring(&h));
        // And it is still General that is open: the focus is not the choice
        assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "general".into() }));
    }
}

#[test]
fn the_arrow_keys_move_along_the_tabs_and_space_opens_one() {
    let mut h = demo();
    let t = Tokens::get(&h.view.ctx);
    show(&mut h, "general");
    let ring = |h: &Headless, id: &str| ring(&h.paint(), tab(h, id), t.text);
    // Given the focus on the first tab
    key(&mut h, "Tab");
    assert!(ring(&h, "general") > 60, "General has the focus");
    // When → is pressed twice
    key(&mut h, "ArrowRight");
    key(&mut h, "ArrowRight");
    // Then the focus is on the third tab, and nothing has opened yet
    assert!(ring(&h, "performance") > 60, "Performance has the focus");
    assert!(ring(&h, "general") < 60 && ring(&h, "import") < 60, "and no other tab has");
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "general".into() }));
    // And ← goes back one
    key(&mut h, "ArrowLeft");
    assert!(ring(&h, "import") > 60, "Import has the focus");
    // When Space is pressed, that tab opens and keeps the focus
    key(&mut h, "Space");
    assert_eq!(h.app.ui.dialog, Some(crate::state::Dialog::Settings { tab: "import".into() }));
    assert!(ring(&h, "import") > 60, "Import still has the focus");
}

#[test]
fn a_chosen_option_has_an_outline_and_the_others_do_not() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        // Given Performance, where Preview size is Automatic (the first choice)
        show(&mut h, "performance");
        assert_eq!(h.app.ui.settings.preview_limit, 0);
        let image = h.paint();
        let outline = |id: &str| edges(rect(&h, id), 1.0).iter().map(|e| count(&image, *e, t.text_label)).sum::<usize>();
        // Then the chosen one is outlined in the label colour, and one that is not chosen is not
        let chosen = rect(&h, "button:settingsPreview-0");
        assert!(
            outline("button:settingsPreview-0") as f32 > chosen.width() + chosen.height(),
            "{kind:?}: the chosen size is outlined ({})",
            outline("button:settingsPreview-0")
        );
        assert!(outline("button:settingsPreview-1") < 8, "{kind:?}: another size is not ({})", outline("button:settingsPreview-1"));
        // And an outline is not the focus ring: nothing in the ring's colour runs round the chosen one
        assert!(ring(&image, chosen, t.text) < 8, "{kind:?}: chosen is not focused ({})", ring(&image, chosen, t.text));
        // And the outline stands out from the button it is on, from one that is not chosen and from the window
        for (what, other) in [("its fill", t.pressed), ("a button", t.button), ("the window", t.chrome)] {
            assert!(contrast(t.text_label, other) >= 3.0, "{kind:?}: the outline against {what} is {}", contrast(t.text_label, other));
        }
    }
}

#[test]
fn a_choice_reached_with_the_keyboard_shows_the_focus() {
    let mut h = demo();
    let t = Tokens::get(&h.view.ctx);
    show(&mut h, "performance");
    let ring = |h: &Headless, id: &str| ring(&h.paint(), rect(h, id), t.text);
    assert!(ring(&h, "button:settingsPreview-1") < 8);
    // When Tab is pressed until the preview sizes have the focus (the tabs and the GPU checkbox
    // come first; the row's stop is the size that holds, the first) and then → (to the second)
    let reached = (0..40).any(|_| {
        key(&mut h, "Tab");
        ring(&h, "button:settingsPreview-0") > 60
    });
    key(&mut h, "ArrowRight");
    let reached = reached && ring(&h, "button:settingsPreview-1") > 60;
    // Then it shows a ring, and is not chosen by that
    assert!(reached, "a choice shows the focus");
    assert_eq!(h.app.ui.settings.preview_limit, 0);
    // And Space chooses it
    key(&mut h, "Space");
    assert_eq!(h.app.ui.settings.preview_limit, crate::state::PREVIEW_LIMITS[1]);
}

/// What a screen reader is told: a choice is a radio button that says whether it is the one
/// chosen, a tab a button that says whether it is the one open.
#[test]
fn choices_and_tabs_say_what_they_are_and_which_one_holds() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut found = Vec::new();
    for _ in 0..3 {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut row = crate::widgets::Roving::new(ui, "sizes", Some(0));
            let _ = crate::widgets::choice_button_in(ui, &mut row, "size-0", "Automatic", true);
            let _ = crate::widgets::choice_button_in(ui, &mut row, "size-1", "1600 px", false);
            row.end(ui);
            let mut bar = crate::widgets::Roving::new(ui, "tabs", Some(0));
            let _ = crate::widgets::tab_button_in(ui, &mut bar, "tab-general", "General", true);
            let _ = crate::widgets::tab_button_in(ui, &mut bar, "tab-import", "Import", false);
            bar.end(ui);
        });
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            found = update
                .nodes
                .iter()
                .map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string(), format!("{:?}", n.toggled())))
                .collect();
        }
    }
    let has =
        |role: &str, label: &str, on: bool| found.iter().any(|(r, l, t)| r == role && l == label && t.contains(if on { "True" } else { "False" }));
    assert!(has("RadioButton", "Automatic", true), "{found:?}");
    assert!(has("RadioButton", "1600 px", false), "{found:?}");
    assert!(has("Button", "General", true), "{found:?}");
    assert!(has("Button", "Import", false), "{found:?}");
}

#[test]
fn tabs_on_two_lines_each_stand_on_a_line_of_their_own() {
    // Given an app window too narrow for the tabs in one line
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [480.0, 640.0], 1.0);
    h.settle(T);
    let t = Tokens::get(&h.view.ctx);
    show(&mut h, "general");
    let tabs: Vec<egui::Rect> = TABS.iter().map(|(id, _)| tab(&h, id)).collect();
    let first = tabs[0];
    let lower: Vec<&egui::Rect> = tabs.iter().filter(|r| r.top() >= first.bottom()).collect();
    assert!(!lower.is_empty(), "the tabs take more than one line: {tabs:?}");
    let image = h.paint();
    let window = rect(&h, "dialog:window");
    // Then a line the width of the window runs right under each line of tabs, the first too,
    // where the open tab is: its own line does not hang in the air over the names below
    for bottom in [first.bottom().round(), lower[0].bottom().round()] {
        let strip = egui::Rect::from_min_max(egui::pos2(window.left(), bottom), egui::pos2(window.right(), bottom + 1.0));
        assert!(
            count(&image, strip, t.divider) as f32 > 0.8 * (window.width() - 32.0),
            "a line runs under the tabs at y = {bottom} ({} px)",
            count(&image, strip, t.divider)
        );
    }
    // And the lower line of tabs starts clear of it
    assert!(lower[0].top() >= first.bottom() + 4.0, "the second line of tabs {:?} is clear of the first {first:?}", lower[0]);
}
