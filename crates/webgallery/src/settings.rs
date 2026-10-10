//! The Web module's settings: one value per panel control. Everything has a default and every
//! numeric field is clamped by [`GallerySettings::sanitized`], so a hand-edited saved gallery or an
//! agent's arguments can't produce a broken site.

use serde::{Deserialize, Serialize};

/// The gallery layouts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Template {
    /// Rows of thumbnails keeping their aspect ratio, a viewer on click.
    #[default]
    Grid,
    /// Square-cropped thumbnail cells in a fixed number of columns.
    Square,
    /// One horizontal filmstrip track with the large image above it.
    Track,
    /// A single-image viewer: one page per photo with previous / next.
    Single,
}

impl Template {
    pub const ALL: [Template; 4] = [Template::Grid, Template::Square, Template::Track, Template::Single];

    pub fn key(self) -> &'static str {
        match self {
            Template::Grid => "grid",
            Template::Square => "square",
            Template::Track => "track",
            Template::Single => "single",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Template::Grid => "Grid",
            Template::Square => "Square grid",
            Template::Track => "Track",
            Template::Single => "Single image",
        }
    }

    pub fn parse(s: &str) -> Option<Template> {
        Template::ALL.into_iter().find(|t| t.key().eq_ignore_ascii_case(s))
    }
}

/// Site Info panel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SiteInfo {
    pub title: String,
    pub collection_title: String,
    pub description: String,
    pub contact: String,
    /// A web address (`https://…`) or a mail address (`mailto:` is added for bare addresses).
    pub link: String,
}

/// Colour Palette panel: `#rrggbb` strings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Palette {
    pub background: String,
    pub text: String,
    pub detail_text: String,
    pub cell: String,
    pub cell_hover: String,
    pub border: String,
    pub accent: String,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            background: "#1e1e1e".into(),
            text: "#e6e6e6".into(),
            detail_text: "#a0a0a0".into(),
            cell: "#2a2a2a".into(),
            cell_hover: "#3a3a3a".into(),
            border: "#555555".into(),
            accent: "#6ea8fe".into(),
        }
    }
}

impl Palette {
    /// The palette with every colour that isn't `#rgb` / `#rrggbb` replaced by the default.
    pub fn sanitized(&self) -> Palette {
        let d = Palette::default();
        let fix = |v: &str, def: &str| normalize_color(v).unwrap_or_else(|| def.to_string());
        Palette {
            background: fix(&self.background, &d.background),
            text: fix(&self.text, &d.text),
            detail_text: fix(&self.detail_text, &d.detail_text),
            cell: fix(&self.cell, &d.cell),
            cell_hover: fix(&self.cell_hover, &d.cell_hover),
            border: fix(&self.border, &d.border),
            accent: fix(&self.accent, &d.accent),
        }
    }
}

/// `#rgb` or `#rrggbb` (with or without `#`) as lower-case `#rrggbb`.
pub fn normalize_color(s: &str) -> Option<String> {
    let h = s.trim().trim_start_matches('#');
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => Some(format!("#{}", h.to_ascii_lowercase())),
        3 => Some(format!("#{}", h.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_lowercase())),
        _ => None,
    }
}

/// `#rrggbb` as bytes (black for anything else).
pub fn color_rgb(s: &str) -> [u8; 3] {
    let Some(n) = normalize_color(s) else { return [0, 0, 0] };
    let byte = |i: usize| n.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok()).unwrap_or(0);
    [byte(1), byte(3), byte(5)]
}

/// Appearance panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Appearance {
    /// Columns of the Square grid; the target row count driver for Grid (2–10).
    pub columns: u32,
    /// Thumbnail edge in CSS pixels (80–400).
    pub thumb_size: u32,
    pub cell_numbers: bool,
    pub photo_borders: bool,
    /// Border width in CSS pixels (0–20).
    pub border_width: u32,
    /// The longest edge of the image shown in the viewer, in CSS pixels (300–4096).
    pub image_page_size: u32,
    /// Show the title / caption under thumbnails too.
    pub thumb_captions: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            columns: 4,
            thumb_size: 200,
            cell_numbers: false,
            photo_borders: true,
            border_width: 1,
            image_page_size: 1200,
            thumb_captions: false,
        }
    }
}

/// Image Info panel: caption templates (see [`crate::tokens`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ImageInfo {
    pub title: String,
    pub caption: String,
}

impl Default for ImageInfo {
    fn default() -> Self {
        ImageInfo { title: "{title}".into(), caption: "{caption}".into() }
    }
}

/// Which metadata the gallery's JPEGs keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MetadataMode {
    All,
    #[default]
    CopyrightOnly,
}

/// Output Settings panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Output {
    /// The large image's longest edge in pixels (300–4096).
    pub large_size: u32,
    /// JPEG quality 1–100.
    pub quality: u8,
    pub metadata: MetadataMode,
    /// Watermark text drawn into the large images (empty = none).
    pub watermark: String,
    /// Output sharpening for screen.
    pub sharpen: bool,
}

impl Default for Output {
    fn default() -> Self {
        Output { large_size: 1600, quality: 80, metadata: MetadataMode::CopyrightOnly, watermark: String::new(), sharpen: true }
    }
}

/// Everything one web gallery is made of (a saved gallery is this, serialised).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GallerySettings {
    pub template: Template,
    pub site: SiteInfo,
    pub palette: Palette,
    pub appearance: Appearance,
    pub image_info: ImageInfo,
    pub output: Output,
}

impl GallerySettings {
    /// A copy with numbers clamped and colours validated.
    pub fn sanitized(&self) -> GallerySettings {
        let mut s = self.clone();
        let a = &mut s.appearance;
        a.columns = a.columns.clamp(2, 10);
        a.thumb_size = a.thumb_size.clamp(80, 400);
        a.border_width = a.border_width.min(20);
        a.image_page_size = a.image_page_size.clamp(300, 4096);
        s.output.large_size = s.output.large_size.clamp(300, 4096);
        s.output.quality = s.output.quality.clamp(1, 100);
        s.palette = s.palette.sanitized();
        s
    }

    /// Settings from JSON; unknown fields are ignored and missing ones take defaults.
    pub fn from_json(v: &serde_json::Value) -> Result<GallerySettings, String> {
        serde_json::from_value::<GallerySettings>(v.clone()).map(|s| s.sanitized()).map_err(|e| format!("invalid web gallery settings: {e}"))
    }

    /// Merges a partial JSON object into these settings (nested objects merge key by key).
    pub fn merged(&self, patch: &serde_json::Value) -> Result<GallerySettings, String> {
        let mut base = serde_json::to_value(self).map_err(|e| e.to_string())?;
        merge(&mut base, patch, 0);
        GallerySettings::from_json(&base)
    }
}

/// An SFTP upload server preset (the password lives in the keychain, never here).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Server {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The remote folder the site goes into (created when missing).
    pub path: String,
    /// A private key file; empty = password authentication.
    pub key_file: String,
    /// The server's host key fingerprint (`SHA256:…`), filled on first connection.
    pub known_fingerprint: String,
}

fn merge(base: &mut serde_json::Value, patch: &serde_json::Value, depth: usize) {
    match (base, patch) {
        (serde_json::Value::Object(b), serde_json::Value::Object(p)) if depth < 8 => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v, depth + 1),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn colours_normalise_or_fall_back() {
        assert_eq!(normalize_color("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_color("102030").as_deref(), Some("#102030"));
        assert_eq!(normalize_color("red"), None);
        assert_eq!(normalize_color("#12345"), None);
        assert_eq!(color_rgb("#ff8000"), [255, 128, 0]);
        let p = Palette { background: "nope\"><script>".into(), ..Palette::default() }.sanitized();
        assert_eq!(p.background, Palette::default().background);
    }

    #[test]
    fn hostile_numbers_are_clamped() {
        let s =
            GallerySettings::from_json(&json!({"appearance": {"columns": 0, "thumbSize": 99999}, "output": {"quality": 0, "largeSize": 1}})).unwrap();
        assert_eq!(s.appearance.columns, 2);
        assert_eq!(s.appearance.thumb_size, 400);
        assert_eq!(s.output.quality, 1);
        assert_eq!(s.output.large_size, 300);
        assert!(GallerySettings::from_json(&json!({"appearance": {"columns": -3}})).is_err());
    }

    #[test]
    fn merge_patches_nested_fields() {
        let s = GallerySettings::default().merged(&json!({"site": {"title": "Trip"}, "template": "track"})).unwrap();
        assert_eq!(s.site.title, "Trip");
        assert_eq!(s.template, Template::Track);
        assert_eq!(s.palette, Palette::default());
        let back: GallerySettings = serde_json::from_value(serde_json::to_value(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
