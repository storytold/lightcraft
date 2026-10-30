//! Settings by keyboard: a short way to any setting, and one look for "the focus is here".
//!
//! Tab stops once at the tab bar and once at each row of choices (the open tab, the choice that
//! holds), and the arrow keys move inside; every control that has the focus is ringed 2 pt in
//! the text colour; a theme's favourite star can be reached and pressed without the mouse.
//!
//! Before, Tab stopped at each of the eight tabs and then each of the thirteen languages, a focused checkbox, button or list showed a thin line
//! of another colour than a focused tab or choice, a focused text box an accent outline, and the
//! star of a theme that was no favourite was only there under the mouse.

use std::time::Duration;

use egui::Color32;
use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn demo() -> Headless {
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 1000.0], 1.0);
    // let the first renders finish (a render still running at exit can crash in GPU teardown)
    h.settle(T);
    h
}

fn steps(h: &mut Headless) {
    for _ in 0..4 {
        h.step();
    }
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
    h.settle(T);
    steps(h);
}

fn show(h: &mut Headless, tab: &str) {
    run(h, "app.settings", json!({"tab": tab}));
}

fn key(h: &mut Headless, key: &str) {
    assert_eq!(h.request("ui.key", json!({"key": key}), T)["ok"], true, "{key}");
    steps(h);
}

fn back_tab(h: &mut Headless) {
    assert_eq!(h.request("ui.key", json!({"key": "Tab", "shift": true}), T)["ok"], true);
    steps(h);
}

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

/// The widget the keyboard focus is on, by the name it is registered under (a control's, not a
/// heading's or a page's), or "" when it is on none.
fn focused(h: &Headless) -> String {
    let Some(id) = h.view.ctx.memory(|m| m.focused()) else { return String::new() };
    let Some(at) = h.view.ctx.read_response(id).map(|r| r.rect) else { return String::new() };
    let near = |a: egui::Rect| (a.min - at.min).abs().max_elem() < 1.5 && (a.max - at.max).abs().max_elem() < 1.5;
    h.app
        .widgets
        .iter()
        .find(|(w, r)| ["button:", "check:", "radio:", "field:"].iter().any(|kind| w.starts_with(kind)) && near(*r))
        .map(|(w, _)| w.clone())
        .unwrap_or_else(|| format!("{at:?}"))
}

/// How many pixels within `rect` are exactly `colour` (one pixel per point here).
fn count(image: &egui::ColorImage, rect: egui::Rect, colour: Color32) -> usize {
    let [w, h] = image.size;
    let (x0, x1) = (rect.left().round().max(0.0) as usize, (rect.right().round().max(0.0) as usize).min(w));
    let (y0, y1) = (rect.top().round().max(0.0) as usize, (rect.bottom().round().max(0.0) as usize).min(h));
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|(x, y)| image.pixels[y * w + x] == colour).count()
}

/// How many pixels of `colour` lie in the band round the edge of `rect`: from 3 pt outside it to
/// 3 pt inside (a control's ring is drawn at its edge, a point or two either way).
fn band(image: &egui::ColorImage, rect: egui::Rect, colour: Color32) -> usize {
    count(image, rect.expand(3.0), colour) - count(image, rect.shrink(3.0), colour)
}

#[test]
fn tab_stops_once_at_the_tabs_and_once_at_each_row_of_choices() {
    let mut h = demo();
    show(&mut h, "general");
    // Given Settings just opened: the focus is in the filter box, which is ready to type in
    assert_eq!(focused(&h), "field:settingsFilter");
    // When Tab is pressed, it goes to the tab that is open
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsTab-general");
    // And again: to the language that is chosen, past the other seven tabs
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsLanguage-0");
    // And again: to the Lightroom catalog question, past the other twelve languages
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsLightroomImport-0");
    // And again: to Open in
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsStartup-0");
    // And the fifth press reaches the first checkbox of General
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "check:settings.autoAdvance");
    // And Shift+Tab goes back the same way, one stop a row
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsStartup-0");
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsLightroomImport-0");
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsLanguage-0");
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsTab-general");
}

#[test]
fn the_stop_of_a_row_is_the_choice_that_holds() {
    let mut h = demo();
    // Given LightCraft set to open in Detail (the third choice) and Interface open (the fifth tab)
    h.app.ui.settings.startup_view = crate::state::StartupView::Detail;
    show(&mut h, "general");
    for _ in 0..4 {
        key(&mut h, "Tab");
    }
    assert_eq!(focused(&h), "button:settingsStartup-2");
    // (Settings closed and opened on Interface: the focus starts in the filter again)
    h.app.ui.dialog = None;
    steps(&mut h);
    show(&mut h, "interface");
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsTab-interface");
}

#[test]
fn the_arrow_keys_move_inside_the_tabs_and_do_not_leave_them() {
    let mut h = demo();
    show(&mut h, "general");
    key(&mut h, "Tab");
    // → and ↓ go to the next tab, ← and ↑ to the one before; nothing opens by that
    key(&mut h, "ArrowRight");
    assert_eq!(focused(&h), "button:settingsTab-import");
    key(&mut h, "ArrowDown");
    assert_eq!(focused(&h), "button:settingsTab-performance");
    key(&mut h, "ArrowUp");
    assert_eq!(focused(&h), "button:settingsTab-import");
    key(&mut h, "ArrowLeft");
    assert_eq!(focused(&h), "button:settingsTab-general");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "general".into() }));
    // ← on the first stays there; End goes to the last, → stays there, Home goes to the first
    key(&mut h, "ArrowLeft");
    assert_eq!(focused(&h), "button:settingsTab-general");
    key(&mut h, "End");
    assert_eq!(focused(&h), "button:settingsTab-denoise");
    key(&mut h, "ArrowRight");
    assert_eq!(focused(&h), "button:settingsTab-denoise");
    key(&mut h, "Home");
    assert_eq!(focused(&h), "button:settingsTab-general");
    // Space opens the tab the focus is on, which keeps the focus and is the bar's stop from then on
    key(&mut h, "ArrowRight");
    key(&mut h, "Space");
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "import".into() }));
    assert_eq!(focused(&h), "button:settingsTab-import");
    key(&mut h, "Tab");
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsTab-import");
}

#[test]
fn the_arrow_keys_move_inside_a_row_of_choices_and_space_chooses() {
    let mut h = demo();
    show(&mut h, "general");
    for _ in 0..4 {
        key(&mut h, "Tab");
    }
    assert_eq!(focused(&h), "button:settingsStartup-0");
    // → moves the focus, and the setting stays what it was until Space
    key(&mut h, "ArrowRight");
    assert_eq!(focused(&h), "button:settingsStartup-1");
    assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Last);
    key(&mut h, "Space");
    assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Grid);
    // And the row is left with Tab from wherever in it the focus is: to the next control
    key(&mut h, "ArrowRight");
    assert_eq!(focused(&h), "button:settingsStartup-2");
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "check:settings.autoAdvance");
    // And coming back, the stop is the choice that holds
    back_tab(&mut h);
    assert_eq!(focused(&h), "button:settingsStartup-1");
}

#[test]
fn an_arrow_key_held_with_a_modifier_is_not_the_rows() {
    for modifier in ["cmd", "shift", "alt", "ctrl"] {
        let mut h = demo();
        show(&mut h, "general");
        for _ in 0..4 {
            key(&mut h, "Tab");
        }
        assert_eq!(focused(&h), "button:settingsStartup-0");
        // When ⌘, ⇧, ⌥ or ⌃ is held with →, or with Home or End, the focus stays where it was
        for k in ["ArrowRight", "ArrowDown", "End"] {
            assert_eq!(h.request("ui.key", json!({"key": k, modifier: true}), T)["ok"], true);
            steps(&mut h);
            assert_eq!(focused(&h), "button:settingsStartup-0", "{modifier}+{k}");
        }
        // And the same key without it moves the focus on
        key(&mut h, "ArrowRight");
        assert_eq!(focused(&h), "button:settingsStartup-1", "{modifier}");
    }
}

/// `key` pressed and let go within one frame, and that one frame drawn.
fn press(h: &mut Headless, key: egui::Key) {
    for pressed in [true, false] {
        h.events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    }
    h.step();
}

#[test]
fn arrow_keys_pressed_faster_than_frames_are_drawn_stay_in_the_row() {
    let mut h = demo();
    show(&mut h, "general");
    key(&mut h, "Tab");
    key(&mut h, "Tab");
    assert_eq!(focused(&h), "button:settingsLanguage-0");
    // Given the focus walked along the languages (so each of them has been the row's stop)
    for _ in 0..4 {
        key(&mut h, "ArrowRight");
    }
    assert_eq!(focused(&h), "button:settingsLanguage-4");
    // When ← arrives on one frame after the other, with none drawn in between
    for _ in 0..3 {
        press(&mut h, egui::Key::ArrowLeft);
    }
    steps(&mut h);
    // Then the focus has gone back along the row, and not up to the tabs
    assert_eq!(focused(&h), "button:settingsLanguage-1");
    for _ in 0..6 {
        press(&mut h, egui::Key::ArrowRight);
    }
    steps(&mut h);
    assert_eq!(focused(&h), "button:settingsLanguage-7");
}

#[test]
fn a_click_on_a_choice_or_tab_leaves_no_ring() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        show(&mut h, "performance");
        // (the top edge only: the open tab's own line is along the bottom, in the same colour)
        let ring_of = |h: &mut Headless, id: &str| {
            let r = rect(h, id);
            count(&h.paint(), egui::Rect::from_min_max(r.min, egui::pos2(r.right(), r.top() + 2.0)), t.text)
        };
        for id in ["button:settingsPreview-1", "button:settingsTab-import"] {
            let quiet = ring_of(&mut h, id);
            // When it is clicked (a choice is chosen, a tab opens), no ring is drawn round it
            assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true);
            steps(&mut h);
            let clicked = ring_of(&mut h, id);
            assert!(clicked < quiet + 8, "{kind:?}: a click on {id} leaves {clicked} px of ring (none: {quiet})");
            assert_ne!(focused(&h), id, "{kind:?}: a click does not give {id} the keyboard focus");
        }
    }
}

#[test]
fn a_click_on_a_choice_that_is_not_the_stop_still_chooses_it() {
    let mut h = demo();
    show(&mut h, "general");
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsStartup-2"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert_eq!(h.app.ui.settings.startup_view, crate::state::StartupView::Detail);
    let r = h.request("ui.clickWidget", json!({"id": "button:settingsTab-performance"}), T);
    assert_eq!(r["ok"], true, "{r}");
    steps(&mut h);
    assert_eq!(h.app.ui.dialog, Some(Dialog::Settings { tab: "performance".into() }));
}

#[test]
fn every_kind_of_control_shows_the_focus_by_a_ring_in_the_text_colour() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        // General has a text box, Performance a checkbox, a row of choices and a button
        show(&mut h, "performance");
        let controls = ["button:settingsTab-performance", "check:settings.gpu", "button:settingsPreview-0", "button:settingsClearCache"];
        // (how much of the text colour is at each control's edge with the focus elsewhere)
        key(&mut h, "Tab");
        let plain = h.paint();
        let mut before: Vec<usize> = controls.iter().map(|id| band(&plain, rect(&h, id), t.text)).collect();
        // (the tab has the focus in that picture: it is measured with the focus on the next stop)
        key(&mut h, "Tab");
        before[0] = band(&h.paint(), rect(&h, controls[0]), t.text);
        back_tab(&mut h);
        for (control, before) in controls.iter().zip(before) {
            let reached = (0..30).any(|_| {
                if focused(&h) == *control {
                    return true;
                }
                key(&mut h, "Tab");
                false
            });
            assert!(reached, "{kind:?}: Tab reaches {control} (the focus is on {})", focused(&h));
            let at = rect(&h, control);
            let ring = band(&h.paint(), at, t.text).saturating_sub(before);
            // Then a line in the text colour runs round the whole of it
            let least = at.width() + at.height();
            assert!(ring as f32 >= least, "{kind:?}: {control} shows the focus by {ring} px in the text colour (at least {least})");
        }
    }
}

#[test]
fn a_text_box_in_use_shows_the_focus_by_a_ring_in_the_text_colour() {
    for kind in ThemeKind::ALL {
        let mut h = demo();
        run(&mut h, "view.theme", json!({"theme": kind.id()}));
        let t = Tokens::for_kind(kind);
        show(&mut h, "general");
        let at = rect(&h, "field:photocraftPath");
        let before = band(&h.paint(), at, t.text);
        // When Tab is pressed until the PhotoCraft box has the focus
        let reached = (0..30).any(|_| {
            key(&mut h, "Tab");
            focused(&h) == "field:photocraftPath"
        });
        assert!(reached, "{kind:?}: Tab reaches the box (the focus is on {})", focused(&h));
        // Then a line in the text colour runs round the whole of it
        let ring = band(&h.paint(), at, t.text).saturating_sub(before);
        let least = at.width() + at.height();
        assert!(ring as f32 >= least, "{kind:?}: the box shows the focus by {ring} px in the text colour (at least {least})");
    }
}

#[test]
fn a_themes_star_is_reached_and_pressed_with_the_keyboard() {
    let mut h = demo();
    h.app.ui.settings.favorite_themes.clear();
    show(&mut h, "interface");
    // Given no theme is a favourite, and the mouse is nowhere near
    // When Tab is pressed until the star of Paper has the focus
    let reached = (0..40).any(|_| {
        key(&mut h, "Tab");
        focused(&h) == "button:themeFavorite-paper"
    });
    assert!(reached, "the star of a theme that is no favourite is a Tab stop (the focus is on {})", focused(&h));
    // Then the star is drawn there, ringed
    let t = Tokens::get(&h.view.ctx);
    let star = rect(&h, "button:themeFavorite-paper");
    let image = h.paint();
    assert!(band(&image, star, t.text) as f32 >= star.width() + star.height(), "the star shows the focus");
    let inside = star.shrink(5.0);
    let drawn = (inside.area() as usize).saturating_sub(count(&image, inside, t.inset));
    assert!(drawn > 12, "the star itself is drawn while it has the focus ({drawn} px of it)");
    // And the theme's radio button before it, with the focus, is ringed round the whole of it
    back_tab(&mut h);
    assert_eq!(focused(&h), "radio:theme-paper");
    let radio = rect(&h, "radio:theme-paper");
    let ring = band(&h.paint(), radio, t.text);
    assert!(ring as f32 >= radio.width() + radio.height(), "the radio button shows the focus by {ring} px");
    key(&mut h, "Tab");
    // And Space makes Paper a favourite, and the focus stays on the star
    key(&mut h, "Space");
    assert_eq!(h.app.ui.settings.favorite_themes, ["paper"]);
    assert_eq!(focused(&h), "button:themeFavorite-paper");
    key(&mut h, "Space");
    assert!(h.app.ui.settings.favorite_themes.is_empty());
}
