//! Headless tests of the Masking panel's section headers and their eyes (`mask.sectionEnabled`).

use std::time::Duration;

use lightcraft_develop::{LocalAdjustments, Mask};
use serde_json::json;

use crate::headless::Headless;
use crate::panels::masking::{LOCAL, local_section};
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// Detail view, Masking panel, a radial mask selected.
fn mask_panel() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1200.0, 3600.0], 1.0);
    assert_eq!(h.request("ui.set", json!({"view": "detail"}), T)["ok"], true);
    exec(&mut h, "panel.masking", json!({}));
    exec(&mut h, "mask.add", json!({"kind": "radial"}));
    h.step();
    h.step();
    h
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

fn widgets(h: &mut Headless) -> String {
    h.request("ui.widgets", json!({}), T).to_string()
}

fn mask(h: &Headless) -> Mask {
    let id = h.app.session.active().expect("active photo");
    let d = h.app.session.develop_of(id).unwrap_or_default();
    d.masks.iter().find(|m| Some(m.id) == h.app.session.active_mask).cloned().expect("a selected mask")
}

fn click_eye(h: &mut Headless, section: &str) {
    assert_eq!(h.request("ui.hoverWidget", json!({"id": format!("section:mask:{section}")}), T)["ok"], true, "{section}");
    let r = h.request("ui.clickWidget", json!({"id": format!("sectionEye:mask:{section}")}), T);
    assert_eq!(r["ok"], true, "{section}: {r}");
    h.step();
}

#[test]
fn every_local_slider_sits_under_the_header_of_its_engine_section() {
    for s in LOCAL {
        let section = local_section(s.id).unwrap_or_else(|| panic!("slider {} is under no section header", s.id));
        assert!(Mask::SECTIONS.contains(&section), "{}: {section} is not an engine section", s.id);
        // the engine agrees: switching that section off neutralises the slider, switching any other leaves it alone
        let adjust: LocalAdjustments = serde_json::from_value(json!({ s.id: 10.0 })).unwrap();
        for other in Mask::SECTIONS {
            let mut m = Mask { adjust, ..Mask::default() };
            m.set_section_enabled(other, false);
            let kept = crate::panels::masking::local_get(&m.effective().adjust, s.id);
            assert_eq!(kept == 0.0, other == section, "{}: section {other} off leaves {kept} (belongs to {section})", s.id);
        }
    }
}

#[test]
fn the_selected_mask_shows_a_header_with_an_eye_per_section() {
    let mut h = mask_panel();
    let all = widgets(&mut h);
    for id in ["tone", "color", "effects", "detail", "pointColor"] {
        assert!(all.contains(&format!("section:mask:{id}")), "header {id}: {all}");
    }
    // every slider stays shown with the section on
    for s in LOCAL {
        assert!(all.contains(&format!("slider:{}", s.id)), "slider {}: {all}", s.id);
    }
}

#[test]
fn a_header_eye_toggles_that_masks_section_in_one_undo_step() {
    let mut h = mask_panel();
    exec(&mut h, "mask.adjust", json!({"values": {"exposure": 1.0}}));
    h.step();
    for section in ["tone", "color", "effects", "detail", "pointColor"] {
        let before = h.app.session.undo.len();
        click_eye(&mut h, section);
        assert!(!mask(&h).section_enabled(section), "{section} is off");
        assert_eq!(mask(&h).disabled_sections, vec![section.to_string()], "only that section");
        assert_eq!(h.app.session.undo.len(), before + 1, "{section}: one undo step");
        click_eye(&mut h, section);
        assert!(mask(&h).section_enabled(section), "{section} is on again");
        assert_eq!(h.app.session.undo.len(), before + 2);
    }
    // an off section keeps its sliders, editable, and its values
    click_eye(&mut h, "tone");
    h.step();
    assert!(widgets(&mut h).contains("slider:exposure"), "the slider stays");
    assert_eq!(mask(&h).adjust.exposure, 1.0, "its value is kept");
    assert_eq!(h.request("ui.hoverWidget", json!({"id": "slider:exposure"}), T)["ok"], true);
    h.request("ui.key", json!({"key": "Up", "shift": true}), T);
    assert!(mask(&h).adjust.exposure > 1.0, "still editable: {}", mask(&h).adjust.exposure);
}
