//! The built-in profiles: TactileBridge's curated layouts (Default, Wedding, Portrait, Fashion,
//! Landscape, Real Estate) for the MX Creative Keypad and Dialpad, with every action mapped to
//! LightCraft, plus the pages that follow LightCraft's tools.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: the profile names, pages and
//! their key and dial layouts are TactileBridge's (`tools/GenerateProfiles.cs`); its tool-context
//! layouts (`TactileBridgeSettings.CreateDefaultToolContexts`) shaped the tool pages.
//!
//! Each TactileBridge profile came in three Logi device variants: `…70` for the Keypad (key
//! pages), `…71` for the Dialpad (four buttons and dial pages) and `…72` for the Actions Ring
//! (a copy of the Keypad's first pages and the Dialpad's dial pages). Here the three are one
//! profile: key pages from `70`, dial pages and buttons from `71` (`72` adds nothing new).
//! One change: the Dialpad's fourth button turns the dial pages (TactileBridge left that to Logi
//! Options+); the action it had is on a key page too.

use serde_json::json;

use crate::binding::{Binding, Modifier};
use crate::devices::PageGroup;
use crate::profile::{Page, Profile};
use crate::tactilebridge::binding_of;

/// A key page from TactileBridge action keys (key 1 … key 9).
fn keys(name: &str, tb: [&str; 9]) -> Page {
    let mut p = Page { name: name.to_string(), groups: vec![PageGroup::Keys], ..Default::default() };
    for (i, k) in tb.iter().enumerate() {
        if let Some(b) = binding_of(k) {
            p.bindings.insert(format!("key.{}", i + 1), b);
        }
    }
    p
}

/// A dial page from TactileBridge dial values in its order: encoder 0 (the roller), then
/// encoder 1 (the big dial) — TactileBridge's Wedding layout keeps Exposure on the roller and
/// Kelvin on the dial.
fn dials(name: &str, roller: &str, dial: &str) -> Page {
    let mut p = Page { name: name.to_string(), groups: vec![PageGroup::Dials], ..Default::default() };
    for (slot, k) in [("roller", roller), ("dial", dial)] {
        if let Some(b) = binding_of(k) {
            p.bindings.insert(slot.to_string(), b);
        }
    }
    p
}

/// A dial page from LightCraft targets.
fn lc_dials(name: &str, dial: &str, roller: &str) -> Page {
    Page::new(name, &[PageGroup::Dials], [("dial", Binding::value(dial)), ("roller", Binding::value(roller))])
}

/// The Dialpad's buttons: three TactileBridge actions and the dial-page switch.
fn pads(p: &mut Profile, tb: [&str; 3]) {
    for (i, k) in tb.iter().enumerate() {
        if let Some(b) = binding_of(k) {
            p.base.insert(format!("pad.{}", i + 1), b);
        }
    }
    p.base.insert("pad.4".into(), Binding::PageNext { group: Some(PageGroup::Dials) });
}

fn cmd(c: &str) -> Binding {
    Binding::command(c)
}
fn cmdp(c: &str, p: serde_json::Value) -> Binding {
    Binding::command_with(c, p)
}
fn val(t: &str) -> Binding {
    Binding::value(t)
}

/// Pages that follow LightCraft's tools, shared by every built-in profile, with the contexts they
/// show in.
pub fn tool_pages() -> Vec<(Page, &'static [&'static str])> {
    let both: &[PageGroup] = &[];
    vec![
        (
            // TactileBridge's Crop context: crop orientation, zoom, overlays; the dial straightens
            Page::new(
                "Crop",
                both,
                [
                    ("key.1", cmd("crop.rotateAspect")),
                    ("key.2", cmd("crop.autoStraighten")),
                    ("key.3", cmd("crop.reset")),
                    ("key.4", cmd("view.zoomIn")),
                    ("key.5", cmd("view.zoomOut")),
                    ("key.6", cmd("view.zoomFit")),
                    ("key.7", cmd("view.cropOverlay")),
                    ("key.8", cmd("view.cropOverlayOrientation")),
                    ("key.9", cmd("tool.done")),
                    ("dial", val("crop.angle")),
                    ("roller", val("ui.zoom")),
                ],
            ),
            &["crop"],
        ),
        (
            Page::new(
                "Masking",
                both,
                [
                    ("key.1", cmdp("tool.brush", json!({"new": true}))),
                    ("key.2", cmd("tool.linear")),
                    ("key.3", cmd("tool.radial")),
                    ("key.4", cmdp("mask.add", json!({"kind": "subject"}))),
                    ("key.5", cmdp("mask.add", json!({"kind": "sky"}))),
                    ("key.6", cmdp("mask.add", json!({"kind": "luminanceRange"}))),
                    ("key.7", cmd("view.maskOverlay")),
                    ("key.8", cmd("edit.undo")),
                    ("key.9", cmd("tool.done")),
                    ("dial", val("ui.zoom")),
                ],
            ),
            &["masking"],
        ),
        (
            // a selected mask: its sliders on the keys (press one to put it on the dial)
            Page::new(
                "Mask",
                both,
                [
                    ("key.1", val("mask.exposure")),
                    ("key.2", val("mask.contrast")),
                    ("key.3", val("mask.highlights")),
                    ("key.4", val("mask.shadows")),
                    ("key.5", val("mask.temp")),
                    ("key.6", val("mask.saturation")),
                    ("key.7", cmd("mask.invert")),
                    ("key.8", cmd("surface.maskPrevious")),
                    ("key.9", cmd("surface.maskNext")),
                    ("dial", val("mask.exposure")),
                    ("roller", val("mask.amount")),
                ],
            ),
            &["masking.mask", "masking.mask.gradient"],
        ),
        (
            // TactileBridge's Draw Mask context: mask actions on the keys, brush size and feather
            // on the dials
            Page::new(
                "Brush",
                both,
                [
                    ("key.1", cmd("mask.invert")),
                    ("key.2", cmd("brush.erase")),
                    ("key.3", cmdp("tool.brush", json!({"new": true}))),
                    // size and feather on keys too: one hand changes them while the other paints
                    ("key.4", val("ui.brushSize")),
                    ("key.5", val("ui.brushFeather")),
                    ("key.6", cmd("mask.visible")),
                    ("key.7", val("ui.brushFlow")),
                    ("key.8", cmd("edit.undo")),
                    ("key.9", cmd("tool.done")),
                    ("dial", val("ui.brushSize")),
                    ("roller", val("ui.brushFeather")),
                ],
            ),
            &["masking.mask.brush"],
        ),
        (
            // TactileBridge's Draw Healing Mask context
            Page::new(
                "Remove",
                both,
                [
                    ("key.1", cmd("view.visualizeSpots")),
                    ("key.2", cmd("view.zoomIn")),
                    ("key.3", cmd("view.zoomOut")),
                    ("key.4", cmd("view.zoomFit")),
                    ("key.5", cmd("view.zoom100")),
                    ("key.6", cmd("spot.delete")),
                    ("key.7", cmd("edit.undo")),
                    ("key.8", cmd("edit.redo")),
                    ("key.9", cmd("tool.done")),
                    ("dial", val("ui.removeSize")),
                    ("roller", val("ui.removeFeather")),
                ],
            ),
            &["remove"],
        ),
        (
            // TactileBridge's Import / Export / Process dialog contexts
            Page::new(
                "Dialog",
                &[PageGroup::Keys],
                [
                    ("key.1", cmd("surface.dialogConfirm")),
                    ("key.3", cmd("surface.dialogCancel")),
                    ("key.4", cmd("library.selectAll")),
                    ("key.6", cmd("library.selectNone")),
                ],
            ),
            &["dialog"],
        ),
        // the Edit panel's sections on the dials (TactileBridge's Direct Color Editor context put
        // colour on the dials): ready to map to `develop.<section>` in Settings > Controls; not
        // mapped by default, so the profile's own dial pages stay in charge while editing
        (lc_dials("Light", "light.exposure", "light.contrast"), &[]),
        (lc_dials("Color", "wb.temp", "wb.tint"), &[]),
        (lc_dials("Effects", "effects.clarity", "effects.texture"), &[]),
        (lc_dials("Detail", "detail.sharpenAmount", "detail.nrLuminance"), &[]),
        (lc_dials("Optics", "optics.distortion", "optics.vignetting"), &[]),
    ]
}

/// The Actions Ring's slots in every built-in profile: undo, redo, pick, reject, previous and
/// next photo, before / after and fit (nothing that deletes or resets).
pub fn ring() -> Vec<(String, Binding)> {
    [
        cmd("edit.undo"),
        cmd("edit.redo"),
        cmd("photo.pick"),
        cmd("photo.reject"),
        cmd("library.previous"),
        cmd("library.next"),
        cmd("view.beforeAfter"),
        cmd("view.zoomFit"),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, b)| (format!("ring.{}", i + 1), b))
    .collect()
}

fn finish(mut p: Profile) -> Profile {
    for (c, b) in ring() {
        p.base.entry(c).or_insert(b);
    }
    let first_keys = p.pages_in(PageGroup::Keys).next().map(|p| p.name.clone());
    let first_dials = p.pages_in(PageGroup::Dials).next().map(|p| p.name.clone());
    p.contexts.insert("default".into(), first_keys.into_iter().chain(first_dials).collect());
    for (page, contexts) in tool_pages() {
        for c in contexts {
            p.contexts.entry((*c).to_string()).or_default().push(page.name.clone());
        }
        // a profile's own page of the same name (Fashion's "Color" dial page) serves instead
        if p.page(&page.name).is_none() {
            p.pages.push(Page { auto: true, ..page });
        }
    }
    p.sanitize();
    p
}

/// The built-in profiles, in TactileBridge's order.
pub fn profiles() -> Vec<Profile> {
    let mut out = Vec::new();

    let mut d = Profile { name: "Default".into(), description: "Browse, rate and edit: TactileBridge's default layout".into(), ..Default::default() };
    d.pages.push(keys(
        "Page 1",
        [
            "PreviousImage",
            "NextImage",
            "ZoomToFit",
            "Zoom100",
            "ToggleBeforeAfter",
            "CopyAdjustments",
            "ApplyAdjustments",
            "AutoAdjust",
            "ResetAdjustments",
        ],
    ));
    d.pages.push(keys(
        "Page 2",
        ["ClearRating", "Rate1Star", "Rate2Stars", "Rate3Stars", "Rate4Stars", "Rate5Stars", "ColorTagRed", "ColorTagGreen", "ColorTagBlue"],
    ));
    d.pages.push(keys(
        "Page 3",
        [
            "SelectTool",
            "CropTool",
            "SpotRemovalTool",
            "WBPickerTool",
            "DrawMaskTool",
            "EraseMaskTool",
            "LinearGradientTool",
            "RadialGradientTool",
            "LoupeTool",
        ],
    ));
    d.pages.push(dials("Dial Page", "Exposure", "Contrast"));
    // TactileBridge's Actions Ring variant turned white balance
    d.pages.push(dials("White Balance", "Kelvin", "Tint"));
    pads(&mut d, ["CopyAdjustments", "ApplyAdjustments", "Redo"]);
    out.push(finish(d));

    let mut w = Profile {
        name: "Wedding".into(),
        description: "Fast culling, white balance, exposure, copy and paste, skin retouching".into(),
        ..Default::default()
    };
    w.pages.push(keys(
        "Cull",
        ["PreviousImage", "NextImage", "ToggleBeforeAfter", "Rate1Star", "Rate3Stars", "Rate5Stars", "ColorTagGreen", "ColorTagRed", "ColorTagNone"],
    ));
    w.pages.push(keys(
        "Edit",
        ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust", "WBPickerTool", "CropTool", "StraightenTool", "Undo", "Redo", "ResetAdjustments"],
    ));
    w.pages.push(keys(
        "Retouch",
        ["SpotRemovalTool", "DrawMaskTool", "EraseMaskTool", "NewEmptyLayer", "NewHealLayer", "ToggleLayerOnOff", "ZoomToFit", "Zoom100", "Process"],
    ));
    w.pages.push(dials("Exposure/WB", "Exposure", "Kelvin"));
    w.pages.push(dials("HDR", "Highlight", "Shadow"));
    pads(&mut w, ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust"]);
    out.push(finish(w));

    let mut po = Profile {
        name: "Portrait".into(),
        description: "Skin tones, white balance, gentle adjustments, black and white".into(),
        ..Default::default()
    };
    po.pages.push(keys(
        "Cull",
        ["PreviousImage", "NextImage", "ToggleBeforeAfter", "Rate1Star", "Rate3Stars", "Rate5Stars", "ColorTagGreen", "ColorTagRed", "ColorTagNone"],
    ));
    po.pages.push(keys(
        "Tone",
        ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust", "WBPickerTool", "CropTool", "RotateRight", "Undo", "Redo", "ResetAdjustments"],
    ));
    po.pages.push(keys(
        "Skin & BW",
        [
            "SpotRemovalTool",
            "DrawHealTool",
            "DirectColorEditor",
            "DrawMaskTool",
            "EraseMaskTool",
            "NewEmptyLayer",
            "BWEnable",
            "ZoomToFit",
            "Process",
        ],
    ));
    po.pages.push(dials("Exposure/WB", "Exposure", "Kelvin"));
    po.pages.push(dials("Tone (dials)", "Contrast", "Saturation"));
    pads(&mut po, ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust"]);
    out.push(finish(po));

    let mut f = Profile {
        name: "Fashion".into(),
        description: "Layer-heavy retouching, colour grading, skin tone, creative colour".into(),
        ..Default::default()
    };
    f.pages.push(keys(
        "Cull",
        ["PreviousImage", "NextImage", "ToggleBeforeAfter", "Rate1Star", "Rate3Stars", "Rate5Stars", "ColorTagGreen", "ColorTagRed", "ColorTagNone"],
    ));
    f.pages.push(keys(
        "Retouch",
        [
            "SpotRemovalTool",
            "DrawHealTool",
            "DrawCloneTool",
            "DrawMaskTool",
            "EraseMaskTool",
            "NewEmptyLayer",
            "NewHealLayer",
            "SelectLayerUp",
            "SelectLayerDown",
        ],
    ));
    f.pages.push(keys(
        "Grade",
        ["Undo", "Redo", "ToggleBeforeAfter", "CropTool", "WBPickerTool", "DirectColorEditor", "CopyAdjustments", "ApplyAdjustments", "Process"],
    ));
    f.pages.push(dials("Exposure", "Exposure", "Contrast"));
    f.pages.push(dials("Color", "Saturation", "Kelvin"));
    f.pages.push(dials("Detail", "Clarity", "SharpeningAmount"));
    pads(&mut f, ["SpotRemovalTool", "DrawMaskTool", "NewEmptyLayer"]);
    out.push(finish(f));

    let mut l = Profile {
        name: "Landscape".into(),
        description: "Highlight and shadow recovery, clarity and texture, graduated masks, vivid colour".into(),
        ..Default::default()
    };
    l.pages.push(keys(
        "Navigate",
        ["PreviousImage", "NextImage", "ZoomToFit", "ToggleBeforeAfter", "Zoom100", "Zoom200", "Rate1Star", "Rate5Stars", "ColorTagGreen"],
    ));
    l.pages.push(keys(
        "Tone",
        ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust", "WBPickerTool", "CropTool", "StraightenTool", "Undo", "Redo", "ResetAdjustments"],
    ));
    l.pages.push(keys(
        "Layers",
        [
            "LinearGradientTool",
            "RadialGradientTool",
            "DrawMaskTool",
            "EraseMaskTool",
            "NewEmptyLayer",
            "ToggleLayerOnOff",
            "SelectLayerUp",
            "SelectLayerDown",
            "Process",
        ],
    ));
    l.pages.push(dials("Exposure/WB", "Exposure", "Kelvin"));
    l.pages.push(dials("HDR", "Highlight", "Shadow"));
    l.pages.push(dials("Detail", "Clarity", "Structure"));
    pads(&mut l, ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust"]);
    out.push(finish(l));

    let mut r = Profile {
        name: "Real Estate".into(),
        description: "Perspective correction, white balance, highlights and shadows, batch work".into(),
        ..Default::default()
    };
    r.pages.push(keys(
        "Navigate",
        ["PreviousImage", "NextImage", "ZoomToFit", "ToggleBeforeAfter", "Rate1Star", "Rate5Stars", "ColorTagGreen", "ColorTagRed", "ColorTagNone"],
    ));
    r.pages.push(keys(
        "Correct",
        [
            "CropTool",
            "StraightenTool",
            "KeystoneTool",
            "KeystoneVertical",
            "KeystoneHorizontal",
            "WBPickerTool",
            "AutoAdjust",
            "RotateLeft",
            "RotateRight",
        ],
    ));
    r.pages.push(keys(
        "Process",
        ["CopyAdjustments", "ApplyAdjustments", "ResetAdjustments", "SelectAll", "EditAllSelected", "Process", "Undo", "Redo", "ToggleExposureWarn"],
    ));
    r.pages.push(dials("Exposure/WB", "Exposure", "Kelvin"));
    r.pages.push(dials("HDR", "Highlight", "Shadow"));
    pads(&mut r, ["CopyAdjustments", "ApplyAdjustments", "AutoAdjust"]);
    out.push(finish(r));

    out
}

/// A fine-steps modifier on a key, for profiles the user builds (offered by the editor).
pub fn fine_modifier() -> Binding {
    Binding::Modifier { modifier: Modifier::Fine }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn six_profiles_every_key_page_full_every_binding_known() {
        let ps = profiles();
        let names: Vec<_> = ps.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Default", "Wedding", "Portrait", "Fashion", "Landscape", "Real Estate"]);
        for p in &ps {
            for page in p.pages.iter().filter(|pg| pg.groups == [PageGroup::Keys] && !pg.auto) {
                assert_eq!(page.bindings.len(), 9, "{} / {}: every TactileBridge key is mapped", p.name, page.name);
            }
            for b in p.pages.iter().flat_map(|pg| pg.bindings.values()).chain(p.base.values()) {
                if let Binding::Value { target } = b {
                    assert!(crate::targets::target_spec(target).is_some(), "{} → {target}", p.name);
                }
            }
            assert_eq!(p.base.len(), 12, "{}: four Dialpad buttons, eight Actions Ring slots", p.name);
            assert!(p.contexts.contains_key("default") && p.contexts.contains_key("crop") && p.contexts.contains_key("masking.mask.brush"));
            let mut clone = p.clone();
            clone.sanitize();
            assert_eq!(&clone, p, "built-ins are already clean");
        }
        // Real Estate's Keystone keys became Upright
        let re = &ps[5];
        assert_eq!(re.page("Correct").unwrap().bindings["key.3"], Binding::command("tool.guidedUpright"));
    }
}
