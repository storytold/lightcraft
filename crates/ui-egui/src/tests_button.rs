//! The button component: its sizes, its looks in every theme and state, what screen readers hear,
//! and that a disabled one can't be clicked.

use egui::{Color32, Pos2, Rect, pos2, vec2};

use crate::button::{Button, Kind, Size, State, contrast, look};
use crate::icons::Icon;
use crate::theme::{ThemeKind, Tokens};

/// Run frames of `draw` (with pointer `events` in the second), returning the AccessKit nodes'
/// (role, name) and what `draw` reported.
fn frames<T: Default>(events: Vec<egui::Event>, mut draw: impl FnMut(&mut egui::Ui) -> T) -> (Vec<(String, String)>, T) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut found = Vec::new();
    let mut last = T::default();
    for frame in 0..3 {
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))),
            events: if frame == 1 { events.clone() } else { Vec::new() },
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| last = draw(ui));
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            found = update.nodes.iter().map(|(_, n)| (format!("{:?}", n.role()), n.label().unwrap_or_default().to_string())).collect();
        }
    }
    (found, last)
}

#[test]
fn the_three_sizes_and_an_icon_alone() {
    let (_, sizes) = frames(Vec::new(), |ui| {
        [Size::Small, Size::Medium, Size::Large]
            .map(|s| (Button::new("Import Photos…").size(s).desired_size(ui), Button::icon_only(Icon::Trash, "Delete").size(s).desired_size(ui)))
            .to_vec()
    });
    let heights: Vec<f32> = sizes.iter().map(|(text, _)| text.y).collect();
    assert_eq!(heights, vec![24.0, 30.0, 36.0]);
    for (text, icon) in &sizes {
        assert_eq!(icon.x, icon.y, "an icon alone is square");
        assert!(text.x > icon.x, "text makes it wider");
    }
    // an icon beside the text makes it wider still, and a minimum width holds
    let (_, (plain, with_icon, wide)) = frames(Vec::new(), |ui| {
        (
            Button::new("Remove").desired_size(ui).x,
            Button::new("Remove").icon(Icon::Trash).desired_size(ui).x,
            Button::new("OK").min_width(120.0).desired_size(ui).x,
        )
    });
    assert!(with_icon > plain);
    assert_eq!(wide, 120.0);
}

#[test]
fn a_long_text_wraps_inside_its_max_width_and_grows_taller() {
    let label = "Add 1234 photos to My Photos right now, please";
    let (_, (free, capped, short)) = frames(Vec::new(), |ui| {
        (
            Button::new(label).small().desired_size(ui),
            Button::new(label).small().max_width(120.0).desired_size(ui),
            Button::new("OK").small().max_width(120.0).desired_size(ui),
        )
    });
    assert!(free.x > 120.0, "one line is wider than the cap");
    assert!(capped.x <= 120.0 && capped.y > free.y, "capped: {capped:?}, free: {free:?}");
    let (_, cut) = frames(Vec::new(), |ui| Button::new(label).small().max_width(120.0).truncate().desired_size(ui));
    assert!(cut.x <= 120.0 && cut.y == Size::Small.height(), "truncated: one line, {cut:?}");
    assert_eq!(short.y, Size::Small.height(), "a text that fits keeps its size's height");
}

#[test]
fn a_trailing_icon_is_painted_after_the_text_and_monospace_changes_the_face() {
    let text_start = |trailing: bool| {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 100.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let b = Button::new("Back");
            let b = if trailing { b.trailing_icon(Icon::ChevronRight) } else { b.icon(Icon::ChevronRight) };
            b.show(ui);
        });
        out.textures_delta.clear();
        let mut text_x = f32::MAX;
        for s in &out.shapes {
            if let egui::epaint::Shape::Text(t) = &s.shape {
                text_x = text_x.min(t.pos.x);
            }
        }
        text_x
    };
    let leading = text_start(false);
    let trailing = text_start(true);
    assert!(leading > trailing, "an icon before the text pushes it right; one after does not ({leading} vs {trailing})");

    let (_, (plain, mono)) =
        frames(Vec::new(), |ui| (Button::new("{date}").desired_size(ui).x, Button::new("{date}").monospace().desired_size(ui).x));
    assert_ne!(plain, mono, "another face has other widths");
}

#[test]
fn verbatim_text_is_not_translated_and_align_start_pins_the_content() {
    crate::i18n::set_language(crate::i18n::Locale::De);
    // "Cancel" is a UI string (translated); as data, an album that happens to be called so is not
    let (names, _) = frames(Vec::new(), |ui| {
        Button::new("Cancel").show(ui);
        Button::new("Cancel").verbatim().id_salt("v").show(ui);
    });
    crate::i18n::set_language(crate::i18n::Locale::En);
    let labels: Vec<&str> = names.iter().map(|(_, n)| n.as_str()).collect();
    assert!(labels.contains(&"Cancel"), "verbatim keeps the text: {labels:?}");
    assert!(labels.iter().any(|l| *l != "Cancel" && !l.is_empty()), "the other is translated: {labels:?}");

    // a wide button: centred text starts well right of the left edge; aligned to the start it doesn't
    let text_x = |start: bool| {
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 100.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let b = Button::new("Album").small();
            let b = if start { b.align_start() } else { b };
            b.show_at(ui, Rect::from_min_size(Pos2::ZERO, vec2(300.0, 24.0)));
        });
        out.textures_delta.clear();
        out.shapes.iter().filter_map(|s| if let egui::epaint::Shape::Text(t) = &s.shape { Some(t.pos.x) } else { None }).fold(f32::MAX, f32::min)
    };
    assert!(text_x(true) < 20.0 && text_x(false) > 100.0, "{} vs {}", text_x(true), text_x(false));
}

#[test]
fn a_screen_reader_hears_every_button_by_its_name() {
    let (found, _) = frames(Vec::new(), |ui| {
        Button::new("Import Photos…").primary().show(ui);
        Button::icon_only(Icon::Trash, "Delete mask").ghost().show(ui);
        Button::new("Remove").icon(Icon::Trash).danger().enabled(false).show(ui);
    });
    let has = |label: &str| found.iter().any(|(r, l)| r == "Button" && l == label);
    assert!(has("Import Photos…"), "{found:?}");
    assert!(has("Delete mask"), "an icon alone is named: {found:?}");
    assert!(has("Remove"), "a disabled one is still announced: {found:?}");
}

#[test]
fn text_on_a_fill_reads_at_aa_in_every_theme_and_state() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind(kind);
        for k in [Kind::Primary, Kind::Danger, Kind::Secondary] {
            for state in [State::Idle, State::Hovered, State::Pressed] {
                let lk = look(&t, k, state);
                let fill = lk.fill.unwrap_or(t.canvas);
                let c = contrast(lk.content, fill);
                assert!(c >= 4.5, "{kind:?} {k:?} {state:?}: {c:.2}:1");
            }
        }
        // a ghost's text on the surfaces it sits on
        let ghost = look(&t, Kind::Ghost, State::Idle);
        assert!(ghost.fill.is_none());
        assert!(contrast(ghost.content, t.chrome) >= 4.5, "{kind:?} ghost on chrome");
    }
}

#[test]
fn hover_and_press_are_darker_and_disabled_is_faded() {
    let t = Tokens::for_kind(ThemeKind::Charcoal);
    let luma = |c: Color32| u32::from(c.r()) + u32::from(c.g()) + u32::from(c.b());
    let fill = |s| look(&t, Kind::Primary, s).fill.map(luma).unwrap_or(0);
    assert!(fill(State::Hovered) < fill(State::Idle));
    assert!(fill(State::Pressed) < fill(State::Hovered));
    let disabled = look(&t, Kind::Primary, State::Disabled);
    assert!(disabled.content.a() < 255, "faded");
}

#[test]
fn a_disabled_button_cannot_be_clicked() {
    let at = pos2(40.0, 15.0);
    let click = vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
        egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    ];
    let place = Rect::from_min_size(Pos2::ZERO, vec2(120.0, 30.0));
    let mut disabled_clicked = false;
    let _ = frames(click.clone(), |ui| {
        if Button::new("Delete").danger().enabled(false).show_at(ui, place).clicked() {
            disabled_clicked = true;
        }
    });
    assert!(!disabled_clicked, "on any frame");
    let mut seen = false;
    let _ = frames(click, |ui| {
        if Button::new("Delete").danger().show_at(ui, place).clicked() {
            seen = true;
        }
    });
    assert!(seen, "an enabled one is");
}

#[test]
fn buttons_in_one_place_with_the_same_text_are_told_apart() {
    // two "Delete mask" buttons side by side, placed by the caller: two widgets, not one
    let (found, _) = frames(Vec::new(), |ui| {
        let a = Button::icon_only(Icon::Trash, "Delete mask").show_at(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(30.0, 30.0)));
        let b = Button::icon_only(Icon::Trash, "Delete mask").show_at(ui, Rect::from_min_size(pos2(40.0, 0.0), vec2(30.0, 30.0)));
        assert_ne!(a.id, b.id);
    });
    assert_eq!(found.iter().filter(|(r, l)| r == "Button" && l == "Delete mask").count(), 2, "{found:?}");
}

#[test]
fn a_button_in_a_disabled_part_of_the_window_says_it_is_disabled() {
    // (egui refuses the click there anyway: what the button adds is looking and reading disabled)
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    ctx.enable_accesskit();
    let mut disabled = None;
    for _ in 0..3 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(800.0, 600.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            ui.add_enabled_ui(false, |ui| {
                let resp = Button::new("Delete").show_at(ui, Rect::from_min_size(Pos2::ZERO, vec2(120.0, 30.0)));
                // drawn as disabled: it asks for hover only, as a button turned off does
                assert!(!resp.sense.senses_click(), "drawn as a button that can be clicked");
            });
        });
        out.textures_delta.clear();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            disabled = update.nodes.iter().find(|(_, n)| n.label() == Some("Delete")).map(|(_, n)| n.is_disabled());
        }
    }
    assert_eq!(disabled, Some(true), "a screen reader hears it disabled");
}
