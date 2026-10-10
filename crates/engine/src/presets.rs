//! Built-in presets and profiles (all values are our own; no third-party preset content), and
//! preset files: our `.lcpreset` JSON (import + export, groups preserved) and XMP presets
//! (`crs:` fields, read only — see [`crate::crs`]).

use std::path::Path;

use lightcraft_develop::{DevelopSettings, Preset};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Session;

/// File extension of LightCraft preset files.
pub const LCPRESET_EXT: &str = "lcpreset";
/// The `format` tag of a `.lcpreset` file.
pub const LCPRESET_FORMAT: &str = "lightcraft.preset";
/// Function curves need a distinct tag: older readers ignored the wrapper's version number.
pub const LCPRESET_FUNCTION_FORMAT: &str = "lightcraft.preset.v2";

/// A `.lcpreset` file: one or more presets with their groups.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetFile {
    pub format: String,
    pub version: u32,
    pub presets: Vec<Preset>,
}

/// The session's presets (built-in, then the user's) with a generation that changes on every
/// mutable access, so saving and the UI's caches skip their O(presets) work while the list is
/// untouched: a Lightroom collection imported whole is 10 000+ presets and ~40 MB of JSON.
///
/// Reads go through `Deref` (`iter`, `len`, `find`…) and keep the generation; `push`, `iter_mut`,
/// `remove`… go through `DerefMut` and bump it, whether or not anything is then changed.
/// Generations are unique across all lists in the process.
#[derive(Clone, Debug)]
pub struct PresetList {
    items: Vec<Preset>,
    generation: u64,
}

static PRESET_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_generation() -> u64 {
    PRESET_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl PresetList {
    pub fn new(items: Vec<Preset>) -> PresetList {
        PresetList { items, generation: next_generation() }
    }

    /// Changes whenever the list may have changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Default for PresetList {
    fn default() -> PresetList {
        PresetList::new(Vec::new())
    }
}

impl std::ops::Deref for PresetList {
    type Target = Vec<Preset>;
    fn deref(&self) -> &Vec<Preset> {
        &self.items
    }
}

impl std::ops::DerefMut for PresetList {
    fn deref_mut(&mut self) -> &mut Vec<Preset> {
        self.generation = next_generation();
        &mut self.items
    }
}

impl<'a> IntoIterator for &'a PresetList {
    type Item = &'a Preset;
    type IntoIter = std::slice::Iter<'a, Preset>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl<'a> IntoIterator for &'a mut PresetList {
    type Item = &'a mut Preset;
    type IntoIter = std::slice::IterMut<'a, Preset>;
    fn into_iter(self) -> Self::IntoIter {
        self.generation = next_generation();
        self.items.iter_mut()
    }
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
    let (format, version) =
        if presets.iter().any(|p| has_curve_object(&p.settings) || p.settings.get("version").and_then(Value::as_u64).is_some_and(|v| v > 1)) {
            (LCPRESET_FUNCTION_FORMAT, 2)
        } else {
            (LCPRESET_FORMAT, 1)
        };
    let presets = presets.iter().map(|p| Preset { favorite: false, builtin: false, ..p.clone() }).collect();
    serde_json::to_string_pretty(&PresetFile { format: format.into(), version, presets }).unwrap_or_default()
}

fn has_curve_object(settings: &Value) -> bool {
    settings
        .get("curve")
        .is_some_and(|curve| ["master", "red", "green", "blue"].iter().any(|channel| curve.get(*channel).is_some_and(Value::is_object)))
}

// Check the original JSON in addition to the retained partial Value. Value would discard
// duplicate settings keys, including a newer schema version hidden by a later supported one.
#[derive(Deserialize)]
struct CheckedPresetSettings {
    #[serde(rename = "settings")]
    _settings: DevelopSettings,
}

#[derive(Deserialize)]
struct CheckedPresetFileSettings {
    #[serde(rename = "presets")]
    _presets: Vec<CheckedPresetSettings>,
}

/// Read a preset file by name: `.lcpreset` (a [`PresetFile`], a bare preset object or an array of
/// presets) or `.xmp` (a `crs:` preset).
pub fn parse_preset_file(name: &str, bytes: &[u8]) -> Result<Vec<Preset>, String> {
    let stem = Path::new(name).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Preset".into());
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    if ext == "xmp" || (ext != LCPRESET_EXT && text.trim_start().starts_with('<')) {
        return crate::crs::preset_from_xmp(text, &stem).map(|p| vec![p]).ok_or_else(|| "no develop settings in this XMP file".into());
    }
    let v: Value = serde_json::from_str(text).map_err(|e| format!("not a preset file: {e}"))?;
    let list = match &v {
        Value::Object(o) if o.contains_key("presets") => {
            let f: PresetFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
            match (f.format.as_str(), f.version) {
                (LCPRESET_FORMAT, 1) | (LCPRESET_FUNCTION_FORMAT, 2) => {}
                (LCPRESET_FORMAT | LCPRESET_FUNCTION_FORMAT, _) => {
                    return Err(format!("unsupported preset version {} for `{}`", f.version, f.format));
                }
                _ => return Err(format!("unknown preset format `{}`", f.format)),
            }
            if f.format == LCPRESET_FORMAT && f.presets.iter().any(|p| has_curve_object(&p.settings)) {
                return Err("function point curves require lightcraft.preset.v2 version 2".into());
            }
            serde_json::from_str::<CheckedPresetFileSettings>(text).map_err(|e| format!("invalid preset settings: {e}"))?;
            f.presets
        }
        Value::Array(_) => {
            serde_json::from_str::<Vec<CheckedPresetSettings>>(text).map_err(|e| format!("invalid preset settings: {e}"))?;
            serde_json::from_str(text).map_err(|e| e.to_string())?
        }
        _ => {
            serde_json::from_str::<CheckedPresetSettings>(text).map_err(|e| format!("invalid preset settings: {e}"))?;
            vec![serde_json::from_str(text).map_err(|e| e.to_string())?]
        }
    };
    let mut out = Vec::new();
    for mut p in list {
        if !p.settings.is_object() {
            return Err(format!("preset `{}` has no settings object", p.name));
        }
        lightcraft_develop::try_apply_partial(&DevelopSettings::default(), &p.settings, 1.0)
            .map_err(|e| format!("preset `{}` has invalid settings: {e}", p.name))?;
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
        ext == LCPRESET_EXT || ["xmp", "lrtemplate", "zip", "lmp", "mplumpack", crate::cmd::mask_effect_files::EFFECT_EXT].contains(&ext.as_str())
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

impl Session {
    /// Add imported presets. Identical presets (same name, group, settings and diagnostics) already present are
    /// skipped; id clashes get a fresh id. Returns the ids added.
    pub fn add_presets(&mut self, presets: Vec<Preset>) -> Vec<String> {
        use std::collections::{HashMap, HashSet};
        // indexed: a collection of 10 000+ presets was quadratic (minutes) to import
        let mut ids: HashSet<String> = self.presets.iter().map(|q| q.id.clone()).collect();
        let mut by_name: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (i, q) in self.presets.iter().enumerate() {
            by_name.entry((q.name.clone(), q.group.clone())).or_default().push(i);
        }
        let mut added = Vec::new();
        for mut p in presets {
            p.unsupported_edits = lightcraft_develop::unsupported_edits::bounded_names(p.unsupported_edits.iter().map(String::as_str));
            let key = (p.name.clone(), p.group.clone());
            let same = by_name.get(&key).is_some_and(|ix| ix.iter().any(|&i| self.presets.get(i).is_some_and(|q| q.settings == p.settings && q.unsupported_edits == p.unsupported_edits)));
            if same {
                continue;
            }
            if p.id.trim().is_empty() || p.id.starts_with("lc.") || ids.contains(&p.id) {
                let base = format!("user.{}", slug(&format!("{}-{}", p.group, p.name)));
                let mut id = base.clone();
                let mut n = 2;
                while ids.contains(&id) {
                    id = format!("{base}-{n}");
                    n += 1;
                }
                p.id = id;
            }
            p.builtin = false;
            ids.insert(p.id.clone());
            by_name.entry(key).or_default().push(self.presets.len());
            added.push(p.id.clone());
            self.presets.push(p);
        }
        added
    }
}

fn p(group: &str, id: &str, name: &str, settings: serde_json::Value) -> Preset {
    Preset { id: format!("lc.{id}"), name: name.into(), group: group.into(), settings, unsupported_edits: Vec::new(), favorite: false, builtin: true }
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

impl Session {
    /// A profile by id, including the active photo's embedded profile definition.
    pub fn profile_info(&self, id: &str) -> Option<(&str, &str)> {
        if let Some(p) = profile(id) {
            return Some((p.name, p.group));
        }
        if id == lightcraft_develop::IMPORTED_LOOK_ID {
            let p = self.catalog.photo(self.active()?)?;
            return p.develop.profile.imported_look().map(|look| (look.name.as_str(), "Imported"));
        }
        self.lut_profiles.iter().find(|p| p.id == id).map(|p| (p.name.as_str(), p.group.as_str()))
    }

    /// Remember `id` as the most recently applied profile.
    pub fn note_profile_used(&mut self, id: &str) {
        // This id identifies a definition on one photo, not an asset in the shared library.
        if id == lightcraft_develop::IMPORTED_LOOK_ID {
            return;
        }
        self.profile_recent.retain(|p| p != id);
        self.profile_recent.insert(0, id.to_string());
        self.profile_recent.truncate(RECENT_PROFILES);
    }

    /// The profile menu: favourites, recent, then every group (`profiles.menu`).
    pub fn profile_menu(&self) -> Value {
        let item =
            |p: &ProfileInfo| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.iter().any(|f| f == p.id)});
        let lut_item = |id: &str| {
            self.lut_profiles
                .iter()
                .find(|p| p.id == id)
                .map(|p| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.contains(&p.id), "imported": true}))
        };
        let list = |ids: &[String]| ids.iter().filter_map(|id| profile(id).map(item).or_else(|| lut_item(id))).collect::<Vec<_>>();
        let mut groups: Vec<Value> = profile_groups()
            .into_iter()
            .map(|g| json!({"name": g, "profiles": PROFILES.iter().filter(|p| p.group == g).map(item).collect::<Vec<_>>()}))
            .collect();
        if let Some((name, group)) = self.profile_info(lightcraft_develop::IMPORTED_LOOK_ID) {
            groups.insert(0, json!({"name": group, "profiles": [{"id": lightcraft_develop::IMPORTED_LOOK_ID, "name": name, "group": group, "favorite": false, "photoSpecific": true}]}));
        }
        // imported LUT profiles, by their groups
        let mut lut_groups: Vec<&str> = self.lut_profiles.iter().map(|p| p.group.as_str()).collect();
        lut_groups.sort_unstable();
        lut_groups.dedup();
        for g in lut_groups {
            let profiles: Vec<Value> = self
                .lut_profiles
                .iter()
                .filter(|p| p.group == g)
                .map(|p| json!({"id": p.id, "name": p.name, "group": p.group, "favorite": self.profile_favorites.contains(&p.id), "imported": true}))
                .collect();
            groups.push(json!({"name": g, "profiles": profiles, "imported": true}));
        }
        json!({
            "favorites": list(&self.profile_favorites),
            "recent": list(&self.profile_recent),
            "groups": groups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imported_preset_identity_includes_bounded_diagnostics() {
        let mut s = Session::new();
        let mut preset = p("Own", "warnings", "Same look", json!({"light":{"exposure":0.5}}));
        preset.unsupported_edits = vec!["Look".into(), "FilterList".into(), "Look".into()];
        assert_eq!(s.add_presets(vec![preset.clone()]).len(), 1);
        assert!(s.add_presets(vec![preset.clone()]).is_empty());
        let saved = s.presets.iter().find(|p| p.name == "Same look").unwrap();
        assert_eq!(saved.unsupported_edits, ["FilterList", "Look"]);
        preset.unsupported_edits.clear();
        assert_eq!(s.add_presets(vec![preset.clone()]).len(), 1, "same pixels with different source limitations is a distinct preset");
        preset.name = "Bounded".into();
        preset.unsupported_edits = (0..300).map(|i| format!("Field{i}")).collect();
        preset.unsupported_edits.push("Bad\nName".into());
        s.add_presets(vec![preset]);
        let saved = s.presets.iter().find(|p| p.name == "Bounded").unwrap();
        assert_eq!(saved.unsupported_edits.len(), lightcraft_develop::unsupported_edits::MAX_NAMES);
        assert!(saved.unsupported_edits.iter().all(|n| lightcraft_develop::unsupported_edits::name_valid(n)));
        assert!(saved.unsupported_edits.iter().any(|n| n == lightcraft_develop::unsupported_edits::INVALID_DIAGNOSTICS));
    }

    #[test]
    fn native_preset_file_and_import_report_preserve_warning_names() {
        let mut preset = p("Own", "warnings", "Warnings", json!({"light":{"exposure":0.5}}));
        preset.unsupported_edits = vec!["FilterList".into(), "Look".into()];
        let file = to_lcpreset(std::slice::from_ref(&preset));
        let parsed = crate::preset_import::read_presets("own.lcpreset", file.as_bytes(), None).unwrap();
        assert_eq!(parsed[0].preset.unsupported_edits, preset.unsupported_edits);
        assert_eq!(parsed[0].unmapped, preset.unsupported_edits);
    }

    #[test]
    fn builtin_presets_parse_and_apply() {
        let base = lightcraft_develop::DevelopSettings::default();
        let mut ids = std::collections::HashSet::new();
        for pr in builtin() {
            assert!(ids.insert(pr.id.clone()));
            let out = pr.apply(&base, 1.0);
            assert_ne!(out, base, "{} had no effect", pr.id);
        }
    }

    #[test]
    fn every_profile_has_a_look_and_ids_are_unique() {
        let looks = crate::pipeline::profiles::LOOK_IDS;
        let mut ids = std::collections::HashSet::new();
        for p in PROFILES {
            assert!(ids.insert(p.id), "{}", p.id);
            assert!(p.id == "lc.color" || looks.contains(&p.id), "{} has no look", p.id);
        }
        assert!(PROFILES.len() >= 22);
        assert_eq!(profile_groups(), ["Basic", "Film", "Cinematic", "Muted", "B&W"]);
    }

    #[test]
    fn creative_profiles_render_without_moving_the_sliders() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.4})).unwrap();
        let before = s.develop_of(id).unwrap();
        let plain = s.render_now(id, 96, 96).unwrap().image;
        s.execute("develop.profile", &json!({"id": "lc.cine.teal-amber", "amount": 150})).unwrap();
        let after = s.develop_of(id).unwrap();
        assert_eq!((after.profile.id.as_str(), after.profile.amount), ("lc.cine.teal-amber", 150.0));
        let mut same = (*after).clone();
        same.profile = before.profile.clone();
        assert_eq!(same, *before, "only the profile changed");
        let looked = s.render_now(id, 96, 96).unwrap().image;
        assert_ne!(plain.as_bytes(), looked.as_bytes());

        let imported = lightcraft_develop::try_apply_partial(
            &after,
            &json!({"profile": {
                "id": lightcraft_develop::IMPORTED_LOOK_ID, "amount": 120,
                "imported": {"name": "Own curve", "contrast": 8, "master": [{"x":0,"y":0.1},{"x":1,"y":1}]}
            }}),
            1.0,
        )
        .unwrap();
        s.set_develop(id, imported.clone(), "Import profile").unwrap();
        assert_eq!(s.profile_info(lightcraft_develop::IMPORTED_LOOK_ID), Some(("Own curve", "Imported")));
        assert!(s.execute("profiles.list", &json!({})).unwrap().as_array().unwrap().iter().any(|p| p["name"] == "Own curve"));
        s.execute("develop.profile", &json!({"id": lightcraft_develop::IMPORTED_LOOK_ID, "amount": 50})).unwrap();
        let scaled = s.develop_of(id).unwrap();
        assert_eq!(scaled.profile.amount, 50.0);
        assert_eq!(scaled.profile.imported, imported.profile.imported);
        assert_eq!(scaled.light, imported.light);
        assert_eq!(scaled.curve, imported.curve);
        s.execute("develop.profile", &json!({"id": "lc.color"})).unwrap();
        let replaced = s.develop_of(id).unwrap();
        assert!(replaced.profile.imported.is_none());
        let delta = crate::json_delta(&scaled.to_json(), &replaced.to_json()).unwrap();
        let synced = lightcraft_develop::try_apply_partial(&scaled, &delta, 1.0).unwrap();
        assert_eq!(synced, *replaced, "sync must remove the old profile payload too");
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Warm & Soft!"), "warm-soft");
        assert_eq!(slug("  "), "preset");
        assert_eq!(slug("B&W/Film 2"), "b-w-film-2");
    }

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("lc-presets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn function_curve() -> Value {
        json!({"kind":"function","version":1,"base":{"pieces":[{"end":1.0,"hermite":[0.1,0.9,0.8,0.8]}]},"adjustments":[{"x":0.0,"y":0.0},{"x":0.4,"y":0.03},{"x":1.0,"y":0.0}]})
    }

    #[test]
    fn sparse_presets_keep_the_legacy_format_and_bare_input_shapes() {
        let preset =
            p("Own", "legacy", "Sparse", json!({"version":1,"light":{"exposure":0.7},"curve":{"red":[{"x":0.0,"y":0.1},{"x":1.0,"y":1.0}]}}));
        let file = to_lcpreset(std::slice::from_ref(&preset));
        let wrapper: Value = serde_json::from_str(&file).unwrap();
        assert_eq!((wrapper["format"].as_str(), wrapper["version"].as_u64()), (Some(LCPRESET_FORMAT), Some(1)));
        for input in [file, serde_json::to_string(&preset).unwrap(), serde_json::to_string(&vec![preset.clone()]).unwrap()] {
            let parsed = parse_preset_file("own.lcpreset", input.as_bytes()).unwrap();
            assert_eq!(parsed.len(), 1);
            assert_eq!(parsed[0].settings, preset.settings, "partial settings must stay partial");
            assert!(!parsed[0].builtin);
        }
    }

    #[test]
    fn function_presets_use_a_new_tag_and_roundtrip_without_resampling() {
        let mut preset =
            p("Own", "function", "Continuous", json!({"version":lightcraft_develop::SCHEMA_VERSION,"curve":{"master":function_curve()}}));
        preset.unsupported_edits = vec!["FilterList".into()];
        preset.favorite = true;
        let file = to_lcpreset(std::slice::from_ref(&preset));
        let wrapper: Value = serde_json::from_str(&file).unwrap();
        assert_eq!((wrapper["format"].as_str(), wrapper["version"].as_u64()), (Some(LCPRESET_FUNCTION_FORMAT), Some(2)));
        assert_ne!(wrapper["format"], LCPRESET_FORMAT, "old readers check the tag, not the version");
        for input in [file, serde_json::to_string(&preset).unwrap(), serde_json::to_string(&vec![preset.clone()]).unwrap()] {
            let parsed = parse_preset_file("own.lcpreset", input.as_bytes()).unwrap();
            assert_eq!(parsed[0].settings, preset.settings);
            assert_eq!(parsed[0].unsupported_edits, preset.unsupported_edits);
            assert!(!parsed[0].builtin && !parsed[0].favorite);
        }
        let legacy = json!({"format":LCPRESET_FORMAT,"version":1,"presets":[preset]});
        assert!(parse_preset_file("own.lcpreset", legacy.to_string().as_bytes()).is_err());
    }

    #[test]
    fn native_preset_validation_keeps_partial_process_hdr_and_legacy_markers() {
        for version in [0, 1, 2] {
            let settings = json!({"version":version,"process":77,"hdr":{"enabled":true,"max_ev":4.0},"profile":{"camera_local_tone":true},"light":{"exposure":0.3}});
            let preset = p("Own", "partial", "Partial", settings.clone());
            let saved = to_lcpreset(std::slice::from_ref(&preset));
            let wrapper: Value = serde_json::from_str(&saved).unwrap();
            assert_eq!(wrapper["version"], if version > 1 { 2 } else { 1 });
            for input in [saved, serde_json::to_string(&preset).unwrap(), serde_json::to_string(&[preset]).unwrap()] {
                let parsed = parse_preset_file("own.lcpreset", input.as_bytes()).unwrap();
                assert_eq!(parsed[0].settings, settings, "validation must not materialize omitted groups or drop upstream fields");
                let applied = parsed[0].try_apply(&DevelopSettings::default(), 1.0).unwrap();
                assert_eq!(applied.process, lightcraft_develop::ProcessVersion(77), "explicit hand-written process remains allowed");
                assert!(applied.hdr.enabled && applied.profile.camera_local_tone);
                assert_eq!(applied.hdr.max_ev, 4.0);
            }
        }
    }

    #[test]
    fn original_preset_json_rejects_duplicate_settings_and_nested_function_versions() {
        let curve = r#"{"kind":"function","version":999,"version":1,"base":{"pieces":[{"end":1.0,"hermite":[0.0,1.0,1.0,1.0]}]},"adjustments":[]}"#;
        let nested = format!(r#"{{"id":"p","name":"P","group":"Own","settings":{{"version":2,"curve":{{"master":{curve}}}}}}}"#);
        let duplicate = r#"{"id":"p","name":"P","group":"Own","settings":{"version":999},"settings":{}}"#;
        for preset in [nested.as_str(), duplicate] {
            for input in
                [preset.to_string(), format!("[{preset}]"), format!(r#"{{"format":"lightcraft.preset.v2","version":2,"presets":[{preset}]}}"#)]
            {
                assert!(parse_preset_file("own.lcpreset", input.as_bytes()).is_err());
            }
        }
    }

    #[test]
    fn unsupported_wrapper_versions_and_partial_settings_are_refused() {
        let preset = p("Own", "valid", "Valid", json!({"light":{"exposure":0.7}}));
        for (format, version) in
            [(LCPRESET_FORMAT, 0), (LCPRESET_FORMAT, 2), (LCPRESET_FORMAT, 999), (LCPRESET_FUNCTION_FORMAT, 1), (LCPRESET_FUNCTION_FORMAT, 3)]
        {
            let file = json!({"format":format,"version":version,"presets":[preset]});
            assert!(parse_preset_file("own.lcpreset", file.to_string().as_bytes()).is_err(), "{file}");
        }
        let mut future_curve = function_curve();
        future_curve["version"] = json!(2);
        let mut malformed_curve = function_curve();
        malformed_curve["base"]["pieces"][0]["end"] = json!(0.5);
        for settings in [
            json!({"version":lightcraft_develop::SCHEMA_VERSION + 1}),
            json!({"version":lightcraft_develop::SCHEMA_VERSION + 1,"light":{"exposure":0.7}}),
            json!({"light":false}),
            json!({"curve":{"master":future_curve}}),
            json!({"curve":{"blue":malformed_curve}}),
            json!({"curve":{"green":{"kind":"future","version":1}}}),
            json!([]),
        ] {
            let invalid = p("", "invalid", "Invalid", settings);
            for file in [
                serde_json::to_value(&invalid).unwrap(),
                json!([invalid]),
                json!({"format":LCPRESET_FUNCTION_FORMAT,"version":2,"presets":[invalid]}),
            ] {
                assert!(parse_preset_file("own.lcpreset", file.to_string().as_bytes()).is_err(), "{file}");
            }
        }
        let missing_group = json!({"id":"invalid","name":"Invalid","settings":{"version":lightcraft_develop::SCHEMA_VERSION+1}});
        assert!(parse_preset_file("own.lcpreset", missing_group.to_string().as_bytes()).is_err());
    }

    #[test]
    fn duplicate_native_versions_cannot_hide_unsupported_preset_data() {
        let preset = r#"{"id":"own","name":"Own","group":"Own","settings":{"version":999,"version":1}}"#;
        for text in [
            preset.to_string(),
            format!("[{preset}]"),
            format!(r#"{{"format":"lightcraft.preset","version":1,"presets":[{preset}]}}"#),
            r#"{"format":"lightcraft.preset","version":999,"version":1,"presets":[]}"#.into(),
        ] {
            assert!(parse_preset_file("own.lcpreset", text.as_bytes()).is_err(), "{text}");
        }
    }

    #[test]
    fn one_invalid_preset_rejects_its_file_before_import_mutates_the_library() {
        let d = dir("unsupported-function");
        let valid = p("Own", "valid", "Valid", json!({"light":{"exposure":0.7}}));
        let invalid = p("Own", "invalid", "Invalid", json!({"version":lightcraft_develop::SCHEMA_VERSION+1}));
        let file = d.join("future.lcpreset");
        let bytes = json!({"format":LCPRESET_FUNCTION_FORMAT,"version":2,"presets":[valid,invalid]}).to_string();
        std::fs::write(&file, &bytes).unwrap();
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let mut s = Session::with_demo();
        let before = s.presets.clone();
        let catalog = s.catalog.to_snapshot();
        let result = s.execute("preset.import", &json!({"paths":[file]})).unwrap();
        assert_eq!(result["failed"].as_array().unwrap().len(), 1, "{result}");
        assert!(result["imported"].as_array().unwrap().is_empty());
        assert_eq!(s.presets, before);
        assert_eq!(s.catalog.to_snapshot(), catalog);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), bytes);
        assert_eq!(std::fs::metadata(&file).unwrap().modified().unwrap(), modified);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn lcpreset_export_import_roundtrip_keeps_groups() {
        let d = dir("rt");
        let mut s = Session::with_demo();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.6})).unwrap();
        s.execute("preset.create", &json!({"name": "Bright", "group": "Travel"})).unwrap();
        s.execute("develop.set", &json!({"control": "effects.clarity", "value": 30})).unwrap();
        s.execute("preset.create", &json!({"name": "Crisp", "group": "Street"})).unwrap();
        let mine: Vec<Preset> = s.presets.iter().filter(|p| !p.builtin).cloned().collect();
        assert_eq!(mine.len(), 2);
        let path = d.join("mine");
        let r = s.execute("preset.export", &json!({"path": path.to_string_lossy()})).unwrap();
        assert_eq!(r["count"], 2);
        let file = d.join("mine.lcpreset");
        assert!(file.is_file());
        let one = d.join("travel.lcpreset");
        s.execute("preset.export", &json!({"path": one.to_string_lossy(), "group": "Travel"})).unwrap();
        assert!(s.execute("preset.export", &json!({"path": one.to_string_lossy(), "group": "Nope"})).is_err());

        // a fresh session imports both files from the folder; the duplicate is skipped
        let mut t = Session::new();
        let r = t.execute("preset.import", &json!({"paths": [d.to_string_lossy()]})).unwrap();
        assert_eq!(r["imported"].as_array().unwrap().len(), 2, "{r}");
        assert_eq!(r["skipped"], 1, "{r}");
        for p in &mine {
            let q = t.presets.iter().find(|q| q.name == p.name).unwrap();
            assert_eq!((&q.group, &q.settings, q.builtin), (&p.group, &p.settings, false));
        }
        // importing into the session that has them: everything is skipped, nothing duplicated
        let n = s.presets.len();
        let r = s.execute("preset.import", &json!({"paths": [file.to_string_lossy()]})).unwrap();
        assert_eq!((r["skipped"].as_u64(), s.presets.len()), (Some(2), n));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn id_clashes_get_fresh_ids_and_bad_files_are_reported() {
        let d = dir("clash");
        let builtin = builtin().remove(0);
        let mut clash = builtin.clone();
        clash.settings = json!({"light": {"exposure": 1.0}});
        std::fs::write(d.join("a.lcpreset"), serde_json::to_string(&clash).unwrap()).unwrap();
        std::fs::write(d.join("b.lcpreset"), "{not json").unwrap();
        std::fs::write(d.join("c.lcpreset"), r#"{"format": "other", "version": 1, "presets": []}"#).unwrap();
        std::fs::write(d.join("notes.txt"), "ignored inside folders").unwrap();
        let mut s = Session::new();
        let r = s.execute("preset.import", &json!({"paths": [d.to_string_lossy()]})).unwrap();
        assert_eq!(r["failed"].as_array().unwrap().len(), 2, "{r}");
        let id = r["imported"][0]["id"].as_str().unwrap();
        assert_ne!(id, builtin.id);
        assert!(id.starts_with("user."));
        assert!(s.presets.iter().find(|p| p.id == builtin.id).unwrap().builtin, "built-in untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn xmp_preset_file_imports_and_applies() {
        let d = dir("xmp");
        // Hand-written for this test (not a third-party preset).
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            crs:PresetType="Normal" crs:Exposure2012="+0.30" crs:Shadows2012="+20" crs:GrainAmount="12">
            <crs:Group><rdf:Alt><rdf:li xml:lang="x-default">Test Group</rdf:li></rdf:Alt></crs:Group>
          </rdf:Description></rdf:RDF></x:xmpmeta>"#;
        std::fs::write(d.join("Soft Lift.xmp"), x).unwrap();
        let mut s = Session::with_demo();
        let r = s.execute("preset.import", &json!({"paths": [d.join("Soft Lift.xmp").to_string_lossy()]})).unwrap();
        let id = r["imported"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(r["imported"][0]["name"], "Soft Lift", "file name when crs:Name is missing");
        assert_eq!(r["imported"][0]["group"], "Test Group");
        s.execute("preset.apply", &json!({"id": id})).unwrap();
        let dv = s.develop_of(s.active().unwrap()).unwrap();
        assert_eq!((dv.light.exposure, dv.light.shadows, dv.grain.amount), (0.3, 20.0, 12.0));
        let _ = std::fs::remove_dir_all(&d);
    }
}
