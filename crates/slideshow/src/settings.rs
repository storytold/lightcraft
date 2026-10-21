//! [`SlideSettings`]: Layout (margins, guides, aspect preview), Options (zoom to fill, stroke
//! border, cast shadow) and Backdrop (colour, colour wash, background image).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Largest margin (fraction of the slide's shorter side).
pub const MARGIN_MAX: f64 = 0.45;
/// Widest stroke border (fraction of the slide's shorter side).
pub const STROKE_MAX: f64 = 0.05;
/// Longest shadow offset (fraction of the slide's shorter side).
pub const SHADOW_OFFSET_MAX: f64 = 0.1;
/// Largest shadow radius (fraction of the slide's shorter side).
pub const SHADOW_RADIUS_MAX: f64 = 0.1;
/// Largest slide side [`crate::compose`] draws (bigger requests are scaled down to it).
pub const MAX_SIDE: usize = 8192;

/// The aspect the slide is previewed at (Lightroom Classic's Aspect Preview). Playback always
/// fills the screen; the settings preview and `slideshow.preview` use this.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Aspect {
    /// The screen's own aspect.
    #[default]
    #[serde(rename = "screen")]
    Screen,
    #[serde(rename = "16:9")]
    Wide,
    #[serde(rename = "4:3")]
    Standard,
}

impl Aspect {
    pub const ALL: [Aspect; 3] = [Aspect::Screen, Aspect::Wide, Aspect::Standard];

    pub fn id(self) -> &'static str {
        match self {
            Aspect::Screen => "screen",
            Aspect::Wide => "16:9",
            Aspect::Standard => "4:3",
        }
    }

    pub fn parse(s: &str) -> Option<Aspect> {
        Aspect::ALL.into_iter().find(|a| a.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Width / height, given the screen's (used for [`Aspect::Screen`]; a bad one gives 16:10).
    pub fn ratio(self, screen: f64) -> f64 {
        match self {
            Aspect::Screen if screen.is_finite() && screen > 0.05 && screen < 20.0 => screen,
            Aspect::Screen => 1.6,
            Aspect::Wide => 16.0 / 9.0,
            Aspect::Standard => 4.0 / 3.0,
        }
    }
}

/// How a slide looks (everything but the photo). Lengths are fractions of the slide's shorter
/// side, angles degrees counter-clockwise from the right, opacities 0–1.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SlideSettings {
    // Layout
    pub margin_left: f64,
    pub margin_right: f64,
    pub margin_top: f64,
    pub margin_bottom: f64,
    /// Changing one margin changes all four.
    pub link_margins: bool,
    /// Show the margin guides in the slide editor (never in playback or export).
    pub show_guides: bool,
    pub aspect: Aspect,
    // Options
    /// Crop the photo to fill its cell (off: the whole photo fits inside it).
    pub zoom_to_fill: bool,
    pub stroke_border: bool,
    pub stroke_color: [u8; 3],
    pub stroke_width: f64,
    pub cast_shadow: bool,
    pub shadow_opacity: f64,
    pub shadow_offset: f64,
    pub shadow_radius: f64,
    pub shadow_angle: f64,
    // Backdrop
    pub backdrop_color: [u8; 3],
    /// A gradient from `wash_color` (full at the side `wash_angle` points to) to the backdrop.
    pub color_wash: bool,
    pub wash_color: [u8; 3],
    pub wash_opacity: f64,
    pub wash_angle: f64,
    /// A library photo (its id) drawn over the backdrop colour, filling the slide.
    pub background_image: Option<u64>,
    pub background_opacity: f64,
}

impl Default for SlideSettings {
    fn default() -> Self {
        SlideSettings {
            margin_left: 0.0,
            margin_right: 0.0,
            margin_top: 0.0,
            margin_bottom: 0.0,
            link_margins: true,
            show_guides: true,
            aspect: Aspect::Screen,
            zoom_to_fill: false,
            stroke_border: false,
            stroke_color: [255, 255, 255],
            stroke_width: 0.004,
            cast_shadow: false,
            shadow_opacity: 0.4,
            shadow_offset: 0.012,
            shadow_radius: 0.02,
            shadow_angle: -45.0,
            backdrop_color: [0, 0, 0],
            color_wash: false,
            wash_color: [96, 96, 96],
            wash_opacity: 0.6,
            wash_angle: 90.0,
            background_image: None,
            background_opacity: 0.5,
        }
    }
}

/// The keys [`SlideSettings::merged`] takes (for error messages and docs).
const KEYS: &str = "margins, marginLeft, marginRight, marginTop, marginBottom, linkMargins, showGuides, aspect, zoomToFill, strokeBorder, strokeColor, strokeWidth, castShadow, shadowOpacity, shadowOffset, shadowRadius, shadowAngle, backdropColor, colorWash, washColor, washOpacity, washAngle, backgroundImage, backgroundOpacity";

fn clamp_or(v: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}

/// An angle in degrees into (−180, 180]; non-finite → `fallback`.
fn wrap_angle(a: f64, fallback: f64) -> f64 {
    if !a.is_finite() {
        return fallback;
    }
    let w = (a + 180.0).rem_euclid(360.0) - 180.0;
    if w <= -180.0 { 180.0 } else { w }
}

impl SlideSettings {
    /// The settings with every value in range: lengths and opacities clamped, angles wrapped,
    /// non-finite numbers replaced by the defaults.
    pub fn sanitized(mut self) -> Self {
        let d = SlideSettings::default();
        for m in [&mut self.margin_left, &mut self.margin_right, &mut self.margin_top, &mut self.margin_bottom] {
            *m = clamp_or(*m, 0.0, MARGIN_MAX, 0.0);
        }
        self.stroke_width = clamp_or(self.stroke_width, 0.0, STROKE_MAX, d.stroke_width);
        self.shadow_opacity = clamp_or(self.shadow_opacity, 0.0, 1.0, d.shadow_opacity);
        self.shadow_offset = clamp_or(self.shadow_offset, 0.0, SHADOW_OFFSET_MAX, d.shadow_offset);
        self.shadow_radius = clamp_or(self.shadow_radius, 0.0, SHADOW_RADIUS_MAX, d.shadow_radius);
        self.shadow_angle = wrap_angle(self.shadow_angle, d.shadow_angle);
        self.wash_opacity = clamp_or(self.wash_opacity, 0.0, 1.0, d.wash_opacity);
        self.wash_angle = wrap_angle(self.wash_angle, d.wash_angle);
        self.background_opacity = clamp_or(self.background_opacity, 0.0, 1.0, d.background_opacity);
        self
    }

    /// Does a slide with these settings show nothing but the photo fitted on black (the plain
    /// slideshow)? Settings that are switched on but draw nothing (a 0-wide stroke, a shadow at
    /// 0 % opacity) count as plain.
    pub fn is_plain(&self) -> bool {
        let s = self.clone().sanitized();
        s.margin_left == 0.0
            && s.margin_right == 0.0
            && s.margin_top == 0.0
            && s.margin_bottom == 0.0
            && !s.zoom_to_fill
            && !(s.stroke_border && s.stroke_width > 0.0)
            && !(s.cast_shadow && s.shadow_opacity > 0.0)
            && s.backdrop_color == [0, 0, 0]
            && !(s.color_wash && s.wash_opacity > 0.0)
            && !(s.background_image.is_some() && s.background_opacity > 0.0)
    }

    /// A hash of what changes the slide's pixels (guides and the aspect preview don't), for
    /// render keys. Stable within a run.
    pub fn pixel_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut s = self.clone().sanitized();
        s.show_guides = false;
        s.aspect = Aspect::Screen;
        s.link_margins = false;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.to_json().to_string().hash(&mut h);
        h.finish()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "marginLeft": self.margin_left,
            "marginRight": self.margin_right,
            "marginTop": self.margin_top,
            "marginBottom": self.margin_bottom,
            "linkMargins": self.link_margins,
            "showGuides": self.show_guides,
            "aspect": self.aspect.id(),
            "zoomToFill": self.zoom_to_fill,
            "strokeBorder": self.stroke_border,
            "strokeColor": hex(self.stroke_color),
            "strokeWidth": self.stroke_width,
            "castShadow": self.cast_shadow,
            "shadowOpacity": self.shadow_opacity,
            "shadowOffset": self.shadow_offset,
            "shadowRadius": self.shadow_radius,
            "shadowAngle": self.shadow_angle,
            "backdropColor": hex(self.backdrop_color),
            "colorWash": self.color_wash,
            "washColor": hex(self.wash_color),
            "washOpacity": self.wash_opacity,
            "washAngle": self.wash_angle,
            "backgroundImage": self.background_image,
            "backgroundOpacity": self.background_opacity,
        })
    }

    /// These settings with the keys `p` names changed (the others kept), validated and clamped.
    /// A wrong type, an unknown key or a non-finite number is an error, and nothing changes.
    /// With `linkMargins` on (already, or set in `p`), a margin given sets all four; `margins`
    /// sets all four in any case.
    pub fn merged(&self, p: &Value) -> Result<Self, String> {
        let Some(obj) = p.as_object() else { return Err("parameters must be an object".into()) };
        let mut s = self.clone();
        let mut margin: Option<f64> = None;
        for (k, v) in obj {
            match k.as_str() {
                "margins" => {
                    let m = number(k, v)?;
                    (s.margin_left, s.margin_right, s.margin_top, s.margin_bottom) = (m, m, m, m);
                }
                "marginLeft" => s.margin_left = *margin.insert(number(k, v)?),
                "marginRight" => s.margin_right = *margin.insert(number(k, v)?),
                "marginTop" => s.margin_top = *margin.insert(number(k, v)?),
                "marginBottom" => s.margin_bottom = *margin.insert(number(k, v)?),
                "linkMargins" => s.link_margins = boolean(k, v)?,
                "showGuides" => s.show_guides = boolean(k, v)?,
                "aspect" => {
                    s.aspect =
                        v.as_str().and_then(Aspect::parse).ok_or_else(|| format!("`aspect` must be \"screen\", \"16:9\" or \"4:3\", not {v}"))?
                }
                "zoomToFill" => s.zoom_to_fill = boolean(k, v)?,
                "strokeBorder" => s.stroke_border = boolean(k, v)?,
                "strokeColor" => s.stroke_color = color(k, v)?,
                "strokeWidth" => s.stroke_width = number(k, v)?,
                "castShadow" => s.cast_shadow = boolean(k, v)?,
                "shadowOpacity" => s.shadow_opacity = number(k, v)?,
                "shadowOffset" => s.shadow_offset = number(k, v)?,
                "shadowRadius" => s.shadow_radius = number(k, v)?,
                "shadowAngle" => s.shadow_angle = number(k, v)?,
                "backdropColor" => s.backdrop_color = color(k, v)?,
                "colorWash" => s.color_wash = boolean(k, v)?,
                "washColor" => s.wash_color = color(k, v)?,
                "washOpacity" => s.wash_opacity = number(k, v)?,
                "washAngle" => s.wash_angle = number(k, v)?,
                "backgroundImage" => {
                    s.background_image = match v {
                        Value::Null => None,
                        _ => Some(v.as_u64().ok_or_else(|| format!("`backgroundImage` must be a photo id or null, not {v}"))?),
                    }
                }
                "backgroundOpacity" => s.background_opacity = number(k, v)?,
                other => return Err(format!("unknown setting `{other}` ({KEYS})")),
            }
        }
        if s.link_margins
            && let Some(m) = margin
        {
            (s.margin_left, s.margin_right, s.margin_top, s.margin_bottom) = (m, m, m, m);
        }
        Ok(s.sanitized())
    }
}

fn number(k: &str, v: &Value) -> Result<f64, String> {
    match v.as_f64() {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(format!("`{k}` must be a finite number, not {v}")),
    }
}

fn boolean(k: &str, v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("`{k}` must be true or false, not {v}"))
}

fn color(k: &str, v: &Value) -> Result<[u8; 3], String> {
    parse_color(v).ok_or_else(|| format!("`{k}` must be \"#rrggbb\" or [r, g, b] (0–255), not {v}"))
}

/// `"#rrggbb"` (the `#` optional) or `[r, g, b]` with 0–255 components.
pub fn parse_color(v: &Value) -> Option<[u8; 3]> {
    match v {
        Value::String(s) => {
            let h = s.trim().strip_prefix('#').unwrap_or(s.trim());
            if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
            Some([byte(0)?, byte(2)?, byte(4)?])
        }
        Value::Array(a) if a.len() == 3 => {
            let mut c = [0u8; 3];
            for (o, x) in c.iter_mut().zip(a) {
                *o = u8::try_from(x.as_u64()?).ok()?;
            }
            Some(c)
        }
        _ => None,
    }
}

/// `#rrggbb`.
pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_validates_links_margins_and_round_trips() {
        let d = SlideSettings::default();
        assert!(d.is_plain(), "the default slide is the plain slideshow");
        let s = d.merged(&json!({"marginLeft": 0.1})).unwrap();
        assert_eq!((s.margin_left, s.margin_right, s.margin_top, s.margin_bottom), (0.1, 0.1, 0.1, 0.1), "linked: all four");
        let s = d.merged(&json!({"linkMargins": false, "marginTop": 0.2})).unwrap();
        assert_eq!((s.margin_left, s.margin_top), (0.0, 0.2), "unlinked: one");
        let s = s.merged(&json!({"margins": 0.05})).unwrap();
        assert_eq!((s.margin_left, s.margin_bottom), (0.05, 0.05));
        let s = d
            .merged(&json!({"strokeBorder": true, "strokeColor": "#FF8000", "washColor": [1, 2, 3], "aspect": "16:9", "backgroundImage": 7}))
            .unwrap();
        assert_eq!(s.stroke_color, [255, 128, 0]);
        assert_eq!(s.wash_color, [1, 2, 3]);
        assert_eq!(s.aspect, Aspect::Wide);
        assert_eq!(s.background_image, Some(7));
        assert!(!s.is_plain());
        // the JSON form reads back as the same settings
        assert_eq!(d.merged(&s.to_json()).unwrap(), s);
        // serde (prefs.json) round trip, and missing keys take the defaults
        let back: SlideSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let partial: SlideSettings = serde_json::from_str(r#"{"marginTop": 0.3}"#).unwrap();
        assert_eq!(partial, SlideSettings { margin_top: 0.3, ..d.clone() });
        assert_eq!(d.merged(&json!({"backgroundImage": null})).unwrap().background_image, None);
    }

    #[test]
    fn hostile_settings_are_rejected_or_clamped() {
        let d = SlideSettings::default();
        for bad in [
            json!({"marginLeft": "wide"}),
            json!({"margins": null}),
            json!({"zoomToFill": 1}),
            json!({"strokeColor": "#12345"}),
            json!({"strokeColor": "#ééééé"}),
            json!({"strokeColor": "#+f+f+f"}),
            json!({"backdropColor": "+12345"}),
            json!({"washColor": "#-00000"}),
            json!({"washColor": [256, 0, 0]}),
            json!({"washColor": [-1, 0, 0]}),
            json!({"backdropColor": [1, 2]}),
            json!({"aspect": "21:9"}),
            json!({"backgroundImage": -3}),
            json!({"backgroundImage": "photo"}),
            json!({"sparkle": true}),
            json!([1]),
        ] {
            assert!(d.merged(&bad).is_err(), "{bad}");
        }
        let s = d
            .merged(&json!({"margins": 1e300, "strokeWidth": -2, "shadowOpacity": 7, "shadowOffset": 1e9, "shadowRadius": -1, "shadowAngle": 540, "washAngle": -190, "washOpacity": -0.5}))
            .unwrap();
        assert_eq!((s.margin_left, s.margin_top), (MARGIN_MAX, MARGIN_MAX));
        assert_eq!((s.stroke_width, s.shadow_opacity, s.shadow_offset, s.shadow_radius), (0.0, 1.0, SHADOW_OFFSET_MAX, 0.0));
        assert_eq!((s.shadow_angle, s.wash_angle, s.wash_opacity), (180.0, 170.0, 0.0));
        // NaN / ±∞ (a hand-edited prefs file, an in-process caller) take the defaults
        let s = SlideSettings {
            margin_left: f64::NAN,
            stroke_width: f64::INFINITY,
            shadow_angle: f64::NEG_INFINITY,
            background_opacity: f64::NAN,
            ..d.clone()
        }
        .sanitized();
        assert_eq!(
            (s.margin_left, s.stroke_width, s.shadow_angle, s.background_opacity),
            (0.0, d.stroke_width, d.shadow_angle, d.background_opacity)
        );
        assert_eq!(Aspect::Screen.ratio(f64::NAN), 1.6);
        assert_eq!(Aspect::Screen.ratio(1.5), 1.5);
        assert!((Aspect::Wide.ratio(1.0) - 16.0 / 9.0).abs() < 1e-12);
    }

    #[test]
    fn pixel_key_ignores_what_does_not_change_pixels() {
        let d = SlideSettings::default();
        let guides = SlideSettings { show_guides: false, aspect: Aspect::Standard, ..d.clone() };
        assert_eq!(d.pixel_key(), guides.pixel_key());
        assert_ne!(d.pixel_key(), SlideSettings { backdrop_color: [9, 9, 9], ..d.clone() }.pixel_key());
    }
}
