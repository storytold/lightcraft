//! Slideshow title screens (Lightroom Classic's Slideshow ▸ Titles panel): an intro shown before
//! the first slide and an ending shown after the last one, each switched on on its own, with a
//! background colour and, centred on it, the identity plate (the one of the Overlays panel: its
//! text or image) or a line of text, at a scale. [`TitleSettings`] is a library preference
//! (`prefs.json`), edited with the `slideshow.titles` command; [`TitleCompose`] draws one screen.
//!
//! **One composer:** a title screen is a slide with no photo: `lightcraft_slideshow::compose`
//! fills it with the title's colour, then the overlay code ([`crate::slide_overlay`]) draws the
//! plate or text with the shared text renderer ([`crate::text`]). Playback, the Settings preview
//! and `slideshow.preview {title}` all go through [`TitleCompose::compose`], so they draw the same
//! pixels.
//!
//! **Timing** is in [`crate::slideshow::Playback`]: each screen stays up for its own duration
//! (default: the slide duration) and fades in and out with the playback fade.
//!
//! **Resolution independence:** the scale is the height of the plate (or of a text line) as a
//! fraction of the slide's shorter side; text wider than the slide is scaled down to fit.
//!
//! **Never crash:** settings are validated by [`TitleSettings::merged`] and clamped by
//! [`TitleSettings::sanitized`]; an unreadable plate image draws the colour alone (and
//! `slideshow.titles` reports it).

use std::sync::Arc;

use lightcraft_raster::Rgba8;
use lightcraft_slideshow::{Layout, Rect, SlideSettings, hex, parse_color};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::export::Anchor;
use crate::slide_overlay::{IdentityPlate, OverlaySettings, PLATE_SIZE_MAX, Placement, RelativeTo, SIZE_MIN, SlideOverlays, TEXT_MAX_CHARS};
pub use crate::slideshow::Title;

/// Widest a title's plate or text may be (fraction of the slide's width): wider is scaled down.
pub const FIT_WIDTH: f64 = 0.9;

/// What a title screen shows on its colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TitleContent {
    /// The colour alone.
    None,
    /// The identity plate of the Overlays panel (its text or image).
    #[default]
    IdentityPlate,
    /// This screen's own text.
    Text,
}

impl TitleContent {
    pub const ALL: [TitleContent; 3] = [TitleContent::None, TitleContent::IdentityPlate, TitleContent::Text];

    pub fn id(self) -> &'static str {
        match self {
            TitleContent::None => "none",
            TitleContent::IdentityPlate => "identityPlate",
            TitleContent::Text => "text",
        }
    }

    fn parse(s: &str) -> Option<TitleContent> {
        TitleContent::ALL.into_iter().find(|c| c.id().eq_ignore_ascii_case(s.trim()))
    }
}

/// One title screen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleScreen {
    pub enabled: bool,
    /// The screen's background colour.
    pub color: [u8; 3],
    pub content: TitleContent,
    /// The text drawn with [`TitleContent::Text`].
    pub text: String,
    /// Draw the plate or text in [`Self::text_color`] (off: in the identity plate's colour).
    /// An image plate keeps its own colours.
    pub override_color: bool,
    pub text_color: [u8; 3],
    /// Height of the plate (or of a text line), fraction of the slide's shorter side.
    pub scale: f64,
    /// Seconds the screen stays up (`None`: the slide duration).
    pub duration: Option<f64>,
}

impl TitleScreen {
    fn new(text: &str) -> Self {
        TitleScreen {
            enabled: false,
            color: [0, 0, 0],
            content: TitleContent::IdentityPlate,
            text: text.into(),
            override_color: false,
            text_color: [255, 255, 255],
            scale: 0.1,
            duration: None,
        }
    }
}

/// The intro and ending screens.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TitleSettings {
    pub intro: TitleScreen,
    pub ending: TitleScreen,
}

impl Default for TitleSettings {
    fn default() -> Self {
        TitleSettings { intro: TitleScreen::new("Welcome"), ending: TitleScreen::new("The End") }
    }
}

impl Default for TitleScreen {
    fn default() -> Self {
        TitleScreen::new("")
    }
}

fn cut(s: &mut String) {
    if let Some((i, _)) = s.char_indices().nth(TEXT_MAX_CHARS) {
        s.truncate(i);
    }
}

const SCREEN_KEYS: &str = "enabled, color, content, text, overrideColor, textColor, scale, duration";

impl TitleScreen {
    fn sanitized(mut self) -> Self {
        cut(&mut self.text);
        self.scale = if self.scale.is_finite() { self.scale.clamp(SIZE_MIN, PLATE_SIZE_MAX) } else { 0.1 };
        self.duration = self.duration.filter(|d| d.is_finite()).map(|d| d.clamp(crate::slideshow::SLIDE_MIN, crate::slideshow::SLIDE_MAX));
        self
    }

    pub fn to_json(&self) -> Value {
        let mut o = Map::new();
        o.insert("enabled".into(), json!(self.enabled));
        o.insert("color".into(), json!(hex(self.color)));
        o.insert("content".into(), json!(self.content.id()));
        o.insert("text".into(), json!(self.text));
        o.insert("overrideColor".into(), json!(self.override_color));
        o.insert("textColor".into(), json!(hex(self.text_color)));
        o.insert("scale".into(), json!(self.scale));
        o.insert("duration".into(), json!(self.duration));
        Value::Object(o)
    }

    fn merge(&mut self, ctx: &str, v: &Value) -> Result<(), String> {
        let obj = v.as_object().ok_or_else(|| format!("`{ctx}` must be an object, not {v}"))?;
        let num = |k: &str, v: &Value| match v.as_f64() {
            Some(x) if x.is_finite() => Ok(x),
            _ => Err(format!("`{ctx}.{k}` must be a finite number, not {v}")),
        };
        let boolean = |k: &str, v: &Value| v.as_bool().ok_or_else(|| format!("`{ctx}.{k}` must be true or false, not {v}"));
        let colour = |k: &str, v: &Value| parse_color(v).ok_or_else(|| format!("`{ctx}.{k}` must be \"#rrggbb\" or [r, g, b] (0–255), not {v}"));
        for (k, v) in obj {
            match k.as_str() {
                "enabled" => self.enabled = boolean(k, v)?,
                "color" => self.color = colour(k, v)?,
                "content" => {
                    self.content = v
                        .as_str()
                        .and_then(TitleContent::parse)
                        .ok_or_else(|| format!("`{ctx}.content` must be \"none\", \"identityPlate\" or \"text\", not {v}"))?
                }
                "text" => {
                    let mut s = v.as_str().ok_or_else(|| format!("`{ctx}.text` must be text, not {v}"))?.to_string();
                    cut(&mut s);
                    self.text = s;
                }
                "overrideColor" => self.override_color = boolean(k, v)?,
                "textColor" => self.text_color = colour(k, v)?,
                "scale" => self.scale = num(k, v)?,
                "duration" => {
                    self.duration = match v {
                        Value::Null => None,
                        _ => Some(num(k, v).map_err(|_| format!("`{ctx}.duration` must be seconds or null (the slide duration), not {v}"))?),
                    }
                }
                _ => return Err(format!("unknown key `{ctx}.{k}` ({SCREEN_KEYS})")),
            }
        }
        Ok(())
    }
}

impl TitleSettings {
    pub fn screen(&self, t: Title) -> &TitleScreen {
        match t {
            Title::Intro => &self.intro,
            Title::Ending => &self.ending,
        }
    }

    /// Every value in range (non-finite numbers take the defaults).
    pub fn sanitized(mut self) -> Self {
        self.intro = self.intro.sanitized();
        self.ending = self.ending.sanitized();
        self
    }

    pub fn to_json(&self) -> Value {
        json!({"intro": self.intro.to_json(), "ending": self.ending.to_json()})
    }

    /// These settings with the keys `p` names changed, validated and clamped; a wrong type or an
    /// unknown key is an error and nothing changes.
    pub fn merged(&self, p: &Value) -> Result<Self, String> {
        let Some(obj) = p.as_object() else { return Err("parameters must be an object".into()) };
        let mut s = self.clone();
        for (k, v) in obj {
            match k.as_str() {
                "intro" => s.intro.merge(k, v)?,
                "ending" => s.ending.merge(k, v)?,
                _ => return Err(format!("unknown setting `{k}` (intro, ending)")),
            }
        }
        Ok(s.sanitized())
    }

    /// How long each screen that is on stays up, given the slide duration (for
    /// [`crate::slideshow::Playback::with_titles`]): (intro, ending).
    pub fn durations(&self, slide: f64) -> (Option<f64>, Option<f64>) {
        let d = |s: &TitleScreen| s.enabled.then(|| s.duration.unwrap_or(slide));
        (d(&self.intro), d(&self.ending))
    }

    /// Problems worth telling the user about: a screen showing an identity plate image that
    /// can't be read, or one with nothing to show.
    pub fn warnings(&self, overlays: &OverlaySettings) -> Vec<String> {
        let mut out = Vec::new();
        let plate = &overlays.identity_plate;
        for t in Title::ALL {
            let s = self.screen(t);
            if !s.enabled {
                continue;
            }
            match s.content {
                TitleContent::IdentityPlate if !plate.image.trim().is_empty() && crate::export::watermark_logo(plate.image.trim()).is_none() => {
                    out.push(format!("{} screen: identity plate image {} can't be read; only the colour shows", t.id(), plate.image.trim()))
                }
                TitleContent::IdentityPlate if plate.image.trim().is_empty() && plate.text.trim().is_empty() => {
                    out.push(format!("{} screen: the identity plate has no text or image; only the colour shows", t.id()))
                }
                TitleContent::Text if s.text.trim().is_empty() => out.push(format!("{} screen: no text; only the colour shows", t.id())),
                _ => {}
            }
        }
        out
    }
}

/// How to draw one title screen at a size: its colour and its plate or text, resolved from the
/// library's settings (no session needed, so it runs on a render worker).
#[derive(Clone, Debug)]
pub struct TitleCompose {
    pub title: Title,
    /// The screen as a slide: its colour as the backdrop, nothing else.
    pub settings: SlideSettings,
    /// The plate or text, as a centred identity plate (`None`: the colour alone).
    pub overlays: Option<Arc<SlideOverlays>>,
    pub width: usize,
    pub height: usize,
}

impl TitleCompose {
    /// Screen `title` of `titles` at `width × height` (a bigger one is drawn at the composer's
    /// [`lightcraft_slideshow::MAX_SIDE`]),
    /// with `overlays`' identity plate when the screen shows it.
    pub fn new(titles: &TitleSettings, overlays: &OverlaySettings, title: Title, width: usize, height: usize) -> Self {
        let s = titles.screen(title).clone().sanitized();
        let settings = SlideSettings { backdrop_color: s.color, ..SlideSettings::default() }.sanitized();
        let plate = &overlays.identity_plate;
        let centre = Placement { anchor: Anchor::Center, offset_x: 0.0, offset_y: 0.0, relative_to: RelativeTo::Slide };
        let colour = |own: [u8; 3]| if s.override_color { s.text_color } else { own };
        let shown = match s.content {
            TitleContent::None => None,
            TitleContent::IdentityPlate => Some(IdentityPlate {
                enabled: true,
                text: plate.text.clone(),
                image: plate.image.clone(),
                size: s.scale,
                color: colour(plate.color),
                opacity: plate.opacity,
                place: centre,
                behind: false,
            }),
            TitleContent::Text => Some(IdentityPlate {
                enabled: true,
                text: s.text.clone(),
                image: String::new(),
                size: s.scale,
                color: colour(plate.color),
                opacity: 1.0,
                place: centre,
                behind: false,
            }),
        };
        let mut compose = TitleCompose { title, settings, overlays: None, width, height };
        if let Some(p) = shown.filter(|p| !p.text.trim().is_empty() || !p.image.trim().is_empty()) {
            let resolved =
                |p: IdentityPlate| Arc::new(SlideOverlays::resolve(&OverlaySettings { identity_plate: p, ..OverlaySettings::default() }, None));
            compose.overlays = Some(resolved(p.clone()));
            // a long text (or a wide logo) at a large scale: shrink it to fit the slide's width
            if let Some(b) = compose.plate_box() {
                let max = FIT_WIDTH * compose.layout().slide.width();
                if b.width() > max && b.width() > 0.0 {
                    let size = (p.size * max / b.width()).max(SIZE_MIN);
                    compose.overlays = Some(resolved(IdentityPlate { size, ..p }));
                }
            }
        }
        compose
    }

    fn layout(&self) -> Layout {
        lightcraft_slideshow::layout(&self.settings, 1.0, self.width, self.height)
    }

    /// Where the plate or text is on the screen (`None`: nothing is drawn but the colour).
    pub fn plate_box(&self) -> Option<Rect> {
        let o = self.overlays.as_ref()?;
        o.boxes(&self.layout()).into_iter().next().map(|(_, r)| r)
    }

    /// What makes the screen's pixels differ (for render keys).
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        "title".hash(&mut h);
        self.title.hash(&mut h);
        self.settings.pixel_key().hash(&mut h);
        self.overlays.as_ref().map(|o| o.key()).hash(&mut h);
        (self.width, self.height).hash(&mut h);
        h.finish()
    }

    /// The screen's pixels.
    pub fn compose(&self) -> Rgba8 {
        let empty = Rgba8::new(0, 0);
        let (mut img, l) = lightcraft_slideshow::compose_with(&empty, None, &self.settings, self.width, self.height, &mut |_, _| {});
        if let Some(o) = &self.overlays {
            o.draw_front(&mut img, &l);
        }
        img
    }

    /// [`Self::compose`] as a render result (for a frontend's render queue): request `request_id`,
    /// under this screen's key. Title screens show no photo (`photo` is 0).
    pub fn result(&self, request_id: u64) -> crate::media::RenderResult {
        let image = self.compose();
        let histogram = lightcraft_raster::Histogram::of_srgb8(&image);
        crate::media::RenderResult {
            cache_generation: 0,
            cache_owner: std::sync::Weak::new(),
            request_id,
            source_key: None,
            photo: lightcraft_catalog::PhotoId(0),
            level: crate::media::SourceLevel::Preview,
            key: self.key(),
            rendered: Ok(lightcraft_pipeline::Rendered { image, histogram, deep: None }),
            loaded: None,
            quick: None,
            display: None,
            gpu_fallback: None,
        }
    }
}

impl crate::Session {
    /// Title screen `title` at `width × height` with the library's titles and identity plate.
    pub fn title_compose(&self, title: Title, width: usize, height: usize) -> Arc<TitleCompose> {
        Arc::new(TitleCompose::new(&self.slide_titles, &self.slide_overlays, title, width, height))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(img: &Rgba8, x: usize, y: usize) -> [u8; 3] {
        let p = img.get(x, y);
        [p[0], p[1], p[2]]
    }

    /// The bounding box of the pixels that differ from `bg` (x0, y0, x1, y1).
    fn ink(img: &Rgba8, bg: [u8; 3]) -> Option<(usize, usize, usize, usize)> {
        let mut b: Option<(usize, usize, usize, usize)> = None;
        for y in 0..img.height {
            for x in 0..img.width {
                let c = at(img, x, y);
                if c.iter().zip(bg).any(|(a, b)| a.abs_diff(b) > 40) {
                    b = Some(match b {
                        None => (x, y, x + 1, y + 1),
                        Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1)),
                    });
                }
            }
        }
        b
    }

    fn text_titles(scale: f64) -> TitleSettings {
        TitleSettings::default()
            .merged(&json!({"intro": {"enabled": true, "color": "#203040", "content": "text", "text": "Holiday", "overrideColor": true, "textColor": "#ffcc00", "scale": scale}}))
            .unwrap()
    }

    #[test]
    fn a_text_title_is_centred_in_its_colours_and_scales_with_the_slide() {
        let o = OverlaySettings::default();
        let t = text_titles(0.2);
        let mut boxes = Vec::new();
        for (w, h) in [(400, 250), (800, 500)] {
            let c = TitleCompose::new(&t, &o, Title::Intro, w, h);
            let img = c.compose();
            assert_eq!((img.width, img.height), (w, h));
            assert_eq!(at(&img, 2, 2), [0x20, 0x30, 0x40], "the title's colour");
            let (x0, y0, x1, y1) = ink(&img, [0x20, 0x30, 0x40]).expect("the text is drawn");
            // centred (glyph ink sits inside the line box, so allow some slack vertically)
            let (cx, cy) = ((x0 + x1) as f64 / 2.0, (y0 + y1) as f64 / 2.0);
            assert!((cx - w as f64 / 2.0).abs() < w as f64 * 0.03, "centred across: {cx} of {w}");
            assert!((cy - h as f64 / 2.0).abs() < h as f64 * 0.06, "centred down: {cy} of {h}");
            let mid = at(&img, (x0..x1).find(|x| at(&img, *x, (y0 + y1) / 2) != [0x20, 0x30, 0x40]).unwrap_or(x0), (y0 + y1) / 2);
            assert!(mid[0] > 150 && mid[2] < 120, "drawn in the text colour: {mid:?}");
            boxes.push(((x1 - x0) as f64 / w as f64, (y1 - y0) as f64 / h as f64));
            let b = c.plate_box().unwrap();
            assert!((b.height() - 0.2 * h as f64).abs() < 2.0, "the scale is the line height: {b:?}");
        }
        let ((w1, h1), (w2, h2)) = (boxes[0], boxes[1]);
        assert!((w1 - w2).abs() < 0.02 && (h1 - h2).abs() < 0.02, "proportional at two sizes: {boxes:?}");
        // a bigger scale draws bigger text
        let big = ink(&TitleCompose::new(&text_titles(0.3), &o, Title::Intro, 800, 500).compose(), [0x20, 0x30, 0x40]).unwrap();
        assert!((big.2 - big.0) as f64 / 800.0 > w2 * 1.3);
    }

    #[test]
    fn the_identity_plate_shows_in_its_own_colour_unless_overridden() {
        let o = OverlaySettings::default()
            .merged(&json!({"identityPlate": {"text": "Studio", "color": "#00ff00", "opacity": 1, "anchor": "topLeft"}}))
            .unwrap();
        assert!(!o.identity_plate.enabled, "the overlay plate itself may be off");
        let t = TitleSettings::default().merged(&json!({"ending": {"enabled": true, "color": "#000000", "scale": 0.15}})).unwrap();
        let img = TitleCompose::new(&t, &o, Title::Ending, 600, 400).compose();
        let (x0, y0, x1, y1) = ink(&img, [0, 0, 0]).expect("the plate is drawn");
        assert!(x0 > 100 && x1 < 500 && y0 > 120 && y1 < 280, "centred, not at the overlay's anchor: {:?}", (x0, y0, x1, y1));
        let greens = (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| {
            let c = at(&img, x, y);
            c[1] > 200 && c[0] < 60
        });
        assert!(greens.count() > 100, "in the plate's colour");
        let over = t.merged(&json!({"ending": {"overrideColor": true, "textColor": "#ff0000"}})).unwrap();
        let img = TitleCompose::new(&over, &o, Title::Ending, 600, 400).compose();
        let reds = img.data.iter().filter(|p| p[0] > 200 && p[1] < 60).count();
        assert!(reds > 100, "overridden");
        // the colour alone
        let none = t.merged(&json!({"ending": {"content": "none", "color": "#123456"}})).unwrap();
        let c = TitleCompose::new(&none, &o, Title::Ending, 64, 40);
        assert!(c.plate_box().is_none());
        assert!(c.compose().data.iter().all(|p| p[..3] == [0x12, 0x34, 0x56]));
    }

    #[test]
    fn a_missing_plate_image_draws_the_colour_and_is_reported() {
        let o = OverlaySettings::default().merged(&json!({"identityPlate": {"image": "/no/such/logo.png"}})).unwrap();
        let t = TitleSettings::default().merged(&json!({"intro": {"enabled": true, "color": "#336699"}})).unwrap();
        let c = TitleCompose::new(&t, &o, Title::Intro, 120, 80);
        assert!(c.compose().data.iter().all(|p| p[..3] == [0x33, 0x66, 0x99]));
        let w = t.warnings(&o);
        assert!(w.iter().any(|w| w.contains("intro") && w.contains("can't be read")), "{w:?}");
        let t = t.merged(&json!({"intro": {"content": "text", "text": "  "}})).unwrap();
        assert!(t.warnings(&o).iter().any(|w| w.contains("no text")));
    }

    #[test]
    fn long_text_is_scaled_down_to_fit_the_width() {
        let t = TitleSettings::default()
            .merged(&json!({"intro": {"enabled": true, "content": "text", "text": "A very long title that would never fit across", "scale": 0.5}}))
            .unwrap();
        let c = TitleCompose::new(&t, &OverlaySettings::default(), Title::Intro, 800, 450);
        let b = c.plate_box().unwrap();
        assert!(b.width() <= 800.0 * FIT_WIDTH + 2.0 && b.x0 >= 0.0, "{b:?}");
    }

    #[test]
    fn settings_are_validated_clamped_and_round_trip() {
        let d = TitleSettings::default();
        assert!(!d.intro.enabled && !d.ending.enabled, "off by default");
        assert_eq!(d.durations(4.0), (None, None));
        for bad in [
            json!({"intro": {"enabled": 1}}),
            json!({"intro": {"color": "#12"}}),
            json!({"ending": {"content": "video"}}),
            json!({"ending": {"scale": "big"}}),
            json!({"ending": {"duration": "long"}}),
            json!({"intro": {"sparkle": true}}),
            json!({"intro": []}),
            json!({"credits": {}}),
            json!({"INTRO": {}}),
            json!([1]),
        ] {
            assert!(d.merged(&bad).is_err(), "{bad}");
        }
        let s = d
            .merged(&json!({"intro": {"enabled": true, "scale": 9, "duration": 1e9}, "ending": {"enabled": true, "scale": -1, "duration": 0}}))
            .unwrap();
        assert_eq!((s.intro.scale, s.ending.scale), (PLATE_SIZE_MAX, SIZE_MIN));
        assert_eq!((s.intro.duration, s.ending.duration), (Some(crate::slideshow::SLIDE_MAX), Some(crate::slideshow::SLIDE_MIN)));
        assert_eq!(s.durations(4.0), (Some(crate::slideshow::SLIDE_MAX), Some(crate::slideshow::SLIDE_MIN)));
        let s = s.merged(&json!({"intro": {"duration": null}})).unwrap();
        assert_eq!(s.durations(4.0).0, Some(4.0), "null: the slide duration");
        assert_eq!(d.merged(&s.to_json()).unwrap(), s, "the JSON form reads back");
        let back: TitleSettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let partial: TitleSettings = serde_json::from_str(r#"{"ending": {"enabled": true}}"#).unwrap();
        assert!(partial.ending.enabled && !partial.intro.enabled);
        // NaN / ∞ from a hand-edited file
        let s = TitleSettings { intro: TitleScreen { scale: f64::NAN, duration: Some(f64::INFINITY), ..d.intro.clone() }, ..d.clone() }.sanitized();
        assert_eq!((s.intro.scale, s.intro.duration), (0.1, None));
        // any size composes
        for (w, h) in [(0, 0), (1, 100_000), (usize::MAX, 3)] {
            let c = TitleCompose::new(&text_titles(0.5), &OverlaySettings::default(), Title::Intro, w, h);
            let img = c.compose();
            assert!(img.width >= 1 && img.height >= 1 && img.width <= lightcraft_slideshow::MAX_SIDE);
        }
    }
}
