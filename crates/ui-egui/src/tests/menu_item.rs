//! The menu row and the shortcut display: what a checked row tells screen readers and draws, that a
//! disabled row can't be chosen, how a shortcut reads on each platform, that a tooltip follows a
//! remapped key, and that the keys read on their keycaps in every theme.

use egui::{Event, Key, Modifiers, PointerButton, Pos2, Rect, Shape, pos2, vec2};

use crate::button::contrast;
use crate::kbd::{Kbd, tip_with_shortcut};
use crate::menu_item::{MenuItem, ROW_H};
use crate::theme::{ThemeKind, Tokens};

/// What a run of frames of `draw` found out.
#[derive(Default)]
struct Run {
    /// AccessKit nodes: (role, name, toggled).
    nodes: Vec<(String, String, Option<bool>)>,
    /// Each row's response rect from the last frame, and whether it was clicked in any frame.
    rects: Vec<Rect>,
    clicked: Vec<bool>,
    shapes: Vec<Rect>,
}

fn flatten(shape: &Shape, out: &mut Vec<Rect>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| flatten(s, out)),
        Shape::Path(_) | Shape::LineSegment { .. } => out.push(shape.visual_bounding_rect()),
        _ => {}
    }
}

/// Draw `rows` in a menu-like vertical layout for four frames; the pointer presses and releases
/// the centre of row `press` (when given) in frames 1–2, `keys` are sent in frame 1.
fn run(press: Option<usize>, keys: Vec<Event>, rows: impl Fn(&mut egui::Ui) -> Vec<egui::Response>) -> Run {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    ctx.enable_accesskit();
    let mut r = Run::default();
    for frame in 0..4 {
        let mut events = Vec::new();
        if let Some(i) = press.filter(|_| frame >= 1)
            && let Some(rect) = r.rects.get(i)
        {
            let at: Pos2 = rect.center();
            match frame {
                1 => {
                    events.push(Event::PointerMoved(at));
                    events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
                    events.extend(keys.clone());
                }
                2 => events.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }),
                _ => {}
            }
        } else if frame == 1 {
            events.extend(keys.clone());
        }
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0))), events, ..Default::default() };
        let mut found = Vec::new();
        let mut out = ctx.run_ui(raw, |ui| {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| found = rows(ui));
        });
        out.textures_delta.clear();
        r.rects = found.iter().map(|f| f.rect).collect();
        if r.clicked.len() < found.len() {
            r.clicked.resize(found.len(), false);
        }
        for (i, f) in found.iter().enumerate() {
            r.clicked[i] |= f.clicked();
        }
        r.shapes.clear();
        for s in &out.shapes {
            flatten(&s.shape, &mut r.shapes);
        }
        if let Some(update) = out.platform_output.accesskit_update.take() {
            r.nodes = update
                .nodes
                .iter()
                .map(|(_, n)| {
                    (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string(), n.toggled().map(|t| t == egui::accesskit::Toggled::True))
                })
                .collect();
        }
    }
    r
}

#[test]
fn a_checked_row_says_it_is_checked_and_draws_its_check() {
    let rows = |ui: &mut egui::Ui| {
        vec![MenuItem::new("Show Grid").checked(true).show(ui), MenuItem::new("Show Guides").checked(false).show(ui), MenuItem::new("Plain").show(ui)]
    };
    let r = run(None, Vec::new(), rows);
    let state = |name: &str| r.nodes.iter().find(|(_, n, _)| n == name).map(|(_, _, t)| *t);
    assert_eq!(state("Show Grid"), Some(Some(true)), "{:?}", r.nodes);
    assert_eq!(state("Show Guides"), Some(Some(false)), "{:?}", r.nodes);
    assert_eq!(state("Plain"), Some(None), "an ordinary row isn't a toggle: {:?}", r.nodes);
    // the check is a vector drawn inside the reserved slot of the checked row only
    let in_slot = |row: Rect| r.shapes.iter().filter(|s| row.contains_rect(**s) && s.left() < row.left() + 30.0).count();
    assert!(in_slot(r.rects[0]) > 0, "no check drawn: {:?}", r.shapes);
    assert_eq!(in_slot(r.rects[1]), 0, "an unchecked row draws no check");
    // the slot is reserved on both checkable rows, so their labels line up
    assert_eq!(r.rects[0].height(), ROW_H);
    assert_eq!(r.rects[1].height(), ROW_H);
}

#[test]
fn a_disabled_row_ignores_clicks_and_an_enabled_one_answers() {
    let rows =
        |ui: &mut egui::Ui| vec![MenuItem::new("Paste").enabled(false).disabled_reason("Nothing copied").show(ui), MenuItem::new("Copy").show(ui)];
    let off = run(Some(0), Vec::new(), rows);
    assert_eq!(off.clicked, vec![false, false], "a disabled row was clicked");
    let on = run(Some(1), Vec::new(), rows);
    assert_eq!(on.clicked, vec![false, true]);
    // still announced, as off
    assert!(on.nodes.iter().any(|(r, n, _)| r == "Button" && n == "Paste"), "{:?}", on.nodes);
}

#[test]
fn rows_of_one_command_with_different_params_are_chosen_one_at_a_time() {
    // (Rating, Sort and Select by rows share the command's widget name: it names them for
    // automation and must not make them one egui widget)
    let rows = |ui: &mut egui::Ui| {
        vec![
            MenuItem::new("Set 1 Star").widget("menuitem:photo.rate").show(ui),
            MenuItem::new("Set 2 Stars").widget("menuitem:photo.rate").show(ui),
            MenuItem::new("Set 3 Stars").widget("menuitem:photo.rate").show(ui),
        ]
    };
    let second = run(Some(1), Vec::new(), rows);
    assert_eq!(second.clicked, vec![false, true, false], "only the pressed row answers");
}

#[test]
fn space_and_return_choose_the_focused_row() {
    for k in [Key::Enter, Key::Space] {
        let ev = |pressed| Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::NONE };
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let mut clicked = false;
        for frame in 0..4 {
            let events = match frame {
                1 => vec![],
                2 => vec![ev(true)],
                3 => vec![ev(false)],
                _ => vec![],
            };
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let r = MenuItem::new("Row").show(ui);
                if frame == 0 {
                    r.request_focus();
                }
                clicked |= r.clicked();
            });
            out.textures_delta.clear();
        }
        assert!(clicked, "{k:?} did not choose the focused row");
    }
}

#[test]
fn every_kind_of_row_is_one_height() {
    let r = run(None, Vec::new(), |ui| {
        vec![
            MenuItem::new("A").show(ui),
            MenuItem::new("B").checked(true).show(ui),
            MenuItem::new("C").icon(crate::icons::Icon::Star).show(ui),
            MenuItem::new("D").shortcut("Cmd+Shift+Z").show(ui),
            MenuItem::new("E").enabled(false).show(ui),
            MenuItem::new("F").swatch(egui::Color32::RED).show(ui),
        ]
    });
    assert!(r.rects.iter().all(|x| x.height() == ROW_H), "{:?}", r.rects);
}

#[test]
fn a_shortcut_reads_as_the_platform_writes_it() {
    assert_eq!(Kbd::new("Cmd+Shift+Z", true).text(), "⌘⇧Z");
    assert_eq!(Kbd::new("Cmd+Shift+Z", false).text(), "Ctrl+Shift+Z");
    assert_eq!(Kbd::new("Cmd+Shift+Z", true).keys(), ["⌘", "⇧", "Z"]);
    assert_eq!(Kbd::new("Cmd+Shift+Z", false).keys(), ["Ctrl", "Shift", "Z"]);
    assert_eq!(Kbd::new("Delete", true).keys(), ["⌫"]);
    // the plus key itself isn't lost to the separator
    assert_eq!(Kbd::new("Cmd++", false).keys(), ["Ctrl", "+"]);
    assert!(Kbd::new("", false).keys().is_empty());
}

#[test]
fn the_tooltip_shows_the_key_as_it_is_now_mapped() {
    let mut keymap = crate::shortcuts::Keymap::new();
    let default = tip_with_shortcut("Reset all edits", "develop.reset", &keymap, true);
    assert_eq!(default, "Reset all edits (⌘⇧R)");
    assert_eq!(tip_with_shortcut("Reset all edits", "develop.reset", &keymap, false), "Reset all edits (Ctrl+Shift+R)");
    keymap.insert("develop.reset".to_string(), "Shift+K".to_string());
    assert_eq!(tip_with_shortcut("Reset all edits", "develop.reset", &keymap, true), "Reset all edits (⇧K)");
    assert_eq!(tip_with_shortcut("Reset all edits", "develop.reset", &keymap, false), "Reset all edits (Shift+K)");
    // an unbound command (or one that has no key) has just its name
    assert_eq!(tip_with_shortcut("Activity", "no.such.command", &keymap, true), "Activity");
    // a key taken away leaves the plain text
    keymap.insert("develop.reset".to_string(), String::new());
    assert_eq!(tip_with_shortcut("Reset all edits", "develop.reset", &keymap, true), "Reset all edits");
}

#[test]
fn keys_read_on_their_keycaps_in_every_theme() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        let c = contrast(t.text_label, t.button);
        assert!(c >= 4.5, "{kind:?}: label on keycap {c:.2}:1");
        let c = contrast(t.text_disabled, t.chrome);
        assert!(c >= 1.5, "{kind:?}: disabled keys {c:.2}:1 are invisible");
    }
}

#[test]
fn a_keycap_row_is_as_wide_as_its_keys() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut sizes = Vec::new();
    let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(600.0, 400.0))), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| {
        sizes = vec![
            Kbd::new("Z", true).keycaps().size(ui).x,
            Kbd::new("Cmd+Shift+Z", true).keycaps().size(ui).x,
            Kbd::new("Cmd+Shift+Z", true).size(ui).x,
        ];
    });
    out.textures_delta.clear();
    assert!(sizes[1] > sizes[0], "{sizes:?}");
    assert!(sizes[1] > sizes[2], "keycaps are wider than the compact string: {sizes:?}");
}

/// Where each row's label starts, read from the AccessKit-free layout: the x of the galley is the
/// row's left plus its columns, so rows are compared by drawing them over several frames and
/// measuring the width a row grows by when a column is reserved.
#[test]
fn the_rows_of_a_mixed_menu_start_their_text_at_one_x() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    let mut text_x: Vec<f32> = Vec::new();
    for _ in 0..4 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(600.0, 400.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            ui.with_layout(egui::Layout::top_down_justified(egui::Align::LEFT), |ui| {
                MenuItem::new("Red").swatch(egui::Color32::RED).checked(true).show(ui);
                MenuItem::new("None").checked(false).show(ui);
                MenuItem::new("Edit Label Names…").show(ui);
            });
        });
        out.textures_delta.clear();
        text_x.clear();
        for clipped in &out.shapes {
            collect_text_x(&clipped.shape, &mut text_x);
        }
    }
    assert_eq!(text_x.len(), 3, "{text_x:?}");
    assert!(text_x.windows(2).all(|w| (w[0] - w[1]).abs() < 0.01), "labels start at different x: {text_x:?}");
}

fn collect_text_x(shape: &Shape, out: &mut Vec<f32>) {
    match shape {
        Shape::Vec(v) => v.iter().for_each(|s| collect_text_x(s, out)),
        Shape::Text(t) => out.push(t.pos.x),
        _ => {}
    }
}

#[test]
fn exclusive_choices_are_radio_buttons_and_independent_toggles_are_checkboxes() {
    let r = run(None, Vec::new(), |ui| {
        vec![
            MenuItem::new("Day").selected(true).show(ui),
            MenuItem::new("Month").selected(false).show(ui),
            MenuItem::new("Solo Mode").checked(true).show(ui),
            MenuItem::new("Group").selected(true).submenu(ui, |_| {}).response,
        ]
    });
    let role = |name: &str| r.nodes.iter().find(|(_, n, _)| n == name).map(|(role, _, t)| (role.clone(), *t));
    assert_eq!(role("Day"), Some(("RadioButton".to_string(), Some(true))), "{:?}", r.nodes);
    assert_eq!(role("Month"), Some(("RadioButton".to_string(), Some(false))));
    assert_eq!(role("Solo Mode"), Some(("CheckBox".to_string(), Some(true))));
    // a submenu row marking the current group is a plain button
    assert_eq!(role("Group"), Some(("Button".to_string(), None)));
}

#[test]
fn an_inert_current_choice_ignores_clicks_without_the_disabled_look() {
    let rows = |ui: &mut egui::Ui| vec![MenuItem::new("Display P3").selected(true).inert().show(ui)];
    let r = run(Some(0), Vec::new(), rows);
    assert_eq!(r.clicked, vec![false]);
    assert!(r.nodes.iter().any(|(role, n, t)| role == "RadioButton" && n == "Display P3" && *t == Some(true)), "{:?}", r.nodes);
}

#[test]
fn the_info_line_list_is_made_of_menu_rows() {
    use crate::state::LoupeInfoField as F;
    let r = run(None, Vec::new(), |ui| {
        let mut line = F::FileName;
        crate::panels::settings::info_line_rows(ui, "viewOptionsInfo1Line1", &mut line);
        Vec::new()
    });
    // a `MenuItem::selected` row announces itself as a radio button; a `selectable_label` doesn't
    let radios: Vec<_> = r.nodes.iter().filter(|(role, ..)| role == "RadioButton").collect();
    assert_eq!(radios.len(), F::ALL.len(), "{:?}", r.nodes);
    let current = crate::i18n::tr(F::FileName.label());
    assert!(radios.iter().any(|(_, n, t)| n == current && *t == Some(true)), "{:?}", r.nodes);
}
