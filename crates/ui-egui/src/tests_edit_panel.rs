//! Headless tests of the Edit panel's readability: cards that hold their sub-panel's sliders,
//! dots on edited sections and cards, and the header pinned while the panel scrolls.

use std::time::Duration;

use egui::{Rect, pos2};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::edit::{PlacedSection, pinned_section};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view with the Edit panel showing, Light open with its Curve card.
fn edit_panel(size: [f32; 2]) -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, size, 1.0);
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.app.ui.right = crate::state::RightPanel::Edit;
    if !h.app.ui.section_open("light") {
        h.app.ui.toggle_section("light");
    }
    if !h.app.ui.flyout_open("curve") {
        h.app.ui.toggle_flyout("curve");
    }
    h.step();
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn develop(h: &Headless) -> lightcraft_develop::DevelopSettings {
    let id = h.app.session.active().expect("active photo");
    (*h.app.session.develop_of(id).unwrap_or_default()).clone()
}

fn widget(h: &Headless, id: &str) -> Option<Rect> {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r)
}

#[test]
fn sections_and_cards_show_a_dot_only_when_edited() {
    let mut h = edit_panel([1200.0, 1400.0]);
    assert!(widget(&h, "section:light").is_some());
    assert!(widget(&h, "sectionEdited:light").is_none(), "an unedited photo has no dots");
    assert!(widget(&h, "flyoutEdited:curve").is_none());

    exec(&mut h, "develop.set", json!({"control": "light.exposure", "value": 0.35}));
    h.step();
    assert!(widget(&h, "sectionEdited:light").is_some(), "Exposure is a Light edit");
    assert!(widget(&h, "sectionEdited:color").is_none(), "Color is untouched");
    assert!(widget(&h, "flyoutEdited:curve").is_none(), "the curve is untouched");

    exec(&mut h, "develop.set", json!({"control": "curve.darks", "value": -12.0}));
    h.step();
    assert!(widget(&h, "flyoutEdited:curve").is_some(), "a curve edit marks the Curve card");
}

#[test]
fn a_cards_sliders_sit_inside_the_card() {
    let h = edit_panel([1200.0, 1400.0]);
    let card = widget(&h, "flyout:curve").expect("Curve card header");
    let section_slider = widget(&h, "slider:light.exposure").expect("Exposure");
    let card_slider = widget(&h, "slider:curve.darks").expect("Curve's Darks slider");
    assert!(card_slider.top() > card.bottom(), "the card's sliders follow its header");
    assert!(card_slider.left() > card.left() && card_slider.right() < card.right(), "inside the card: {card_slider:?} in {card:?}");
    assert!(card_slider.left() > section_slider.left(), "inset further than the section's own sliders");
}

#[test]
fn the_open_sections_header_stays_pinned_and_a_click_collapses_it() {
    let mut h = edit_panel([1200.0, 800.0]);
    assert!(widget(&h, "sectionPinned:light").is_none(), "nothing is pinned before scrolling");
    let panel = widget(&h, "panel:right_panel").expect("right panel");
    h.request("ui.move", json!({"x": panel.center().x, "y": panel.center().y}), T);
    h.request("ui.scroll", json!({"dy": -500.0}), T);
    h.settle(Duration::from_millis(300));
    let header = widget(&h, "section:light").expect("Light's header is still laid out");
    let pinned = widget(&h, "sectionPinned:light").expect("Light's header is pinned once it scrolls away");
    assert!(header.top() < pinned.top(), "the real header has scrolled above the pinned one");
    assert!(pinned.top() >= panel.top() - 0.5 && pinned.top() <= panel.top() + 80.0, "pinned at the panel's top: {pinned:?} in {panel:?}");

    // a slider scrolled beneath the pinned header is out of reach: the arrow keys with the
    // pointer resting on the header change nothing
    let before = develop(&h);
    h.request("ui.move", json!({"x": pinned.center().x, "y": pinned.center().y}), T);
    h.step();
    h.request("ui.key", json!({"key": "ArrowUp"}), T);
    h.step();
    assert_eq!(develop(&h), before, "no hidden slider moved");

    let r = h.request("ui.clickWidget", json!({"id": "sectionPinned:light"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    h.step();
    assert!(!h.app.ui.section_open("light"), "clicking the pinned header collapses its section");
    assert!(widget(&h, "sectionPinned:light").is_none());
}

#[test]
fn the_pinned_header_is_pushed_up_by_the_next_section() {
    let viewport = Rect::from_min_max(pos2(0.0, 100.0), pos2(270.0, 700.0));
    let s = |id: &'static str, top: f32, bottom: f32, open: bool| PlacedSection { id, title: id, open, edited: false, top, bottom };
    // Light's header is above the top, its body runs on: pinned at the top
    let sections = [s("light", 40.0, 600.0, true), s("color", 600.0, 653.0, false)];
    let (p, r) = pinned_section(&sections, viewport).expect("Light pinned");
    assert_eq!((p.id, r.top()), ("light", 100.0));
    // Light ends 20 points below the top: the pinned header slides up with its end, Color's
    // header right below it
    let sections = [s("light", -400.0, 120.0, true), s("color", 120.0, 600.0, true)];
    let (p, r) = pinned_section(&sections, viewport).expect("Light still pinned");
    assert_eq!(p.id, "light");
    assert!((r.bottom() - 119.0).abs() < 1e-3, "{r:?}");
    // once Light has scrolled away entirely, Color's header is the real one at the top
    let sections = [s("light", -400.0, 100.5, true), s("color", 100.5, 600.0, true)];
    assert!(pinned_section(&sections, viewport).is_none());
    // a header in view, or a collapsed section, is not pinned
    let sections = [s("light", 150.0, 600.0, true)];
    assert!(pinned_section(&sections, viewport).is_none());
    let sections = [s("light", 40.0, 600.0, false)];
    assert!(pinned_section(&sections, viewport).is_none());
}
