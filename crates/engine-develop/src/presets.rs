//! Built-in presets and profiles (all values are our own; no third-party preset content), and
//! preset files: our `.lcpreset` JSON (import + export, groups preserved) and XMP presets
//! (`crs:` fields, read only — see [`crate::crs`]).

use std::path::Path;

use dac_develop::Preset;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// File extension of the app's preset files (legacy extensions are read too, see [`dac_brand::preset_exts`]).
pub const LCPRESET_EXT: &str = dac_brand::PRESET_EXT;
/// The `format` tag of a `.lcpreset` file.
pub const LCPRESET_FORMAT: &str = crate::legacy::PRESET_FORMAT;

/// A `.lcpreset` file: one or more presets with their groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetFile {
    pub format: String,
    pub version: u32,
    pub presets: Vec<Preset>,
}

/// Lower-case ASCII slug for ids (`"Warm & Soft"` → `warm-soft`).
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "preset".into() } else { out }
}

/// Serialise presets as a `.lcpreset` file (favourite/built-in flags are not exported).
pub fn to_lcpreset(presets: &[Preset]) -> String {
    let presets = presets.iter().map(|p| Preset { favorite: false, builtin: false, ..p.clone() }).collect();
    serde_json::to_string_pretty(&PresetFile { format: LCPRESET_FORMAT.into(), version: 1, presets }).unwrap_or_default()
}

/// Read a preset file by name: `.lcpreset` (a [`PresetFile`], a bare preset object or an array of
/// presets) or `.xmp` (a `crs:` preset).
pub fn parse_preset_file(name: &str, bytes: &[u8]) -> Result<Vec<Preset>, String> {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Preset".into());
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    if ext == "xmp" || (!dac_brand::preset_exts().any(|e| e == ext) && text.trim_start().starts_with('<')) {
        return crate::crs::preset_from_xmp(text, &stem).map(|p| vec![p]).ok_or_else(|| "no develop settings in this XMP file".into());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not a preset file: {e}"))?;
    let list = match &v {
        Value::Object(o) if o.contains_key("presets") => {
            let f: PresetFile = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
            if f.format != LCPRESET_FORMAT {
                return Err(format!("unknown preset format `{}`", f.format));
            }
            f.presets
        }
        Value::Array(_) => serde_json::from_value(v).map_err(|e| e.to_string())?,
        _ => vec![serde_json::from_value(v).map_err(|e| e.to_string())?],
    };
    let mut out = Vec::new();
    for mut p in list {
        if !p.settings.is_object() {
            return Err(format!("preset `{}` has no settings object", p.name));
        }
        p.builtin = false;
        p.favorite = false;
        if p.name.trim().is_empty() {
            p.name = stem.clone();
        }
        if p.group.trim().is_empty() {
            p.group = "Imported Presets".into();
        }
        out.push(p);
    }
    Ok(out)
}

/// Expand files/folders (recursively, bounded: see [`crate::walk`]) into preset files
/// (`.lcpreset`, `.xmp`).
pub fn expand_preset_paths(paths: &[String]) -> Vec<String> {
    let preset_file = |p: &Path| {
        let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        dac_brand::preset_exts().any(|e| e == ext) || ["xmp", "lrtemplate", "zip", "lmp", "mplumpack"].contains(&ext.as_str())
    };
    let mut out = Vec::new();
    for p in paths {
        let path = Path::new(p);
        if path.is_dir() {
            let w = crate::walk::files_in(path, None, crate::walk::Limits::default(), preset_file);
            out.extend(w.files.into_iter().map(|f| f.to_string_lossy().to_string()));
        } else {
            out.push(p.clone());
        }
    }
    out
}

fn p(group: &str, id: &str, name: &str, settings: serde_json::Value) -> Preset {
    Preset { id: format!("lc.{id}"), name: name.into(), group: group.into(), settings, favorite: false, builtin: true }
}

pub fn builtin() -> Vec<Preset> {
    vec![
        p(
            "Color",
            "warm-glow",
            "Warm Glow",
            json!({"wb": {"mode": "custom", "temp": 7200.0, "tint": 8.0}, "light": {"contrast": 8.0, "shadows": 12.0}, "color": {"vibrance": 18.0}}),
        ),
        p(
            "Color",
            "cool-morning",
            "Cool Morning",
            json!({"wb": {"mode": "custom", "temp": 5600.0, "tint": -4.0}, "light": {"highlights": -20.0}, "color": {"vibrance": 10.0}, "grading": {"shadows": {"hue": 215.0, "sat": 12.0, "lum": 0.0}}}),
        ),
        p(
            "Color",
            "teal-orange",
            "Teal & Orange",
            json!({"grading": {"shadows": {"hue": 195.0, "sat": 28.0, "lum": 0.0}, "highlights": {"hue": 35.0, "sat": 22.0, "lum": 0.0}, "balance": 10.0}, "color": {"vibrance": 12.0}, "light": {"contrast": 14.0}}),
        ),
        p(
            "Color",
            "vivid-pop",
            "Vivid Pop",
            json!({"color": {"vibrance": 40.0, "saturation": 8.0}, "light": {"contrast": 18.0, "whites": 10.0, "blacks": -10.0}, "effects": {"clarity": 12.0}}),
        ),
        p(
            "Color",
            "soft-pastel",
            "Soft Pastel",
            json!({"light": {"contrast": -25.0, "highlights": -30.0, "shadows": 35.0, "blacks": 20.0}, "color": {"saturation": -18.0, "vibrance": 10.0}, "curve": {"shadows": 25.0}}),
        ),
        p(
            "Film",
            "faded-matte",
            "Faded Matte",
            json!({"curve": {"shadows": 40.0, "highlights": -15.0}, "light": {"contrast": -10.0}, "color": {"saturation": -12.0}, "grain": {"amount": 18.0, "size": 25.0, "roughness": 50.0}}),
        ),
        p(
            "Film",
            "warm-film",
            "Warm Film",
            json!({"wb": {"mode": "custom", "temp": 6900.0, "tint": 5.0}, "curve": {"shadows": 22.0}, "grading": {"highlights": {"hue": 45.0, "sat": 15.0, "lum": 0.0}}, "grain": {"amount": 22.0, "size": 30.0, "roughness": 55.0}, "vignette": {"amount": -12.0}}),
        ),
        p(
            "Film",
            "muted-film",
            "Muted Film",
            json!({"color": {"saturation": -25.0, "vibrance": 5.0}, "light": {"contrast": 10.0}, "curve": {"shadows": 18.0}, "grain": {"amount": 15.0}}),
        ),
        p(
            "B&W",
            "bw-high-contrast",
            "High Contrast B&W",
            json!({"treatment": "bw", "light": {"contrast": 45.0, "whites": 20.0, "blacks": -25.0}, "effects": {"clarity": 20.0}, "bw_mix": {"blue": -30.0, "red": 15.0}}),
        ),
        p("B&W", "bw-soft", "Soft B&W", json!({"treatment": "bw", "light": {"contrast": -15.0, "shadows": 25.0}, "curve": {"shadows": 15.0}})),
        p(
            "B&W",
            "bw-selenium",
            "Selenium Tone",
            json!({"treatment": "bw", "light": {"contrast": 20.0}, "grading": {"shadows": {"hue": 285.0, "sat": 18.0, "lum": 0.0}, "highlights": {"hue": 40.0, "sat": 10.0, "lum": 0.0}}}),
        ),
        p(
            "Landscape",
            "crisp-landscape",
            "Crisp Landscape",
            json!({"light": {"highlights": -45.0, "shadows": 30.0, "contrast": 12.0}, "effects": {"clarity": 22.0, "dehaze": 12.0, "texture": 15.0}, "color": {"vibrance": 25.0}}),
        ),
        p(
            "Landscape",
            "golden-hour",
            "Golden Hour",
            json!({"wb": {"mode": "custom", "temp": 7600.0, "tint": 12.0}, "light": {"highlights": -35.0, "shadows": 20.0}, "grading": {"highlights": {"hue": 38.0, "sat": 25.0, "lum": 5.0}}, "vignette": {"amount": -15.0}}),
        ),
        p(
            "Landscape",
            "blue-hour",
            "Blue Hour Boost",
            json!({"wb": {"mode": "custom", "temp": 5200.0, "tint": 6.0}, "light": {"shadows": 25.0, "exposure": 0.2}, "mixer": {"blue": {"hue": 0.0, "sat": 25.0, "lum": 0.0}, "purple": {"hue": 0.0, "sat": 15.0, "lum": 0.0}}}),
        ),
        p(
            "Portrait",
            "soft-skin",
            "Soft Skin",
            json!({"effects": {"texture": -25.0, "clarity": -10.0}, "light": {"contrast": -5.0, "shadows": 15.0}, "mixer": {"orange": {"hue": 0.0, "sat": -8.0, "lum": 10.0}}}),
        ),
        p(
            "Portrait",
            "bright-airy",
            "Bright & Airy",
            json!({"light": {"exposure": 0.45, "contrast": -15.0, "highlights": -40.0, "shadows": 40.0, "whites": 15.0}, "color": {"vibrance": 8.0, "saturation": -8.0}}),
        ),
        p(
            "Style",
            "moody",
            "Moody",
            json!({"light": {"exposure": -0.3, "contrast": 20.0, "highlights": -30.0, "blacks": -15.0}, "color": {"saturation": -20.0}, "grading": {"shadows": {"hue": 200.0, "sat": 15.0, "lum": -5.0}}, "vignette": {"amount": -30.0}}),
        ),
        p(
            "Style",
            "cinematic",
            "Cinematic",
            json!({"curve": {"shadows": 20.0, "highlights": -10.0}, "grading": {"shadows": {"hue": 190.0, "sat": 25.0, "lum": 0.0}, "highlights": {"hue": 30.0, "sat": 18.0, "lum": 0.0}}, "light": {"contrast": 15.0}, "vignette": {"amount": -20.0}}),
        ),
        // ---- Portrait
        p(
            "Portrait",
            "moody-portrait",
            "Moody Portrait",
            json!({"light": {"exposure": -0.25, "contrast": 20.0, "highlights": -30.0, "blacks": -12.0}, "color": {"saturation": -15.0}, "vignette": {"amount": -25.0, "midpoint": 40.0}, "mixer": {"orange": {"sat": 6.0}}}),
        ),
        p(
            "Portrait",
            "golden-glow",
            "Golden Glow",
            json!({"grading": {"highlights": {"hue": 40.0, "sat": 18.0, "lum": 0.0}, "midtones": {"hue": 35.0, "sat": 8.0, "lum": 0.0}}, "light": {"shadows": 15.0}, "effects": {"clarity": -8.0}}),
        ),
        p(
            "Portrait",
            "clean-studio",
            "Clean Studio",
            json!({"light": {"contrast": 10.0, "whites": 15.0, "blacks": -8.0}, "effects": {"texture": -10.0}, "color": {"vibrance": 5.0}}),
        ),
        // ---- Landscape
        p(
            "Landscape",
            "crisp-vista",
            "Crisp Vista",
            json!({"effects": {"dehaze": 15.0, "clarity": 18.0, "texture": 12.0}, "light": {"contrast": 12.0, "highlights": -25.0, "shadows": 18.0}, "color": {"vibrance": 20.0}}),
        ),
        p(
            "Landscape",
            "deep-sky",
            "Deep Sky",
            json!({"mixer": {"blue": {"lum": -25.0, "sat": 15.0}, "aqua": {"lum": -10.0}}, "light": {"highlights": -30.0}, "effects": {"dehaze": 10.0}}),
        ),
        p(
            "Landscape",
            "lush-greens",
            "Lush Greens",
            json!({"mixer": {"green": {"hue": 12.0, "sat": 18.0, "lum": 8.0}, "yellow": {"hue": 10.0, "sat": 10.0}}, "color": {"vibrance": 12.0}, "effects": {"clarity": 8.0}}),
        ),
        p(
            "Landscape",
            "desert-warmth",
            "Desert Warmth",
            json!({"mixer": {"orange": {"sat": 15.0, "lum": 5.0}, "yellow": {"hue": -8.0, "sat": 10.0}, "blue": {"sat": -10.0}}, "grading": {"highlights": {"hue": 38.0, "sat": 12.0, "lum": 0.0}}, "light": {"contrast": 10.0}}),
        ),
        p(
            "Landscape",
            "misty-morning",
            "Misty Morning",
            json!({"effects": {"dehaze": -18.0, "clarity": -12.0}, "light": {"contrast": -18.0, "highlights": -10.0}, "grading": {"shadows": {"hue": 210.0, "sat": 10.0, "lum": 0.0}}, "color": {"saturation": -10.0}}),
        ),
        // ---- Urban
        p(
            "Urban",
            "gritty-street",
            "Gritty Street",
            json!({"effects": {"clarity": 35.0, "texture": 20.0}, "light": {"contrast": 25.0, "blacks": -15.0}, "color": {"saturation": -30.0}, "vignette": {"amount": -18.0}}),
        ),
        p(
            "Urban",
            "neon-night",
            "Neon Night",
            json!({"mixer": {"magenta": {"sat": 20.0}, "purple": {"sat": 18.0}, "blue": {"sat": 12.0}, "aqua": {"sat": 15.0}}, "light": {"contrast": 18.0, "blacks": -12.0}, "color": {"vibrance": 15.0}, "grading": {"shadows": {"hue": 250.0, "sat": 15.0, "lum": 0.0}}}),
        ),
        p(
            "Urban",
            "concrete-cool",
            "Concrete Cool",
            json!({"grading": {"shadows": {"hue": 205.0, "sat": 14.0, "lum": 0.0}, "highlights": {"hue": 200.0, "sat": 6.0, "lum": 0.0}}, "color": {"saturation": -20.0}, "light": {"contrast": 12.0}, "effects": {"clarity": 15.0}}),
        ),
        p(
            "Urban",
            "faded-urban",
            "Faded Urban",
            json!({"curve": {"shadows": 30.0}, "color": {"saturation": -22.0}, "light": {"contrast": -8.0}, "grain": {"amount": 12.0, "size": 25.0, "roughness": 45.0}}),
        ),
        // ---- Food
        p(
            "Food",
            "fresh-bright",
            "Fresh & Bright",
            json!({"light": {"exposure": 0.25, "shadows": 20.0, "whites": 10.0}, "color": {"vibrance": 22.0}, "effects": {"texture": 15.0}}),
        ),
        p(
            "Food",
            "warm-table",
            "Warm Table",
            json!({"grading": {"midtones": {"hue": 32.0, "sat": 10.0, "lum": 0.0}}, "mixer": {"orange": {"sat": 10.0}, "red": {"sat": 8.0}}, "light": {"contrast": 8.0}}),
        ),
        p(
            "Food",
            "dark-moody-food",
            "Dark & Moody",
            json!({"light": {"exposure": -0.3, "contrast": 22.0, "highlights": -20.0, "blacks": -15.0}, "effects": {"texture": 18.0}, "vignette": {"amount": -22.0}}),
        ),
        // ---- Seasons
        p(
            "Seasons",
            "autumn-gold",
            "Autumn Gold",
            json!({"mixer": {"green": {"hue": -30.0, "sat": -10.0}, "yellow": {"hue": -15.0, "sat": 15.0}, "orange": {"sat": 18.0}}, "grading": {"highlights": {"hue": 40.0, "sat": 10.0, "lum": 0.0}}}),
        ),
        p(
            "Seasons",
            "winter-blue",
            "Winter Blue",
            json!({"grading": {"shadows": {"hue": 215.0, "sat": 18.0, "lum": 0.0}, "highlights": {"hue": 205.0, "sat": 8.0, "lum": 0.0}}, "color": {"saturation": -12.0}, "light": {"whites": 12.0}}),
        ),
        p(
            "Seasons",
            "spring-fresh",
            "Spring Fresh",
            json!({"mixer": {"green": {"hue": 10.0, "sat": 12.0, "lum": 10.0}, "magenta": {"sat": 10.0}}, "light": {"shadows": 15.0}, "color": {"vibrance": 15.0}}),
        ),
        p(
            "Seasons",
            "summer-haze",
            "Summer Haze",
            json!({"effects": {"dehaze": -12.0}, "curve": {"shadows": 18.0}, "grading": {"highlights": {"hue": 45.0, "sat": 14.0, "lum": 0.0}}, "light": {"contrast": -10.0}}),
        ),
        // ---- Vintage
        p(
            "Vintage",
            "instant-70s",
            "Instant ’70s",
            json!({"curve": {"shadows": 28.0, "highlights": -18.0}, "grading": {"shadows": {"hue": 170.0, "sat": 14.0, "lum": 0.0}, "highlights": {"hue": 45.0, "sat": 18.0, "lum": 0.0}}, "color": {"saturation": -10.0}, "vignette": {"amount": -15.0}, "grain": {"amount": 20.0, "size": 30.0, "roughness": 55.0}}),
        ),
        p(
            "Vintage",
            "cross-process",
            "Cross Process",
            json!({"grading": {"shadows": {"hue": 230.0, "sat": 25.0, "lum": 0.0}, "highlights": {"hue": 60.0, "sat": 25.0, "lum": 0.0}}, "light": {"contrast": 22.0}, "color": {"saturation": 10.0}}),
        ),
        p(
            "Vintage",
            "bleach-bypass",
            "Bleach Bypass",
            json!({"color": {"saturation": -40.0}, "light": {"contrast": 35.0, "highlights": -15.0}, "effects": {"clarity": 15.0}}),
        ),
        // ---- B&W toners
        p(
            "B&W",
            "bw-sepia",
            "Sepia Tone",
            json!({"treatment": "bw", "grading": {"shadows": {"hue": 32.0, "sat": 22.0, "lum": 0.0}, "highlights": {"hue": 42.0, "sat": 18.0, "lum": 0.0}}, "curve": {"shadows": 10.0}}),
        ),
    ]
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
}

/// Our base profiles (rendering looks). Implemented in the pipeline.
pub const PROFILES: &[ProfileInfo] = &[
    ProfileInfo { id: "lc.color", name: "Color", group: "Basic" },
    ProfileInfo { id: "lc.neutral", name: "Neutral", group: "Basic" },
    ProfileInfo { id: "lc.vivid", name: "Vivid", group: "Basic" },
    ProfileInfo { id: "lc.landscape", name: "Landscape", group: "Basic" },
    ProfileInfo { id: "lc.portrait", name: "Portrait", group: "Basic" },
    ProfileInfo { id: "lc.mono", name: "Monochrome", group: "Basic" },
    ProfileInfo { id: "lc.film.warm-print", name: "Warm Print", group: "Film" },
    ProfileInfo { id: "lc.film.cool-fade", name: "Cool Fade", group: "Film" },
    ProfileInfo { id: "lc.film.golden-hour", name: "Golden Hour", group: "Film" },
    ProfileInfo { id: "lc.film.faded-slide", name: "Faded Slide", group: "Film" },
    ProfileInfo { id: "lc.cine.teal-amber", name: "Teal & Amber", group: "Cinematic" },
    ProfileInfo { id: "lc.cine.night-blue", name: "Night Blue", group: "Cinematic" },
    ProfileInfo { id: "lc.cine.desert-heat", name: "Desert Heat", group: "Cinematic" },
    ProfileInfo { id: "lc.cine.neon-dusk", name: "Neon Dusk", group: "Cinematic" },
    ProfileInfo { id: "lc.muted.matte-soft", name: "Matte Soft", group: "Muted" },
    ProfileInfo { id: "lc.muted.bleached", name: "Bleached", group: "Muted" },
    ProfileInfo { id: "lc.muted.pastel-haze", name: "Pastel Haze", group: "Muted" },
    ProfileInfo { id: "lc.muted.quiet-green", name: "Quiet Green", group: "Muted" },
    ProfileInfo { id: "lc.bw.mono-rich", name: "Mono Rich", group: "B&W" },
    ProfileInfo { id: "lc.bw.red-filter", name: "Mono Red Filter", group: "B&W" },
    ProfileInfo { id: "lc.bw.soft", name: "Mono Soft", group: "B&W" },
    ProfileInfo { id: "lc.bw.sepia", name: "Sepia Tone", group: "B&W" },
];

/// Profile groups in menu/browser order.
pub fn profile_groups() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for p in PROFILES {
        if !out.contains(&p.group) {
            out.push(p.group);
        }
    }
    out
}

/// A profile by id.
pub fn profile(id: &str) -> Option<&'static ProfileInfo> {
    PROFILES.iter().find(|p| p.id == id)
}

/// How many recently used profiles the profile menu lists.
pub const RECENT_PROFILES: usize = 5;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_presets_parse_and_apply() {
        let base = dac_develop::DevelopSettings::default();
        let mut ids = std::collections::HashSet::new();
        for pr in builtin() {
            assert!(ids.insert(pr.id.clone()));
            let out = pr.apply(&base, 1.0);
            assert_ne!(out, base, "{} had no effect", pr.id);
        }
    }

    #[test]
    fn every_profile_has_a_look_and_ids_are_unique() {
        let looks = dac_pipeline::profiles::LOOK_IDS;
        let mut ids = std::collections::HashSet::new();
        for p in PROFILES {
            assert!(ids.insert(p.id), "{}", p.id);
            assert!(p.id == "lc.color" || looks.contains(&p.id), "{} has no look", p.id);
        }
        assert!(PROFILES.len() >= 22);
        assert_eq!(profile_groups(), ["Basic", "Film", "Cinematic", "Muted", "B&W"]);
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Warm & Soft!"), "warm-soft");
        assert_eq!(slug("  "), "preset");
        assert_eq!(slug("B&W/Film 2"), "b-w-film-2");
    }
}
