//! The Edit panel's sections are Lightroom Classic's Develop panels, in its order and with its names, and ⌘1–⌘9
//! open them top to bottom as there (fork-owned).

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LccApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn develop() -> Headless {
    let app = LccApp::new(lcc_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1300.0, 1400.0], 1.0);
    let r = h.request("ui.set", json!({"module": "develop", "view": "detail", "right": "edit"}), T);
    assert_eq!(r["ok"], true, "{r}");
    // this is Eboda's panel (the Witchcraft mode inserts its own sections after Basic)
    let r = h.request("engine.execute", json!({"command": "view.gradingMode", "params": {"mode": "eboda"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn sections(h: &Headless) -> Vec<String> {
    let mut rows: Vec<(f32, String)> =
        h.app.widgets.iter().filter_map(|(w, r)| w.strip_prefix("section:").map(|id| (r.top(), id.to_string()))).collect();
    rows.sort_by(|a, b| a.0.total_cmp(&b.0));
    rows.into_iter().map(|(_, id)| id).collect()
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn the_edit_panel_is_lightroom_classics_develop_panels_and_every_header_folds() {
    let mut h = develop();
    // the edit panel is lightroom classics develop panels
    {
        // every section folded, so all the headers are on screen
        for id in h.app.ui.open_sections.clone() {
            h.app.ui.toggle_section(&id);
        }
        h.settle(SETTLE);
        assert_eq!(
            sections(&h),
            ["light", "levels", "curve", "mixer", "grading", "detail", "retouch", "optics", "geometry", "effects", "calibration", "pluginFilters"],
            "then the fork's Plug-in Filters"
        );
        let text = h.painted_text();
        for title in
            ["Basic", "Levels", "Tone Curve", "Color Mixer", "Color Grading", "Detail", "Lens Corrections", "Transform", "Effects", "Calibration"]
        {
            assert!(text.iter().any(|t| t == title), "{title}: {text:?}");
        }
        // a section's on/off switch shows while the pointer is on its header; Basic has none, as in Lightroom Classic
        let eye = |h: &mut Headless, id: &str| {
            let r = h.request("ui.hoverWidget", json!({"id": format!("section:{id}")}), T);
            assert_eq!(r["ok"], true, "{r}");
            h.step();
            h.step();
            h.app.widgets.iter().any(|(w, _)| *w == format!("sectionEye:{id}"))
        };
        assert!(!eye(&mut h, "light"), "Basic has no on/off switch");
        for id in ["curve", "mixer", "grading"] {
            assert!(eye(&mut h, id), "{id} has one");
        }
        // ⌘1–⌘9 open them top to bottom
        let key = |id: &str| crate::menus::ui_commands().find(|c| c.0 == id).and_then(|c| c.2);
        let order = [
            "section.light",
            "section.curve",
            "section.mixer",
            "section.grading",
            "section.detail",
            "section.optics",
            "section.geometry",
            "section.effects",
            "section.calibration",
        ];
        for (i, id) in order.iter().enumerate() {
            assert_eq!(key(id), Some(format!("Cmd+{}", i + 1).as_str()), "{id}");
        }
        let r = h.request("engine.execute", json!({"command": "section.mixer"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.section_open("mixer"));
    }
    // every section header opens and folds its section
    // Clicking any section's header opens it and clicking again folds it: each header's `section.*` command exists
    // (Plug-in Filters' didn't, and its header only showed "no such Edit section").
    {
        for id in h.app.ui.open_sections.clone() {
            h.app.ui.toggle_section(&id);
        }
        h.settle(SETTLE);
        for id in sections(&h) {
            assert!(crate::menus::ui_commands().any(|c| c.0 == format!("section.{id}")), "section.{id} isn't a command");
            for open in [true, false] {
                let r = h.request("ui.clickWidget", json!({"id": format!("section:{id}")}), T);
                assert_eq!(r["ok"], true, "{r}");
                h.settle(SETTLE);
                assert_eq!(h.app.ui.section_open(&id), open, "{id}");
            }
        }
    }
}

/// One session, several steps; each step was a test of its own and says what it checks.
#[test]
fn the_color_mixer_has_two_tabs_and_adjusts_by_hsl_groups() {
    let mut h = develop();
    // the color mixer has mixer and point color tabs
    // The Color Mixer's two tabs: Mixer, then Point Color.
    {
        let r = h.request("engine.execute", json!({"command": "section.mixer", "params": {"solo": true}}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        let r = h.request("ui.clickWidget", json!({"id": "button:colorMixerTab-pointColor"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(h.app.ui.flyout_open("pointColor"), "Point Color tab");
        let r = h.request("ui.clickWidget", json!({"id": "button:colorMixerTab-mixer"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert!(!h.app.ui.flyout_open("pointColor"), "Mixer tab");
    }
    // the color mixer adjusts by hsl groups
    // The Mixer adjusts by HSL by default, one group of eight colour sliders at a time; its group
    // tabs and `view.colorMixer` pick what is shown, and a targeted adjustment whose group is hidden
    // is put down.
    {
        assert_eq!(h.app.ui.mixer_adjust, "hsl");
        let r = h.request("ui.clickWidget", json!({"id": "button:mixerGroup-sat"}), T);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(SETTLE);
        assert_eq!(h.app.ui.mixer_group, "sat");
        h.app.ui.tool = "tat:sat".into();
        let r = h.request("engine.execute", json!({"command": "view.colorMixer", "params": {"group": "lum"}}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!(h.app.ui.mixer_group, "lum");
        assert!(h.app.ui.tool.is_empty(), "the hidden group's targeted adjustment is put down");
        let r = h.request("engine.execute", json!({"command": "view.colorMixer", "params": {"adjust": "color", "band": "blue"}}), T);
        assert_eq!(r["ok"], true, "{r}");
        assert_eq!((h.app.ui.mixer_adjust.as_str(), h.app.ui.mixer_mode.as_str()), ("color", "blue"));
        let r = h.request("engine.execute", json!({"command": "view.colorMixer", "params": {"group": "nope"}}), T);
        assert_ne!(r["ok"], true, "an unknown group is refused: {r}");
    }
}
