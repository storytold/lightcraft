//! The filter manifest (`pc_manifest`) and parameter schema (ported from the PhotoCraft plug-in host).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::about::About;
use crate::{Error, Result};

/// Longest manifest the host reads.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Most parameters a plug-in may declare.
pub const MAX_PARAMS: usize = 64;
/// Largest neighbourhood (`overlap`) a plug-in may ask for, in pixels.
pub const MAX_OVERLAP: u32 = 256;

/// What a plug-in is. ABI v1 has filters only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    Filter,
}

/// Which pixels a filter is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Area {
    /// The layer's non-empty pixels (like Invert or Gaussian Blur).
    #[default]
    Content,
    /// The whole canvas, including transparent pixels (like Clouds).
    Canvas,
}

/// One declared parameter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ParamSpec {
    /// A real number in `min..=max` (a slider in the dialog).
    Number { min: f64, max: f64, default: f64 },
    /// A whole number in `min..=max`.
    Int {
        #[serde(default = "int_min")]
        min: i64,
        #[serde(default = "int_max")]
        max: i64,
        #[serde(default)]
        default: i64,
    },
    /// A checkbox.
    Bool {
        #[serde(default)]
        default: bool,
    },
    /// One of `options` (a dropdown).
    Choice {
        options: Vec<String>,
        #[serde(default)]
        default: Option<String>,
    },
}

fn int_min() -> i64 {
    -1_000_000
}
fn int_max() -> i64 {
    1_000_000
}

/// The manifest a plug-in returns from `pc_manifest` (UTF-8 JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Stable id, `[A-Za-z0-9._-]{1,64}` (reverse-DNS style is recommended: `org.example.invert`).
    pub id: String,
    /// Menu label.
    pub name: String,
    #[serde(default)]
    pub version: String,
    pub kind: Kind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Parameters in declaration (dialog) order; a JSON object `{"name": spec, …}` in the manifest.
    #[serde(default, deserialize_with = "ordered_params", serialize_with = "params_as_object")]
    pub params: Vec<(String, ParamSpec)>,
    /// Pixels of context the filter needs around each output pixel (0 = point filter).
    #[serde(default)]
    pub overlap: u32,
    #[serde(default)]
    pub area: Area,
    /// Where in a raw developer's pipeline the filter belongs (`"stage": "source" | "display"`, an
    /// ABI v1.1 proposal; hosts without stages ignore it). Unknown values are ignored.
    #[serde(default, deserialize_with = "lenient_stage", skip_serializing_if = "Option::is_none")]
    pub stage: Option<Stage>,
    /// Which part of the photo `pc_analyze` measures (`"analyzeArea": "frame" | "crop"`, an ABI v1.1
    /// proposal alongside `pc_analyze`; hosts without it measure the frame). Unknown values mean the frame.
    #[serde(default, deserialize_with = "lenient_area", skip_serializing_if = "AnalyzeArea::is_frame")]
    pub analyze_area: AnalyzeArea,
    /// How a raw developer tones a Source filter's output (`"baseTone": "camera" | "neutral"`, an ABI
    /// v1.1 proposal; hosts without it use their base tone). Unknown values mean the camera's.
    #[serde(default, deserialize_with = "lenient_base_tone", skip_serializing_if = "BaseTone::is_camera")]
    pub base_tone: BaseTone,
    /// How the host cuts the image into bands (`"bands": "rows" | "tiles"`, an ABI v1.1 proposal):
    /// full-width rows, or near-square tiles when the filter has an `overlap`, which repeat less of the
    /// image in the margins and keep a long feature (a hair, a scratch) in one band more often. Hosts
    /// without it cut rows. Unknown values mean rows.
    #[serde(default, deserialize_with = "lenient_bands", skip_serializing_if = "Bands::is_rows")]
    pub bands: Bands,
    /// Display labels of parameters that declare one (`{"type": …, "label": "Dynamic range"}`), by
    /// parameter name. Read by [`Manifest::parse`]; labels that aren't 1-64 printable characters are dropped.
    #[serde(skip)]
    pub labels: Vec<(String, String)>,
    /// Menu groups of choice parameters that declare them (`"groups": ["", "Kodak", "Kodak", …]`, one
    /// per option, `""` = not in a group), by parameter name. Read by [`Manifest::parse`]; a list
    /// whose length differs from the options', or with a name that isn't 1-64 printable characters,
    /// is dropped (the menu stays flat).
    #[serde(skip)]
    pub groups: Vec<(String, Vec<String>)>,
    /// Parameters that declare `"remember": true` (an ABI v1.1 proposal): the host may start a
    /// newly added copy of the filter with the values last set for them (the film type, say)
    /// instead of their defaults. Read by [`Manifest::parse`]; hosts without it ignore the key.
    #[serde(skip)]
    pub remember: Vec<String>,
    /// A header picture the host may show at the top of the filter's controls (`"header": "<svg…>"`,
    /// or `{"dark": "<svg…>", "light": "<svg…>"}` for dark and light interface themes). Read by
    /// [`Manifest::parse`]; a picture that isn't an SVG document of at most [`MAX_HEADER_BYTES`]
    /// is dropped (no header).
    #[serde(skip)]
    pub header: Option<Header>,
    /// Headings the host shows the parameters under (`"sections": [{"label": "Tone", "params":
    /// ["exposure", …], "collapsed": false}, …]`, an ABI v1.1 proposal), in order. Read by
    /// [`Manifest::parse`]; a section without a printable 1-64 character label or without a
    /// parameter of the manifest is dropped, a parameter named twice stays in the first section,
    /// and parameters in no section come first. Hosts without the field show a flat list.
    #[serde(skip)]
    pub sections: Vec<ParamSection>,
    /// The key the host may offer for adding the filter to the selected photos (`"shortcut":
    /// "Ctrl+Alt+N"`, an ABI v1.1 proposal: modifiers `Cmd`, `Ctrl`, `Alt`, `Shift` and a key, joined
    /// with `+`). Read by [`Manifest::parse`]; a value that isn't 1-32 printable characters is
    /// dropped, and the host decides whether it can bind the key; the user may change it like any
    /// shortcut. Hosts without the field ignore it.
    #[serde(skip)]
    pub shortcut: Option<String>,
    /// A square icon the host may show for the filter (`"icon": "<svg…>"`, drawn on the host's own
    /// background, so it suits dark and light themes). Read by [`Manifest::parse`]; a picture that
    /// isn't an SVG document of at most [`MAX_HEADER_BYTES`] is dropped (the host draws its own).
    #[serde(skip)]
    pub icon: Option<String>,
    /// Where the filter comes from (`homepage`, `repository`, `changelog`, `license`, an ABI v1.1
    /// proposal; hosts without the fields ignore them). Read by [`Manifest::parse`]; a value the
    /// host can't use is dropped and named in [`Manifest::dropped`].
    #[serde(skip)]
    pub about: About,
    /// Manifest values [`Manifest::parse`] dropped, each with why (`"homepage: not an https://
    /// link"`).
    #[serde(skip)]
    pub dropped: Vec<String>,
}

/// Most parameter sections a manifest may declare (see [`Manifest::sections`]).
pub const MAX_SECTIONS: usize = 16;

/// A heading and the parameters under it (see [`Manifest::sections`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParamSection {
    pub label: String,
    pub params: Vec<String>,
    /// Shown folded until the user opens it.
    #[serde(default)]
    pub collapsed: bool,
}

/// `v` as the parameter sections of a manifest with parameters `params` (see [`Manifest::sections`]).
fn param_sections(v: Option<&Value>, params: &[(String, ParamSpec)]) -> Vec<ParamSection> {
    let Some(list) = v.and_then(Value::as_array) else { return Vec::new() };
    let printable = |l: &str| l.chars().count() <= 64 && l.chars().all(|c| !c.is_control() && !hidden_format(c)) && l.chars().any(visible);
    let mut placed: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for sec in list.iter().take(MAX_SECTIONS) {
        let Some(label) = sec.get("label").and_then(Value::as_str).map(str::trim).filter(|l| printable(l)) else { continue };
        let Some(names) = sec.get("params").and_then(Value::as_array) else { continue };
        let mut mine = Vec::new();
        for n in names.iter().filter_map(Value::as_str) {
            if params.iter().any(|(k, _)| k == n) && !placed.contains(&n) {
                placed.push(n);
                mine.push(n.to_string());
            }
        }
        if mine.is_empty() {
            continue;
        }
        out.push(ParamSection { label: label.to_string(), params: mine, collapsed: sec.get("collapsed").and_then(Value::as_bool).unwrap_or(false) });
    }
    out
}

/// The largest header SVG kept (a logo drawn as paths is a few kilobytes).
pub const MAX_HEADER_BYTES: usize = 24 * 1024;

/// A filter's header picture (see [`Manifest::header`]): SVG documents for dark and light
/// interface themes, at least one of them.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub dark: Option<String>,
    pub light: Option<String>,
}

/// `v` as an SVG document of at most [`MAX_HEADER_BYTES`] (a picture a filter carries), or `None`.
fn svg_document(v: Option<&Value>) -> Option<String> {
    let t = v?.as_str()?.trim();
    (t.len() <= MAX_HEADER_BYTES && (t.starts_with("<svg") || t.starts_with("<?xml")) && t.contains("</svg>")).then(|| t.to_string())
}

impl Header {
    fn parse(v: &Value) -> Option<Header> {
        let h = match v {
            Value::String(_) => Header { dark: svg_document(Some(v)), light: None },
            _ => Header { dark: svg_document(v.get("dark")), light: svg_document(v.get("light")) },
        };
        (h.dark.is_some() || h.light.is_some()).then_some(h)
    }

    /// The picture for a dark (`true`) or light interface theme, or the other one when the
    /// filter gives only one.
    pub fn for_theme(&self, dark: bool) -> &str {
        let (want, other) = if dark { (&self.dark, &self.light) } else { (&self.light, &self.dark) };
        want.as_deref().or(other.as_deref()).unwrap_or_default()
    }
}

/// The pipeline stage a filter asks for (see [`Manifest::stage`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    /// On the decoded source, before the develop adjustments (e.g. a negative conversion).
    Source,
    /// On the finished image.
    Display,
}

/// What a filter's `pc_analyze` is given (see [`Manifest::analyze_area`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnalyzeArea {
    /// The whole uncropped photo, as the source decodes it: a crop or zoom never moves the measurement.
    #[default]
    Frame,
    /// The photo as the develop geometry frames it (orientation, crop, straighten, flips, lens and
    /// perspective corrections): what the source stage sees. A new crop needs a new measurement.
    Crop,
}

/// How the image is cut into bands (see [`Manifest::bands`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Bands {
    /// Full-width rows (a very wide image is also cut across).
    #[default]
    Rows,
    /// Near-square tiles, when the filter has an `overlap` (rows otherwise: nothing to save).
    Tiles,
}

impl Bands {
    pub fn is_rows(&self) -> bool {
        *self == Bands::Rows
    }
}

fn lenient_bands<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Bands, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.as_str() == Some("tiles") { Bands::Tiles } else { Bands::Rows })
}

/// The base tone a raw gets after a Source filter (see [`Manifest::base_tone`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BaseTone {
    /// The raw's own: its camera's tone curve, or the host's default raw curve.
    #[default]
    Camera,
    /// None: the filter's output is already a finished rendering (a negative conversion with its
    /// own tone curve), shown as it is, like a JPEG's. Contrast, Whites and Blacks still apply.
    Neutral,
}

impl BaseTone {
    pub fn is_camera(&self) -> bool {
        *self == BaseTone::Camera
    }
}

fn lenient_base_tone<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<BaseTone, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.as_str() == Some("neutral") { BaseTone::Neutral } else { BaseTone::Camera })
}

impl AnalyzeArea {
    pub fn is_frame(&self) -> bool {
        *self == AnalyzeArea::Frame
    }
}

fn lenient_area<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<AnalyzeArea, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(if v.as_str() == Some("crop") { AnalyzeArea::Crop } else { AnalyzeArea::Frame })
}

fn lenient_stage<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Option<Stage>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(match v.as_str() {
        Some("source") => Some(Stage::Source),
        Some("display") => Some(Stage::Display),
        _ => None,
    })
}

impl Manifest {
    /// Parses and validates manifest JSON.
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(Error::Manifest(format!("manifest is {} bytes (limit {MAX_MANIFEST_BYTES})", bytes.len())));
        }
        let text = std::str::from_utf8(bytes).map_err(|_| Error::Manifest("manifest is not UTF-8".into()))?;
        let mut man: Manifest = serde_json::from_str(text).map_err(|e| Error::Manifest(format!("manifest: {e}")))?;
        man.validate()?;
        // optional per-parameter labels (unknown keys to the parameter schema itself)
        let raw: Value = serde_json::from_str(text).map_err(|e| Error::Manifest(format!("manifest: {e}")))?;
        man.header = raw.get("header").and_then(Header::parse);
        man.icon = svg_document(raw.get("icon"));
        (man.about, man.dropped) = About::parse(&raw);
        man.sections = param_sections(raw.get("sections"), &man.params);
        man.shortcut = raw
            .get("shortcut")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.chars().count() <= 32 && t.chars().all(|c| !c.is_control() && !hidden_format(c)) && t.chars().any(visible))
            .map(str::to_string);
        if let Some(ps) = raw.get("params").and_then(Value::as_object) {
            man.labels = man
                .params
                .iter()
                .filter_map(|(k, _)| {
                    let l = ps.get(k)?.get("label")?.as_str()?.trim();
                    let ok = l.chars().count() <= 64 && l.chars().all(|c| !c.is_control() && !hidden_format(c)) && l.chars().any(visible);
                    ok.then(|| (k.clone(), l.to_string()))
                })
                .collect();
            man.groups = man
                .params
                .iter()
                .filter_map(|(k, spec)| {
                    let ParamSpec::Choice { options, .. } = spec else { return None };
                    let names: Vec<String> =
                        ps.get(k)?.get("groups")?.as_array()?.iter().map(|g| g.as_str().map(|s| s.trim().to_string())).collect::<Option<_>>()?;
                    let printable = |g: &String| {
                        g.is_empty() || (g.chars().count() <= 64 && g.chars().all(|c| !c.is_control() && !hidden_format(c)) && g.chars().any(visible))
                    };
                    (names.len() == options.len() && names.iter().all(printable) && names.iter().any(|g| !g.is_empty())).then(|| (k.clone(), names))
                })
                .collect();
            man.remember = man
                .params
                .iter()
                .filter(|(k, _)| ps.get(k).and_then(|p| p.get("remember")).and_then(Value::as_bool) == Some(true))
                .map(|(k, _)| k.clone())
                .collect();
        }
        Ok(man)
    }

    fn validate(&self) -> Result<()> {
        if !valid_id(&self.id) {
            return Err(Error::Manifest(format!("invalid id {:?}: use 1-64 characters from A-Z a-z 0-9 . _ -", self.id)));
        }
        let name = self.name.trim();
        let printable = name.chars().all(|c| !c.is_control() && !hidden_format(c)) && name.chars().any(visible);
        if !printable || name.chars().count() > 64 {
            return Err(Error::Manifest("`name` must be 1-64 printable characters".into()));
        }
        if self.version.len() > 32 || self.description.len() > 1024 || self.author.len() > 128 {
            return Err(Error::Manifest("`version`, `description` or `author` is too long".into()));
        }
        if self.overlap > MAX_OVERLAP {
            return Err(Error::Manifest(format!("`overlap` {} exceeds {MAX_OVERLAP}", self.overlap)));
        }
        for (k, spec) in &self.params {
            if !valid_key(k) {
                return Err(Error::Manifest(format!("invalid parameter name {k:?}")));
            }
            match spec {
                ParamSpec::Number { min, max, default } if !(min.is_finite() && max.is_finite() && default.is_finite()) || min > max => {
                    return Err(Error::Manifest(format!("parameter `{k}`: bad range")));
                }
                ParamSpec::Int { min, max, .. } if min > max => return Err(Error::Manifest(format!("parameter `{k}`: bad range"))),
                ParamSpec::Choice { options, default } => {
                    if options.is_empty() || options.len() > 64 || options.iter().any(|o| o.is_empty() || o.contains(['|', '"'])) {
                        return Err(Error::Manifest(format!("parameter `{k}`: choices must be 1-64 non-empty strings without | or \"")));
                    }
                    if default.as_ref().is_some_and(|d| !options.contains(d)) {
                        return Err(Error::Manifest(format!("parameter `{k}`: default is not one of the options")));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// The display label of parameter `key`: the manifest's `label`, else the key humanised
    /// (`dynamic_range` / `dynamicRange` → "Dynamic range").
    pub fn param_label(&self, key: &str) -> String {
        match self.labels.iter().find(|(k, _)| k == key) {
            Some((_, l)) => l.clone(),
            None => humanise(key),
        }
    }

    /// The menu group of each option of choice parameter `key` (`""`: none), when the manifest gives them.
    pub fn param_groups(&self, key: &str) -> Option<&[String]> {
        self.groups.iter().find(|(k, _)| k == key).map(|(_, g)| g.as_slice())
    }

    /// The parameter notation the command registry and the generated dialogs use
    /// (`{"amount":0..100=50,"mode":"a|b","mono":bool=false}`).
    pub fn params_notation(&self) -> String {
        let parts: Vec<String> = self
            .params
            .iter()
            .map(|(k, spec)| match spec {
                ParamSpec::Number { min, max, default } => format!("\"{k}\":{min}..{max}={default}"),
                ParamSpec::Int { default, .. } => format!("\"{k}\":int={default}"),
                ParamSpec::Bool { default } => format!("\"{k}\":bool={default}"),
                ParamSpec::Choice { options, default } => {
                    // The dialog's default is the first option, so list the default first.
                    let mut opts = options.clone();
                    if let Some(d) = default
                        && let Some(i) = opts.iter().position(|o| o == d)
                    {
                        let d = opts.remove(i);
                        opts.insert(0, d);
                    }
                    format!("\"{k}\":\"{}\"", opts.join("|"))
                }
            })
            .collect();
        format!("{{{}}}", parts.join(","))
    }

    /// Validates user `params` against the schema: missing keys take their defaults, numbers (defaults
    /// included) are clamped into range, unknown keys are dropped; a value of the wrong type is an error.
    pub fn resolve_params(&self, params: &Value) -> Result<Map<String, Value>> {
        let given = match params {
            Value::Null => &Map::new(),
            Value::Object(m) => m,
            _ => return Err(Error::Params("plug-in parameters must be a JSON object".into())),
        };
        let mut out = Map::new();
        for (k, spec) in &self.params {
            let v = given.get(k).filter(|v| !v.is_null());
            let wrong = |what: &str| Error::Params(format!("`{k}` must be {what}"));
            let resolved = match spec {
                ParamSpec::Number { min, max, default } => match v {
                    None => json!((*default).clamp(*min, *max)),
                    Some(v) => json!(v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| wrong("a number"))?.clamp(*min, *max)),
                },
                ParamSpec::Int { min, max, default } => match v {
                    None => json!((*default).clamp(*min, *max)),
                    Some(v) => {
                        let f = v.as_f64().filter(|f| f.is_finite()).ok_or_else(|| wrong("a number"))?;
                        json!((f.round().clamp(*min as f64, *max as f64)) as i64)
                    }
                },
                ParamSpec::Bool { default } => match v {
                    None => json!(default),
                    Some(v) => json!(v.as_bool().ok_or_else(|| wrong("true or false"))?),
                },
                ParamSpec::Choice { options, default } => match v {
                    None => json!(default.clone().or_else(|| options.first().cloned()).unwrap_or_default()),
                    Some(v) => {
                        let s = v.as_str().ok_or_else(|| wrong("a string"))?;
                        if !options.iter().any(|o| o == s) {
                            return Err(Error::Params(format!("`{k}` must be one of {}", options.join(", "))));
                        }
                        json!(s)
                    }
                },
            };
            out.insert(k.clone(), resolved);
        }
        Ok(out)
    }
}

/// Reads the `params` object keeping its key order (serde_json's `Map` would sort it).
fn ordered_params<'de, D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Vec<(String, ParamSpec)>, D::Error> {
    struct V;
    impl<'de> serde::de::Visitor<'de> for V {
        type Value = Vec<(String, ParamSpec)>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object of parameter specs")
        }
        fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Self::Value, E> {
            Ok(Vec::new())
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some((k, v)) = map.next_entry::<String, ParamSpec>()? {
                if out.len() >= MAX_PARAMS {
                    return Err(serde::de::Error::custom(format!("too many parameters (limit {MAX_PARAMS})")));
                }
                out.push((k, v));
            }
            Ok(out)
        }
    }
    d.deserialize_any(V)
}

fn params_as_object<S: serde::Serializer>(p: &[(String, ParamSpec)], s: S) -> std::result::Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut m = s.serialize_map(Some(p.len()))?;
    for (k, v) in p {
        m.serialize_entry(k, v)?;
    }
    m.end()
}

/// A parameter name as a label: words split at `_` and lower→upper case changes, the first capitalised.
pub fn humanise(key: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in key.chars() {
        if c == '_' || c == '-' || c == ' ' {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        // keep acronyms (`ISO`) as they are; lower-case the rest after the first word
        let lowered = if w.chars().all(|c| !c.is_lowercase()) && w.chars().count() > 1 { w.clone() } else { w.to_lowercase() };
        if i == 0 {
            let mut cs = lowered.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        } else {
            out.push_str(&lowered);
        }
    }
    if out.is_empty() { key.to_string() } else { out }
}

/// `[A-Za-z0-9._-]{1,64}`.
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// The Unicode format characters (general category Cf, Unicode 15.1). Most draw nothing, and the bidi
/// controls among them (U+061C, U+200E-200F, U+202A-202E, U+2066-2069) reorder the text around them.
const FORMAT: &[(char, char)] = &[
    ('\u{AD}', '\u{AD}'),
    ('\u{600}', '\u{605}'),
    ('\u{61C}', '\u{61C}'),
    ('\u{6DD}', '\u{6DD}'),
    ('\u{70F}', '\u{70F}'),
    ('\u{890}', '\u{891}'),
    ('\u{8E2}', '\u{8E2}'),
    ('\u{180E}', '\u{180E}'),
    ('\u{200B}', '\u{200F}'),
    ('\u{202A}', '\u{202E}'),
    ('\u{2060}', '\u{2064}'),
    ('\u{2066}', '\u{206F}'),
    ('\u{FEFF}', '\u{FEFF}'),
    ('\u{FFF9}', '\u{FFFB}'),
    ('\u{110BD}', '\u{110BD}'),
    ('\u{110CD}', '\u{110CD}'),
    ('\u{13430}', '\u{1343F}'),
    ('\u{1BCA0}', '\u{1BCA3}'),
    ('\u{1D173}', '\u{1D17A}'),
    ('\u{E0001}', '\u{E0001}'),
    ('\u{E0020}', '\u{E007F}'),
];

/// Default-ignorable characters outside Cf (fillers, variation selectors): allowed in a name, but they
/// draw nothing on their own.
const BLANK: &[(char, char)] = &[
    ('\u{34F}', '\u{34F}'),
    ('\u{115F}', '\u{1160}'),
    ('\u{17B4}', '\u{17B5}'),
    ('\u{180B}', '\u{180D}'),
    ('\u{180F}', '\u{180F}'),
    ('\u{3164}', '\u{3164}'),
    ('\u{FE00}', '\u{FE0F}'),
    ('\u{FFA0}', '\u{FFA0}'),
    ('\u{E0100}', '\u{E01EF}'),
];

fn in_ranges(c: char, ranges: &[(char, char)]) -> bool {
    ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&c))
}

/// A format character a menu label must not carry: it hides in the label or reorders it. The zero-width
/// non-joiner and joiner stay allowed, since Persian, Indic scripts and emoji sequences need them and they
/// cannot reorder text.
fn hidden_format(c: char) -> bool {
    !matches!(c, '\u{200C}' | '\u{200D}') && in_ranges(c, FORMAT)
}

/// A character that draws something on its own.
fn visible(c: char) -> bool {
    !c.is_whitespace() && !c.is_control() && !in_ranges(c, FORMAT) && !in_ranges(c, BLANK)
}

fn valid_key(k: &str) -> bool {
    (1..=64).contains(&k.len()) && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') && !k.starts_with('_') && k != "id" && k != "layer"
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"{"id":"org.example.tone","name":"Tone","version":"1.2","kind":"filter",
        "params":{"amount":{"type":"number","min":0,"max":100,"default":50},
                  "mode":{"type":"choice","options":["soft","hard"],"default":"hard"},
                  "mono":{"type":"bool"},"seed":{"type":"int","min":0,"max":9}},
        "overlap":2,"area":"canvas"}"#;

    #[test]
    fn bands_are_rows_unless_a_filter_asks_for_tiles() {
        let with = |v: &str| format!(r#"{{"id":"a.b","name":"A","kind":"filter"{v}}}"#);
        assert_eq!(Manifest::parse(with("").as_bytes()).unwrap().bands, Bands::Rows);
        assert_eq!(Manifest::parse(with(r#","bands":"tiles""#).as_bytes()).unwrap().bands, Bands::Tiles);
        assert_eq!(Manifest::parse(with(r#","bands":"hexagons""#).as_bytes()).unwrap().bands, Bands::Rows);
        assert_eq!(Manifest::parse(with(r#","bands":3"#).as_bytes()).unwrap().bands, Bands::Rows);
    }

    #[test]
    fn analyze_area_is_the_frame_unless_a_filter_asks_for_the_crop() {
        let with = |v: &str| format!(r#"{{"id":"a.b","name":"A","kind":"filter"{v}}}"#);
        assert_eq!(Manifest::parse(with("").as_bytes()).unwrap().analyze_area, AnalyzeArea::Frame);
        assert_eq!(Manifest::parse(with(r#","analyzeArea":"crop""#).as_bytes()).unwrap().analyze_area, AnalyzeArea::Crop);
        assert_eq!(Manifest::parse(with(r#","analyzeArea":"somewhere""#).as_bytes()).unwrap().analyze_area, AnalyzeArea::Frame);
        assert_eq!(Manifest::parse(with(r#","analyzeArea":7"#).as_bytes()).unwrap().analyze_area, AnalyzeArea::Frame);
    }

    #[test]
    fn a_filters_pictures_are_optional_and_must_be_svg_documents() {
        let with = |v: &str| format!(r#"{{"id":"a.b","name":"A","kind":"filter"{v}}}"#);
        let none = Manifest::parse(with("").as_bytes()).unwrap();
        assert!(none.icon.is_none() && none.header.is_none());
        let svg = r#"<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 8 8'><rect width='8' height='8'/></svg>"#;
        let m = Manifest::parse(with(&format!(r#","icon":"{svg}","header":"{svg}""#)).as_bytes()).unwrap();
        assert_eq!(m.icon.as_deref(), Some(svg));
        let h = m.header.unwrap();
        assert_eq!(h.for_theme(true), svg);
        assert_eq!(h.for_theme(false), svg, "one picture serves both themes");
        let m = Manifest::parse(with(&format!(r#","header":{{"light":"{svg}"}}"#)).as_bytes()).unwrap();
        assert_eq!(m.header.as_ref().map(|h| h.for_theme(true)), Some(svg));
        // not an SVG document, too large, or the wrong type: dropped, the manifest still loads
        for bad in [r#""icon":"<html></html>""#, r#""icon":7"#, r#""header":{"dark":"png bytes"}"#, r#""header":["<svg></svg>"]"#] {
            let m = Manifest::parse(with(&format!(",{bad}")).as_bytes()).unwrap();
            assert!(m.icon.is_none() && m.header.is_none(), "{bad}");
        }
        let huge = format!("<svg>{}</svg>", "x".repeat(MAX_HEADER_BYTES));
        assert!(Manifest::parse(with(&format!(r#","icon":"{huge}""#)).as_bytes()).unwrap().icon.is_none());
    }

    #[test]
    fn parses_and_keeps_param_order() {
        let m = Manifest::parse(FULL.as_bytes()).unwrap();
        assert_eq!(m.id, "org.example.tone");
        assert_eq!(m.params.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["amount", "mode", "mono", "seed"]);
        assert_eq!(m.overlap, 2);
        assert_eq!(m.area, Area::Canvas);
        assert_eq!(m.params_notation(), r#"{"amount":0..100=50,"mode":"hard|soft","mono":bool=false,"seed":int=0}"#);
    }

    #[test]
    fn resolves_defaults_clamps_and_rejects_wrong_types() {
        let m = Manifest::parse(FULL.as_bytes()).unwrap();
        let p = m.resolve_params(&json!({"amount": 500, "seed": 3.6, "junk": 1})).unwrap();
        assert_eq!(Value::Object(p), json!({"amount": 100.0, "mode": "hard", "mono": false, "seed": 4}));
        assert!(m.resolve_params(&json!({"amount": "lots"})).is_err());
        assert!(m.resolve_params(&json!({"mode": "medium"})).is_err());
        assert!(m.resolve_params(&json!({"mono": 1})).is_err());
        assert!(m.resolve_params(&json!([1, 2])).is_err());
        assert!(m.resolve_params(&Value::Null).is_ok());
    }

    #[test]
    fn out_of_range_defaults_are_clamped() {
        // Issue #1012: a missing value fell back to a default outside the declared range unclamped.
        let m = Manifest::parse(
            br#"{"id":"org.example.tone","name":"Tone","kind":"filter",
                "params":{"amount":{"type":"number","min":0,"max":100,"default":1000},
                          "steps":{"type":"int","min":1,"max":16,"default":1000},
                          "low":{"type":"number","min":-1,"max":1,"default":-5},
                          "count":{"type":"int","min":3,"max":9}}}"#,
        )
        .unwrap();
        let resolved = json!({"amount": 100.0, "steps": 16, "low": -1.0, "count": 3});
        assert_eq!(Value::Object(m.resolve_params(&json!({})).unwrap()), resolved);
        assert_eq!(Value::Object(m.resolve_params(&Value::Null).unwrap()), resolved);
        assert_eq!(Value::Object(m.resolve_params(&json!({"amount": null, "steps": null})).unwrap()), resolved);
        let given = m.resolve_params(&json!({"amount": 500, "steps": 500, "low": 0.5, "count": 5})).unwrap();
        assert_eq!(Value::Object(given), json!({"amount": 100.0, "steps": 16, "low": 0.5, "count": 5}));
        // In-range defaults are unchanged.
        let full = Manifest::parse(FULL.as_bytes()).unwrap();
        assert_eq!(Value::Object(full.resolve_params(&json!({})).unwrap()), json!({"amount": 50.0, "mode": "hard", "mono": false, "seed": 0}));
    }

    #[test]
    fn labels_and_stage_are_optional_and_lenient() {
        let m = Manifest::parse(
            br#"{"id":"a.b","name":"N","kind":"filter","stage":"source",
                "params":{"temp":{"type":"number","min":0,"max":1,"default":0},
                          "dynamic_range":{"type":"number","min":0,"max":1,"default":0},
                          "filmBase":{"type":"bool","label":"Film base colour"},
                          "x":{"type":"bool","label":"\u202Eevil"}}}"#,
        )
        .unwrap();
        assert_eq!(m.stage, Some(Stage::Source));
        assert_eq!(m.param_label("temp"), "Temp");
        assert_eq!(m.param_label("dynamic_range"), "Dynamic range");
        assert_eq!(m.param_label("filmBase"), "Film base colour");
        assert_eq!(m.param_label("x"), "X", "an unprintable label falls back to the key");
        assert_eq!(humanise("dynamicRange"), "Dynamic range");
        assert_eq!(humanise("ISO_boost"), "ISO boost");
        let grouped = Manifest::parse(
            br#"{"id":"a.b","name":"N","kind":"filter",
                "params":{"stock":{"type":"choice","options":["Generic","Portra","Gold","Superia"],"groups":["","Kodak","Kodak","Fujifilm"]},
                          "short":{"type":"choice","options":["a","b"],"groups":["G"]},
                          "flat":{"type":"choice","options":["a","b"],"groups":["",""]},
                          "bad":{"type":"choice","options":["a","b"],"groups":["G","\u202Eevil"]},
                          "num":{"type":"number","min":0,"max":1,"default":0,"groups":["G"]}}}"#,
        )
        .unwrap();
        assert_eq!(grouped.param_groups("stock"), Some(&["".to_string(), "Kodak".into(), "Kodak".into(), "Fujifilm".into()][..]));
        for k in ["short", "flat", "bad", "num", "absent"] {
            assert_eq!(grouped.param_groups(k), None, "{k}: no usable groups, a flat menu");
        }
        let odd = Manifest::parse(br#"{"id":"a.b","name":"N","kind":"filter","stage":7}"#).unwrap();
        assert_eq!(odd.stage, None, "an unknown stage is ignored, not an error");
        assert!(odd.sections.is_empty());
    }

    #[test]
    fn sections_group_known_parameters_once_and_drop_the_rest() {
        let m = Manifest::parse(
            br#"{"id":"a.b","name":"N","kind":"filter",
                "params":{"a":{"type":"bool"},"b":{"type":"bool"},"c":{"type":"bool"},"d":{"type":"bool"}},
                "sections":[{"label":"Tone","params":["a","b","nope"]},
                            {"label":"Again","params":["b"]},
                            {"label":"\u202Eevil","params":["c"]},
                            {"label":"Colour","params":["c"],"collapsed":true},
                            "junk", {"params":["d"]}, {"label":"Empty","params":[]}]}"#,
        )
        .unwrap();
        let got: Vec<(&str, Vec<&str>, bool)> =
            m.sections.iter().map(|s| (s.label.as_str(), s.params.iter().map(String::as_str).collect(), s.collapsed)).collect();
        assert_eq!(got, vec![("Tone", vec!["a", "b"], false), ("Colour", vec!["c"], true)]);
        let none = Manifest::parse(br#"{"id":"a.b","name":"N","kind":"filter","sections":{"label":"x"}}"#).unwrap();
        assert!(none.sections.is_empty(), "not a list: no sections, not an error");
    }

    #[test]
    fn a_shortcut_is_kept_as_text_when_printable() {
        let m = Manifest::parse(br#"{"id":"a.b","name":"N","kind":"filter","shortcut":" Ctrl+Alt+N "}"#).unwrap();
        assert_eq!(m.shortcut.as_deref(), Some("Ctrl+Alt+N"));
        for bad in [r#""shortcut":7"#, r#""shortcut":"""#, r#""shortcut":"\u202EN""#, r#""shortcut":"Ctrl+Alt+NNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNNN""#]
        {
            let m = Manifest::parse(format!(r#"{{"id":"a.b","name":"N","kind":"filter",{bad}}}"#).as_bytes()).unwrap();
            assert_eq!(m.shortcut, None, "{bad}");
        }
    }

    #[test]
    fn parameters_that_remember_say_so_with_a_true() {
        let m = Manifest::parse(
            br#"{"id":"a.b","name":"N","kind":"filter","params":{
                "film":{"type":"choice","options":["Colour","B&W"],"remember":true},
                "border":{"type":"number","min":0,"max":25,"default":10,"remember":true},
                "amount":{"type":"number","min":0,"max":1,"default":0.5},
                "odd":{"type":"bool","remember":"yes"}}}"#,
        )
        .unwrap();
        assert_eq!(m.remember, ["film", "border"], "in declaration order, `true` only");
        assert!(Manifest::parse(br#"{"id":"a.b","name":"N","kind":"filter"}"#).unwrap().remember.is_empty());
    }

    #[test]
    fn rejects_bad_manifests() {
        for bad in [
            "",
            "[]",
            "not json",
            r#"{"id":"","name":"x","kind":"filter"}"#,
            r#"{"id":"a b","name":"x","kind":"filter"}"#,
            r#"{"id":"a","name":"","kind":"filter"}"#,
            r#"{"id":"a","name":"x","kind":"panel"}"#,
            r#"{"id":"a","name":"x"}"#,
            r#"{"id":"a","name":"x","kind":"filter","overlap":100000}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"r":{"type":"number","min":5,"max":1,"default":1}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"c":{"type":"choice","options":[]}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":{"__x":{"type":"bool"}}}"#,
            r#"{"id":"a","name":"x","kind":"filter","params":[1]}"#,
        ] {
            assert!(Manifest::parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(Manifest::parse(&[0xff, 0xfe]).is_err());
        assert!(Manifest::parse(&vec![b' '; MAX_MANIFEST_BYTES + 1]).is_err());
    }

    fn with_name(name: &str) -> Result<Manifest> {
        Manifest::parse(json!({"id": "org.example.tone", "name": name, "kind": "filter"}).to_string().as_bytes())
    }

    #[test]
    fn name_must_be_printable_unicode() {
        for bad in [
            "Tone\u{202E}evil",
            "\u{2067}Tone\u{2069}",
            "Tone\u{200F}",
            "\u{200B}\u{200B}\u{200B}",
            "Tone\u{00AD}",
            "\u{FEFF}Tone",
            "\u{200D}",
            "\u{3164}\u{FE0F}",
            " \u{3000} ",
            "Tone\u{7}",
        ] {
            assert!(with_name(bad).is_err(), "{bad:?}");
        }
        // Unicode names still install, with joiners where the script or emoji needs them.
        for good in ["Tone…", "Kontrast ±", "می\u{200C}خواهم", "\u{1F469}\u{200D}\u{1F4BB} Code", "\u{2764}\u{FE0F} Love", "色调"] {
            assert_eq!(with_name(good).map(|m| m.name).ok().as_deref(), Some(good));
        }
        // The documented limit counts characters, not bytes.
        assert!(with_name(&"é".repeat(64)).is_ok());
        assert!(with_name(&"é".repeat(65)).is_err());
    }
}
