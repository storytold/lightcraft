//! Reduce Transparency (`view.reduceTransparency`, Settings ▸ Interface): the see-through badges,
//! labels and backdrops are drawn solid; the system's setting is followed until the user chooses.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use egui::epaint::Shape;
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn app_with(services: Services) -> LightcraftApp {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    app
}

fn headless(services: Services) -> Headless {
    let mut h = Headless::new(app_with(services), [1200.0, 800.0], 1.0);
    h.settle(SETTLE);
    h
}

/// A headless app whose system says `system` (shared, so a test can change it).
fn with_system(system: Rc<Cell<Option<bool>>>) -> Headless {
    headless(Services { png: None, system_reduce_transparency: Some(Box::new(move || system.get())), ..Default::default() })
}

fn plain() -> Headless {
    headless(Services { png: None, ..Default::default() })
}

fn run(h: &mut Headless, command: &str, params: serde_json::Value) -> serde_json::Value {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    r["result"].clone()
}

fn translucent(fill: egui::Color32) -> bool {
    (1..255).contains(&fill.a())
}

/// The filled rectangles and circles the last frame painted with a see-through fill (shadows,
/// which are blurred by nature, are not counted).
fn see_through(h: &Headless) -> Vec<(egui::Rect, egui::Color32)> {
    h.view
        .painted_shapes()
        .into_iter()
        .filter_map(|shape| match shape {
            Shape::Rect(r) if translucent(r.fill) && r.blur_width == 0.0 => Some((r.rect, r.fill)),
            Shape::Circle(c) if translucent(c.fill) => Some((egui::Rect::from_center_size(c.center, egui::Vec2::splat(2.0 * c.radius)), c.fill)),
            _ => None,
        })
        .collect()
}

/// The rectangles the last frame painted with a fully opaque fill.
fn opaque_rects(h: &Headless) -> Vec<egui::Rect> {
    h.view
        .painted_shapes()
        .into_iter()
        .filter_map(|shape| match shape {
            Shape::Rect(r) if r.fill.a() == 255 && r.blur_width == 0.0 => Some(r.rect),
            _ => None,
        })
        .collect()
}

/// The fill of the rectangle that covers the whole window (a dialog's backdrop), if one is painted.
fn backdrop(h: &Headless) -> Option<egui::Color32> {
    let screen = h.view.ctx.content_rect();
    h.view.painted_shapes().into_iter().find_map(|shape| match shape {
        Shape::Rect(r) if r.rect == screen => Some(r.fill),
        _ => None,
    })
}

#[test]
fn reduced_surfaces_are_solid_and_the_usual_ones_are_not() {
    let ctx = egui::Context::default();
    crate::theme::apply(&ctx);
    let fills = |reduced: bool| {
        crate::transparency::sync(&ctx, reduced);
        [
            crate::transparency::hud(&ctx, 165),
            crate::transparency::hud(&ctx, 220),
            crate::transparency::scrim(&ctx, 140),
            crate::transparency::solid(&ctx, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220)),
            crate::transparency::fade(&ctx, egui::Color32::from_rgb(10, 20, 30), 0.7),
        ]
    };
    let usual = fills(false);
    assert_eq!(usual[0], egui::Color32::from_black_alpha(165));
    assert_eq!(usual[2], egui::Color32::from_black_alpha(140));
    assert!(usual.iter().all(|c| c.a() < 255), "see-through: {usual:?}");
    let solid = fills(true);
    assert!(solid.iter().all(|c| c.a() == 255), "solid: {solid:?}");
    assert!(solid[1].r() < solid[0].r(), "a hovered pill stays darker than a resting one: {solid:?}");
}

#[test]
fn the_photo_grid_badges_are_solid_with_it_on() {
    let mut h = plain();
    // the dimming of a rejected photo is the photo's own state and stays see-through: the demo's rejects (ids 4, 10, 16, 22)
    // are cleared so the count of what is see-through does not depend on how far down the grid draws
    run(&mut h, "photo.flag", json!({"flag": "none", "ids": [4, 10, 16, 22]}));
    h.app.ui.settings.grid_badges = crate::state::GridBadges::Always;
    h.request("ui.set", json!({"view": "photoGrid", "right": "none"}), T);
    h.settle(SETTLE);
    run(&mut h, "view.reduceTransparency", json!({"on": false}));
    let usual = see_through(&h);
    assert!(!usual.is_empty(), "the badges are see-through as usual");
    run(&mut h, "view.reduceTransparency", json!({"on": true}));
    h.settle(SETTLE);
    assert_eq!(see_through(&h), vec![]);
    // (and not because nothing is painted: each badge that was see-through is still there, opaque)
    let solid = opaque_rects(&h);
    let gone: Vec<_> = usual.iter().filter(|(rect, _)| !solid.contains(rect)).collect();
    assert!(gone.is_empty(), "badges painted see-through before and not painted solid now: {gone:?}");
}

#[test]
fn detail_labels_the_navigator_and_the_filmstrip_are_solid_with_it_on() {
    let mut h = plain();
    h.request("ui.set", json!({"view": "detail", "right": "none"}), T);
    run(&mut h, "view.reduceTransparency", json!({"on": false}));
    h.settle(SETTLE);
    let usual = see_through(&h);
    assert!(!usual.is_empty(), "the filmstrip's badges are see-through as usual");
    run(&mut h, "view.reduceTransparency", json!({"on": true}));
    h.settle(SETTLE);
    assert_eq!(see_through(&h), vec![]);
}

#[test]
fn the_backdrop_of_a_dialog_is_solid_with_it_on() {
    let mut h = plain();
    run(&mut h, "dialog.export", json!({}));
    h.settle(SETTLE);
    run(&mut h, "view.reduceTransparency", json!({"on": false}));
    assert_eq!(backdrop(&h), Some(egui::Color32::from_black_alpha(140)), "the dimming backdrop");
    run(&mut h, "view.reduceTransparency", json!({"on": true}));
    h.settle(SETTLE);
    assert_eq!(backdrop(&h).map(|c| c.a()), Some(255), "the backdrop is solid");
}

#[test]
fn a_toast_is_solid_with_it_on() {
    let toast_fills = |on: bool| {
        let mut h = plain();
        h.request("ui.set", json!({"view": "photoGrid", "right": "none"}), T);
        run(&mut h, "view.reduceTransparency", json!({"on": on}));
        let ctx = h.view.ctx.clone();
        h.app.toast(&ctx, "Saved");
        h.step();
        assert!(h.view.painted_text().iter().any(|t| t == "Saved"), "the toast is up");
        // the toast's rectangle is the one the text sits in
        let text_at = h.view.painted_shapes().into_iter().find_map(|s| match s {
            Shape::Text(t) if t.galley.text() == "Saved" => Some(t.pos),
            _ => None,
        });
        let at = text_at.expect("the toast's text");
        h.view
            .painted_shapes()
            .into_iter()
            .filter_map(|s| match s {
                Shape::Rect(r) if r.rect.contains(at) && r.rect.height() < 80.0 && r.rect.width() < 760.0 => Some(r.fill),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let usual = toast_fills(false);
    assert!(usual.contains(&egui::Color32::from_black_alpha(200)), "see-through as usual: {usual:?}");
    let solid = toast_fills(true);
    assert!(!solid.is_empty() && solid.iter().all(|c| c.a() == 255), "solid: {solid:?}");
}

#[test]
fn the_system_setting_is_followed_until_the_user_chooses() {
    let system = Rc::new(Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    assert_eq!(h.app.ui.settings.reduce_transparency, None);
    assert!(crate::transparency::reduced(&h.view.ctx), "the system says reduce");
    // the user chooses to keep the usual look: the system no longer decides
    let r = run(&mut h, "view.reduceTransparency", json!({"on": false}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(false), json!(false)));
    assert!(!crate::transparency::reduced(&h.view.ctx));
    // back to following
    let r = run(&mut h, "view.reduceTransparency", json!({"follow": true}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(true)));
    assert!(crate::transparency::reduced(&h.view.ctx));
    // the system switches while the app runs: noticed within about a second
    system.set(Some(false));
    for _ in 0..90 {
        h.step();
    }
    assert!(!h.app.reduce_transparency());
    assert!(!crate::transparency::reduced(&h.view.ctx));
    // no params toggles what is in effect
    let r = run(&mut h, "view.reduceTransparency", json!({}));
    assert_eq!((r["on"].clone(), r["followsSystem"].clone()), (json!(true), json!(false)));
}

#[test]
fn the_choice_is_saved_and_a_state_from_before_loads() {
    let mut h = plain();
    run(&mut h, "view.reduceTransparency", json!({"on": true}));
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    let back = serde_json::from_str::<crate::UiState>(&saved).unwrap().sanitized();
    assert_eq!(back.settings.reduce_transparency, Some(true));
    assert_eq!(crate::state::AppSettings::default().reduce_transparency, None, "follows the system by default");
    let mut old = serde_json::to_value(&h.app.ui).unwrap();
    old["settings"].as_object_mut().unwrap().remove("reduceTransparency").expect("saved under its camelCase key");
    let loaded = serde_json::from_value::<crate::UiState>(old).unwrap().sanitized();
    assert_eq!(loaded.settings.reduce_transparency, None, "state saved before the setting existed still loads");
}

#[test]
fn follow_true_is_refused_where_there_is_no_system_setting_and_changes_nothing() {
    let mut h = plain();
    run(&mut h, "view.reduceTransparency", json!({"on": true}));
    let r = h.request("engine.execute", json!({"command": "view.reduceTransparency", "params": {"follow": true}}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(r.to_string().contains("no Reduce Transparency setting to follow"), "{r}");
    assert_eq!(h.app.ui.settings.reduce_transparency, Some(true), "the choice made stays");
}

#[test]
fn the_settings_window_can_hand_the_choice_back_to_the_system() {
    let system = Rc::new(Cell::new(Some(true)));
    let mut h = with_system(system.clone());
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    h.settle(SETTLE);
    let no_button = h.request("ui.clickWidget", json!({"id": "button:settings.reduceTransparencyFollow"}), T);
    assert_ne!(no_button["ok"], true, "no button while following the system");
    let r = h.request("ui.clickWidget", json!({"id": "check:settings.reduceTransparency"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.reduce_transparency, Some(false), "the tick box shows the system's true and the click turns it off");
    let r = h.request("ui.clickWidget", json!({"id": "button:settings.reduceTransparencyFollow"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert_eq!(h.app.ui.settings.reduce_transparency, None);
    assert!(h.app.reduce_transparency(), "follows the system again");
}

#[test]
fn the_settings_label_goes_through_the_catalog() {
    use crate::i18n::Locale;
    let mut h = plain();
    run(&mut h, "app.settings", json!({"tab": "interface"}));
    h.app.run("app.language.german", json!({})).unwrap();
    h.settle(SETTLE);
    let texts = h.view.painted_text();
    h.app.run("app.language.english", json!({})).unwrap();
    assert!(texts.iter().any(|t| *t == Locale::De.tr("Reduce transparency")), "the German label is drawn: {texts:?}");
    assert!(!texts.iter().any(|t| t == "Reduce transparency"), "and not the English one");
}
