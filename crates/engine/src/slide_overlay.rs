//! Slideshow overlays (Lightroom Classic's Slideshow ▸ Overlays panel): the identity plate (text
//! or an image file), the photo's rating as stars, text overlays with tokens (file name, title,
//! caption, capture date, exposure, camera, lens, rating…) and the export watermark.
//! [`OverlaySettings`] is a library preference (`prefs.json`), edited with the
//! `slideshow.overlays` command; [`SlideOverlays`] is those settings resolved for one photo (its
//! texts expanded, its rating) and drawn onto the composed slide.
//!
//! **Where the drawing lives:** text needs the fonts (Inter plus the craft-fonts CJK fallbacks,
//! [`crate::text`]) and tokens need the catalog's photo, both in the engine, so overlays are a
//! second pass in the engine over `lightcraft_slideshow::compose_with`'s slide (its `behind` hook
//! draws an identity plate set to "render behind image" before the photo). The composer stays a
//! layout-and-pixels crate with no fonts and no catalog.
//!
//! **Resolution independence:** sizes and offsets are fractions of the slide's shorter side (also
//! for overlays anchored to the photo), so a slide at any size has the same proportions. An
//! overlay is placed at one of nine anchors of the slide or of the photo, moved by its offset
//! (x right, y down) and kept on the slide.
//!
//! **Never crash:** settings are validated by [`OverlaySettings::merged`] and clamped by
//! [`OverlaySettings::sanitized`]; text is cut at [`TEXT_MAX_CHARS`]; an unreadable identity
//! plate image draws nothing (and `slideshow.overlays` reports it); unknown tokens stay as typed
//! (as in rename and export file names) and are reported.

use std::sync::Arc;

use lightcraft_catalog::Photo;
use lightcraft_raster::{Image, Plane, Rgba8};
use lightcraft_slideshow::{Layout, Rect, hex, parse_color};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::export::{Anchor, Watermark};

/// Smallest text, plate and star size (fraction of the slide's shorter side).
pub const SIZE_MIN: f64 = 0.005;
/// Largest text line height.
pub const TEXT_SIZE_MAX: f64 = 0.3;
/// Largest identity plate height.
pub const PLATE_SIZE_MAX: f64 = 0.5;
/// Largest star.
pub const STAR_SIZE_MAX: f64 = 0.15;
/// Largest offset either way (fraction of the shorter side).
pub const OFFSET_MAX: f64 = 1.0;
/// Longest shadow offset and radius (fractions of the shorter side).
pub const SHADOW_OFFSET_MAX: f64 = 0.05;
pub const SHADOW_RADIUS_MAX: f64 = 0.05;
/// Most text overlays.
pub const MAX_TEXTS: usize = 16;
/// Longest overlay text or template (characters; longer is cut).
pub const TEXT_MAX_CHARS: usize = 1000;

/// What an overlay is anchored to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RelativeTo {
    #[default]
    Slide,
    /// The visible photo.
    Photo,
}

impl RelativeTo {
    fn id(self) -> &'static str {
        match self {
            RelativeTo::Slide => "slide",
            RelativeTo::Photo => "photo",
        }
    }
}

/// The nine anchors, in reading order (for pickers and tests).
pub const ANCHORS: [Anchor; 9] = [
    Anchor::TopLeft,
    Anchor::Top,
    Anchor::TopRight,
    Anchor::Left,
    Anchor::Center,
    Anchor::Right,
    Anchor::BottomLeft,
    Anchor::Bottom,
    Anchor::BottomRight,
];

/// An anchor's id (`topLeft`…).
pub fn anchor_id(a: Anchor) -> &'static str {
    match a {
        Anchor::TopLeft => "topLeft",
        Anchor::Top => "top",
        Anchor::TopRight => "topRight",
        Anchor::Left => "left",
        Anchor::Center => "center",
        Anchor::Right => "right",
        Anchor::BottomLeft => "bottomLeft",
        Anchor::Bottom => "bottom",
        Anchor::BottomRight => "bottomRight",
    }
}

/// Where an overlay goes: an anchor of the slide or the photo, and an offset from it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Placement {
    pub anchor: Anchor,
    /// Fractions of the slide's shorter side, x to the right, y down.
    pub offset_x: f64,
    pub offset_y: f64,
    pub relative_to: RelativeTo,
}

impl Default for Placement {
    fn default() -> Self {
        Placement { anchor: Anchor::BottomRight, offset_x: -0.03, offset_y: -0.03, relative_to: RelativeTo::Slide }
    }
}

impl Placement {
    fn sanitized(mut self) -> Self {
        self.offset_x = clamp_or(self.offset_x, -OFFSET_MAX, OFFSET_MAX, 0.0);
        self.offset_y = clamp_or(self.offset_y, -OFFSET_MAX, OFFSET_MAX, 0.0);
        self
    }

    fn json(&self, o: &mut Map<String, Value>) {
        o.insert("anchor".into(), json!(anchor_id(self.anchor)));
        o.insert("offsetX".into(), json!(self.offset_x));
        o.insert("offsetY".into(), json!(self.offset_y));
        o.insert("relativeTo".into(), json!(self.relative_to.id()));
    }

    /// Apply a placement key; `false` when `k` isn't one.
    fn merge(&mut self, ctx: &str, k: &str, v: &Value) -> Result<bool, String> {
        match k {
            "anchor" => {
                self.anchor = serde_json::from_value(v.clone()).map_err(|_| {
                    format!("`{ctx}.anchor` must be one of topLeft|top|topRight|left|center|right|bottomLeft|bottom|bottomRight, not {v}")
                })?
            }
            "offsetX" => self.offset_x = number(ctx, k, v)?,
            "offsetY" => self.offset_y = number(ctx, k, v)?,
            "relativeTo" => {
                self.relative_to = match v.as_str() {
                    Some("slide") => RelativeTo::Slide,
                    Some("photo") => RelativeTo::Photo,
                    _ => return Err(format!("`{ctx}.relativeTo` must be \"slide\" or \"photo\", not {v}")),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// The box of a `w × h` overlay on the slide `l`: at the anchor of the slide or the visible
    /// photo, moved by the offset, then kept on the slide (when it fits).
    pub fn place(&self, l: &Layout, w: f64, h: f64) -> Rect {
        let c = match self.relative_to {
            RelativeTo::Slide => l.slide,
            RelativeTo::Photo => l.visible,
        };
        let short = l.slide.width().min(l.slide.height());
        use Anchor::*;
        let x = match self.anchor {
            TopLeft | Left | BottomLeft => c.x0,
            Top | Center | Bottom => (c.x0 + c.x1 - w) / 2.0,
            TopRight | Right | BottomRight => c.x1 - w,
        } + self.offset_x * short;
        let y = match self.anchor {
            TopLeft | Top | TopRight => c.y0,
            Left | Center | Right => (c.y0 + c.y1 - h) / 2.0,
            BottomLeft | Bottom | BottomRight => c.y1 - h,
        } + self.offset_y * short;
        let s = l.slide;
        let fit = |v: f64, lo: f64, hi: f64, len: f64| if v.is_finite() { v.min(hi - len).max(lo) } else { lo };
        let (x, y) = (fit(x, s.x0, s.x1, w), fit(y, s.y0, s.y1, h));
        Rect { x0: x, y0: y, x1: x + w, y1: y + h }
    }
}

/// A soft drop shadow under text.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TextShadow {
    pub on: bool,
    pub opacity: f64,
    /// Fraction of the slide's shorter side.
    pub offset: f64,
    /// Blur radius, fraction of the slide's shorter side.
    pub radius: f64,
    /// Degrees counter-clockwise from the right (−45 = down right).
    pub angle: f64,
}

impl Default for TextShadow {
    fn default() -> Self {
        TextShadow { on: false, opacity: 0.6, offset: 0.004, radius: 0.004, angle: -45.0 }
    }
}

/// The identity plate: a line of text, or an image file (PNG / JPEG…, its transparency kept).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IdentityPlate {
    pub enabled: bool,
    pub text: String,
    /// An image file drawn instead of the text (empty = the text).
    pub image: String,
    /// Height: the text's line height or the image's height, fraction of the shorter side.
    pub size: f64,
    pub color: [u8; 3],
    pub opacity: f64,
    pub place: Placement,
    /// Draw it before the photo, so the photo covers it where they meet.
    pub behind: bool,
}

impl Default for IdentityPlate {
    fn default() -> Self {
        IdentityPlate {
            enabled: false,
            text: "LightCraft".into(),
            image: String::new(),
            size: 0.05,
            color: [255, 255, 255],
            opacity: 0.8,
            place: Placement { anchor: Anchor::TopLeft, offset_x: 0.03, offset_y: 0.03, relative_to: RelativeTo::Slide },
            behind: false,
        }
    }
}

/// The photo's star rating, drawn as stars.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RatingStars {
    pub enabled: bool,
    pub color: [u8; 3],
    /// A star's height, fraction of the shorter side.
    pub size: f64,
    pub place: Placement,
}

impl Default for RatingStars {
    fn default() -> Self {
        RatingStars {
            enabled: false,
            color: [235, 235, 235],
            size: 0.03,
            place: Placement { anchor: Anchor::TopRight, offset_x: -0.015, offset_y: 0.015, relative_to: RelativeTo::Photo },
        }
    }
}

/// A text overlay: a template with tokens (`{title}`, `{caption}`, `{exposure}`…; see [`TOKENS`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TextOverlay {
    pub text: String,
    /// Line height, fraction of the shorter side.
    pub size: f64,
    pub color: [u8; 3],
    pub opacity: f64,
    pub place: Placement,
    pub shadow: TextShadow,
}

impl Default for TextOverlay {
    fn default() -> Self {
        TextOverlay {
            text: "{file}".into(),
            size: 0.03,
            color: [255, 255, 255],
            opacity: 0.9,
            place: Placement { anchor: Anchor::Bottom, offset_x: 0.0, offset_y: -0.02, relative_to: RelativeTo::Slide },
            shadow: TextShadow::default(),
        }
    }
}

/// Every overlay of a slide.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OverlaySettings {
    pub identity_plate: IdentityPlate,
    pub rating_stars: RatingStars,
    pub texts: Vec<TextOverlay>,
    /// Draw [`Self::watermark`] on the photo.
    pub show_watermark: bool,
    /// The export watermark's settings (sizes relative to the photo, as in export).
    pub watermark: Watermark,
}

/// Tokens text overlays add to the rename tokens ([`crate::rename::TOKENS`]): tag, meaning.
pub const TOKENS: &[(&str, &str)] = &[
    ("{file}", "File name with its extension"),
    ("{caption}", "Caption metadata"),
    ("{copyright}", "Copyright metadata"),
    ("{exposure}", "Shutter speed and aperture (1/250 s at f/8)"),
    ("{shutter}", "Shutter speed"),
    ("{aperture}", "Aperture (f/8)"),
    ("{focal}", "Focal length (50 mm)"),
    ("{date}", "Capture date as YYYY-MM-DD (or {date:%d.%m.%Y} …)"),
];

fn clamp_or(v: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi) } else { fallback }
}

fn wrap_angle(a: f64, fallback: f64) -> f64 {
    if !a.is_finite() {
        return fallback;
    }
    let w = (a + 180.0).rem_euclid(360.0) - 180.0;
    if w <= -180.0 { 180.0 } else { w }
}

/// `s` cut at [`TEXT_MAX_CHARS`] characters.
fn cut(s: &mut String) {
    if let Some((i, _)) = s.char_indices().nth(TEXT_MAX_CHARS) {
        s.truncate(i);
    }
}

fn number(ctx: &str, k: &str, v: &Value) -> Result<f64, String> {
    match v.as_f64() {
        Some(x) if x.is_finite() => Ok(x),
        _ => Err(format!("`{ctx}.{k}` must be a finite number, not {v}")),
    }
}

fn boolean(ctx: &str, k: &str, v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("`{ctx}.{k}` must be true or false, not {v}"))
}

fn string(ctx: &str, k: &str, v: &Value) -> Result<String, String> {
    let mut s = v.as_str().ok_or_else(|| format!("`{ctx}.{k}` must be text, not {v}"))?.to_string();
    cut(&mut s);
    Ok(s)
}

fn colour(ctx: &str, k: &str, v: &Value) -> Result<[u8; 3], String> {
    parse_color(v).ok_or_else(|| format!("`{ctx}.{k}` must be \"#rrggbb\" or [r, g, b] (0–255), not {v}"))
}

fn object<'a>(ctx: &str, v: &'a Value) -> Result<&'a Map<String, Value>, String> {
    v.as_object().ok_or_else(|| format!("`{ctx}` must be an object, not {v}"))
}

fn unknown(ctx: &str, k: &str, keys: &str) -> String {
    format!("unknown key `{ctx}.{k}` ({keys})")
}

impl IdentityPlate {
    fn sanitized(mut self) -> Self {
        let d = IdentityPlate::default();
        cut(&mut self.text);
        cut(&mut self.image);
        self.size = clamp_or(self.size, SIZE_MIN, PLATE_SIZE_MAX, d.size);
        self.opacity = clamp_or(self.opacity, 0.0, 1.0, d.opacity);
        self.place = self.place.sanitized();
        self
    }

    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("enabled".into(), json!(self.enabled));
        o.insert("text".into(), json!(self.text));
        o.insert("image".into(), json!(self.image));
        o.insert("size".into(), json!(self.size));
        o.insert("color".into(), json!(hex(self.color)));
        o.insert("opacity".into(), json!(self.opacity));
        self.place.json(&mut o);
        o.insert("behind".into(), json!(self.behind));
        Value::Object(o)
    }

    fn merge(&mut self, v: &Value) -> Result<(), String> {
        const C: &str = "identityPlate";
        for (k, v) in object(C, v)? {
            if self.place.merge(C, k, v)? {
                continue;
            }
            match k.as_str() {
                "enabled" => self.enabled = boolean(C, k, v)?,
                "text" => self.text = string(C, k, v)?,
                "image" => self.image = string(C, k, v)?.trim().to_string(),
                "size" => self.size = number(C, k, v)?,
                "color" => self.color = colour(C, k, v)?,
                "opacity" => self.opacity = number(C, k, v)?,
                "behind" => self.behind = boolean(C, k, v)?,
                _ => return Err(unknown(C, k, "enabled, text, image, size, color, opacity, anchor, offsetX, offsetY, relativeTo, behind")),
            }
        }
        Ok(())
    }
}

impl RatingStars {
    fn sanitized(mut self) -> Self {
        self.size = clamp_or(self.size, SIZE_MIN, STAR_SIZE_MAX, RatingStars::default().size);
        self.place = self.place.sanitized();
        self
    }

    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("enabled".into(), json!(self.enabled));
        o.insert("color".into(), json!(hex(self.color)));
        o.insert("size".into(), json!(self.size));
        self.place.json(&mut o);
        Value::Object(o)
    }

    fn merge(&mut self, v: &Value) -> Result<(), String> {
        const C: &str = "ratingStars";
        for (k, v) in object(C, v)? {
            if self.place.merge(C, k, v)? {
                continue;
            }
            match k.as_str() {
                "enabled" => self.enabled = boolean(C, k, v)?,
                "color" => self.color = colour(C, k, v)?,
                "size" => self.size = number(C, k, v)?,
                _ => return Err(unknown(C, k, "enabled, color, size, anchor, offsetX, offsetY, relativeTo")),
            }
        }
        Ok(())
    }
}

impl TextOverlay {
    fn sanitized(mut self) -> Self {
        let d = TextOverlay::default();
        cut(&mut self.text);
        self.size = clamp_or(self.size, SIZE_MIN, TEXT_SIZE_MAX, d.size);
        self.opacity = clamp_or(self.opacity, 0.0, 1.0, d.opacity);
        self.place = self.place.sanitized();
        let ds = TextShadow::default();
        let sh = &mut self.shadow;
        sh.opacity = clamp_or(sh.opacity, 0.0, 1.0, ds.opacity);
        sh.offset = clamp_or(sh.offset, 0.0, SHADOW_OFFSET_MAX, ds.offset);
        sh.radius = clamp_or(sh.radius, 0.0, SHADOW_RADIUS_MAX, ds.radius);
        sh.angle = wrap_angle(sh.angle, ds.angle);
        self
    }

    fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("text".into(), json!(self.text));
        o.insert("size".into(), json!(self.size));
        o.insert("color".into(), json!(hex(self.color)));
        o.insert("opacity".into(), json!(self.opacity));
        self.place.json(&mut o);
        o.insert("shadow".into(), json!(self.shadow.on));
        o.insert("shadowOpacity".into(), json!(self.shadow.opacity));
        o.insert("shadowOffset".into(), json!(self.shadow.offset));
        o.insert("shadowRadius".into(), json!(self.shadow.radius));
        o.insert("shadowAngle".into(), json!(self.shadow.angle));
        Value::Object(o)
    }

    fn merge(&mut self, ctx: &str, v: &Value) -> Result<(), String> {
        for (k, v) in object(ctx, v)? {
            if self.place.merge(ctx, k, v)? {
                continue;
            }
            match k.as_str() {
                "text" => self.text = string(ctx, k, v)?,
                "size" => self.size = number(ctx, k, v)?,
                "color" => self.color = colour(ctx, k, v)?,
                "opacity" => self.opacity = number(ctx, k, v)?,
                "shadow" => self.shadow.on = boolean(ctx, k, v)?,
                "shadowOpacity" => self.shadow.opacity = number(ctx, k, v)?,
                "shadowOffset" => self.shadow.offset = number(ctx, k, v)?,
                "shadowRadius" => self.shadow.radius = number(ctx, k, v)?,
                "shadowAngle" => self.shadow.angle = number(ctx, k, v)?,
                _ => {
                    return Err(unknown(
                        ctx,
                        k,
                        "text, size, color, opacity, anchor, offsetX, offsetY, relativeTo, shadow, shadowOpacity, shadowOffset, shadowRadius, shadowAngle",
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The keys [`OverlaySettings::merged`] takes.
const KEYS: &str = "identityPlate, ratingStars, texts, addText, removeText, showWatermark, watermark";

impl OverlaySettings {
    /// Every value in range (non-finite numbers take the defaults), at most [`MAX_TEXTS`] texts.
    pub fn sanitized(mut self) -> Self {
        self.identity_plate = self.identity_plate.sanitized();
        self.rating_stars = self.rating_stars.sanitized();
        self.texts.truncate(MAX_TEXTS);
        self.texts = self.texts.into_iter().map(TextOverlay::sanitized).collect();
        let d = Watermark::default();
        let w = &mut self.watermark;
        let f = |v: f32, lo: f32, hi: f32, fb: f32| if v.is_finite() { v.clamp(lo, hi) } else { fb };
        w.size = f(w.size, 0.005, 0.5, d.size);
        w.opacity = f(w.opacity, 0.0, 1.0, d.opacity);
        w.inset_x = f(w.inset_x, 0.0, 0.4, d.inset_x);
        w.inset_y = f(w.inset_y, 0.0, 0.4, d.inset_y);
        if !matches!(w.rotation, 0 | 90 | 180 | 270) {
            w.rotation = 0;
        }
        w.image_width = f(w.image_width, 0.01, 1.0, d.image_width);
        cut(&mut w.text);
        w.target = None;
        self
    }

    /// Does any overlay draw (on some photo)?
    pub fn any(&self) -> bool {
        let p = &self.identity_plate;
        (p.enabled && p.opacity > 0.0 && (!p.text.trim().is_empty() || !p.image.trim().is_empty()))
            || self.rating_stars.enabled
            || self.texts.iter().any(|t| t.opacity > 0.0 && !t.text.trim().is_empty())
            || (self.show_watermark && (!self.watermark.text.trim().is_empty() || !self.watermark.image.trim().is_empty()))
    }

    pub fn to_json(&self) -> Value {
        let wm = serde_json::to_value(&self.watermark).unwrap_or(Value::Null);
        json!({
            "identityPlate": self.identity_plate.to_json(),
            "ratingStars": self.rating_stars.to_json(),
            "texts": self.texts.iter().map(TextOverlay::to_json).collect::<Vec<_>>(),
            "showWatermark": self.show_watermark,
            "watermark": wm,
        })
    }

    /// These settings with the keys `p` names changed, validated and clamped; a wrong type or an
    /// unknown key is an error and nothing changes. `texts` replaces the list (each entry merged
    /// onto the one at its index, or a new default text); `addText` appends one (an object, `{}`
    /// for the default); `removeText` removes the one at an index; `watermark` takes the export
    /// watermark's keys (or text alone) merged onto the current one.
    pub fn merged(&self, p: &Value) -> Result<Self, String> {
        let Some(obj) = p.as_object() else { return Err("parameters must be an object".into()) };
        let mut s = self.clone();
        for (k, v) in obj {
            match k.as_str() {
                "identityPlate" => s.identity_plate.merge(v)?,
                "ratingStars" => s.rating_stars.merge(v)?,
                "texts" => {
                    let list = v.as_array().ok_or_else(|| format!("`texts` must be a list of text overlays, not {v}"))?;
                    if list.len() > MAX_TEXTS {
                        return Err(format!("at most {MAX_TEXTS} text overlays, not {}", list.len()));
                    }
                    let mut next = Vec::with_capacity(list.len());
                    for (i, item) in list.iter().enumerate() {
                        let mut t = s.texts.get(i).cloned().unwrap_or_default();
                        t.merge(&format!("texts[{i}]"), item)?;
                        next.push(t);
                    }
                    s.texts = next;
                }
                "addText" => {
                    if s.texts.len() >= MAX_TEXTS {
                        return Err(format!("at most {MAX_TEXTS} text overlays"));
                    }
                    let mut t = TextOverlay::default();
                    t.merge("addText", v)?;
                    s.texts.push(t);
                }
                "removeText" => {
                    let i = v.as_u64().ok_or_else(|| format!("`removeText` must be the index of a text overlay, not {v}"))?;
                    let i = usize::try_from(i).ok().filter(|i| *i < s.texts.len()).ok_or_else(|| format!("no text overlay {i}"))?;
                    s.texts.remove(i);
                }
                "showWatermark" => s.show_watermark = boolean("", k, v).map_err(|_| format!("`showWatermark` must be true or false, not {v}"))?,
                "watermark" => {
                    crate::export::ExportOptions::validate_watermark("slideshow.overlays", v).map_err(|e| e.to_string())?;
                    s.watermark = match v {
                        Value::String(t) => Watermark { text: t.clone(), ..s.watermark.clone() },
                        _ => {
                            let mut cur = serde_json::to_value(&s.watermark).map_err(|e| e.to_string())?;
                            if let (Some(c), Some(o)) = (cur.as_object_mut(), v.as_object()) {
                                for (k, v) in o.iter().filter(|(_, v)| !v.is_null()) {
                                    if k == "inset" {
                                        // the legacy single margin sets whichever of the two the patch leaves out
                                        for axis in ["insetX", "insetY"] {
                                            if !o.contains_key(axis) {
                                                c.insert(axis.into(), v.clone());
                                            }
                                        }
                                    } else {
                                        c.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            serde_json::from_value(cur).map_err(|e| format!("`watermark`: {e}"))?
                        }
                    };
                }
                other => return Err(format!("unknown setting `{other}` ({KEYS})")),
            }
        }
        Ok(s.sanitized())
    }

    /// A hash of the settings (for render keys). Stable within a run.
    pub fn pixel_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.clone().sanitized().to_json().to_string().hash(&mut h);
        h.finish()
    }

    /// Problems worth telling the user about: an identity plate image that can't be read, tags in
    /// text overlays that aren't tokens (they show as typed).
    pub fn warnings(&self) -> Vec<String> {
        let mut out = Vec::new();
        let p = &self.identity_plate;
        if p.enabled && !p.image.trim().is_empty() && crate::export::watermark_logo(p.image.trim()).is_none() {
            out.push(format!("identity plate image {} can't be read; nothing is drawn", p.image.trim()));
        }
        for (i, t) in self.texts.iter().enumerate() {
            for tag in unknown_tokens(&t.text) {
                out.push(format!("text overlay {}: {tag} isn't a token and shows as typed", i + 1));
            }
        }
        out
    }
}

/// An exposure as "1/250 s at f/8" (either part alone when the other is unknown).
fn exposure(p: &Photo) -> String {
    let parts: Vec<String> = [shutter(p), aperture(p)].into_iter().filter(|s| !s.is_empty()).collect();
    parts.join(" at ")
}

fn shutter(p: &Photo) -> String {
    let s = p.meta.shutter.trim();
    if s.is_empty() || s.ends_with('s') || s.ends_with('"') { s.to_string() } else { format!("{s} s") }
}

fn aperture(p: &Photo) -> String {
    p.meta.aperture.filter(|a| a.is_finite() && *a > 0.0).map(|a| format!("f/{a:.1}").replace(".0", "")).unwrap_or_default()
}

/// The overlay tokens beyond the rename ones; `{seq}` has no meaning on a slide and stays as typed.
fn overlay_token(p: &Photo, name: &str, arg: Option<&str>, tag: &str) -> Option<String> {
    Some(match name {
        "file" => p.file_name.clone(),
        "caption" => p.meta.caption.clone(),
        "copyright" => p.meta.copyright.clone(),
        "exposure" => exposure(p),
        "shutter" => shutter(p),
        "aperture" => aperture(p),
        "focal" => p.meta.focal_mm.filter(|f| f.is_finite() && *f > 0.0).map(|f| format!("{f:.0} mm")).unwrap_or_default(),
        "date" if arg.is_none() => crate::rename::expand_tokens("{date:%Y-%m-%d}", p, 1, 1),
        "seq" | "n" => format!("{{{tag}}}"),
        _ => return None,
    })
}

/// `template`'s tokens replaced for `p`: the rename tokens ([`crate::rename::expand_tokens`]) and
/// [`TOKENS`]. Unknown tags stay as typed.
pub fn expand(template: &str, p: &Photo) -> String {
    crate::rename::expand_tokens_with(template, p, 1, 1, &|name, arg, tag| overlay_token(p, name, arg, tag))
}

/// The `{…}` tags in `template` that aren't tokens.
pub fn unknown_tokens(template: &str) -> Vec<String> {
    let p = crate::rename::sample_photo();
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(i) = rest.find('{') {
        let Some(after) = rest.get(i..) else { break };
        let Some(j) = after.find('}') else { break };
        let Some(tag) = after.get(..j + 1) else { break };
        if expand(tag, &p) == tag && !out.iter().any(|t| t == tag) {
            out.push(tag.to_string());
        }
        rest = after.get(j + 1..).unwrap_or("");
    }
    out
}

/// What an overlay box is, for [`SlideOverlays::boxes`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    IdentityPlate,
    RatingStars,
    /// The text overlay at this index.
    Text(usize),
    Watermark,
}

impl Kind {
    pub fn id(&self) -> String {
        match self {
            Kind::IdentityPlate => "identityPlate".into(),
            Kind::RatingStars => "ratingStars".into(),
            Kind::Text(i) => format!("text{i}"),
            Kind::Watermark => "watermark".into(),
        }
    }
}

/// The overlays of one photo's slide: the settings, its texts expanded, its rating.
#[derive(Clone, Debug, PartialEq)]
pub struct SlideOverlays {
    pub settings: OverlaySettings,
    /// One per text overlay, tokens replaced.
    pub texts: Vec<String>,
    pub rating: u8,
}

/// Gap between stars, as a share of a star's size.
const STAR_GAP: f64 = 0.2;

impl SlideOverlays {
    /// `settings` resolved for `photo` (`None`: no photo, so no tokens and no rating).
    pub fn resolve(settings: &OverlaySettings, photo: Option<&Photo>) -> SlideOverlays {
        let settings = settings.clone().sanitized();
        let texts = settings.texts.iter().map(|t| photo.map(|p| expand(&t.text, p)).unwrap_or_else(|| t.text.clone())).collect();
        SlideOverlays { settings, texts, rating: photo.map_or(0, |p| p.rating.min(5)) }
    }

    /// What makes the drawn overlays differ (for render keys).
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.settings.pixel_key().hash(&mut h);
        self.texts.hash(&mut h);
        self.rating.hash(&mut h);
        h.finish()
    }

    fn short(l: &Layout) -> f64 {
        l.slide.width().min(l.slide.height()).max(1.0)
    }

    /// The identity plate's text layout, or its image scaled to its size.
    fn plate(&self, l: &Layout) -> Option<PlateContent> {
        let p = &self.settings.identity_plate;
        if !p.enabled || p.opacity <= 0.0 {
            return None;
        }
        let short = Self::short(l);
        let px = (p.size * short).max(1.0);
        if !p.image.trim().is_empty() {
            let img = crate::export::watermark_logo(p.image.trim())?;
            if img.width == 0 || img.height == 0 {
                return None;
            }
            let h = px.round().clamp(1.0, l.slide.height().max(1.0));
            let w = (h * img.width as f64 / img.height as f64).round().clamp(1.0, l.slide.width().max(1.0));
            return Some(PlateContent::Image(img, w as usize, h as usize));
        }
        let text = p.text.trim();
        crate::text::TextLayout::new(text, px as f32, false, crate::fonts::CRAFT_FONTS).map(PlateContent::Text)
    }

    fn text_layout(&self, i: usize, l: &Layout) -> Option<crate::text::TextLayout> {
        let t = self.settings.texts.get(i)?;
        let text = self.texts.get(i)?.trim();
        if t.opacity <= 0.0 {
            return None;
        }
        crate::text::TextLayout::new(text, (t.size * Self::short(l)).max(1.0) as f32, false, crate::fonts::CRAFT_FONTS)
    }

    fn stars_size(&self, l: &Layout) -> Option<(usize, f64)> {
        let r = &self.settings.rating_stars;
        let n = usize::from(self.rating.min(5));
        (r.enabled && n > 0).then(|| (n, (r.size * Self::short(l)).max(2.0)))
    }

    /// Where each overlay goes on the slide `l` (the ones that draw something).
    pub fn boxes(&self, l: &Layout) -> Vec<(Kind, Rect)> {
        let mut out = Vec::new();
        let s = &self.settings;
        if let Some(c) = self.plate(l) {
            let (w, h) = c.size();
            out.push((Kind::IdentityPlate, s.identity_plate.place.place(l, w, h)));
        }
        if let Some((n, size)) = self.stars_size(l) {
            let w = n as f64 * size + (n as f64 - 1.0) * size * STAR_GAP;
            out.push((Kind::RatingStars, s.rating_stars.place.place(l, w, size)));
        }
        for (i, t) in s.texts.iter().enumerate() {
            if let Some(tl) = self.text_layout(i, l) {
                out.push((Kind::Text(i), t.place.place(l, tl.width as f64, tl.height as f64)));
            }
        }
        if s.show_watermark && (!s.watermark.text.trim().is_empty() || !s.watermark.image.trim().is_empty()) {
            out.push((Kind::Watermark, l.visible));
        }
        out
    }

    /// Draw the overlays drawn behind the photo (an identity plate with "render behind image").
    pub fn draw_behind(&self, img: &mut Rgba8, l: &Layout) {
        if self.settings.identity_plate.behind {
            self.draw_plate(img, l);
        }
    }

    /// Draw the overlays over the composed slide: the watermark on the photo, then the identity
    /// plate (unless it is behind), the rating stars and the text overlays.
    pub fn draw_front(&self, img: &mut Rgba8, l: &Layout) {
        let s = &self.settings;
        if s.show_watermark {
            let (x0, y0, x1, y1) = pixels(&l.visible, img.width, img.height);
            if x1 > x0 && y1 > y0 {
                crate::export::draw_watermark_in(img, &s.watermark, x0, y0, x1 - x0, y1 - y0);
            }
        }
        if !s.identity_plate.behind {
            self.draw_plate(img, l);
        }
        if let Some((n, size)) = self.stars_size(l) {
            let w = n as f64 * size + (n as f64 - 1.0) * size * STAR_GAP;
            let b = s.rating_stars.place.place(l, w, size);
            for i in 0..n {
                let x = b.x0 + i as f64 * size * (1.0 + STAR_GAP);
                star(img, x, b.y0, size, s.rating_stars.color);
            }
        }
        for (i, t) in s.texts.iter().enumerate() {
            let Some(tl) = self.text_layout(i, l) else { continue };
            let b = t.place.place(l, tl.width as f64, tl.height as f64);
            let short = Self::short(l);
            if t.shadow.on && t.shadow.opacity > 0.0 {
                let (d, a) = (t.shadow.offset * short, t.shadow.angle.to_radians());
                let (dx, dy) = (d * a.cos(), -d * a.sin());
                shadow(img, &tl, b.x0 + dx, b.y0 + dy, t.shadow.radius * short / 2.0, (t.shadow.opacity * t.opacity) as f32);
            }
            draw_text(img, &tl, b.x0, b.y0, t.color, t.opacity as f32);
        }
    }

    fn draw_plate(&self, img: &mut Rgba8, l: &Layout) {
        let p = &self.settings.identity_plate;
        let Some(c) = self.plate(l) else { return };
        let (w, h) = c.size();
        let b = p.place.place(l, w, h);
        match c {
            PlateContent::Text(tl) => draw_text(img, &tl, b.x0, b.y0, p.color, p.opacity as f32),
            PlateContent::Image(src, w, h) => {
                if let Some(scaled) = scaled_graphic(p.image.trim(), &src, w, h) {
                    draw_graphic(img, &scaled, b.x0.round() as i64, b.y0.round() as i64, p.opacity as f32);
                }
            }
        }
    }
}

enum PlateContent {
    Text(crate::text::TextLayout),
    Image(Arc<Rgba8>, usize, usize),
}

impl PlateContent {
    fn size(&self) -> (f64, f64) {
        match self {
            PlateContent::Text(t) => (t.width as f64, t.height as f64),
            PlateContent::Image(_, w, h) => (*w as f64, *h as f64),
        }
    }
}

/// Whole pixel bounds of `r` clipped to `w × h`.
fn pixels(r: &Rect, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let c = |v: f64, max: usize| if v.is_finite() { v.round().clamp(0.0, max as f64) as usize } else { 0 };
    let (x0, y0) = (c(r.x0, w), c(r.y0, h));
    (x0, y0, c(r.x1, w).max(x0), c(r.y1, h).max(y0))
}

/// Blend `c` into pixel (`x`, `y`) at `k` (0–1); outside the image does nothing.
fn blend(img: &mut Rgba8, x: i64, y: i64, k: f32, c: [u8; 3]) {
    if x < 0 || y < 0 || k <= 0.0 {
        return;
    }
    let (x, y) = (x as usize, y as usize);
    if x >= img.width || y >= img.height {
        return;
    }
    let w = img.width;
    if let Some(p) = img.data.get_mut(y * w + x) {
        let k = k.min(1.0);
        for i in 0..3 {
            p[i] = (p[i] as f32 + (c[i] as f32 - p[i] as f32) * k).round().clamp(0.0, 255.0) as u8;
        }
    }
}

fn draw_text(img: &mut Rgba8, tl: &crate::text::TextLayout, x: f64, y: f64, c: [u8; 3], opacity: f32) {
    let (w, h) = (img.width as f32, img.height as f32);
    tl.draw_in(x as f32, y as f32, w, h, |gx, gy, cov| blend(img, i64::from(gx), i64::from(gy), cov * opacity, c));
}

/// A soft black shadow of the text at (`x`, `y`): its coverage blurred by `sigma` pixels. Only
/// the part that can reach the image is rasterised.
fn shadow(img: &mut Rgba8, tl: &crate::text::TextLayout, x: f64, y: f64, sigma: f64, opacity: f32) {
    let pad = (sigma * 3.0).ceil().clamp(0.0, 512.0);
    let (iw, ih) = (img.width as f64, img.height as f64);
    let mx0 = (x - pad).floor().max(-pad);
    let my0 = (y - pad).floor().max(-pad);
    let mx1 = (x + tl.width as f64 + pad).ceil().min(iw + pad);
    let my1 = (y + tl.height as f64 + pad).ceil().min(ih + pad);
    if !(mx1 > mx0 && my1 > my0 && mx0.is_finite() && my0.is_finite()) {
        return;
    }
    let (w, h) = ((mx1 - mx0) as usize, (my1 - my0) as usize);
    let mut mask = Plane::new(w, h);
    tl.draw_in((x - mx0) as f32, (y - my0) as f32, w as f32, h as f32, |gx, gy, cov| {
        if gx >= 0
            && gy >= 0
            && (gx as usize) < w
            && (gy as usize) < h
            && let Some(m) = mask.data.get_mut(gy as usize * w + gx as usize)
        {
            *m = (*m + cov).min(1.0);
        }
    });
    let mask = if sigma > 0.3 { lightcraft_raster::blur::gaussian(&mask, sigma as f32) } else { mask };
    let (ox, oy) = (mx0 as i64, my0 as i64);
    for (i, k) in mask.data.iter().enumerate() {
        if *k > 1e-3 {
            blend(img, ox + (i % w) as i64, oy + (i / w) as i64, k * opacity, [0, 0, 0]);
        }
    }
}

/// The identity plate image scaled to its size, premultiplied: (file, its decoded image, w, h) →
/// pixels, so the slides of a slideshow don't each scale a large logo again.
type ScaledCache = std::sync::Mutex<Option<(String, usize, usize, usize, Arc<Image<[f32; 4]>>)>>;
static SCALED: ScaledCache = std::sync::Mutex::new(None);

/// `src` (decoded from `path`) scaled to `w × h` (premultiplied, so transparent edges stay clean).
fn scaled_graphic(path: &str, src: &Arc<Rgba8>, w: usize, h: usize) -> Option<Arc<Image<[f32; 4]>>> {
    if src.width == 0 || src.height == 0 || w == 0 || h == 0 || src.data.len() != src.width * src.height {
        return None;
    }
    let id = Arc::as_ptr(src) as usize;
    let mut cache = SCALED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((p, i, cw, ch, img)) = cache.as_ref()
        && (p.as_str(), *i, *cw, *ch) == (path, id, w, h)
    {
        return Some(img.clone());
    }
    let pre: Image<[f32; 4]> = Image::from_fn(src.width, src.height, |x, y| {
        let p = src.get(x, y);
        let a = p[3] as f32 / 255.0;
        [p[0] as f32 * a, p[1] as f32 * a, p[2] as f32 * a, a]
    });
    let scaled = Arc::new(lightcraft_raster::resample::resize(&pre, w, h, lightcraft_raster::resample::Filter::Mitchell));
    *cache = Some((path.to_string(), id, w, h, scaled.clone()));
    Some(scaled)
}

/// The scaled graphic at (`x`, `y`), blended with its alpha × `opacity`.
fn draw_graphic(img: &mut Rgba8, scaled: &Image<[f32; 4]>, x: i64, y: i64, opacity: f32) {
    for yy in 0..scaled.height {
        for xx in 0..scaled.width {
            let [r, g, b, a] = scaled.get(xx, yy);
            let a = a.clamp(0.0, 1.0);
            if a <= 1e-4 {
                continue;
            }
            let c = [r, g, b].map(|v| (v / a).round().clamp(0.0, 255.0) as u8);
            blend(img, x + xx as i64, y + yy as i64, a * opacity, c);
        }
    }
}

/// The ten points of a five-pointed star in a `size` box at (`x`, `y`), point up.
fn star_points(x: f64, y: f64, size: f64) -> [(f64, f64); 10] {
    let r = size / 2.0;
    // the star's height is r (1 + cos 36°); centre it vertically in the box
    let cy = y + size / 2.0 + r * (1.0 - (36f64).to_radians().cos()) / 2.0;
    let cx = x + size / 2.0;
    let inner = r * 0.382;
    let mut pts = [(0.0, 0.0); 10];
    for (i, p) in pts.iter_mut().enumerate() {
        let a = (90.0 + 36.0 * i as f64).to_radians();
        let rr = if i % 2 == 0 { r } else { inner };
        *p = (cx + rr * a.cos(), cy - rr * a.sin());
    }
    pts
}

/// A filled star: even-odd scanlines, four a pixel row, with exact horizontal coverage.
fn star(img: &mut Rgba8, x: f64, y: f64, size: f64, c: [u8; 3]) {
    if !(x.is_finite() && y.is_finite() && size.is_finite() && size > 0.0) {
        return;
    }
    let pts = star_points(x, y, size);
    let (x0, y0) = (x.floor() as i64, y.floor() as i64);
    let n = size.ceil() as usize + 2;
    let mut row = vec![0f32; n];
    let mut xs: Vec<f64> = Vec::with_capacity(pts.len());
    for py in y0..y0 + n as i64 {
        row.fill(0.0);
        for s in 0..4 {
            let sy = py as f64 + (s as f64 + 0.5) / 4.0;
            xs.clear();
            for (i, &(xi, yi)) in pts.iter().enumerate() {
                let Some(&(xj, yj)) = pts.get((i + pts.len() - 1) % pts.len()) else { continue };
                if (yi > sy) != (yj > sy) {
                    xs.push((xj - xi) * (sy - yi) / (yj - yi) + xi);
                }
            }
            xs.sort_by(f64::total_cmp);
            for &[a, b] in xs.as_chunks::<2>().0 {
                let (a, b) = (a - x0 as f64, b - x0 as f64);
                let first = a.floor().max(0.0) as usize;
                let last = (b.ceil().max(0.0) as usize).min(n);
                for (i, v) in row.iter_mut().enumerate().take(last).skip(first) {
                    let cover = (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0);
                    *v += cover as f32 / 4.0;
                }
            }
        }
        for (i, k) in row.iter().enumerate() {
            if *k > 0.0 {
                blend(img, x0 + i as i64, py, k.min(1.0), c);
            }
        }
    }
}

#[cfg(test)]
mod tests;
