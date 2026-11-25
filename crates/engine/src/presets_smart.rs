//! Built-in smart presets (group "Smart Masks"): masks as recipes — AI selections computed on each
//! photo, People by role, gradients placed relative to the couple — with their local adjustments.
//! All values are our own (no third-party preset content).
//!
//! Ported from OneSlips' fork (own design there). Applying one adds its masks
//! to the photo's own, so they stack: a look first, then "Recover Blue Suit", then "Bring Sky
//! Down".

use lcc_develop::Preset;
use serde_json::{Value, json};

/// The group the smart presets are listed under.
pub const GROUP: &str = "Smart Masks";

fn p(id: &str, name: &str, masks: Value) -> Preset {
    Preset {
        id: format!("lc.smart.{id}"),
        name: name.into(),
        group: GROUP.into(),
        settings: json!({ "masks": masks }),
        favorite: false,
        builtin: true,
    }
}

fn mask(name: &str, components: Value, adjust: Value) -> Value {
    json!({"id": 1, "name": name, "visible": true, "invert": false, "components": components, "adjust": adjust})
}

/// [`mask`] with Refine Edges (the edges snap to the photo's: a sleeve or a veil, not a blur).
fn refined(name: &str, components: Value, adjust: Value, refine: f64) -> Value {
    let mut m = mask(name, components, adjust);
    m["refine"] = json!(refine);
    m
}

/// OkLab samples of suit blues (navy, royal, steel, grey-blue) for a colour range.
fn suit_blues() -> Value {
    json!([
        [0.30, -0.005, -0.060],
        [0.47, -0.015, -0.130],
        [0.62, -0.020, -0.075],
        [0.70, -0.012, -0.045],
        [0.58, -0.008, -0.040],
        [0.72, -0.007, -0.030]
    ])
}

/// The group the adaptive presets are listed under.
pub const ADAPTIVE_GROUP: &str = "Smart Masks: Adaptive";

/// An adaptive preset: a global look (`settings`) plus mask recipes computed on each photo.
fn adaptive_preset(id: &str, name: &str, mut settings: Value, masks: Value) -> Preset {
    settings["masks"] = masks;
    Preset { id: format!("lc.adaptive.{id}"), name: name.into(), group: ADAPTIVE_GROUP.into(), settings, favorite: false, builtin: true }
}

/// Built-in adaptive presets (group "Smart Masks: Adaptive"): a look plus Subject, Sky or Background masks, all
/// from the built-in scene masks (no model download), so they work in every build. Our own values.
pub fn adaptive() -> Vec<Preset> {
    vec![
        adaptive_preset(
            "subjectPop",
            "Subject Pop",
            json!({"treatment": "color", "color": {"vibrance": 8.0}}),
            json!([mask(
                "Subject Pop",
                json!([{"op": "add", "shape": {"kind": "subject"}}]),
                json!({"exposure": 0.25, "texture": 12.0, "clarity": 8.0, "amount": 100.0})
            )]),
        ),
        adaptive_preset(
            "deepSky",
            "Deep Sky",
            json!({"treatment": "color", "effects": {"dehaze": 4.0}}),
            json!([mask(
                "Deep Sky",
                json!([{"op": "add", "shape": {"kind": "sky"}}]),
                json!({"exposure": -0.4, "highlights": -25.0, "saturation": 12.0, "temp": -6.0, "amount": 100.0})
            )]),
        ),
        adaptive_preset(
            "quietBackground",
            "Quiet Background",
            json!({"treatment": "color", "light": {"contrast": 6.0}}),
            json!([mask(
                "Quiet Background",
                json!([{"op": "add", "shape": {"kind": "background"}}]),
                json!({"exposure": -0.3, "clarity": -15.0, "saturation": -15.0, "amount": 100.0})
            )]),
        ),
        adaptive_preset(
            "subjectAndSky",
            "Subject and Sky",
            json!({"treatment": "color", "color": {"vibrance": 6.0}}),
            json!([
                mask("", json!([{"op": "add", "shape": {"kind": "subject"}}]), json!({"exposure": 0.2, "shadows": 15.0, "amount": 100.0})),
                mask(
                    "",
                    json!([{"op": "add", "shape": {"kind": "sky"}}]),
                    json!({"exposure": -0.35, "highlights": -20.0, "saturation": 8.0, "amount": 100.0})
                )
            ]),
        ),
    ]
}

pub fn smart() -> Vec<Preset> {
    vec![
        // the background behind the couple, minus a gradient up from their feet: warm light from
        // behind them, not on the ground in front
        p(
            "sunKissed",
            "Sun Kissed",
            json!([mask(
                "Sun Kissed",
                json!([
                    {"op": "add", "shape": {"kind": "background"}},
                    {"op": "subtract", "anchor": {"to": "people", "shape": {"kind": "linear", "start": {"x": 0.5, "y": 1.0}, "end": {"x": 0.5, "y": 0.55}}},
                     "shape": {"kind": "linear", "start": {"x": 0.5, "y": 1.0}, "end": {"x": 0.5, "y": 0.55}}}
                ]),
                json!({"temp": 18.0, "tint": 4.0, "exposure": 0.25, "highlights": -10.0, "dehaze": -8.0, "color_hue": 40.0, "color_sat": 25.0, "amount": 100.0}),
            )]),
        ),
        // the groom's clothes, the blue in them: the colour most wedding looks mute
        p(
            "recoverBlueSuit",
            "Recover Blue Suit",
            json!([refined(
                "Recover Blue Suit",
                json!([
                    {"op": "add", "shape": {"kind": "people", "person": 0, "parts": ["Clothes"], "who": {"role": "Groom"}}},
                    {"op": "intersect", "shape": {"kind": "colorRange", "samples": suit_blues(), "refine": 50.0}}
                ]),
                json!({"saturation": 30.0, "exposure": 0.1, "shadows": 10.0, "amount": 100.0}),
                50.0,
            )]),
        ),
        p(
            "bringSkyDown",
            "Bring Sky Down",
            json!([mask(
                "Bring Sky Down",
                json!([{"op": "add", "shape": {"kind": "sky"}}]),
                json!({"exposure": -0.6, "highlights": -35.0, "dehaze": 12.0, "saturation": 8.0, "amount": 100.0}),
            )]),
        ),
        // the bride's clothes where they are bright: the dress, not a dark sash or the bouquet
        p(
            "brightenDress",
            "Brighten Dress",
            json!([refined(
                "Brighten Dress",
                json!([
                    {"op": "add", "shape": {"kind": "people", "person": 0, "parts": ["Clothes"], "who": {"role": "Bride"}}},
                    {"op": "intersect", "shape": {"kind": "luminanceRange", "lo": 0.55, "hi": 1.0, "lo_feather": 0.12, "hi_feather": 0.05}}
                ]),
                json!({"exposure": 0.3, "whites": 10.0, "shadows": 15.0, "temp": -5.0, "saturation": -10.0, "amount": 100.0}),
                50.0,
            )]),
        ),
        p(
            "darkenBackground",
            "Darken Background",
            json!([mask(
                "Darken Background",
                json!([{"op": "add", "shape": {"kind": "background"}}]),
                json!({"exposure": -0.5, "clarity": -10.0, "saturation": -10.0, "amount": 100.0}),
            )]),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use lcc_develop::{DevelopSettings, MaskShape};

    #[test]
    fn smart_presets_are_mask_only_recipes_that_apply() {
        let all = smart();
        assert_eq!(all.len(), 5);
        for pr in &all {
            assert!(pr.mask_only() && pr.has_ai_masks(), "{}", pr.name);
            let out = pr.apply(&DevelopSettings::default(), 1.0);
            assert_eq!(out.masks.len(), 1, "{}", pr.name);
            assert!(out.masks[0].uses_ai(), "{}: computed per photo", pr.name);
            assert_eq!(out.masks[0].preset.as_deref(), Some(pr.id.as_str()));
        }
        let sun = all[0].apply(&DevelopSettings::default(), 1.0);
        let c = &sun.masks[0].components[1];
        assert!(c.anchor.is_some(), "the gradient follows the couple");
        assert!(matches!(c.shape, MaskShape::Linear { .. }));
        let suit = all[1].apply(&DevelopSettings::default(), 1.0);
        assert!(matches!(&suit.masks[0].components[0].shape, MaskShape::People { who: Some(_), .. }));
        // they stack: a look, then two smart presets
        let both = all[2].apply(&suit, 1.0);
        assert_eq!(both.masks.len(), 2);
    }

    #[test]
    fn adaptive_presets_set_a_look_and_add_masks_every_build_computes() {
        for pr in adaptive() {
            assert!(pr.has_ai_masks() && !pr.mask_only(), "{}: a look plus masks", pr.name);
            let (same, skipped) = crate::presets::without_unavailable_masks(&pr, false);
            assert!(skipped.is_empty() && same == pr, "{}: no model needed", pr.name);
            let base = DevelopSettings::default();
            let out = pr.apply(&base, 1.0);
            assert!(!out.masks.is_empty() && out.masks.iter().all(|m| m.uses_ai()), "{}", pr.name);
            assert!(out.masks.iter().all(|m| !m.name.trim().is_empty()), "{}: masks named", pr.name);
            // applied again: its own masks are replaced, not doubled
            let again = pr.apply(&out, 1.0);
            assert_eq!(again.masks.len(), out.masks.len(), "{}", pr.name);
        }
    }
}
