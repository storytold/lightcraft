//! A slider's `label_override` is only painted: screen readers hear the control's own label, so
//! the Channel Mixer's nine short-named rows ("Red" under each output) stay apart for them.
use super::slider;
use egui::{Pos2, Rect, vec2};
use lightcraft_develop::controls;

/// One mixer row drawn for a few frames: the painted texts and the AccessKit names.
fn mixer_row(id: &str, short: &str) -> (Vec<String>, Vec<String>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx);
    crate::theme::apply(&ctx);
    ctx.enable_accesskit();
    let spec = controls::find(id).unwrap();
    let (mut painted, mut names) = (vec![], vec![]);
    for _ in 0..3 {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(330.0, 100.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| {
            let _ = slider(ui, spec, 0.0, true, Some(short));
        });
        out.textures_delta.clear();
        let _ = super::take_registry(&ctx);
        painted = out
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        if let Some(update) = out.platform_output.accesskit_update.take() {
            names = update.nodes.iter().filter_map(|(_, n)| n.label().map(str::to_string)).collect();
        }
    }
    (painted, names)
}

#[test]
fn a_mixer_rows_short_name_is_painted_and_its_full_name_is_heard() {
    let (painted, names) = mixer_row("channelMixer.blue.red", "Red");
    assert!(painted.iter().any(|t| t == "Red"), "the row paints its short name: {painted:?}");
    assert!(!painted.iter().any(|t| t == "Blue from Red"), "the full name isn't painted: {painted:?}");
    for want in ["Blue from Red", "Reset Blue from Red", "Blue from Red value"] {
        assert!(names.iter().any(|n| n == want), "a screen reader hears {want:?}: {names:?}");
    }
    assert!(!names.iter().any(|n| n == "Red"), "no node is named only \"Red\": {names:?}");
}

#[test]
fn the_nine_mixer_rows_have_nine_names() {
    let mut heard = std::collections::BTreeSet::new();
    for k in ["red", "green", "blue"] {
        for (short, src) in [("Red", "red"), ("Green", "green"), ("Blue", "blue")] {
            let (_, names) = mixer_row(&format!("channelMixer.{k}.{src}"), short);
            let slider = controls::find(&format!("channelMixer.{k}.{src}")).unwrap().label;
            assert!(names.iter().any(|n| n == slider), "{slider}: {names:?}");
            heard.insert(slider);
        }
    }
    assert_eq!(heard.len(), 9, "{heard:?}");
}
