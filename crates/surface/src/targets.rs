//! The values a dial can move.
//!
//! - develop sliders by their control id (`light.exposure`, `mixer.red.hue`, `pointColor.0.hueShift`
//!   …: [`lightcraft_develop::controls`]);
//! - the selected mask's sliders, `mask.<field>` (`mask.exposure`, `mask.amount`, …), as the
//!   `mask.adjust` command names them;
//! - brush, Remove and view values the UI keeps (`ui.brushSize`, `ui.removeSize`, `ui.zoom`, …),
//!   with the ids and display scales the panels already use.

use lightcraft_develop::controls;
use serde::Serialize;

/// Who owns the value, and so how the host changes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TargetKind {
    /// A develop slider of the active photo (`develop.adjust`; undoable, Auto Sync applies).
    Develop,
    /// A slider of the selected mask (`mask.adjust`; undoable).
    Mask,
    /// A tool or view value of the interface (not part of the photo's edit; no undo step).
    Ui,
}

/// Range, default, step and display precision of a target, like a develop control spec.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct TargetSpec {
    pub id: String,
    /// English label (the UI translates it).
    pub label: &'static str,
    /// The panel it belongs to (English), for the editor's list.
    pub group: &'static str,
    pub kind: TargetKind,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    /// Value units per dial tick at speed 1 without acceleration.
    pub step: f64,
    pub decimals: u8,
}

impl TargetSpec {
    pub fn clamp(&self, v: f64) -> f64 {
        if v.is_finite() { v.clamp(self.min, self.max) } else { self.default }
    }

    /// The value as the panels print it (`+0.35`, `5600`, `-12`).
    pub fn format(&self, v: f64) -> String {
        let v = if v.is_finite() { v } else { self.default };
        let s = format!("{:.*}", self.decimals as usize, v);
        // "-0" / "-0.00" read as zero
        let s = if s.starts_with('-') && s.trim_start_matches('-').chars().all(|c| c == '0' || c == '.') {
            s.trim_start_matches('-').to_string()
        } else {
            s
        };
        if v > 0.0 && self.min < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0') { format!("+{s}") } else { s }
    }

    /// Where `v` sits between the ends, 0…1 (for the OSD bar and device rings).
    pub fn position(&self, v: f64) -> f64 {
        if let Some(spec) = controls::find(&self.id) {
            return spec.position(v);
        }
        let span = self.max - self.min;
        if !(span.is_finite() && span > 0.0) {
            return 0.0;
        }
        let p = (self.clamp(v) - self.min) / span;
        if p.is_finite() { p.clamp(0.0, 1.0) } else { 0.0 }
    }

    /// The largest change one flush may make: the whole range (a runaway delta never overshoots
    /// by more than a slider's travel).
    pub fn max_delta(&self) -> f64 {
        let span = self.max - self.min;
        if span.is_finite() && span > 0.0 { span } else { 0.0 }
    }
}

struct Static {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    min: f64,
    max: f64,
    default: f64,
    step: f64,
    decimals: u8,
}

const fn st(id: &'static str, label: &'static str, group: &'static str, min: f64, max: f64, default: f64, step: f64, decimals: u8) -> Static {
    Static { id, label, group, min, max, default, step, decimals }
}

/// The selected mask's sliders (ranges as `mask.adjust` clamps them).
const MASK: &[Static] = &[
    st("mask.temp", "Temp", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.tint", "Tint", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.exposure", "Exposure", "Mask", -4.0, 4.0, 0.0, 0.01, 2),
    st("mask.contrast", "Contrast", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.highlights", "Highlights", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.shadows", "Shadows", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.whites", "Whites", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.blacks", "Blacks", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.texture", "Texture", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.clarity", "Clarity", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.dehaze", "Dehaze", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.hue", "Hue", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.saturation", "Saturation", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.sharpness", "Sharpness", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.noise", "Noise", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.moire", "Moire", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.defringe", "Defringe", "Mask", -100.0, 100.0, 0.0, 1.0, 0),
    st("mask.amount", "Amount", "Mask", 0.0, 200.0, 100.0, 1.0, 0),
];

/// Tool and view values (ids and display scales as the Masking and Remove panels show them).
const UI: &[Static] = &[
    st("ui.brushSize", "Brush Size", "Brush", 1.0, 100.0, 16.0, 1.0, 0),
    st("ui.brushFeather", "Brush Feather", "Brush", 0.0, 100.0, 50.0, 1.0, 0),
    st("ui.brushFlow", "Brush Flow", "Brush", 1.0, 100.0, 60.0, 1.0, 0),
    st("ui.removeSize", "Remove Size", "Remove", 1.0, 250.0, 20.0, 1.0, 0),
    st("ui.removeFeather", "Remove Feather", "Remove", 0.0, 100.0, 50.0, 1.0, 0),
    st("ui.removeOpacity", "Remove Opacity", "Remove", 0.0, 100.0, 100.0, 1.0, 0),
    st("ui.zoom", "Zoom", "View", 5.0, 800.0, 100.0, 1.0, 0),
];

fn from_static(s: &Static, kind: TargetKind) -> TargetSpec {
    TargetSpec {
        id: s.id.to_string(),
        label: s.label,
        group: s.group,
        kind,
        min: s.min,
        max: s.max,
        default: s.default,
        step: s.step,
        decimals: s.decimals,
    }
}

/// The spec of a target id, or `None` for an id nothing knows.
pub fn target_spec(id: &str) -> Option<TargetSpec> {
    if let Some(s) = MASK.iter().find(|s| s.id == id) {
        return Some(from_static(s, TargetKind::Mask));
    }
    if let Some(s) = UI.iter().find(|s| s.id == id) {
        return Some(from_static(s, TargetKind::Ui));
    }
    let c = controls::find(id)?;
    Some(TargetSpec {
        id: id.to_string(),
        label: c.label,
        group: c.section.label(),
        kind: TargetKind::Develop,
        min: c.min,
        max: c.max,
        default: c.default,
        step: c.step,
        decimals: c.decimals,
    })
}

/// Every target the editor offers: the develop sliders in panel order, then the mask, brush,
/// Remove and view values.
pub fn all_targets() -> Vec<TargetSpec> {
    let mut v: Vec<TargetSpec> = controls::CONTROLS.iter().filter_map(|c| target_spec(c.id)).collect();
    v.extend(MASK.iter().map(|s| from_static(s, TargetKind::Mask)));
    v.extend(UI.iter().map(|s| from_static(s, TargetKind::Ui)));
    v
}

/// The mask field `mask.adjust` takes for a `mask.<field>` target.
pub fn mask_field(id: &str) -> Option<&str> {
    let f = id.strip_prefix("mask.")?;
    MASK.iter().any(|s| s.id == id).then_some(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn develop_mask_and_ui_targets_resolve() {
        let e = target_spec("light.exposure").unwrap();
        assert_eq!((e.kind, e.min, e.max, e.step), (TargetKind::Develop, -5.0, 5.0, 0.01));
        assert_eq!(target_spec("mask.exposure").unwrap().kind, TargetKind::Mask);
        assert_eq!(target_spec("ui.brushSize").unwrap().kind, TargetKind::Ui);
        assert_eq!(target_spec("pointColor.2.hueShift").unwrap().kind, TargetKind::Develop);
        assert!(target_spec("nope").is_none());
        assert_eq!(mask_field("mask.amount"), Some("amount"));
        assert_eq!(mask_field("mask.nope"), None);
        let all = all_targets();
        let mut ids: Vec<_> = all.iter().map(|t| t.id.as_str()).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n, "target ids are unique");
    }

    #[test]
    fn formatting_and_position_are_total() {
        let e = target_spec("light.exposure").unwrap();
        assert_eq!(e.format(0.5), "+0.50");
        assert_eq!(e.format(-0.001), "0.00");
        assert_eq!(e.format(f64::NAN), "0.00");
        assert_eq!(e.position(0.0), 0.5);
        let b = target_spec("ui.brushFlow").unwrap();
        assert_eq!(b.format(60.0), "60");
        assert_eq!(b.position(f64::INFINITY), (b.default - b.min) / (b.max - b.min));
        assert_eq!(b.max_delta(), 99.0);
    }
}
