//! Increase Contrast (`view.increaseContrast`, Settings ▸ Interface): the theme tokens' borders,
//! separators and text reach WCAG's stronger ratios in every theme; the system's setting is followed
//! until the user chooses.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use serde_json::json;

use crate::button::contrast;
use crate::headless::Headless;
use crate::theme::{ThemeKind, Tokens};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

/// The surfaces text is written on: at rest, hovered, pressed, active and selected.
fn surfaces(t: &Tokens) -> [egui::Color32; 11] {
    [t.chrome, t.canvas, t.grid_bg, t.cell, t.inset, t.field, t.button, t.hover, t.pressed, t.tool_active, t.cell_selected]
}

#[test]
fn with_it_off_the_tokens_are_the_ones_each_theme_has_always_had() {
    let rgb = |v: u32| egui::Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
    for (kind, text, divider, button_border) in [
        (ThemeKind::Charcoal, 0xe2e2e2, 0x1c1c1c, 0x4c4c4c),
        (ThemeKind::Midnight, 0xe2e2e2, 0x0e0e0e, 0x404040),
        (ThemeKind::Silver, 0x1c1c1c, 0xc4c4c4, 0xb0b0b0),
        (ThemeKind::Paper, 0x161616, 0xe0e0e0, 0xc6c6c6),
    ] {
        let t = Tokens::for_kind_with(kind, false);
        assert_eq!((t.text, t.divider, t.button_border), (rgb(text), rgb(divider), rgb(button_border)), "{kind:?}");
        assert!(contrast(t.text_dim, t.chrome) < 7.0, "{kind:?}: the dim text is not raised");
        assert_eq!(format!("{t:?}"), format!("{:?}", Tokens::for_kind(kind)), "{kind:?}");
    }
}

#[test]
fn increased_text_reads_7_to_1_on_every_surface_in_every_theme() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind_with(kind, true);
        for (name, ink) in [("text", t.text), ("text_label", t.text_label), ("text_dim", t.text_dim)] {
            for on in surfaces(&t) {
                assert!(contrast(ink, on) >= 7.0, "{kind:?}: {name} {ink:?} on {on:?}: {:.2}", contrast(ink, on));
            }
        }
        for on in surfaces(&t) {
            assert!(contrast(t.icon, on) >= 4.5, "{kind:?}: icon on {on:?}: {:.2}", contrast(t.icon, on));
            assert!(contrast(t.text_disabled, on) >= 3.0, "{kind:?}: disabled text on {on:?}: {:.2}", contrast(t.text_disabled, on));
        }
    }
}

#[test]
fn increased_borders_and_separators_stand_3_to_1_from_what_they_part() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind_with(kind, true);
        for (name, line, parts) in [
            ("field border", t.field_border, [t.field, t.chrome, t.inset]),
            ("button border", t.button_border, [t.button, t.chrome, t.inset]),
            ("hovered button border", t.button_border, [t.hover, t.pressed, t.chrome]),
            ("divider", t.divider, [t.chrome, t.canvas, t.grid_bg]),
            ("slider track", t.track, [t.chrome, t.inset, t.canvas]),
        ] {
            for on in parts {
                assert!(contrast(line, on) >= 3.0, "{kind:?}: {name} {line:?} on {on:?}: {:.2}", contrast(line, on));
            }
        }
        for (name, thumb) in [("thumb", t.thumb), ("hovered thumb", t.thumb_hover)] {
            assert!(
                contrast(thumb, t.track) >= 3.0 && contrast(thumb, t.chrome) >= 3.0,
                "{kind:?}: the {name} stands out from the track and the panel"
            );
        }
    }
}

#[test]
fn the_hovered_thumb_still_differs_from_the_resting_one() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind_with(kind, true);
        assert_ne!(t.thumb, t.thumb_hover, "{kind:?}");
        assert!(contrast(t.thumb_hover, t.chrome) >= contrast(t.thumb, t.chrome), "{kind:?}: the hovered thumb is the stronger one");
        // (the raised resting thumb stands short of the text color: the hovered one keeps a visible step beyond it)
        assert!(contrast(t.thumb_hover, t.thumb) >= 1.1, "{kind:?}: hovered vs resting thumb {:.2}", contrast(t.thumb_hover, t.thumb));
    }
}

#[test]
fn increasing_contrast_never_weakens_a_token() {
    for kind in ThemeKind::ALL {
        let (a, b) = (Tokens::for_kind(kind), Tokens::for_kind_with(kind, true));
        for (name, from, to, on) in [
            ("text", a.text, b.text, a.chrome),
            ("text_label", a.text_label, b.text_label, a.chrome),
            ("text_dim", a.text_dim, b.text_dim, a.chrome),
            ("icon", a.icon, b.icon, a.chrome),
            ("field_border", a.field_border, b.field_border, a.field),
            ("button_border", a.button_border, b.button_border, a.button),
            ("divider", a.divider, b.divider, a.chrome),
        ] {
            assert!(contrast(to, on) >= contrast(from, on), "{kind:?}: {name} weakened");
        }
        // the surfaces themselves are the theme's: only lines and ink change
        assert_eq!((a.chrome, a.canvas, a.grid_bg, a.button, a.field), (b.chrome, b.canvas, b.grid_bg, b.button, b.field), "{kind:?}");
    }
}

#[test]
fn the_selection_keeps_its_fill_and_the_open_and_disabled_looks_hold_up() {
    for kind in ThemeKind::ALL {
        let on = Tokens::for_kind_with(kind, true);
        let off_ctx = egui::Context::default();
        crate::theme::apply_kind(&off_ctx, kind);
        let ctx = egui::Context::default();
        crate::increase_contrast::set(&ctx, true);
        crate::theme::apply_kind(&ctx, kind);
        let v = ctx.global_style().visuals.clone();
        // (the fill is the only sign a row is selected: it stays as far from the panel as it was)
        assert_eq!(v.selection.bg_fill, off_ctx.global_style().visuals.selection.bg_fill, "{kind:?}");
        assert_eq!(v.selection.stroke.width, 1.5, "{kind:?}: a focused field keeps the outline's weight");
        assert_eq!(v.widgets.open.bg_stroke.color, on.button_border, "{kind:?}: the open drop-down's outline");
        assert!(contrast(v.widgets.open.bg_stroke.color, on.chrome) >= 3.0, "{kind:?}");
        assert_eq!(v.disabled_alpha, 0.7, "{kind:?}");
        // off: egui's own
        let ctx = egui::Context::default();
        crate::theme::apply_kind(&ctx, kind);
        let v = ctx.global_style().visuals.clone();
        assert_eq!((v.disabled_alpha, v.selection.stroke.width), (0.5, 1.0), "{kind:?}");
    }
}

#[test]
fn dim_text_is_never_stronger_than_the_label() {
    for kind in ThemeKind::ALL {
        let t = Tokens::for_kind_with(kind, true);
        assert!(contrast(t.text_dim, t.chrome) <= contrast(t.text_label, t.chrome), "{kind:?}");
    }
}

#[test]
fn the_theme_previews_in_settings_follow_the_setting() {
    // (the preview paints the histogram's area in the theme's track color at half strength)
    let track = |increased: bool| Tokens::for_kind_with(ThemeKind::Charcoal, increased).track.gamma_multiply(0.5);
    assert_ne!(track(false), track(true), "the track color differs, or this test cannot tell");
    let mut h = detail();
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    for on in [false, true] {
        run(&mut h, "view.increaseContrast", json!({"on": on}));
        h.settle(SETTLE);
        let fills: Vec<_> =
            h.view.painted_shapes().into_iter().filter_map(|s| if let egui::epaint::Shape::Rect(r) = s { Some(r.fill) } else { None }).collect();
        assert!(fills.contains(&track(on)), "increased: {on}: the preview paints its track in {:?}", track(on));
        assert!(!fills.contains(&track(!on)), "increased: {on}: and not in the other");
    }
}

#[test]
fn increased_lines_are_drawn_thicker_in_egui_s_widgets() {
    for increased in [false, true] {
        let ctx = egui::Context::default();
        crate::increase_contrast::set(&ctx, increased);
        crate::theme::apply_kind(&ctx, ThemeKind::Charcoal);
        let v = &ctx.global_style().visuals;
        let want = if increased { 1.5 } else { 1.0 };
        let w = &v.widgets;
        let widths = [
            v.window_stroke.width,
            w.noninteractive.bg_stroke.width,
            w.inactive.bg_stroke.width,
            w.hovered.bg_stroke.width,
            w.active.bg_stroke.width,
            w.open.bg_stroke.width,
        ];
        assert_eq!(widths, [want; 6], "increased: {increased}");
        assert_eq!(Tokens::get(&ctx).divider, Tokens::for_kind_with(ThemeKind::Charcoal, increased).divider, "the context carries the tokens");
    }
}

fn detail() -> Headless {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "none"}), T);
    h.settle(SETTLE);
    h
}

/// A headless app whose system says `system` (shared, so a test can change it).
fn with_system(system: Rc<Cell<Option<bool>>>) -> Headless {
    let mut app = LightcraftApp::new(
        lightcraft_engine::Session::with_demo(),
        Services { png: None, system_increase_contrast: Some(Box::new(move || system.get())), ..Default::default() },
    );
    app.ui.settings.gpu = false;
    let mut h = Headless::new(app, [1000.0, 700.0], 1.0);
    h.request("ui.set", json!({"view": "detail", "right": "none"}), T);
    h.settle(SETTLE);
    h
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn divider(h: &Headless) -> egui::Color32 {
    Tokens::get(&h.view.ctx).divider
}

#[test]
fn the_system_setting_is_followed_until_the_user_chooses() {
    let system = Rc::new(Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    let (plain, strong) = (Tokens::for_kind(ThemeKind::Charcoal).divider, Tokens::for_kind_with(ThemeKind::Charcoal, true).divider);
    assert_ne!(plain, strong, "the tokens differ");
    assert_eq!(h.app.ui.settings.increase_contrast, None);
    assert_eq!(divider(&h), strong, "the system says increase");
    // the user chooses to keep the usual look: the system no longer decides
    let r = run(&mut h, "view.increaseContrast", json!({"on": false}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(false), json!(false)));
    assert_eq!(divider(&h), plain);
    // back to following
    let r = run(&mut h, "view.increaseContrast", json!({"follow": true}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(true)));
    assert_eq!(divider(&h), strong);
    // the system switches while the app runs: noticed within about a second
    system.set(Some(false));
    for _ in 0..90 {
        h.step();
    }
    assert!(!h.app.increase_contrast());
    assert_eq!(divider(&h), plain);
    // no params toggles what is in effect
    let r = run(&mut h, "view.increaseContrast", json!({}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(false)));
    assert_eq!(divider(&h), strong);
}

#[test]
fn the_choice_is_saved_and_a_state_from_before_loads() {
    let mut h = detail();
    run(&mut h, "view.increaseContrast", json!({"on": true}));
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.settings.increase_contrast, Some(true));
    assert_eq!(crate::state::AppSettings::default().increase_contrast, None, "follows the system by default");
    let mut old = serde_json::to_value(&h.app.ui).unwrap();
    old["settings"].as_object_mut().unwrap().remove("increaseContrast").expect("saved under its camelCase key");
    let loaded = serde_json::from_value::<crate::UiState>(old).unwrap().sanitized();
    assert_eq!(loaded.settings.increase_contrast, None, "state saved before the setting existed still loads");
}

#[test]
fn follow_true_is_refused_where_there_is_no_system_setting_and_changes_nothing() {
    let mut h = detail();
    run(&mut h, "view.increaseContrast", json!({"on": true}));
    let r = h.request("engine.execute", json!({"command": "view.increaseContrast", "params": {"follow": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("no Increase Contrast setting to follow"), "{r}");
    assert_eq!(h.app.ui.settings.increase_contrast, Some(true), "the choice made stays");
}

#[test]
fn the_settings_window_can_hand_the_choice_back_to_the_system() {
    let system = Rc::new(Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    h.settle(SETTLE);
    let no_button = h.request("ui.clickWidget", json!({"id": "button:settings.increaseContrastFollow"}), T);
    assert_ne!(no_button["ok"], true, "no button while following the system");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.increaseContrast"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.increase_contrast, Some(false), "the tick box shows the system's true and the click turns it off");
    let r = h.request("ui.clickWidget", json!({"id": "button:settings.increaseContrastFollow"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.increase_contrast, None);
    assert!(h.app.increase_contrast(), "follows the system again");
}

#[test]
fn the_settings_label_goes_through_the_catalog() {
    use crate::i18n::Locale;
    let mut h = detail();
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    h.app.run("app.language.german", json!({})).unwrap();
    h.settle(SETTLE);
    let texts = h.view.painted_text();
    h.app.run("app.language.english", json!({})).unwrap();
    assert!(texts.iter().any(|t| *t == Locale::De.tr("Increase contrast")), "the German label is drawn: {texts:?}");
    assert!(!texts.iter().any(|t| t == "Increase contrast"), "and not the English one");
}

#[test]
fn follow_true_is_refused_while_the_system_gives_no_answer_and_changes_nothing() {
    let system = Rc::new(Cell::new(None));
    let mut h = with_system(system.clone());
    run(&mut h, "view.increaseContrast", json!({"on": true}));
    let r = h.request("engine.execute", json!({"command": "view.increaseContrast", "params": {"follow": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert_eq!(h.app.ui.settings.increase_contrast, Some(true), "the choice made stays");
    // once the system answers, following is accepted
    system.set(Some(false));
    for _ in 0..90 {
        h.step();
    }
    let r = run(&mut h, "view.increaseContrast", json!({"follow": true}));
    assert_eq!(r["followsSystem"], json!(true));
}

#[test]
fn the_command_refuses_params_that_are_not_true_or_false_and_changes_nothing() {
    let mut h = detail();
    run(&mut h, "view.increaseContrast", json!({"on": true}));
    for (params, says) in [
        (json!({"on": "false"}), "`on` must be true or false"),
        (json!({"on": 0}), "`on` must be true or false"),
        (json!({"follow": "yes"}), "`follow` must be true or false"),
        (json!({"follow": 1, "on": false}), "`follow` must be true or false"),
    ] {
        let r = h.request("engine.execute", json!({"command": "view.increaseContrast", "params": params}), T);
        assert_eq!(r["ok"], false, "{params}: {r}");
        assert!(r.to_string().contains(says), "{params}: {r}");
        assert_eq!(h.app.ui.settings.increase_contrast, Some(true), "{params} changed the setting");
    }
}

#[test]
fn follow_false_is_the_same_as_no_params() {
    let mut h = detail();
    let r = run(&mut h, "view.increaseContrast", json!({"follow": false}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(false)), "toggled like no params");
}
