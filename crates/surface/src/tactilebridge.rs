//! Bring a TactileBridge setup over: its `settings.json` (dial feel, OSD, colours, tool contexts,
//! Auto Sync) and its Logi profiles (`.lp5`), with every TactileBridge action mapped to a
//! LightCraft binding or reported as having no LightCraft counterpart.
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: the action keys, groups,
//! default tool contexts and curated layouts are TactileBridge's. Capture One's own control ids
//! and anything learnt from its files are not used: actions are mapped by their TactileBridge
//! names only.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::binding::Binding;
use crate::devices::PageGroup;
use crate::profile::{Page, Profile};
use crate::settings::{OsdPosition, SurfaceSettings, Tuning, hex_color};

/// Is it a dial value or a key action in TactileBridge?
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TbKind {
    Adjustment,
    Command,
}

/// Where a TactileBridge action goes in LightCraft.
#[derive(Clone, Debug, PartialEq)]
pub enum Mapped {
    /// The LightCraft binding, with a note when the match is not exact.
    To(Binding, Option<&'static str>),
    /// No LightCraft counterpart, and why.
    Dropped(&'static str),
}

/// TactileBridge's dial values (its `KnownAdjustmentGroups`, plus the Color Editor values).
pub const ADJUSTMENTS: &[&str] = &[
    "Kelvin",
    "Tint",
    "Exposure",
    "Contrast",
    "Brightness",
    "Saturation",
    "Highlight",
    "Shadow",
    "White",
    "Black",
    "MasterToneX",
    "MasterToneY",
    "ShadowToneX",
    "ShadowToneY",
    "ShadowLightness",
    "MidtoneToneX",
    "MidtoneToneY",
    "MidtoneLightness",
    "HighlightToneX",
    "HighlightToneY",
    "HighlightLightness",
    "BWRed",
    "BWYellow",
    "BWGreen",
    "BWCyan",
    "BWBlue",
    "BWMagenta",
    "BWHighlightsHue",
    "BWHighlightsSat",
    "BWShadowsHue",
    "BWShadowsSat",
    "Clarity",
    "Structure",
    "Vignetting",
    "Distortion",
    "SharpnessFalloff",
    "LightFalloff",
    "Defringe",
    "UniformLight",
    "SharpeningAmount",
    "SharpeningRadius",
    "SharpeningThreshold",
    "SharpeningHalo",
    "NRLuminance",
    "NRDetail",
    "NRColor",
    "NRSinglePixel",
    "GrainImpact",
    "GrainGranularity",
    "MoireAmount",
    "MoirePattern",
    "LayerOpacity",
    "SpotRadius",
    "BrushSize",
    "BrushHardness",
    "BrushOpacity",
    "BrushFlow",
    "Zoom",
    "OverlayOpacity",
    "OverlayScale",
    "OverlayHorizontal",
    "OverlayVertical",
    "PenSize",
    "CERedHue",
    "CERedSat",
    "CERedLight",
    "CEOrangeHue",
    "CEOrangeSat",
    "CEOrangeLight",
    "CEYellowHue",
    "CEYellowSat",
    "CEYellowLight",
    "CEGreenHue",
    "CEGreenSat",
    "CEGreenLight",
    "CECyanHue",
    "CECyanSat",
    "CECyanLight",
    "CEBlueHue",
    "CEBlueSat",
    "CEBlueLight",
    "CEPurpleHue",
    "CEPurpleSat",
    "CEPurpleLight",
    "CEMagentaHue",
    "CEMagentaSat",
    "CEMagentaLight",
];

/// TactileBridge's key actions (its `KnownCommandGroups`).
pub const COMMANDS: &[&str] = &[
    "ClearRating",
    "Rate1Star",
    "Rate2Stars",
    "Rate3Stars",
    "Rate4Stars",
    "Rate5Stars",
    "ColorTagNone",
    "ColorTagRed",
    "ColorTagOrange",
    "ColorTagYellow",
    "ColorTagGreen",
    "ColorTagBlue",
    "ColorTagPink",
    "ColorTagPurple",
    "PreviousImage",
    "NextImage",
    "FirstImage",
    "LastImage",
    "SelectAll",
    "DeselectAll",
    "InvertSelection",
    "SelectPrimaryOnly",
    "DeselectPrimary",
    "SelectSameColorTag",
    "SelectSameSequenceId",
    "SelectSameVariantPos",
    "PreviousSet",
    "NextSet",
    "PreviousCollection",
    "NextCollection",
    "Undo",
    "Redo",
    "ResetAdjustments",
    "CopyAdjustments",
    "ApplyAdjustments",
    "CopyAndApplyAdj",
    "AutoAdjust",
    "RotateLeft",
    "RotateRight",
    "SnapTo90",
    "NewEmptyLayer",
    "NewFilledLayer",
    "NewCloneLayer",
    "NewHealLayer",
    "RemoveLayer",
    "SelectLayerUp",
    "SelectLayerDown",
    "SelectBackground",
    "ToggleLayerOnOff",
    "InvertMask",
    "FillMask",
    "ClearMask",
    "RasterizeMask",
    "RefineMask",
    "FeatherMask",
    "LumaRange",
    "ToggleSelectionPoints",
    "ZoomIn",
    "ZoomOut",
    "ZoomToFit",
    "Zoom100",
    "Zoom200",
    "Zoom400",
    "IncreaseZoom",
    "DecreaseZoom",
    "ToggleMultiView",
    "ToggleExposureWarn",
    "ToggleFocusMask",
    "ToggleOverlay",
    "ToggleBeforeAfter",
    "ShowGuides",
    "ShowGrid",
    "RecipeProofing",
    "FullScreen",
    "SlideShow",
    "ShowHideViewer",
    "ShowHideBrowser",
    "ToggleTools",
    "SelectTool",
    "MoveTool",
    "PanTool",
    "ZoomInTool",
    "ZoomOutTool",
    "CropTool",
    "SwitchCropOrientation",
    "StraightenTool",
    "RotateFreehandTool",
    "SpotRemovalTool",
    "WBPickerTool",
    "ApplyNormalization",
    "PickNormTarget",
    "PickHighlightLevel",
    "PickShadowLevel",
    "PickCurvePoint",
    "PickLumaCurve",
    "PickColorCorrection",
    "DirectColorEditor",
    "PickSkinColor",
    "AddColorReadout",
    "DeleteColorReadout",
    "OverlayTool",
    "WatermarkTool",
    "FocusMeterTool",
    "PickFocusPoint",
    "LoupeTool",
    "KeystoneTool",
    "KeystoneVertical",
    "KeystoneHorizontal",
    "DrawMaskTool",
    "DrawHealTool",
    "DrawCloneTool",
    "EraseMaskTool",
    "LinearGradientTool",
    "RadialGradientTool",
    "Process",
    "ExportOriginals",
    "ExportVariants",
    "ImportImages",
    "Print",
    "EditAllSelected",
    "Rename",
    "NewVariant",
    "CloneVariant",
    "Delete",
    "MoveToTrash",
    "RegeneratePreviews",
    "SetCompareVariant",
    "ClearCompareVariant",
    "PickVariant",
    "PromoteVariant",
    "DemoteVariant",
    "ShowInLibrary",
    "ShowInFileSystem",
    "Capture",
    "CompositionMode",
    "LiveView",
    "AutoFocus",
    "AutoFocusFine",
    "ShutterSpeedUp",
    "ShutterSpeedDown",
    "ApertureUp",
    "ApertureDown",
    "ExposureBiasUp",
    "ExposureBiasDown",
    "ISOUp",
    "ISODown",
    "BWEnable",
    "ToggleGrading",
    "SelectPen",
    "SelectEraser",
    "PenBlack",
    "PenWhite",
    "PenRed",
    "PenYellow",
    "PenGreen",
    "PenBlue",
    "ToggleSelectImage",
    "NextGroup",
    "PreviousGroup",
    "NextImageImport",
    "PreviousImageImport",
    "FilterRatingNone",
    "FilterRating1Star",
    "FilterRating2Stars",
    "FilterRating3Stars",
    "FilterRating4Stars",
    "FilterRating5Stars",
    "FilterTagNone",
    "FilterTagRed",
    "FilterTagOrange",
    "FilterTagYellow",
    "FilterTagGreen",
    "FilterTagBlue",
    "FilterTagPink",
    "FilterTagPurple",
    "SelectUnrated",
    "Select1Star",
    "Select2Stars",
    "Select3Stars",
    "Select4Stars",
    "Select5Stars",
    "SelectTagNone",
    "SelectTagRed",
    "SelectTagOrange",
    "SelectTagYellow",
    "SelectTagGreen",
    "SelectTagBlue",
    "SelectTagPink",
    "SelectTagPurple",
    "AutoSync",
];

fn v(target: &str) -> Mapped {
    Mapped::To(Binding::value(target), None)
}
fn vn(target: &str, note: &'static str) -> Mapped {
    Mapped::To(Binding::value(target), Some(note))
}
fn c(command: &str) -> Mapped {
    Mapped::To(Binding::command(command), None)
}
fn cn(command: &str, note: &'static str) -> Mapped {
    Mapped::To(Binding::command(command), Some(note))
}
fn cp(command: &str, params: Value) -> Mapped {
    Mapped::To(Binding::command_with(command, params), None)
}
fn cpn(command: &str, params: Value, note: &'static str) -> Mapped {
    Mapped::To(Binding::command_with(command, params), Some(note))
}

const NO_LABEL: &str = "LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's)";
const C1_ONLY: &str = "a Capture One feature LightCraft does not have";
const TETHER: &str = "tethered capture is not in LightCraft yet";
const ANNOTATE: &str = "LightCraft has no annotations";

fn label_of(tag: &str) -> Option<&'static str> {
    Some(match tag {
        "None" => "none",
        "Red" => "red",
        "Yellow" => "yellow",
        "Green" => "green",
        "Blue" => "blue",
        "Purple" => "purple",
        _ => return None,
    })
}

/// Where a TactileBridge action key goes in LightCraft.
pub fn map_key(key: &str) -> Mapped {
    // ratings, labels, filters and selections come in families
    if let Some(n) = key.strip_prefix("Rate").and_then(|r| r.chars().next()).and_then(|d| d.to_digit(10)) {
        return cp("photo.rate", json!({"rating": n}));
    }
    if let Some(tag) = key.strip_prefix("ColorTag") {
        return label_of(tag).map_or(Mapped::Dropped(NO_LABEL), |l| cp("photo.label", json!({"label": l})));
    }
    if let Some(tag) = key.strip_prefix("FilterTag") {
        return label_of(tag)
            .map_or(Mapped::Dropped(NO_LABEL), |l| cp("library.filter", json!({"label": if l == "none" { Value::Null } else { json!(l) }})));
    }
    if let Some(tag) = key.strip_prefix("SelectTag") {
        return label_of(tag).map_or(Mapped::Dropped(NO_LABEL), |l| cp("library.selectBy", json!({"label": l})));
    }
    if let Some(r) = key.strip_prefix("FilterRating") {
        return match r.chars().next().and_then(|d| d.to_digit(10)) {
            Some(n) => cp("library.filter", json!({"rating": n, "ratingOp": "atLeast"})),
            None => cpn("library.filter", json!({"rating": 0, "ratingOp": "exactly"}), "shows unrated photos"),
        };
    }
    if let Some(n) = key.strip_prefix("Select").and_then(|r| r.chars().next()).and_then(|d| d.to_digit(10)) {
        return cp("library.selectBy", json!({"rating": n, "ratingOp": "eq"}));
    }
    if let Some(m) = map_mixer(key) {
        return m;
    }
    match key {
        // ---- dial values
        "Kelvin" => v("wb.temp"),
        "Tint" => v("wb.tint"),
        "Exposure" => v("light.exposure"),
        "Contrast" => v("light.contrast"),
        "Brightness" => Mapped::Dropped("Lightroom and LightCraft have no Brightness slider (use Exposure, Whites or the Tone Curve)"),
        "Saturation" => v("color.saturation"),
        "Highlight" => v("light.highlights"),
        "Shadow" => v("light.shadows"),
        "White" => v("light.whites"),
        "Black" => v("light.blacks"),
        "MasterToneX" => vn("grading.global.hue", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "MasterToneY" => vn("grading.global.sat", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "ShadowToneX" => vn("grading.shadows.hue", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "ShadowToneY" => vn("grading.shadows.sat", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "ShadowLightness" => v("grading.shadows.lum"),
        "MidtoneToneX" => vn("grading.midtones.hue", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "MidtoneToneY" => vn("grading.midtones.sat", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "MidtoneLightness" => v("grading.midtones.lum"),
        "HighlightToneX" => vn("grading.highlights.hue", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "HighlightToneY" => vn("grading.highlights.sat", "Color Grading uses hue and saturation where Capture One's Color Balance uses x / y"),
        "HighlightLightness" => v("grading.highlights.lum"),
        "BWRed" => v("bw.red"),
        "BWYellow" => v("bw.yellow"),
        "BWGreen" => v("bw.green"),
        "BWCyan" => vn("bw.aqua", "Cyan is called Aqua in the B&W mixer"),
        "BWBlue" => v("bw.blue"),
        "BWMagenta" => v("bw.magenta"),
        "BWHighlightsHue" => vn("grading.highlights.hue", "B&W split toning is done with Color Grading"),
        "BWHighlightsSat" => vn("grading.highlights.sat", "B&W split toning is done with Color Grading"),
        "BWShadowsHue" => vn("grading.shadows.hue", "B&W split toning is done with Color Grading"),
        "BWShadowsSat" => vn("grading.shadows.sat", "B&W split toning is done with Color Grading"),
        "Clarity" => v("effects.clarity"),
        "Structure" => vn("effects.texture", "Texture is the closest to Capture One's Structure"),
        "Vignetting" => vn("vignette.amount", "post-crop vignette"),
        "Distortion" => v("optics.distortion"),
        "SharpnessFalloff" => Mapped::Dropped("LightCraft has no sharpness falloff correction"),
        "LightFalloff" => vn("optics.vignetting", "lens vignetting (light falloff) correction"),
        "Defringe" => vn("optics.defringePurple", "purple fringe amount"),
        "UniformLight" => Mapped::Dropped(C1_ONLY),
        "SharpeningAmount" => v("detail.sharpenAmount"),
        "SharpeningRadius" => v("detail.sharpenRadius"),
        "SharpeningThreshold" => vn("detail.sharpenMasking", "Masking is the closest to a sharpening threshold"),
        "SharpeningHalo" => Mapped::Dropped("LightCraft has no halo suppression slider"),
        "NRLuminance" => v("detail.nrLuminance"),
        "NRDetail" => v("detail.nrDetail"),
        "NRColor" => v("detail.nrColor"),
        "NRSinglePixel" => Mapped::Dropped("LightCraft has no single-pixel noise slider"),
        "GrainImpact" => v("grain.amount"),
        "GrainGranularity" => v("grain.size"),
        "MoireAmount" => vn("mask.moire", "moiré is a mask adjustment in LightCraft (as in Lightroom)"),
        "MoirePattern" => Mapped::Dropped("LightCraft's moiré reduction has no pattern size"),
        "LayerOpacity" => vn("mask.amount", "a mask's Amount"),
        "SpotRadius" => vn("ui.removeSize", "the Remove brush size"),
        "BrushSize" => v("ui.brushSize"),
        "BrushHardness" => vn("ui.brushFeather", "Feather (softness) — turn the other way for harder"),
        "BrushOpacity" => Mapped::Dropped("LightCraft brushes have Size, Feather and Flow"),
        "BrushFlow" => v("ui.brushFlow"),
        "Zoom" => v("ui.zoom"),
        "OverlayOpacity" | "OverlayScale" | "OverlayHorizontal" | "OverlayVertical" | "OverlayTool" => Mapped::Dropped(C1_ONLY),
        "PenSize" => Mapped::Dropped(ANNOTATE),
        // ---- keys
        "ClearRating" => cp("photo.rate", json!({"rating": 0})),
        "PreviousImage" => c("library.previous"),
        "NextImage" => c("library.next"),
        "FirstImage" | "LastImage" => Mapped::Dropped("no LightCraft command jumps to the first or last photo yet"),
        "SelectAll" => c("library.selectAll"),
        "DeselectAll" => c("library.selectNone"),
        "InvertSelection" => Mapped::Dropped("no LightCraft command inverts the selection yet"),
        "SelectPrimaryOnly" | "DeselectPrimary" | "SelectSameColorTag" | "SelectSameSequenceId" | "SelectSameVariantPos" => Mapped::Dropped(C1_ONLY),
        "PreviousSet" | "NextSet" | "PreviousCollection" | "NextCollection" => Mapped::Dropped(C1_ONLY),
        "SelectUnrated" => cp("library.selectBy", json!({"rating": 0, "ratingOp": "eq"})),
        "Undo" => c("edit.undo"),
        "Redo" => c("edit.redo"),
        "ResetAdjustments" => c("develop.reset"),
        "CopyAdjustments" => c("develop.copy"),
        "ApplyAdjustments" => c("develop.paste"),
        "CopyAndApplyAdj" => cn("develop.sync", "copies the active photo's settings to the other selected photos"),
        "AutoAdjust" => c("develop.auto"),
        "RotateLeft" => c("photo.rotateLeft"),
        "RotateRight" => c("photo.rotateRight"),
        "SnapTo90" => Mapped::To(Binding::command_with("develop.resetControl", json!({"control": "crop.angle"})), Some("straightens back to 0°")),
        "NewEmptyLayer" => cpn("mask.add", json!({"kind": "brush"}), "a new brush mask"),
        "NewFilledLayer" => {
            cpn("mask.add", json!({"kind": "radial", "center": [0.5, 0.5], "rx": 1.0, "ry": 1.0, "feather": 0.0}), "a new mask over the whole photo")
        }
        "NewCloneLayer" | "NewHealLayer" | "DrawHealTool" | "DrawCloneTool" | "SpotRemovalTool" => {
            cn("panel.remove", "the Remove tool (heal and clone)")
        }
        "RemoveLayer" => cn("mask.delete", "deletes the selected mask"),
        "SelectLayerUp" => cn("surface.maskPrevious", "selects the mask above"),
        "SelectLayerDown" => cn("surface.maskNext", "selects the mask below"),
        "SelectBackground" => Mapped::To(Binding::command_with("mask.select", json!({"id": null})), Some("deselects the mask")),
        "ToggleLayerOnOff" => cn("mask.visible", "shows or hides the selected mask"),
        "InvertMask" => c("mask.invert"),
        "FillMask" | "ClearMask" | "RasterizeMask" | "FeatherMask" => Mapped::Dropped(C1_ONLY),
        "RefineMask" => Mapped::Dropped("Refine runs on a mask's own component in LightCraft"),
        "LumaRange" => cpn("mask.add", json!({"kind": "luminanceRange"}), "a new luminance range mask"),
        "ToggleSelectionPoints" => cn("view.maskPins", "mask pins"),
        "ZoomIn" | "IncreaseZoom" | "ZoomInTool" => c("view.zoomIn"),
        "ZoomOut" | "DecreaseZoom" | "ZoomOutTool" => c("view.zoomOut"),
        "ZoomToFit" => c("view.zoomFit"),
        "Zoom100" => c("view.zoom100"),
        "Zoom200" => cp("view.navigate", json!({"zoom": {"percent": 200}})),
        "Zoom400" => cp("view.navigate", json!({"zoom": {"percent": 400}})),
        "ToggleMultiView" => cn("view.survey", "Survey view"),
        "ToggleExposureWarn" => cn("view.clipping", "clipping warnings"),
        "ToggleFocusMask" => Mapped::Dropped("LightCraft has no focus mask yet"),
        "ToggleOverlay" | "WatermarkTool" => Mapped::Dropped(C1_ONLY),
        "ToggleBeforeAfter" => c("view.beforeAfter"),
        "ShowGuides" | "ShowGrid" => cn("view.cropOverlay", "cycles the crop overlay"),
        "RecipeProofing" => cn("view.softProof", "soft proofing"),
        "FullScreen" => c("view.fullScreenPreview"),
        "SlideShow" => c("view.slideshow"),
        "ShowHideViewer" => Mapped::Dropped(C1_ONLY),
        "ShowHideBrowser" => cn("view.filmstrip", "the filmstrip"),
        "ToggleTools" => cn("panel.edit", "opens or closes the Edit panel"),
        "SelectTool" | "MoveTool" | "PanTool" => cn("tool.none", "no tool: click and drag pans"),
        "CropTool" | "StraightenTool" | "RotateFreehandTool" => cn("panel.crop", "Crop & Rotate (straighten there)"),
        "SwitchCropOrientation" => c("crop.rotateAspect"),
        "WBPickerTool" => c("tool.wbPicker"),
        "ApplyNormalization" | "PickNormTarget" | "PickSkinColor" | "AddColorReadout" | "DeleteColorReadout" => Mapped::Dropped(C1_ONLY),
        "PickHighlightLevel" | "PickShadowLevel" | "PickCurvePoint" | "PickLumaCurve" | "PickColorCorrection" => {
            Mapped::Dropped("LightCraft's targeted adjustment works on the photo with the pointer")
        }
        "DirectColorEditor" => cn("section.color", "the Color section (Point Color is there)"),
        "FocusMeterTool" | "PickFocusPoint" => Mapped::Dropped(C1_ONLY),
        "LoupeTool" => cn("view.zoomToggle", "zooms to 100 % and back"),
        "KeystoneTool" => cn("tool.guidedUpright", "Guided Upright"),
        "KeystoneVertical" => cpn("geometry.upright", json!({"mode": "vertical"}), "Upright: Vertical"),
        "KeystoneHorizontal" => cpn("geometry.upright", json!({"mode": "level"}), "Upright: Level"),
        "DrawMaskTool" => c("tool.brush"),
        "EraseMaskTool" => cn("brush.erase", "erases while the key is held, like Alt"),
        "LinearGradientTool" => c("tool.linear"),
        "RadialGradientTool" => c("tool.radial"),
        "Process" | "ExportOriginals" | "ExportVariants" => cn("dialog.export", "the Export dialog"),
        "ImportImages" => c("file.addPhotos"),
        "Print" => cn("dialog.contactSheet", "LightCraft prints contact sheets as PDF"),
        "EditAllSelected" => cn("develop.autoSync", "Auto Sync: edits change every selected photo"),
        "Rename" => c("dialog.rename"),
        "NewVariant" | "CloneVariant" => cn("photo.virtualCopy", "a virtual copy"),
        "Delete" | "MoveToTrash" => cn("photo.delete", "moves to Recently Deleted"),
        "RegeneratePreviews" => c("library.buildPreviews"),
        "SetCompareVariant" => cn("photo.setReference", "the reference photo"),
        "ClearCompareVariant" => Mapped::Dropped(C1_ONLY),
        "PickVariant" => cn("photo.pick", "flags as Pick"),
        "PromoteVariant" => cn("stack.moveUp", "one place up in the stack"),
        "DemoteVariant" => cn("stack.moveDown", "one place down in the stack"),
        "ShowInLibrary" => cn("view.back", "back to the grid"),
        "ShowInFileSystem" => c("app.showInFinder"),
        "Capture" | "CompositionMode" | "LiveView" | "AutoFocus" | "AutoFocusFine" | "ShutterSpeedUp" | "ShutterSpeedDown" | "ApertureUp"
        | "ApertureDown" | "ExposureBiasUp" | "ExposureBiasDown" | "ISOUp" | "ISODown" => Mapped::Dropped(TETHER),
        "BWEnable" => c("develop.treatment"),
        "ToggleGrading" => Mapped::Dropped("Capture One's grading switch has no single LightCraft counterpart"),
        "SelectPen" | "SelectEraser" | "PenBlack" | "PenWhite" | "PenRed" | "PenYellow" | "PenGreen" | "PenBlue" => Mapped::Dropped(ANNOTATE),
        "ToggleSelectImage" | "NextGroup" | "PreviousGroup" | "NextImageImport" | "PreviousImageImport" => {
            Mapped::Dropped("these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly")
        }
        "AutoSync" => Mapped::To(Binding::ApplyToSelected, Some("dials change every selected photo")),
        _ => Mapped::Dropped("not a TactileBridge action LightCraft knows"),
    }
}

/// Color Editor values (`CERedHue`, …) → the colour mixer.
fn map_mixer(key: &str) -> Option<Mapped> {
    let rest = key.strip_prefix("CE")?;
    let (band, field) =
        ["Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Purple", "Magenta"].iter().find_map(|b| rest.strip_prefix(b).map(|f| (*b, f)))?;
    let field = match field {
        "Hue" => "hue",
        "Sat" => "sat",
        "Light" => "lum",
        _ => return None,
    };
    let lc = if band == "Cyan" { "aqua".to_string() } else { band.to_ascii_lowercase() };
    let target = format!("mixer.{lc}.{field}");
    Some(if band == "Cyan" {
        Mapped::To(Binding::value(&target), Some("Cyan is called Aqua in the colour mixer"))
    } else {
        Mapped::To(Binding::value(&target), None)
    })
}

/// The kind of a known TactileBridge key.
pub fn kind_of(key: &str) -> Option<TbKind> {
    if ADJUSTMENTS.contains(&key) {
        Some(TbKind::Adjustment)
    } else if COMMANDS.contains(&key) {
        Some(TbKind::Command)
    } else {
        None
    }
}

/// The binding of a key, if it has one.
pub fn binding_of(key: &str) -> Option<Binding> {
    match map_key(key) {
        Mapped::To(b, _) => Some(b),
        Mapped::Dropped(_) => None,
    }
}

/// TactileBridge tool contexts → LightCraft contexts.
pub fn context_of(tool: &str) -> Option<&'static str> {
    Some(match tool {
        "default" => "default",
        "Select" => "develop",
        "Crop" => "crop",
        "Draw Mask" | "Erase Mask" => "masking.mask.brush",
        "Draw Healing Mask" => "remove",
        "Direct Color Editor" => "develop.color",
        "Import" => "dialog.import",
        "Export" | "Process" => "dialog.export",
        _ => return None,
    })
}

/// One line of an import report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ReportLine {
    /// What it was in TactileBridge.
    pub from: String,
    /// What it is now (empty when dropped).
    pub to: String,
    /// Why it was dropped or how it differs.
    pub note: String,
}

/// What an import did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub mapped: Vec<ReportLine>,
    pub dropped: Vec<ReportLine>,
}

impl Report {
    fn note(&mut self, from: String, key: &str) -> Option<Binding> {
        match map_key(key) {
            Mapped::To(b, note) => {
                self.mapped.push(ReportLine { from, to: b.describe(), note: note.unwrap_or_default().to_string() });
                Some(b)
            }
            Mapped::Dropped(why) => {
                self.dropped.push(ReportLine { from, to: String::new(), note: why.to_string() });
                None
            }
        }
    }
}

fn num(v: &Value, k: &str) -> Option<f64> {
    v.get(k).and_then(Value::as_f64).filter(|f| f.is_finite())
}
fn int(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_f64).filter(|f| f.is_finite() && *f >= 0.0).map(|f| f.min(f64::from(u32::MAX)) as u32)
}
fn boolean(v: &Value, k: &str) -> Option<bool> {
    v.get(k).and_then(Value::as_bool)
}
/// TactileBridge wrote camelCase; older hand-edited files may use PascalCase.
fn field<'a>(v: &'a Value, camel: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| {
        let mut c = camel.chars();
        let pascal: String = c.next().map(|f| f.to_ascii_uppercase().to_string() + c.as_str()).unwrap_or_default();
        v.get(pascal)
    })
}

/// Apply a TactileBridge `settings.json` to `s`: dial feel, OSD, colours, Auto Sync, automatic
/// switching, and its tool contexts as pages of a "TactileBridge" profile (made active). Returns
/// what was brought over and what was left behind.
pub fn import_settings(s: &mut SurfaceSettings, text: &str) -> Result<Report, String> {
    let root: Value = serde_json::from_str(text).map_err(|e| format!("not a TactileBridge settings file: {e}"))?;
    if !root.is_object() {
        return Err("not a TactileBridge settings file".into());
    }
    let mut r = Report::default();
    if let Some(c) = field(&root, "controls") {
        import_controls(s, c, &mut r);
    }
    if let Some(b) = field(&root, "bridge") {
        import_bridge(s, b, &mut r);
    }
    import_look(s, &root, &mut r);
    if let Some(Value::Object(ctx)) = field(&root, "toolContext").and_then(|t| field(t, "contexts")) {
        import_tool_contexts(s, ctx, &mut r);
    }
    s.sanitize();
    Ok(r)
}

/// A line of the report: what was brought over and where it went.
fn line(r: &mut Report, from: &str, to: &str, note: &str) {
    r.mapped.push(ReportLine { from: from.into(), to: to.into(), note: note.into() });
}

/// `controls`: dial feel, echo suppression and per-value tunings.
fn import_controls(s: &mut SurfaceSettings, c: &Value, r: &mut Report) {
    if let Some(x) = num(c, "globalSpeedMultiplier") {
        s.motion.speed = x;
        line(r, "Global speed", "Dial Feel ▸ Speed", "");
    }
    if let Some(x) = num(c, "globalAccelerationMultiplier") {
        s.motion.acceleration = x;
        line(r, "Global acceleration", "Dial Feel ▸ Acceleration", "");
    }
    if let Some(x) = boolean(c, "invertDirection") {
        s.motion.invert = x;
        line(r, "Invert dial direction", "Dial Feel ▸ Turn every dial the other way", "");
    }
    if let Some(x) = boolean(c, "enableEchoSuppression") {
        s.transport.echo_suppression = x;
    }
    if let Some(x) = int(c, "echoSuppressionMs") {
        s.transport.echo_suppression_ms = x;
        line(r, "Echo suppression", "Editing ▸ Keep the surface's readout steady while turning", "");
    }
    if let Some(Value::Object(profiles)) = field(c, "adjustmentProfiles") {
        for (key, t) in profiles {
            let from = format!("Tuning: {key}");
            let d = Tuning::default();
            let tuning = Tuning {
                speed: num(t, "speedMultiplier").unwrap_or(d.speed),
                acceleration: num(t, "accelerationMultiplier").unwrap_or(d.acceleration),
                acceleration_min: num(t, "accelerationMin").unwrap_or(d.acceleration_min),
                acceleration_max: num(t, "accelerationMax").unwrap_or(d.acceleration_max),
                fast_ms: int(t, "fastThresholdMs").unwrap_or(d.fast_ms),
                slow_ms: int(t, "slowThresholdMs").unwrap_or(d.slow_ms),
                invert: boolean(t, "invertDirection").unwrap_or(false),
            }
            .sanitized();
            // an untouched (default) tuning carries nothing to bring over
            if tuning == Tuning::default() {
                continue;
            }
            let Some(Binding::Value { target }) = binding_of(key) else {
                r.dropped.push(ReportLine { from, to: String::new(), note: "no LightCraft value to tune".into() });
                continue;
            };
            r.mapped.push(ReportLine { from, to: format!("Tuning: {target}"), note: String::new() });
            s.tuning.insert(target, tuning);
        }
    }
}

/// `bridge`: the flush interval (the polling settings have no LightCraft counterpart).
fn import_bridge(s: &mut SurfaceSettings, b: &Value, r: &mut Report) {
    if let Some(x) = int(b, "flushIntervalMs") {
        s.transport.flush_ms = x;
        line(r, "Flush interval", "Editing ▸ Apply movement every", "");
    }
    for k in ["idleReconcileDelayMs", "stateRetentionMs", "periodicValuePollMs", "enableCoalescingDiagnostics"] {
        if b.get(k).is_some() {
            r.dropped.push(ReportLine {
                from: format!("Bridge: {k}"),
                to: String::new(),
                note: "LightCraft pushes its values to the surface; there is nothing to poll or reconcile".into(),
            });
        }
    }
}

/// `hardware`, `osd`, `pluginColors`, `toolSwitching` and `autoSync`.
fn import_look(s: &mut SurfaceSettings, root: &Value, r: &mut Report) {
    if let Some(h) = field(root, "hardware") {
        if let Some(x) = boolean(h, "enableHapticFeedback") {
            s.look.haptics = x;
        }
        if let Some(x) = boolean(h, "enableLedIndicator") {
            s.look.show_values = x;
            line(r, "LED value indicator", "Surface ▸ Show values on the keys and the dial display", "");
        }
    }
    if let Some(o) = field(root, "osd") {
        if let Some(x) = boolean(o, "enabled") {
            s.osd.enabled = x;
        }
        if let Some(x) = num(o, "opacity") {
            s.osd.opacity = x;
        }
        if let Some(x) = o.get("backgroundColor").and_then(Value::as_str) {
            s.osd.background = hex_color(x, "#202020");
        }
        if let Some(x) = int(o, "displayTimeMs") {
            s.osd.display_ms = x;
        }
        if let Some(p) = o.get("position").and_then(Value::as_str) {
            s.osd.position = match p {
                "DownLow" => OsdPosition::BottomEdge,
                "Up" => OsdPosition::Top,
                "UpHigh" => OsdPosition::TopEdge,
                _ => OsdPosition::Bottom,
            };
        }
        line(r, "On-screen display", "On-Screen Readout", "drawn inside the LightCraft window");
    }
    if let Some(c) = field(root, "pluginColors") {
        if let Some(x) = c.get("backgroundColor").and_then(Value::as_str) {
            s.look.background = hex_color(x, "#000000");
        }
        if let Some(x) = c.get("foregroundColor").and_then(Value::as_str) {
            s.look.foreground = hex_color(x, "#FFFFFF");
        }
        line(r, "Plugin colours", "Surface ▸ Key background, Key text", "");
    }
    if let Some(x) = field(root, "toolSwitching").and_then(|t| boolean(t, "enabled")) {
        s.follow_context = x;
        line(r, "Automatic tool switching", "Pages That Follow the App", "");
    }
    if let Some(a) = field(root, "autoSync") {
        if let Some(x) = boolean(a, "enabled") {
            s.apply_to_selected = x;
        }
        if let Some(x) = int(a, "idleTimeoutMs") {
            s.transport.gesture_end_ms = x;
        }
        line(r, "Auto Sync", "Editing ▸ Dials change every selected photo", "one undo step per dial gesture");
        if a.get("copyApplyDelayMs").is_some() {
            r.dropped.push(ReportLine {
                from: "Auto Sync: copy-apply delay".into(),
                to: String::new(),
                note: "LightCraft applies to the selection directly".into(),
            });
        }
    }
}

/// `toolContext.contexts`: the tool layouts, as pages of a new "TactileBridge" profile (made active).
fn import_tool_contexts(s: &mut SurfaceSettings, ctx: &serde_json::Map<String, Value>, r: &mut Report) {
    let mut p = crate::defaults::profiles().into_iter().find(|p| p.name == "Default").unwrap_or_default();
    p.name = unique_name(s, "TactileBridge");
    p.description = "Imported from TactileBridge".into();
    // TactileBridge's tool layouts take the contexts they cover; LightCraft's own tool pages stay
    // for the rest (a selected mask, Masking without a tool, …)
    let mut taken: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (tool, m) in ctx {
        let Some(context) = context_of(tool) else {
            r.dropped.push(ReportLine { from: format!("Tool context: {tool}"), to: String::new(), note: "no matching LightCraft context".into() });
            continue;
        };
        let mut bindings = BTreeMap::new();
        let buttons = field(m, "buttons").and_then(Value::as_array).cloned().unwrap_or_default();
        for (i, k) in buttons.iter().filter_map(Value::as_str).take(9).enumerate() {
            if let Some(b) = r.note(format!("{tool}: key {} ({k})", i + 1), k) {
                bindings.insert(format!("key.{}", i + 1), b);
            }
        }
        let encoders = field(m, "encoders").and_then(Value::as_array).cloned().unwrap_or_default();
        // the Dialpad's encoder 0 is its roller, 1 its big dial
        let slots = ["roller", "dial", "knob.3", "knob.4"];
        for (slot, k) in slots.iter().zip(encoders.iter().filter_map(Value::as_str)) {
            if let Some(b) = r.note(format!("{tool}: {slot} ({k})"), k) {
                bindings.insert((*slot).to_string(), b);
            }
        }
        let name = if tool == "default" { "Tool: Default".to_string() } else { format!("Tool: {tool}") };
        // TactileBridge's default and Select layouts were its everyday ones (Select is Capture
        // One's normal cursor): ordinary pages, first, so the Keypad's own page 1 shows them; the
        // other tools' layouts are tool pages shown by their context
        let everyday = tool == "default" || tool == "Select";
        let page = Page { name: name.clone(), groups: Vec::new(), bindings, auto: !everyday };
        if everyday {
            let at = if tool == "default" { 0 } else { usize::from(p.pages.first().is_some_and(|f| f.name == "Tool: Default")) };
            p.pages.insert(at.min(p.pages.len()), page);
            if tool == "default" {
                p.contexts.insert("default".into(), vec![name]);
            }
        } else {
            p.pages.push(page);
            if taken.insert(context.to_string()) {
                p.contexts.insert(context.to_string(), vec![name]);
            } else {
                p.contexts.entry(context.to_string()).or_default().push(name);
            }
        }
    }
    p.sanitize();
    s.profile = p.name.clone();
    s.profiles.push(p);
}

fn unique_name(s: &SurfaceSettings, base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 2;
    while s.profiles.iter().any(|p| p.name == name) {
        name = format!("{base} {n}");
        n += 1;
    }
    name
}

/// The most pages one imported `.lp5` profile keeps.
pub const MAX_IMPORTED_PAGES: usize = 256;

/// Read a Logi profile package (`.lp5`) TactileBridge shipped or the user saved: its pages, with
/// TactileBridge actions mapped. Keypad profiles (`Loupedeck70`) give key pages, Dialpad profiles
/// (`Loupedeck71`) dial pages and base buttons; Actions Ring profiles (`Loupedeck72`) both.
/// At most [`MAX_IMPORTED_PAGES`] pages are kept: the per-level limits multiply to far more.
pub fn import_lp5(bytes: &[u8]) -> Result<(Profile, Report), String> {
    let info = crate::zip::read_member(bytes, "ProfileInfo.json")?;
    let text = String::from_utf8_lossy(&info);
    let root: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("ProfileInfo.json can't be read: {e}"))?;
    let device = root.get("deviceType").and_then(Value::as_str).unwrap_or("");
    let name = root.get("displayName").and_then(Value::as_str).unwrap_or("Imported").trim();
    let mut profile = Profile { name: if name.is_empty() { "Imported".into() } else { name.to_string() }, ..Default::default() };
    let mut report = Report::default();
    let workspaces = root.pointer("/layout/layoutModes").and_then(Value::as_array).cloned().unwrap_or_default();
    for mode in workspaces.iter().take(16) {
        for ws in mode.get("workspaces").and_then(Value::as_array).into_iter().flatten().take(16) {
            for (pages_key, rotate) in [("pressPages", false), ("rotatePages", true)] {
                for page in ws.get(pages_key).and_then(Value::as_array).into_iter().flatten().take(64) {
                    // the Dialpad's buttons stay put while its dial pages change
                    let is_pads = !rotate && device == "Loupedeck71";
                    if !is_pads && profile.pages.len() >= MAX_IMPORTED_PAGES {
                        // a page past the limit is left out, so its actions don't go in the report
                        continue;
                    }
                    let pname = page.get("displayName").and_then(Value::as_str).unwrap_or("Page").to_string();
                    let mut bindings = BTreeMap::new();
                    for ctl in page.get("controls").and_then(Value::as_array).into_iter().flatten().take(64) {
                        // (an id from the file: only one some device has is taken)
                        let Some(id) = ctl.get("controlId").and_then(Value::as_u64).filter(|n| *n < 64) else { continue };
                        let action = ctl.get(if rotate { "rotateAction" } else { "pressAction" }).and_then(Value::as_str).unwrap_or("");
                        // "$<Plugin>___<Action class>___<key>"
                        let Some(key) = action.rsplit("___").next().filter(|k| !k.is_empty() && *k != action) else { continue };
                        let slot = match (rotate, device, id) {
                            // Logi's encoder 0 on the Dialpad is the roller, 1 the big dial
                            (true, _, 0) => "roller".to_string(),
                            (true, _, 1) => "dial".to_string(),
                            (true, _, n) => format!("knob.{}", n + 1),
                            (false, "Loupedeck71", n) => format!("pad.{}", n + 1),
                            (false, _, n) => format!("key.{}", n + 1),
                        };
                        if crate::devices::find_control(&slot).is_none() {
                            continue;
                        }
                        if let Some(b) = report.note(format!("{pname}: {slot} ({key})"), key) {
                            bindings.insert(slot, b);
                        }
                    }
                    if is_pads {
                        profile.base.extend(bindings);
                    } else {
                        let group = if rotate { PageGroup::Dials } else { PageGroup::Keys };
                        profile.pages.push(Page { name: pname, groups: vec![group], bindings, auto: false });
                    }
                }
            }
        }
    }
    if profile.pages.is_empty() && profile.base.is_empty() {
        return Err("the profile has no pages".into());
    }
    profile.sanitize();
    Ok((profile, report))
}

/// The table of every TactileBridge action and where it went (docs, the import dialog).
pub fn mapping_table() -> Vec<(TbKind, &'static str, Mapped)> {
    ADJUSTMENTS.iter().map(|k| (TbKind::Adjustment, *k, map_key(k))).chain(COMMANDS.iter().map(|k| (TbKind::Command, *k, map_key(k)))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zip::tests::make_zip;

    #[test]
    fn every_tactilebridge_action_is_mapped_or_explained() {
        for (kind, key, m) in mapping_table() {
            match m {
                Mapped::To(Binding::Value { target }, _) => {
                    assert_eq!(kind, TbKind::Adjustment, "{key}");
                    assert!(crate::targets::target_spec(&target).is_some(), "{key} → unknown target {target}");
                }
                Mapped::To(Binding::Command { command, params }, _) => {
                    assert!(command.contains('.'), "{key} → {command}");
                    assert!(params.is_null() || params.is_object(), "{key}");
                }
                Mapped::To(_, _) => {}
                Mapped::Dropped(why) => assert!(!why.is_empty(), "{key}"),
            }
        }
        assert_eq!(map_key("Rate3Stars"), Mapped::To(Binding::command_with("photo.rate", json!({"rating": 3})), None));
        assert!(matches!(map_key("ColorTagOrange"), Mapped::Dropped(_)));
        assert_eq!(binding_of("CECyanSat"), Some(Binding::value("mixer.aqua.sat")));
        assert_eq!(binding_of("Select4Stars"), Some(Binding::command_with("library.selectBy", json!({"rating": 4, "ratingOp": "eq"}))));
        assert_eq!(binding_of("SelectTool"), Some(Binding::command("tool.none")), "Select… families don't swallow tools");
        assert_eq!(binding_of("SelectAll"), Some(Binding::command("library.selectAll")));
        assert!(matches!(map_key("Bogus"), Mapped::Dropped(_)));
        let mapped = mapping_table().iter().filter(|(_, _, m)| matches!(m, Mapped::To(..))).count();
        assert!(mapped > 120, "{mapped}");
    }

    #[test]
    fn settings_import_brings_tuning_osd_colours_and_contexts() {
        let tb = r##"{
          "bridge": {"flushIntervalMs": 20, "idleReconcileDelayMs": 120},
          "controls": {"globalSpeedMultiplier": 1.5, "invertDirection": true, "echoSuppressionMs": 150,
            "adjustmentProfiles": {"Kelvin": {"speedMultiplier": 0.3}, "Exposure": {"speedMultiplier": 0.49, "fastThresholdMs": 1}, "PenSize": {"speedMultiplier": 2}}},
          "hardware": {"enableLedIndicator": false},
          "osd": {"enabled": true, "opacity": 0.6, "position": "Up", "displayTimeMs": 1800, "backgroundColor": "#202020"},
          "pluginColors": {"backgroundColor": "#101010", "foregroundColor": "ff7800"},
          "toolSwitching": {"enabled": false},
          "autoSync": {"enabled": true, "idleTimeoutMs": 1500, "copyApplyDelayMs": 100},
          "toolContext": {"contexts": {
            "Crop": {"buttons": ["SwitchCropOrientation", "ZoomIn", "ToggleOverlay"], "encoders": ["Zoom"]},
            "Draw Annotations": {"buttons": ["SelectPen"], "encoders": ["PenSize"]},
            "default": {"buttons": ["Rate5Stars", "Undo"], "encoders": ["Exposure", "Kelvin"]}
          }}
        }"##;
        let mut s = SurfaceSettings::default();
        let r = import_settings(&mut s, tb).unwrap();
        assert_eq!((s.motion.speed, s.motion.invert, s.transport.echo_suppression_ms, s.transport.flush_ms), (1.5, true, 150, 20));
        assert_eq!(s.tuning["wb.temp"].speed, 0.3);
        assert_eq!((s.tuning["light.exposure"].speed, s.tuning["light.exposure"].fast_ms), (0.49, 5), "clamped");
        assert!(!s.look.show_values);
        assert_eq!((s.osd.opacity, s.osd.position), (0.6, OsdPosition::Top));
        assert_eq!((s.look.background.as_str(), s.look.foreground.as_str()), ("#101010", "#FF7800"));
        assert!(!s.follow_context && s.apply_to_selected);
        assert_eq!(s.transport.gesture_end_ms, 1500);
        let p = s.active_profile().unwrap();
        assert_eq!(p.name, "TactileBridge");
        let crop = p.page("Tool: Crop").unwrap();
        assert_eq!(crop.bindings["key.1"], Binding::command("crop.rotateAspect"));
        assert_eq!(crop.bindings["roller"], Binding::value("ui.zoom"));
        assert!(!crop.bindings.contains_key("key.3"), "ToggleOverlay has no counterpart");
        assert_eq!(p.contexts["crop"], ["Tool: Crop"]);
        let def = p.page("Tool: Default").unwrap();
        assert_eq!(def.bindings["dial"], Binding::value("wb.temp"));
        assert!(!def.auto && p.pages[0].name == "Tool: Default", "TactileBridge's default layout is the first ordinary page");
        assert_eq!(p.contexts["default"], ["Tool: Default"]);
        assert!(p.page("Tool: Crop").unwrap().auto);
        assert!(r.dropped.iter().any(|l| l.from.contains("Draw Annotations")));
        assert!(r.dropped.iter().any(|l| l.from == "Tuning: PenSize"));
        assert!(r.dropped.iter().any(|l| l.from.contains("idleReconcileDelayMs")));
        // importing twice makes a second profile, it doesn't overwrite the first
        import_settings(&mut s, tb).unwrap();
        assert!(s.profiles.iter().any(|p| p.name == "TactileBridge 2"));
    }

    #[test]
    fn junk_settings_are_errors() {
        let mut s = SurfaceSettings::default();
        assert!(import_settings(&mut s, "[").is_err());
        assert!(import_settings(&mut s, "[1]").is_err());
        let r = import_settings(
            &mut s,
            r#"{"controls": {"adjustmentProfiles": {"Exposure": {"speedMultiplier": "fast"}}, "globalSpeedMultiplier": null}}"#,
        )
        .unwrap();
        assert!(r.mapped.is_empty(), "nothing usable, nothing changed");
        assert_eq!(s, SurfaceSettings::default());
    }

    fn lp5(device: &str, press: Value, rotate: Value) -> Vec<u8> {
        let info = json!({"deviceType": device, "displayName": "Wedding", "layout": {"layoutModes": [{"workspaces": [{"pressPages": press, "rotatePages": rotate}]}]}});
        let text = format!("\u{feff}{info}");
        make_zip(&[("ProfileInfo.json", text.as_bytes(), true)])
    }

    #[test]
    fn lp5_profiles_become_pages() {
        let keypad = lp5(
            "Loupedeck70",
            json!([{"displayName": "Cull", "controls": [
                {"controlId": 0, "pressAction": "$CaptureOne___CaptureOneCmd___PreviousImage"},
                {"controlId": 5, "pressAction": "$CaptureOne___CaptureOneCmd___Rate5Stars"},
                {"controlId": 8, "pressAction": "$CaptureOne___CaptureOneCmd___ColorTagOrange"}]}]),
            json!([]),
        );
        let (p, r) = import_lp5(&keypad).unwrap();
        assert_eq!(p.name, "Wedding");
        let cull = p.page("Cull").unwrap();
        assert_eq!(cull.groups, [PageGroup::Keys]);
        assert_eq!(cull.bindings["key.1"], Binding::command("library.previous"));
        assert_eq!(cull.bindings["key.6"], Binding::command_with("photo.rate", json!({"rating": 5})));
        assert_eq!(r.dropped.len(), 1);
        let dialpad = lp5(
            "Loupedeck71",
            json!([{"displayName": "Buttons", "controls": [{"controlId": 0, "pressAction": "$CaptureOne___CaptureOneCmd___CopyAdjustments"}]}]),
            json!([{"displayName": "HDR", "controls": [
                {"controlId": 0, "rotateAction": "$CaptureOne___CaptureOneAdj___Highlight"},
                {"controlId": 1, "rotateAction": "$CaptureOne___CaptureOneAdj___Shadow"}]}]),
        );
        let (p, _) = import_lp5(&dialpad).unwrap();
        assert_eq!(p.base["pad.1"], Binding::command("develop.copy"));
        let hdr = p.page("HDR").unwrap();
        assert_eq!(
            (hdr.groups.as_slice(), &hdr.bindings["dial"], &hdr.bindings["roller"]),
            // encoder 0 (Highlight) is the roller, encoder 1 (Shadow) the big dial
            ([PageGroup::Dials].as_slice(), &Binding::value("light.shadows"), &Binding::value("light.highlights"))
        );
        // ids no device has (or that overflow) are left out, not wrapped round or panicked on
        let hostile = lp5(
            "Loupedeck70",
            json!([{"displayName": "Junk", "controls": [
                {"controlId": u64::MAX, "pressAction": "$CaptureOne___CaptureOneCmd___PreviousImage"},
                {"controlId": 99, "pressAction": "$CaptureOne___CaptureOneCmd___PreviousImage"}]}]),
            json!([{"displayName": "Junk dials", "controls": [{"controlId": u64::MAX, "rotateAction": "$CaptureOne___CaptureOneAdj___Highlight"}]}]),
        );
        let (p, _) = import_lp5(&hostile).unwrap();
        assert!(p.pages.iter().all(|pg| pg.bindings.is_empty()), "{:?}", p.pages);
        assert!(import_lp5(b"not a zip").is_err());
        assert!(import_lp5(&make_zip(&[("ProfileInfo.json", b"{\"layout\": 5}", false)])).is_err());
    }

    /// A small file that lists thousands of same-named pages is cut to [`MAX_IMPORTED_PAGES`] and named quickly
    /// (CodeRabbit on #2947).
    #[test]
    fn a_profile_with_too_many_pages_keeps_the_first_ones() {
        let pages = json!(vec![json!({"controls": []}); 64]);
        let ws = json!({"pressPages": pages, "rotatePages": pages});
        let info = json!({"deviceType": "Loupedeck70", "layout": {"layoutModes": vec![json!({"workspaces": vec![ws; 16]}); 16]}});
        let text = info.to_string();
        let (p, _) = import_lp5(&make_zip(&[("ProfileInfo.json", text.as_bytes(), true)])).unwrap();
        assert_eq!(p.pages.len(), MAX_IMPORTED_PAGES);
        assert_eq!((p.pages[0].name.as_str(), p.pages[1].name.as_str()), ("Page", "Page 2"));
        assert_eq!(p.pages[MAX_IMPORTED_PAGES - 1].name, format!("Page {MAX_IMPORTED_PAGES}"));
    }

    /// The report lists only actions on pages the import keeps; Dialpad buttons count past the page limit
    /// (CodeRabbit on #2947).
    #[test]
    fn the_report_leaves_out_pages_past_the_limit() {
        let page = |name: String, key: &str| json!({"displayName": name, "controls": [{"controlId": 0, "pressAction": format!("$X___Cmd___{key}"), "rotateAction": "$X___Adj___Highlight"}]});
        // one list holds at most 64 pages, so the limit is reached over four workspaces
        let full = |kind: &str| -> Vec<Value> {
            (0..MAX_IMPORTED_PAGES / 64)
                .map(|w| json!({kind: (0..64).map(|i| page(format!("Keep {w}.{i}"), "Rate5Stars")).collect::<Vec<_>>()}))
                .collect()
        };
        let import = |device: &str, mut workspaces: Vec<Value>, extra: Value| {
            workspaces.push(extra);
            let info = json!({"deviceType": device, "layout": {"layoutModes": [{"workspaces": workspaces}]}});
            import_lp5(&make_zip(&[("ProfileInfo.json", info.to_string().as_bytes(), true)])).unwrap()
        };
        let (p, r) = import("Loupedeck70", full("pressPages"), json!({"pressPages": [page("Past".into(), "Rate3Stars")]}));
        assert_eq!(p.pages.len(), MAX_IMPORTED_PAGES);
        assert!(!r.mapped.iter().any(|l| l.from.starts_with("Past:")), "a dropped page's action is reported");
        assert_eq!(r.mapped.len(), MAX_IMPORTED_PAGES);
        // a Dialpad full of dial pages still takes its button page, listed after them
        let extra = json!({"pressPages": [page("Buttons".into(), "CopyAdjustments")], "rotatePages": [page("Too many".into(), "Rate5Stars")]});
        let (p, r) = import("Loupedeck71", full("rotatePages"), extra);
        assert_eq!(p.pages.len(), MAX_IMPORTED_PAGES);
        assert_eq!(p.base["pad.1"], Binding::command("develop.copy"));
        assert!(r.mapped.iter().any(|l| l.from.starts_with("Buttons:")));
        assert!(!r.mapped.iter().any(|l| l.from.starts_with("Too many:")));
    }
}
