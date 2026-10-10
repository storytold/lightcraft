//! What a slideshow looks like and how it plays: the panels of the Classic Slideshow module
//! (Options, Layout, Overlays, Backdrop, Titles, Playback, Music), as one serialisable value that is
//! also a template. Every length is relative (a fraction of the frame's short edge or of its sides),
//! so a preview, a full-screen show and an export at any size look the same.

use serde::{Deserialize, Serialize};

pub use dac_engine::export::Anchor;

/// The whole look and playback of a slideshow (a template is one of these with a name).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub options: Options,
    pub layout: Layout,
    pub overlays: Overlays,
    pub backdrop: Backdrop,
    pub titles: Titles,
    pub playback: Playback,
    pub music: Music,
}

/// Options: how the photo sits in its cell.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    /// Fill the cell (cropping the photo) instead of fitting inside it.
    pub zoom_to_fill: bool,
    pub stroke: bool,
    /// Stroke width, a fraction of the frame's short edge.
    pub stroke_width: f32,
    pub stroke_color: [u8; 3],
    pub shadow: bool,
    /// 0..1.
    pub shadow_opacity: f32,
    /// Fractions of the short edge.
    pub shadow_offset: f32,
    pub shadow_radius: f32,
    /// Degrees, 0 = to the right, counter-clockwise (−45 = down-right).
    pub shadow_angle: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            zoom_to_fill: false,
            stroke: false,
            stroke_width: 0.004,
            stroke_color: [255, 255, 255],
            shadow: true,
            shadow_opacity: 0.35,
            shadow_offset: 0.012,
            shadow_radius: 0.02,
            shadow_angle: -45.0,
        }
    }
}

/// The aspect the slide is laid out for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Aspect {
    /// The screen's (or the export size's) aspect.
    #[default]
    Screen,
    Wide16x9,
    Classic4x3,
}

impl Aspect {
    /// Width / height, `None` = whatever the frame is.
    pub fn ratio(self) -> Option<f32> {
        match self {
            Aspect::Screen => None,
            Aspect::Wide16x9 => Some(16.0 / 9.0),
            Aspect::Classic4x3 => Some(4.0 / 3.0),
        }
    }
}

/// Layout: the photo cell's margins.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Layout {
    pub show_guides: bool,
    /// Left, top, right, bottom, each a fraction of the frame's width (left/right) or height.
    pub margins: [f32; 4],
    /// Moving one margin moves all four.
    pub linked: bool,
    pub aspect: Aspect,
}

impl Default for Layout {
    fn default() -> Self {
        Layout { show_guides: true, margins: [0.08; 4], linked: true, aspect: Aspect::Screen }
    }
}

/// A line of text on the slide; `{Tokens}` are filled per photo (see [`crate::tokens`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TextOverlay {
    pub text: String,
    pub anchor: Anchor,
    /// Anchored to the photo (else to the frame).
    pub on_photo: bool,
    /// Text height, a fraction of the frame's short edge.
    pub size: f32,
    pub color: [u8; 3],
    pub opacity: f32,
    pub shadow: bool,
}

impl Default for TextOverlay {
    fn default() -> Self {
        TextOverlay {
            text: "{Filename}".into(),
            anchor: Anchor::Bottom,
            on_photo: false,
            size: 0.03,
            color: [230, 230, 230],
            opacity: 1.0,
            shadow: true,
        }
    }
}

/// Overlays: identity plate, rating stars, watermark and text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Overlays {
    pub identity_plate: bool,
    /// The identity plate's text (empty: the app's plate text, filled in by the UI).
    pub plate_text: String,
    pub plate_anchor: Anchor,
    pub plate_size: f32,
    pub plate_opacity: f32,
    pub rating: bool,
    pub rating_color: [u8; 3],
    pub rating_size: f32,
    pub rating_opacity: f32,
    /// A watermark (text) over the photo.
    pub watermark: String,
    pub texts: Vec<TextOverlay>,
}

impl Default for Overlays {
    fn default() -> Self {
        Overlays {
            identity_plate: false,
            plate_text: String::new(),
            plate_anchor: Anchor::TopLeft,
            plate_size: 0.04,
            plate_opacity: 0.8,
            rating: false,
            rating_color: [220, 220, 220],
            rating_size: 0.03,
            rating_opacity: 1.0,
            watermark: String::new(),
            texts: Vec::new(),
        }
    }
}

/// Backdrop: a colour, a colour wash and an image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Backdrop {
    pub color: [u8; 3],
    pub wash: bool,
    pub wash_color: [u8; 3],
    /// 0..1.
    pub wash_opacity: f32,
    /// Degrees; the wash fades from `wash_color` at this side to nothing at the other.
    pub wash_angle: f32,
    /// A background image file (empty = none).
    pub image: String,
    pub image_opacity: f32,
}

impl Default for Backdrop {
    fn default() -> Self {
        Backdrop {
            color: [24, 24, 24],
            wash: false,
            wash_color: [80, 80, 80],
            wash_opacity: 0.6,
            wash_angle: 90.0,
            image: String::new(),
            image_opacity: 0.5,
        }
    }
}

/// An intro or ending screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleScreen {
    pub enabled: bool,
    pub color: [u8; 3],
    /// Draw the identity plate (else nothing but the colour).
    pub plate: bool,
    pub text: String,
    pub text_color: [u8; 3],
    pub size: f32,
}

impl Default for TitleScreen {
    fn default() -> Self {
        TitleScreen { enabled: false, color: [0, 0, 0], plate: true, text: String::new(), text_color: [230, 230, 230], size: 0.06 }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Titles {
    pub intro: TitleScreen,
    pub ending: TitleScreen,
}

/// Playback.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Playback {
    /// Advance only on a key press.
    pub manual: bool,
    /// Seconds each slide is shown (fades excluded).
    pub slide_secs: f32,
    /// Seconds of each cross-fade.
    pub fade_secs: f32,
    /// Fade through a colour instead of cross-fading.
    pub color_fade: bool,
    pub fade_color: [u8; 3],
    pub random: bool,
    pub repeat: bool,
    /// Ken Burns: slowly zoom and pan each slide.
    pub pan_zoom: bool,
    /// 0..1.
    pub pan_zoom_amount: f32,
    /// Draft quality (the preview renders) instead of full renders.
    pub draft: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Playback {
            manual: false,
            slide_secs: 4.0,
            fade_secs: 1.0,
            color_fade: false,
            fade_color: [0, 0, 0],
            random: false,
            repeat: true,
            pan_zoom: false,
            pan_zoom_amount: 0.5,
            draft: false,
        }
    }
}

/// Music: the tracks, played one after another.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Music {
    pub enabled: bool,
    pub tracks: Vec<String>,
    /// Stretch the slide durations so the show lasts as long as the music.
    pub fit_to_music: bool,
    /// 0..1.
    pub volume: f32,
    /// −1 (left) .. 1 (right).
    pub balance: f32,
}

impl Default for Music {
    fn default() -> Self {
        Music { enabled: false, tracks: Vec::new(), fit_to_music: false, volume: 0.8, balance: 0.0 }
    }
}

impl Settings {
    /// Clamp every number into its range (settings come from users, agents and files).
    pub fn sanitized(mut self) -> Self {
        fn c(v: &mut f32, lo: f32, hi: f32, def: f32) {
            *v = if v.is_finite() { v.clamp(lo, hi) } else { def };
        }
        let o = &mut self.options;
        c(&mut o.stroke_width, 0.0, 0.05, 0.004);
        c(&mut o.shadow_opacity, 0.0, 1.0, 0.35);
        c(&mut o.shadow_offset, 0.0, 0.1, 0.012);
        c(&mut o.shadow_radius, 0.0, 0.1, 0.02);
        c(&mut o.shadow_angle, -360.0, 360.0, -45.0);
        for m in &mut self.layout.margins {
            c(m, 0.0, 0.45, 0.08);
        }
        let v = &mut self.overlays;
        c(&mut v.plate_size, 0.005, 0.3, 0.04);
        c(&mut v.plate_opacity, 0.0, 1.0, 0.8);
        c(&mut v.rating_size, 0.005, 0.2, 0.03);
        c(&mut v.rating_opacity, 0.0, 1.0, 1.0);
        v.texts.truncate(32);
        for t in &mut v.texts {
            c(&mut t.size, 0.005, 0.3, 0.03);
            c(&mut t.opacity, 0.0, 1.0, 1.0);
        }
        let b = &mut self.backdrop;
        c(&mut b.wash_opacity, 0.0, 1.0, 0.6);
        c(&mut b.wash_angle, -360.0, 360.0, 90.0);
        c(&mut b.image_opacity, 0.0, 1.0, 0.5);
        for s in [&mut self.titles.intro, &mut self.titles.ending] {
            c(&mut s.size, 0.005, 0.3, 0.06);
        }
        let p = &mut self.playback;
        c(&mut p.slide_secs, 0.1, 600.0, 4.0);
        c(&mut p.fade_secs, 0.0, 30.0, 1.0);
        c(&mut p.pan_zoom_amount, 0.0, 1.0, 0.5);
        let m = &mut self.music;
        m.tracks.truncate(64);
        c(&mut m.volume, 0.0, 1.0, 0.8);
        c(&mut m.balance, -1.0, 1.0, 0.0);
        self
    }

    /// Parse settings JSON (unknown keys ignored, missing ones defaulted), sanitized.
    pub fn from_json(v: &serde_json::Value) -> Result<Settings, String> {
        serde_json::from_value::<Settings>(v.clone()).map(Settings::sanitized).map_err(|e| format!("bad slideshow settings: {e}"))
    }

    /// Merge `patch` (a partial settings object) into these settings.
    pub fn patched(&self, patch: &serde_json::Value) -> Result<Settings, String> {
        let mut base = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut base, patch);
        Settings::from_json(&base)
    }
}

fn merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// A named template.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Template {
    pub name: String,
    pub settings: Settings,
}

/// A saved slideshow: a template plus its photos (a special collection).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SavedSlideshow {
    pub name: String,
    pub settings: Settings,
    pub photos: Vec<u64>,
}

/// The templates that come with the app (own designs).
pub fn builtin_templates() -> Vec<Template> {
    let default = Settings::default();
    let mut caption = default.clone();
    caption.overlays.rating = true;
    caption.overlays.texts = vec![TextOverlay { text: "{Caption}".into(), ..TextOverlay::default() }];
    let mut fill = default.clone();
    fill.options.zoom_to_fill = true;
    fill.options.shadow = false;
    fill.layout.margins = [0.0; 4];
    fill.backdrop.color = [0, 0, 0];
    let mut exif = default.clone();
    exif.backdrop.color = [12, 12, 12];
    exif.overlays.rating = true;
    exif.overlays.texts = vec![
        TextOverlay { text: "{Filename}".into(), anchor: Anchor::TopLeft, ..TextOverlay::default() },
        TextOverlay { text: "{Exposure} · {Aperture} · ISO {ISO} · {Focal}".into(), anchor: Anchor::Bottom, ..TextOverlay::default() },
        TextOverlay { text: "{Camera} · {Lens}".into(), anchor: Anchor::BottomLeft, size: 0.022, ..TextOverlay::default() },
    ];
    let mut wide = fill.clone();
    wide.layout.aspect = Aspect::Wide16x9;
    wide.playback.pan_zoom = true;
    wide.playback.fade_secs = 1.5;
    let mut simple = default.clone();
    simple.options.shadow = false;
    simple.backdrop.color = [0, 0, 0];
    simple.layout.margins = [0.03; 4];
    vec![
        Template { name: "Default".into(), settings: default },
        Template { name: "Caption and Rating".into(), settings: caption },
        Template { name: "Crop to Fill".into(), settings: fill },
        Template { name: "Exif Metadata".into(), settings: exif },
        Template { name: "Simple".into(), settings: simple },
        Template { name: "Widescreen".into(), settings: wide },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_numbers_are_clamped() {
        let s = Settings::from_json(&serde_json::json!({
            "playback": {"slideSecs": -5.0, "fadeSecs": 1e9},
            "layout": {"margins": [2.0, -1.0, 0.1, 0.1]},
        }))
        .unwrap();
        assert_eq!(s.playback.slide_secs, 0.1);
        assert_eq!(s.playback.fade_secs, 30.0);
        assert_eq!(s.layout.margins, [0.45, 0.0, 0.1, 0.1]);
    }

    #[test]
    fn patch_merges_nested_keys() {
        let s = Settings::default().patched(&serde_json::json!({"options": {"zoomToFill": true}})).unwrap();
        assert!(s.options.zoom_to_fill);
        assert_eq!(s.options.shadow, Settings::default().options.shadow);
        assert!(Settings::default().patched(&serde_json::json!({"playback": {"slideSecs": "x"}})).is_err());
    }

    #[test]
    fn templates_round_trip() {
        for t in builtin_templates() {
            let v = serde_json::to_value(&t).unwrap();
            assert_eq!(serde_json::from_value::<Template>(v).unwrap(), t);
        }
    }
}
