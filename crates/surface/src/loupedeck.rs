//! The Loupedeck+ (Loupedeck's photo-editing console: knobs, a control dial and buttons, no
//! displays) and importing a Loupedeck profile made for another photo editor.
//!
//! Loupedeck's own software numbers the console's controls; its profiles (`ProfileInfo.json`,
//! or an exported `.lp5` holding one) list, per mode and workspace, the action of each press and
//! each turn by that number. LightCraft's controls are those numbers: `ld.r<n>` turns, `ld.b<n>`
//! presses. The LightCraft plugin forwards them; the console needs no setup in LightCraft — every
//! control has a default here, after the labels printed on the console — and an imported profile
//! replaces those defaults with the user's own layout (only the mapping is read: no icons or
//! images come along).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::binding::Binding;
use crate::devices::{ControlDef, ControlKind, PageGroup};
use crate::tactilebridge::{Mapped, Report, ReportLine};

/// The id of a turn (`ld.r33`) or a press (`ld.b95`).
pub fn rotate_id(n: usize) -> String {
    format!("ld.r{n}")
}
pub fn press_id(n: usize) -> String {
    format!("ld.b{n}")
}

const fn knob(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Encoder, label, group: PageGroup::Dials }
}
const fn button(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Button, label, group: PageGroup::Dials }
}

/// The Loupedeck+'s controls, named as printed on the console. P1–P8 send 1–8, 9–16 or 17–24 by
/// the Hue / Sat / Lum buttons, as in Loupedeck's own profiles.
pub const CONTROLS: &[ControlDef] = &[
    knob("ld.r1", "P1 (Hue)"),
    knob("ld.r2", "P2 (Hue)"),
    knob("ld.r3", "P3 (Hue)"),
    knob("ld.r4", "P4 (Hue)"),
    knob("ld.r5", "P5 (Hue)"),
    knob("ld.r6", "P6 (Hue)"),
    knob("ld.r7", "P7 (Hue)"),
    knob("ld.r8", "P8 (Hue)"),
    knob("ld.r9", "P1 (Sat)"),
    knob("ld.r10", "P2 (Sat)"),
    knob("ld.r11", "P3 (Sat)"),
    knob("ld.r12", "P4 (Sat)"),
    knob("ld.r13", "P5 (Sat)"),
    knob("ld.r14", "P6 (Sat)"),
    knob("ld.r15", "P7 (Sat)"),
    knob("ld.r16", "P8 (Sat)"),
    knob("ld.r17", "P1 (Lum)"),
    knob("ld.r18", "P2 (Lum)"),
    knob("ld.r19", "P3 (Lum)"),
    knob("ld.r20", "P4 (Lum)"),
    knob("ld.r21", "P5 (Lum)"),
    knob("ld.r22", "P6 (Lum)"),
    knob("ld.r23", "P7 (Lum)"),
    knob("ld.r24", "P8 (Lum)"),
    knob("ld.r25", "P1 (B&W)"),
    knob("ld.r26", "P2 (B&W)"),
    knob("ld.r27", "P3 (B&W)"),
    knob("ld.r28", "P4 (B&W)"),
    knob("ld.r29", "P5 (B&W)"),
    knob("ld.r30", "P6 (B&W)"),
    knob("ld.r31", "P7 (B&W)"),
    knob("ld.r32", "P8 (B&W)"),
    knob("ld.r33", "Exposure"),
    knob("ld.r34", "Blacks"),
    knob("ld.r35", "Whites"),
    knob("ld.r36", "Saturation"),
    knob("ld.r37", "Vibrance"),
    knob("ld.r38", "Temperature"),
    knob("ld.r39", "Tint"),
    knob("ld.r40", "Highlights"),
    knob("ld.r41", "D1"),
    knob("ld.r42", "D2"),
    knob("ld.r44", "Shadows"),
    knob("ld.r45", "Clarity"),
    knob("ld.r46", "Contrast"),
    knob("ld.r47", "Knob 47"),
    knob("ld.r48", "Control Dial"),
    button("ld.b33", "Exposure (press)"),
    button("ld.b34", "Blacks (press)"),
    button("ld.b35", "Whites (press)"),
    button("ld.b36", "Saturation (press)"),
    button("ld.b37", "Vibrance (press)"),
    button("ld.b38", "Temperature (press)"),
    button("ld.b39", "Tint (press)"),
    button("ld.b40", "Highlights (press)"),
    button("ld.b41", "D1 (press)"),
    button("ld.b42", "D2 (press)"),
    button("ld.b44", "Shadows (press)"),
    button("ld.b45", "Clarity (press)"),
    button("ld.b46", "Contrast (press)"),
    button("ld.b47", "Knob 47 (press)"),
    button("ld.b48", "Control Dial (press)"),
    button("ld.b49", "C1"),
    button("ld.b50", "C2"),
    button("ld.b51", "C3"),
    button("ld.b52", "C4"),
    button("ld.b53", "C5"),
    button("ld.b54", "C6"),
    button("ld.b66", "Star rating 1"),
    button("ld.b67", "Star rating 2"),
    button("ld.b68", "Star rating 3"),
    button("ld.b69", "Star rating 4"),
    button("ld.b70", "Star rating 5"),
    button("ld.b71", "Fn + Star rating 1"),
    button("ld.b72", "Fn + Star rating 2"),
    button("ld.b73", "Fn + Star rating 3"),
    button("ld.b74", "Fn + Star rating 4"),
    button("ld.b75", "Fn + Star rating 5"),
    button("ld.b76", "Button 76"),
    button("ld.b77", "Button 77"),
    button("ld.b78", "Left arrow"),
    button("ld.b79", "Right arrow"),
    button("ld.b80", "P1 (press)"),
    button("ld.b81", "P2 (press)"),
    button("ld.b82", "P3 (press)"),
    button("ld.b83", "P4 (press)"),
    button("ld.b84", "P5 (press)"),
    button("ld.b85", "P6 (press)"),
    button("ld.b86", "P7 (press)"),
    button("ld.b87", "P8 (press)"),
    button("ld.b88", "Export"),
    button("ld.b89", "Button 89"),
    button("ld.b90", "Button 90"),
    button("ld.b91", "Pick"),
    button("ld.b92", "Copy"),
    button("ld.b93", "Paste"),
    button("ld.b94", "Button 94"),
    button("ld.b95", "Undo"),
    button("ld.b96", "Redo"),
    button("ld.b97", "Screen Mode"),
    button("ld.b98", "Hue"),
    button("ld.b99", "Sat"),
    button("ld.b100", "Lum"),
    button("ld.b101", "Col/BW"),
    button("ld.b102", "Before/After"),
    button("ld.b103", "Button 103"),
    button("ld.b104", "Button 104"),
    button("ld.b110", "Custom Mode"),
    button("ld.b114", "L1"),
    button("ld.b115", "L2"),
    button("ld.b116", "L3"),
];

/// The action names of Loupedeck's own default layout for the console (after which its knobs and
/// buttons are labelled): (control, action).
const DEFAULTS: &[(&str, &str)] = &[
    ("ld.r33", "Exposure"),
    ("ld.r34", "Blacks"),
    ("ld.r35", "Whites"),
    ("ld.r36", "Saturation"),
    ("ld.r37", "Vibrance"),
    ("ld.r38", "Temperature"),
    ("ld.r39", "Tint"),
    ("ld.r40", "Highlights"),
    ("ld.r41", "Dehaze"),
    ("ld.r42", "Sharpness"),
    ("ld.r44", "Shadows"),
    ("ld.r45", "Clarity"),
    ("ld.r46", "Contrast"),
    ("ld.r47", "Dehaze"),
    ("ld.r48", "ActivateCropToolByRotation"),
    ("ld.b48", "ActivateCropTool"),
    ("ld.b49", "TogglePickFlag"),
    ("ld.b50", "ToggleRejectFlag"),
    ("ld.b51", "AutoSettings"),
    ("ld.b52", "AutoWhiteBalance"),
    ("ld.b53", "ToggleZoomView"),
    ("ld.b66", "RatingStar1"),
    ("ld.b67", "RatingStar2"),
    ("ld.b68", "RatingStar3"),
    ("ld.b69", "RatingStar4"),
    ("ld.b70", "RatingStar5"),
    ("ld.b71", "RatingColor1"),
    ("ld.b72", "RatingColor2"),
    ("ld.b73", "RatingColor3"),
    ("ld.b74", "RatingColor4"),
    ("ld.b75", "RatingColor5"),
    ("ld.b78", "MoveToPreviousPhoto"),
    ("ld.b79", "MoveToNextPhoto"),
    ("ld.b88", "OpenExportDialog"),
    ("ld.b89", "ToggleZoomView"),
    ("ld.b90", "ToggleZoomView"),
    ("ld.b91", "TogglePickFlag"),
    ("ld.b92", "CopyAllSettings"),
    ("ld.b93", "PasteAllSettings"),
    ("ld.b94", "ActivateDevelopTool___localized"),
    ("ld.b95", "Undo"),
    ("ld.b96", "Redo"),
    ("ld.b97", "FullScreen"),
    ("ld.b101", "ToggleColorMode"),
    ("ld.b102", "ToggleBeforeAfter"),
    ("ld.b103", "AutoSettings"),
    ("ld.b104", "AutoWhiteBalance"),
    ("ld.b114", "ActivateDevelopTool___localized"),
    ("ld.b115", "ActivateDevelopTool___circularGradient"),
    ("ld.b116", "ActivateDevelopTool___gradient"),
];

const BANDS: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta"];

/// What a control does when nothing (no profile, no import) binds it: Loupedeck's own default
/// layout, so the console works as labelled with no setup. P1–P8 move the colour mixer.
pub fn default_binding(control: &str) -> Option<Binding> {
    if let Some(n) = control.strip_prefix("ld.r").and_then(|n| n.parse::<usize>().ok())
        && (1..=32).contains(&n)
    {
        let band = BANDS.get((n - 1) % 8)?.to_ascii_lowercase();
        return Some(Binding::value(&match (n - 1) / 8 {
            0 => format!("mixer.{band}.hue"),
            1 => format!("mixer.{band}.sat"),
            2 => format!("mixer.{band}.lum"),
            _ => format!("bw.{band}"),
        }));
    }
    let (_, action) = DEFAULTS.iter().find(|(c, _)| *c == control)?;
    match map_action(action, &|_| None) {
        Mapped::To(b, _) => Some(b),
        Mapped::Dropped(_) => None,
    }
}

fn c(command: &str) -> Mapped {
    Mapped::To(Binding::command(command), None)
}
fn cp(command: &str, params: Value) -> Mapped {
    Mapped::To(Binding::command_with(command, params), None)
}
fn cn(command: &str, note: &'static str) -> Mapped {
    Mapped::To(Binding::command(command), Some(note))
}

const MACRO: &str = "Loupedeck macros don't come along: set the control in LightCraft";
const OTHER_APP: &str = "it works on another app";

/// A develop value (`Exposure`, `HueAdjustmentRed`, `LocalizedTemperature` …) by its action
/// name: (value id, note).
fn value_of(name: &str) -> Option<(String, Option<&'static str>)> {
    let plain = |s: &str| Some((s.to_string(), None));
    for (prefix, field) in [("HueAdjustment", "hue"), ("SaturationAdjustment", "sat"), ("LuminanceAdjustment", "lum")] {
        if let Some(band) = name.strip_prefix(prefix).filter(|b| BANDS.contains(b)) {
            return plain(&format!("mixer.{}.{field}", band.to_ascii_lowercase()));
        }
    }
    if let Some(band) = name.strip_prefix("GrayMixer").filter(|b| BANDS.contains(b)) {
        return plain(&format!("bw.{}", band.to_ascii_lowercase()));
    }
    if let Some(local) = name.strip_prefix("Localized") {
        let field = match local {
            "Exposure" => "exposure",
            "Contrast" => "contrast",
            "Highlights" => "highlights",
            "Shadows" => "shadows",
            "Whites" => "whites",
            "Blacks" => "blacks",
            "Temperature" => "temp",
            "Tint" => "tint",
            "Clarity" => "clarity",
            "Texture" => "texture",
            "Dehaze" => "dehaze",
            "Saturation" => "saturation",
            "Defringe" => "defringe",
            "Moire" => "moire",
            "LuminanceNoise" => "noise",
            "Sharpness" => "sharpness",
            "Hue" => "hue",
            _ => return None,
        };
        return plain(&format!("mask.{field}"));
    }
    // a value turned with its clipping view (Alt held) is the value itself
    let base = name.strip_suffix("Clipping").unwrap_or(name);
    let id = match base {
        "Exposure" => "light.exposure",
        "Contrast" => "light.contrast",
        "Highlights" => "light.highlights",
        "Shadows" => "light.shadows",
        "Whites" => "light.whites",
        "Blacks" => "light.blacks",
        "Temperature" => "wb.temp",
        "Tint" => "wb.tint",
        "Vibrance" => "color.vibrance",
        "Saturation" => "color.saturation",
        "Clarity" => "effects.clarity",
        "Texture" => "effects.texture",
        "Dehaze" => "effects.dehaze",
        "Sharpness" => "detail.sharpenAmount",
        "SharpenRadius" => "detail.sharpenRadius",
        "SharpenDetail" => "detail.sharpenDetail",
        "SharpenEdgeMasking" => "detail.sharpenMasking",
        "LuminanceSmoothing" => "detail.nrLuminance",
        "LuminanceNoiseReductionDetail" => "detail.nrDetail",
        "LuminanceNoiseReductionContrast" => "detail.nrContrast",
        "ColorNoiseReduction" => "detail.nrColor",
        "ColorNoiseReductionDetail" => "detail.nrColorDetail",
        "ColorNoiseReductionSmoothness" => "detail.nrColorSmoothness",
        "PostCropVignetteAmount" => "vignette.amount",
        "PostCropVignetteMidpoint" => "vignette.midpoint",
        "PostCropVignetteFeather" => "vignette.feather",
        "PostCropVignetteRoundness" => "vignette.roundness",
        "PostCropVignetteHighlightContrast" => "vignette.highlights",
        "ParametricHighlights" => "curve.highlights",
        "ParametricLights" => "curve.lights",
        "ParametricDarks" => "curve.darks",
        "ParametricShadows" => "curve.shadows",
        "ParametricShadowSplit" => "curve.splitShadows",
        "ParametricMidtoneSplit" => "curve.splitMid",
        "ParametricHighlightSplit" => "curve.splitHighlights",
        "SplitToningHighlightHue" => "grading.highlights.hue",
        "SplitToningHighlightSaturation" => "grading.highlights.sat",
        "SplitToningShadowHue" => "grading.shadows.hue",
        "SplitToningShadowSaturation" => "grading.shadows.sat",
        "SplitToningBalance" => "grading.balance",
        "RedHue" => "calibration.redHue",
        "RedSaturation" => "calibration.redSat",
        "GreenHue" => "calibration.greenHue",
        "GreenSaturation" => "calibration.greenSat",
        "BlueHue" => "calibration.blueHue",
        "BlueSaturation" => "calibration.blueSat",
        "ShadowTint" => "calibration.shadowsTint",
        "AdjustmentBrushSize" => "ui.brushSize",
        "AdjustmentBrushFeatherSize" => "ui.brushFeather",
        "AdjustmentBrushFlow" => "ui.brushFlow",
        "RotateCropArea" | "RotateCropAreaSome" | "ActivateCropToolByRotation" | "Straighten" => {
            return Some(("crop.angle".into(), Some("straightens the photo")));
        }
        _ => return None,
    };
    let note = if base != name { Some("the clipping view isn't shown while turning") } else { None };
    Some((id.into(), note))
}

/// A Loupedeck photo-editor action (`Exposure`, `RatingStar3`, `ApplyDevelopPreset___Group|Name`,
/// `@Macro___<id>` …) as a LightCraft binding. `preset` finds a LightCraft preset id by the
/// preset's name.
pub fn map_action(action: &str, preset: &dyn Fn(&str) -> Option<String>) -> Mapped {
    let (name, param) = match action.split_once("___") {
        Some((n, p)) => (n, Some(p)),
        None => (action, None),
    };
    if let Some((id, note)) = value_of(name) {
        return Mapped::To(Binding::value(&id), note);
    }
    if let Some(value) = name.strip_prefix("Reset").and_then(value_of).map(|(id, _)| id) {
        if value.starts_with("ui.") || value.starts_with("mask.") || value == "crop.angle" {
            return Mapped::Dropped("LightCraft resets a value when its knob is pressed");
        }
        return Mapped::To(Binding::command_with("develop.resetControl", json!({"control": value})), None);
    }
    let digit = |s: &str| s.chars().next().and_then(|d| d.to_digit(10));
    if let Some(n) = name.strip_prefix("RatingStar").and_then(digit) {
        return cp("photo.rate", json!({"rating": n}));
    }
    if let Some(n) = name.strip_prefix("RatingFilter").and_then(digit) {
        return cp("library.filter", json!({"rating": n, "ratingOp": "atLeast"}));
    }
    const LABELS: [&str; 5] = ["red", "yellow", "green", "blue", "purple"];
    let label = |s: &str| digit(s).and_then(|n| LABELS.get((n as usize).wrapping_sub(1)).copied());
    if let Some(l) = name.strip_prefix("RatingColor").and_then(label) {
        return cp("photo.label", json!({"label": l}));
    }
    if let Some(l) = name.strip_prefix("ColorFilter").and_then(label) {
        return cp("library.filter", json!({"label": l}));
    }
    match name {
        "Undo" => c("edit.undo"),
        "Redo" => c("edit.redo"),
        "CopyAllSettings" | "CopySettings" => c("develop.copy"),
        "PasteAllSettings" | "PasteSettings" => c("develop.paste"),
        "PasteFromPrevious" => c("develop.pastePrevious"),
        "SyncSettings" => cn("develop.sync", "copies the active photo's settings to the other selected photos"),
        "AutoSettings" => c("develop.auto"),
        "AutoWhiteBalance" => cp("develop.wb", json!({"mode": "auto"})),
        "ResetAllAdjustments" => c("develop.reset"),
        "ResetCrop" => c("crop.reset"),
        "TogglePickFlag" => c("photo.pick"),
        "ToggleRejectFlag" => c("photo.reject"),
        "FlagFilter" => match param {
            Some("flagged") => cp("library.filter", json!({"flag": "pick"})),
            Some("rejected") => cp("library.filter", json!({"flag": "reject"})),
            Some("unflagged") => cp("library.filter", json!({"flag": "none"})),
            _ => Mapped::Dropped("an unknown flag filter"),
        },
        "MoveToNextPhoto" | "SelectRight" | "MoveRight" => c("library.next"),
        "MoveToPreviousPhoto" | "SelectLeft" | "MoveLeft" => c("library.previous"),
        "SelectAll" => c("library.selectAll"),
        "DeselectOtherPhotos" | "SelectNone" => c("library.selectNone"),
        "ToggleZoomView" => c("view.zoomToggle"),
        "ZoomIn" => c("view.zoomIn"),
        "ZoomOut" => c("view.zoomOut"),
        "FullScreen" => c("view.fullScreenPreview"),
        "ToggleBeforeAfter" => c("view.beforeAfter"),
        "ShowClipping" => c("view.clipping"),
        "TogglePanels" => cn("panel.edit", "opens or closes the Edit panel"),
        "ToggleDevelopLibraryModules" => cn("view.back", "back to the grid"),
        "ToggleColorMode" => cn("develop.treatment", "colour or black & white"),
        "OpenExportDialog" | "ExportWithPreset" | "ExportWithPrevious" => cn("dialog.export", "the Export dialog"),
        "CreateVirtualCopies" | "CreateVirtualCopy" => c("photo.virtualCopy"),
        "ActivateCropTool" => c("panel.crop"),
        "CycleCropGridOverlay" => c("view.cropOverlay"),
        "CycleCropGridOverlayOrientation" => c("view.cropOverlayOrientation"),
        "ToggleCropAspectRatio" => cp("crop.aspect", json!({"aspect": "toggle"})),
        "TogglePerspectiveUpright" => cpn("geometry.upright", json!({"mode": "auto"}), "Upright: Auto"),
        "DeactivateDevelopTool" => c("tool.done"),
        "ActivateDevelopTool" => match param {
            Some("localized") => cn("tool.brush", "the brush"),
            Some("circularGradient") => c("tool.radial"),
            Some("gradient") => c("tool.linear"),
            Some("spotRemoval") => cn("panel.remove", "the Remove tool (heal and clone)"),
            Some("redeye") => c("panel.redeye"),
            Some("crop") => c("panel.crop"),
            Some("masking") => cn("panel.masking", "the Masks panel"),
            _ => Mapped::Dropped("LightCraft has no such tool"),
        },
        "ApplyDevelopPreset" => {
            let full = param.unwrap_or_default();
            // `Group|Name`: LightCraft's presets go by name
            let short = full.rsplit('|').next().unwrap_or(full).trim_end_matches('.').trim();
            match preset(short).or_else(|| preset(full)) {
                Some(id) => Mapped::To(Binding::command_with("preset.apply", json!({"id": id})), None),
                None => Mapped::Dropped("no LightCraft preset has this name: import the preset, then import the profile again"),
            }
        }
        "@Macro" => Mapped::Dropped(MACRO),
        "@ActivateClientApplication" | "@ActivateLoupedeckConfiguration" => {
            Mapped::Dropped("Loupedeck's own button: it switches the console's app or profile")
        }
        "EditInPhotoshop" | "EditInOtherApplication" => Mapped::Dropped(OTHER_APP),
        "SetColorModeHue" | "SetColorModeSaturation" | "SetColorModeLuminance" => {
            Mapped::Dropped("Hue, Sat and Lum switch what P1–P8 send; LightCraft follows the console")
        }
        _ => Mapped::Dropped("LightCraft has no counterpart for this action yet"),
    }
}

fn cpn(command: &str, params: Value, note: &'static str) -> Mapped {
    Mapped::To(Binding::command_with(command, params), Some(note))
}

/// An imported Loupedeck profile: the bindings of the console's controls and what came over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Imported {
    /// The profile's name in Loupedeck's software.
    pub name: String,
    pub bindings: BTreeMap<String, Binding>,
    pub report: Report,
}

/// The largest profile read (a Loupedeck profile is tens of kilobytes).
const MAX_BYTES: usize = 32 << 20;

/// Read a Loupedeck profile: its `ProfileInfo.json`, or an exported `.lp5` (a ZIP holding one).
/// Only the console's main mode (the photo in the editor) and its first workspace are read; the
/// `Fn` workspace's presses go to the same controls when the main one leaves them empty.
pub fn import(bytes: &[u8], preset: &dyn Fn(&str) -> Option<String>) -> Result<Imported, String> {
    if bytes.len() > MAX_BYTES {
        return Err("This file is too large to be a Loupedeck profile.".into());
    }
    let json = if bytes.starts_with(b"PK") {
        crate::zip::read_member(bytes, "ProfileInfo.json").map_err(|e| format!("This .lp5 file has no profile in it: {e}"))?
    } else {
        bytes.to_vec()
    };
    let text = String::from_utf8_lossy(&json);
    let text = text.trim_start_matches('\u{feff}');
    let v: Value = serde_json::from_str(text).map_err(|e| format!("This isn't a Loupedeck profile ({e})."))?;
    let device = v.get("deviceType").and_then(Value::as_str).unwrap_or_default();
    if !device.is_empty() && device != "Loupedeck15" && device != "Loupedeck10" {
        return Err(format!("This profile is for another Loupedeck console ({device}); LightCraft reads Loupedeck+ profiles."));
    }
    let modes = v.pointer("/layout/layoutModes").and_then(Value::as_array).ok_or("This Loupedeck profile has no layout.")?;
    // the editor's own mode: `loupe` (Loupedeck's name for the develop view), else the first
    let mode = modes
        .iter()
        .find(|m| m.get("modeName").and_then(Value::as_str) == Some("loupe"))
        .or_else(|| modes.first())
        .ok_or("This Loupedeck profile has no layout.")?;
    let workspaces = mode.get("workspaces").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Imported { name: v.get("displayName").and_then(Value::as_str).unwrap_or("Loupedeck").to_string(), ..Default::default() };
    // (control, action) of the main workspace, then the Fn workspace for what it leaves empty
    let mut seen = std::collections::BTreeSet::new();
    for ws in workspaces.iter().take(2) {
        for (kind, id_of) in [("rotateActions", rotate_id as fn(usize) -> String), ("pressActions", press_id as fn(usize) -> String)] {
            let Some(list) = ws.get(kind).and_then(Value::as_array) else { continue };
            for (n, a) in list.iter().enumerate().take(512) {
                let Some(raw) = a.as_str() else { continue };
                let control = id_of(n);
                if !seen.insert(control.clone()) {
                    continue;
                }
                // `$Lightroom___Exposure`, `$@Generic___@Macro___<id>`, `LR:Exposure`
                let action = match raw.strip_prefix('$') {
                    // `$<plugin>___<action>`: the action without its plugin
                    Some(owned) => owned.split_once("___").map_or(owned, |(_, rest)| rest),
                    None => raw.strip_prefix("LR:").unwrap_or(raw),
                };
                let from = format!("{}: {}", label(&control), pretty(action));
                // (an index the Loupedeck+ has no control for could never be pressed or turned)
                if !CONTROLS.iter().any(|c| c.id == control) {
                    out.report.dropped.push(ReportLine { from, to: String::new(), note: "no such control on the Loupedeck+".into() });
                    continue;
                }
                match map_action(action, preset) {
                    Mapped::To(b, note) => {
                        out.report.mapped.push(ReportLine { from, to: b.describe(), note: note.unwrap_or_default().into() });
                        out.bindings.insert(control, b);
                    }
                    Mapped::Dropped(why) => out.report.dropped.push(ReportLine { from, to: String::new(), note: why.into() }),
                }
            }
        }
    }
    Ok(out)
}

/// A control's printed name.
pub fn label(control: &str) -> String {
    CONTROLS.iter().find(|c| c.id == control).map_or_else(|| control.to_string(), |c| c.label.to_string())
}

/// `ApplyDevelopPreset___Group|Name` → `ApplyDevelopPreset: Group|Name`, a macro's id dropped.
fn pretty(action: &str) -> String {
    match action.split_once("___") {
        Some(("@Macro", _)) => "Macro".into(),
        Some((n, p)) => format!("{n}: {p}"),
        None => action.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(press: &[(usize, &str)], rotate: &[(usize, &str)]) -> Vec<u8> {
        let mut p = vec![Value::Null; 127];
        for (i, a) in press {
            p[*i] = json!(a);
        }
        let mut r = vec![Value::Null; 127];
        for (i, a) in rotate {
            r[*i] = json!(a);
        }
        serde_json::to_vec(&json!({
            "displayName": "My LD+",
            "deviceType": "Loupedeck15",
            "layout": {"layoutModes": [
                {"modeName": "library", "workspaces": [{"pressActions": [], "rotateActions": []}]},
                {"modeName": "loupe", "workspaces": [{"pressActions": p, "rotateActions": r}]}
            ]}
        }))
        .unwrap()
    }

    #[test]
    fn imports_the_editor_mode_of_a_loupedeck_profile() {
        let bytes = profile(
            &[
                (95, "$Lightroom___Undo"),
                (67, "$Lightroom___RatingStar2"),
                (80, "$Lightroom___ApplyDevelopPreset___Mine|Soft Skin"),
                (76, "$@Generic___@Macro___ABC"),
            ],
            &[(33, "$Lightroom___Exposure"), (34, "$Lightroom___Temperature"), (9, "$Lightroom___SaturationAdjustmentRed")],
        );
        let presets = |n: &str| (n == "Soft Skin").then(|| "p-1".to_string());
        let out = import(&bytes, &presets).unwrap();
        assert_eq!(out.name, "My LD+");
        assert_eq!(out.bindings.get("ld.r33"), Some(&Binding::value("light.exposure")));
        assert_eq!(out.bindings.get("ld.r34"), Some(&Binding::value("wb.temp")));
        assert_eq!(out.bindings.get("ld.r9"), Some(&Binding::value("mixer.red.sat")));
        assert_eq!(out.bindings.get("ld.b95"), Some(&Binding::command("edit.undo")));
        assert_eq!(out.bindings.get("ld.b67"), Some(&Binding::command_with("photo.rate", json!({"rating": 2}))));
        assert_eq!(out.bindings.get("ld.b80"), Some(&Binding::command_with("preset.apply", json!({"id": "p-1"}))));
        assert!(!out.bindings.contains_key("ld.b76"), "macros don't come along");
        assert_eq!(out.report.dropped.len(), 1);
        assert!(out.report.dropped[0].from.contains("Button 76"), "{:?}", out.report.dropped);
    }

    /// An action at an index no control sends is skipped and reported, not kept as a binding.
    #[test]
    fn an_action_for_no_control_is_reported_not_kept() {
        let bytes = profile(&[(0, "$Lightroom___Undo")], &[(43, "$Lightroom___Exposure"), (33, "$Lightroom___Exposure")]);
        let out = import(&bytes, &|_| None).unwrap();
        assert!(!out.bindings.contains_key("ld.b0") && !out.bindings.contains_key("ld.r43"), "{:?}", out.bindings);
        assert!(out.bindings.contains_key("ld.r33"));
        assert_eq!(out.report.dropped.iter().filter(|l| l.note == "no such control on the Loupedeck+").count(), 2, "{:?}", out.report.dropped);
    }

    #[test]
    fn bad_files_are_errors() {
        assert!(import(b"not json", &|_| None).is_err());
        assert!(import(b"{}", &|_| None).is_err());
        assert!(import(br#"{"deviceType":"Loupedeck70","layout":{"layoutModes":[]}}"#, &|_| None).is_err());
        assert!(import(b"PK\x03\x04garbage", &|_| None).is_err());
        assert!(import(br#"{"layout":{"layoutModes":[{"workspaces":[{"pressActions":[1,{"a":2},null,"$Lightroom___Undo"]}]}]}}"#, &|_| None).is_ok());
    }

    #[test]
    fn the_console_works_as_labelled_with_no_setup() {
        assert_eq!(default_binding("ld.r33"), Some(Binding::value("light.exposure")));
        assert_eq!(default_binding("ld.r46"), Some(Binding::value("light.contrast")));
        assert_eq!(default_binding("ld.r1"), Some(Binding::value("mixer.red.hue")));
        assert_eq!(default_binding("ld.r24"), Some(Binding::value("mixer.magenta.lum")));
        assert_eq!(default_binding("ld.r32"), Some(Binding::value("bw.magenta")));
        assert_eq!(default_binding("ld.b95"), Some(Binding::command("edit.undo")));
        assert_eq!(default_binding("ld.b999"), None);
        assert_eq!(default_binding("ld.rx"), None);
        // every default names a known LightCraft action or value
        for (control, _) in DEFAULTS {
            assert!(default_binding(control).is_some(), "{control}");
            assert!(CONTROLS.iter().any(|c| c.id == *control), "{control}");
        }
    }
}
