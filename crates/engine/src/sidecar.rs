//! XMP sidecars: `<stem>.xmp` (default) or `<file>.xmp` next to the original.
//!
//! **Write** ([`Session::save_sidecar`], `photo.saveMetadataToFile`, or automatically after every
//! change when [`XmpPrefs::auto_write`] is on): rating, colour label, title, caption, copyright,
//! creator, keywords, capture time and GPS in the standard namespaces, plus our complete develop
//! settings (`lc:settings`, JSON), the pick/reject flag (`lc:flag`) and location (`lc:location`).
//!
//! An existing sidecar is never replaced wholesale (issue #92): LightCraft's properties
//! ([`OWNED`], [`OWNED_IF_STATED`]) are merged into it ([`lightcraft_meta::merge_xmp`]) and
//! everything else — another editor's `crs:` develop settings, `xmpMM` history, unknown
//! namespaces — is kept byte for byte. A sidecar that can't be read as XMP is first copied to
//! `<name>.xmp.bak-<time>` (never overwritten), then replaced. Writes are atomic (temp file,
//! fsync, rename).
//!
//! **Shared names** (Stem naming): `IMG_0001.CR3` and `IMG_0001.JPG` would share `IMG_0001.xmp`.
//! When two catalogued files map to one stem sidecar, it belongs to one of them (a raw first, then
//! the first by file name — [`Session::sidecar_naming`]); the others use `<file>.xmp`
//! (`IMG_0001.JPG.xmp`) for writing and reading, so neither overwrites the other.
//!
//! **Read** (on import, and `photo.readMetadataFromFile`): the sidecar wins for metadata, except
//! the capture time: a time embedded in the file wins, and the sidecar's (`exif:DateTimeOriginal`,
//! else `photoshop:DateCreated`, else `xmp:CreateDate`) fills in only when the file has none; develop
//! settings are restored from `lc:settings` when present, else mapped from interoperable `crs:`
//! fields ([`crate::crs`]). For raw/DNG files without a sidecar the file's embedded XMP is used.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lightcraft_catalog::{ColorLabel, Flag, MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{EngineError, Result, Session};

/// Sidecar file naming.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SidecarNaming {
    /// `IMG_0001.xmp` (the common convention; shared by `IMG_0001.CR3` and `IMG_0001.JPG`).
    #[default]
    Stem,
    /// `IMG_0001.CR3.xmp` (unambiguous when raw+JPEG pairs live side by side).
    Full,
}

/// Library preferences for XMP sidecars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct XmpPrefs {
    /// Write the sidecar after every change to a photo's metadata or develop settings.
    pub auto_write: bool,
    pub naming: SidecarNaming,
}

/// The sidecar path for an original, by naming convention.
pub fn sidecar_path(original: &str, naming: SidecarNaming) -> PathBuf {
    match naming {
        SidecarNaming::Stem => Path::new(original).with_extension("xmp"),
        SidecarNaming::Full => PathBuf::from(format!("{original}.xmp")),
    }
}

/// An existing sidecar for `original`: the preferred naming first, then the other one (also
/// accepting `.XMP` and `.Xmp`, and a full name whose photo extension differs in case, such as
/// `IMG_1.cr3.xmp` beside `IMG_1.CR3`, which a case-sensitive file system would otherwise miss).
pub fn find_sidecar(original: &str, naming: SidecarNaming) -> Option<PathBuf> {
    sidecar_candidates(original, naming).find(|p| p.is_file())
}

/// The names `original`'s sidecar can have, in the order they are looked for: under `naming`,
/// then under the other naming (the full name also with the photo extension in the other case),
/// each as `.xmp`, `.XMP` and `.Xmp`.
pub(crate) fn sidecar_candidates(original: &str, naming: SidecarNaming) -> impl Iterator<Item = PathBuf> {
    let other = if naming == SidecarNaming::Stem { SidecarNaming::Full } else { SidecarNaming::Stem };
    let ext = Path::new(original).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let base = Path::new(original).with_extension("");
    let recased = |e: String| (e != ext).then(|| format!("{}.{e}", base.to_string_lossy()));
    let full_variants: Vec<String> = [recased(ext.to_lowercase()), recased(ext.to_uppercase())].into_iter().flatten().collect();
    [naming, other]
        .into_iter()
        .flat_map(move |n| {
            let mut v = vec![sidecar_path(original, n)];
            if n == SidecarNaming::Full {
                v.extend(full_variants.iter().map(|o| sidecar_path(o, n)));
            }
            v
        })
        .flat_map(|p| [p.clone(), p.with_extension("XMP"), p.with_extension("Xmp")])
}

/// What a sidecar (or embedded XMP packet) says about a photo. `None` = not stated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidecarData {
    pub rating: Option<u8>,
    pub flag: Option<Flag>,
    pub label: Option<Option<ColorLabel>>,
    /// The label's text as written (`xmp:Label`), for [`SidecarData::resolve_label`].
    pub label_text: Option<String>,
    pub title: Option<String>,
    pub caption: Option<String>,
    pub copyright: Option<String>,
    /// `xmpRights:Marked`: `Some(true)` copyrighted, `Some(false)` public domain.
    pub copyright_marked: Option<bool>,
    pub usage_terms: Option<String>,
    pub copyright_url: Option<String>,
    pub creator: Option<String>,
    pub location: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub alt_text: Option<String>,
    pub extended_description: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub regions: Option<Vec<lightcraft_meta::Region>>,
    /// Capture time (ISO 8601) from `exif:DateTimeOriginal`, `photoshop:DateCreated` or
    /// `xmp:CreateDate` (first found). Used only when the file itself has no capture time.
    pub captured: Option<String>,
    pub develop: Option<DevelopPatch>,
    /// Actionable diagnostics for foreign develop settings that were not fully carried over.
    /// These are surfaced in the photo import report and never persisted in the catalog.
    pub warnings: Vec<crate::import::ImportWarning>,
    /// Names from the selected develop path. None preserves earlier diagnostics; Some(empty) clears.
    pub unsupported_edits: Option<Vec<String>>,
    /// Invalid native diagnostic metadata must not clear known limitations.
    pub unsupported_edits_invalid: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DevelopPatch {
    /// Our own complete settings (`lc:settings`).
    Full(Box<DevelopSettings>),
    /// Mapped `crs:` fields (partial JSON, merged like a preset).
    Partial(Value),
}

use lightcraft_develop::unsupported_edits::{
    INVALID_DIAGNOSTICS as INVALID_UNSUPPORTED_DIAGNOSTICS, MAX_JSON_BYTES as MAX_UNSUPPORTED_JSON_BYTES, MAX_NAMES as MAX_UNSUPPORTED_NAMES,
    name_valid as unsupported_name_valid,
};

#[cfg(test)]
use lightcraft_develop::unsupported_edits::MAX_NAME_BYTES as MAX_UNSUPPORTED_NAME_BYTES;

fn native_unsupported_names(text: &str) -> Option<Vec<String>> {
    if text.len() > MAX_UNSUPPORTED_JSON_BYTES {
        return None;
    }
    let mut fields = serde_json::from_str::<Vec<String>>(text).ok()?;
    if fields.len() > MAX_UNSUPPORTED_NAMES || fields.iter().any(|name| !unsupported_name_valid(name)) {
        return None;
    }
    fields.sort_unstable();
    fields.dedup();
    Some(fields)
}

/// Native settings are authoritative even when unreadable. Never substitute older foreign edits.
fn native_settings(d: &lightcraft_meta::XmpData) -> std::result::Result<Option<DevelopSettings>, String> {
    let value = d.values.get("lc:settings");
    if value.is_none() && d.lc_settings.is_none() && !d.properties.contains_key("lc:settings") {
        return Ok(None);
    }
    if d.lc_settings_count != 1 {
        return Err("native lc:settings must contain exactly one settings object".into());
    }
    let text = match value {
        Some(lightcraft_meta::XmpValue::Text(text)) => text.as_str(),
        None => d.lc_settings.as_deref().ok_or_else(|| "native lc:settings is empty".to_string())?,
        _ => return Err("native lc:settings must contain a JSON settings object, not structured XMP".into()),
    };
    let value: Value = serde_json::from_str(text).map_err(|e| format!("cannot read native lc:settings: {e}"))?;
    if !value.is_object() {
        return Err("native lc:settings must contain a JSON settings object".into());
    }
    // Deserialize the original JSON too: Value alone discards duplicate keys, which could hide
    // an unsupported version or conflicting settings behind a later value.
    serde_json::from_str(text).map(Some).map_err(|e| format!("cannot read native lc:settings: {e}"))
}

/// Parse a sidecar / XMP packet. `target` selects how a `crs:` white balance is read
/// ([`crate::crs::Target`]: absolute Kelvin for raws with a measured illuminant, a shift from the
/// as-shot white for everything developed relative to it).
pub fn parse_sidecar(xmp: &str, target: crate::crs::Target) -> std::result::Result<SidecarData, String> {
    let d = lightcraft_meta::parse_xmp(xmp).map_err(|e| e.to_string())?;
    let m = &d.metadata;
    let lc = |k: &str| d.properties.get(&format!("lc:{k}")).and_then(|v| v.first()).cloned();
    let mut out = SidecarData {
        title: m.title.clone(),
        caption: m.caption.clone(),
        copyright: m.copyright.clone(),
        copyright_marked: m.copyright_marked,
        usage_terms: m.usage_terms.clone(),
        copyright_url: m.copyright_url.clone(),
        creator: m.artist.clone(),
        location: lc("location").or_else(|| m.sublocation.clone()),
        city: m.city.clone(),
        state: m.state.clone(),
        country: m.country.clone(),
        alt_text: m.alt_text.clone(),
        extended_description: m.extended_description.clone(),
        keywords: (!m.keywords.is_empty() || !m.hierarchical_keywords.is_empty())
            .then(|| lightcraft_catalog::keywords::from_file(&m.keywords, &m.hierarchical_keywords)),
        // A sidecar that has `mwg-rs:Regions` at all (even an empty list) was written by an app that
        // knows about regions, so it's authoritative: its list, empty or not, replaces the catalog's.
        // One without it (most writers, LightCraft's own included, which keeps another app's
        // `mwg-rs:Regions` byte for byte but never writes one) says nothing about regions, and the
        // photo's are kept.
        regions: d.values.contains_key("mwg-rs:Regions").then(|| m.regions.clone()),
        captured: m.capture_time.map(|d| d.to_iso()),
        ..Default::default()
    };
    if let Some(r) = m.rating {
        if r < 0 {
            out.flag = Some(Flag::Reject);
            out.rating = Some(0);
        } else {
            out.rating = Some(r.clamp(0, 5) as u8);
        }
    }
    if let Some(f) = lc("flag").and_then(|f| Flag::parse(&f)) {
        out.flag = Some(f);
    }
    if let Some(l) = &m.label {
        out.label = Some(ColorLabel::parse(l.trim()));
        out.label_text = Some(l.trim().to_string());
    }
    let full = native_settings(&d)?;
    out.develop = match full {
        Some(s) => {
            // Restore our names-only diagnostics with our settings. Foreign CRS can be stale;
            // do not derive new warnings from it. Older native packets leave known warnings
            // alone rather than falsely resolving them just because settings were saved.
            if let Some(text) = lc("unsupportedEdits") {
                out.unsupported_edits = native_unsupported_names(&text);
                out.unsupported_edits_invalid = out.unsupported_edits.is_none();
            }
            Some(DevelopPatch::Full(Box::new(s)))
        }
        None if crate::crs::has_adjustments(&d.properties) => {
            let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), target, crate::crs_masks::DEFAULT_ASPECT);
            // Lightroom's default profiles are named by nearly every sidecar; our own starting look stands
            // in for them, so only another profile is worth a warning
            let profile = crate::crs::camera_profile_name(&d.properties)
                .filter(|name| !matches!(name.trim(), "Adobe Standard" | "Adobe Color"))
                .map(str::to_string);
            if let Some(name) = profile {
                out.warnings.push(crate::import::ImportWarning {
                    path: String::new(),
                    code: crate::import::ImportWarningCode::CameraProfile,
                    profile: Some(name),
                    mode: None,
                    fields: Vec::new(),
                });
            }
            let upright = crate::crs::has_saved_upright_transform(&d.properties);
            if upright {
                let mode = d
                    .properties
                    .get("crs:PerspectiveUpright")
                    .and_then(|values| values.first())
                    .map(String::as_str)
                    .map(|mode| match mode {
                        "0" => crate::import::ImportUprightMode::Off,
                        "1" => crate::import::ImportUprightMode::Auto,
                        "2" => crate::import::ImportUprightMode::Level,
                        "3" => crate::import::ImportUprightMode::Vertical,
                        "4" => crate::import::ImportUprightMode::Full,
                        "5" => crate::import::ImportUprightMode::Guided,
                        _ => crate::import::ImportUprightMode::Unknown,
                    })
                    .unwrap_or(crate::import::ImportUprightMode::Unknown);
                out.warnings.push(crate::import::ImportWarning {
                    path: String::new(),
                    code: crate::import::ImportWarningCode::UprightGeometry,
                    profile: None,
                    mode: Some(mode),
                    fields: Vec::new(),
                });
            }
            let other: Vec<&str> = unmapped
                .iter()
                .map(String::as_str)
                .filter(|name| {
                    *name != "CameraProfile"
                        && !name.starts_with("UprightTransform_")
                        && !matches!(*name, "UprightTransformCount" | "UprightCenterNormX" | "UprightCenterNormY" | "UprightVersion")
                })
                .collect();
            if !other.is_empty() {
                out.warnings.push(crate::import::ImportWarning {
                    path: String::new(),
                    code: crate::import::ImportWarningCode::UnmappedXmp,
                    profile: None,
                    mode: None,
                    fields: other.into_iter().map(str::to_string).collect(),
                });
            }
            let invalid = unmapped.len() > MAX_UNSUPPORTED_NAMES || unmapped.iter().any(|name| !unsupported_name_valid(name));
            out.unsupported_edits = Some(unmapped.into_iter().filter(|name| unsupported_name_valid(name)).take(MAX_UNSUPPORTED_NAMES).collect());
            out.unsupported_edits_invalid = invalid;
            Some(DevelopPatch::Partial(partial))
        }
        None => None,
    };
    Ok(out)
}

/// Apply sidecar data to a photo record (the sidecar wins, except that a capture time embedded in
/// the file is kept). Returns true if develop changed.
pub fn merge_into(p: &mut Photo, sc: &SidecarData, now: &str) -> bool {
    if let Some(fields) = &sc.unsupported_edits {
        p.unsupported_edits = fields.clone();
    }
    if sc.unsupported_edits_invalid
        && p.unsupported_edits.len() < MAX_UNSUPPORTED_NAMES
        && !p.unsupported_edits.iter().any(|field| field == INVALID_UNSUPPORTED_DIAGNOSTICS)
    {
        p.unsupported_edits.push(INVALID_UNSUPPORTED_DIAGNOSTICS.into());
    }
    if p.captured.is_none() {
        p.captured = sc.captured.clone();
    }
    if let Some(r) = sc.rating {
        p.rating = r;
    }
    if let Some(f) = sc.flag {
        p.flag = f;
    }
    if let Some(l) = sc.label {
        p.label = l;
    }
    let m = &mut p.meta;
    for (src, dst) in [
        (&sc.title, &mut m.title),
        (&sc.caption, &mut m.caption),
        (&sc.copyright, &mut m.copyright),
        (&sc.usage_terms, &mut m.usage_terms),
        (&sc.copyright_url, &mut m.copyright_url),
        (&sc.creator, &mut m.creator),
        (&sc.location, &mut m.location),
        (&sc.city, &mut m.city),
        (&sc.state, &mut m.state),
        (&sc.country, &mut m.country),
        (&sc.alt_text, &mut m.alt_text),
        (&sc.extended_description, &mut m.extended_description),
    ] {
        if let Some(v) = src {
            *dst = v.clone();
        }
    }
    if let Some(marked) = sc.copyright_marked {
        m.copyright_status = lightcraft_catalog::CopyrightStatus::from_marked(Some(marked));
    }
    if let Some(k) = &sc.keywords {
        m.keywords = k.clone();
    }
    if let Some(r) = &sc.regions {
        m.regions = r.clone();
    }
    let develop = match &sc.develop {
        Some(DevelopPatch::Full(s)) => (**s).clone(),
        Some(DevelopPatch::Partial(v)) => {
            let mut v = v.clone();
            if p.width > 0 && p.height > 0 {
                crate::crs_masks::refit_radials(&mut v, crate::crs_masks::DEFAULT_ASPECT, p.width as f64 / p.height as f64);
            }
            lightcraft_develop::apply_partial(&p.develop, &v, 1.0)
        }
        None => return false,
    };
    if develop == *p.develop {
        return false;
    }
    p.develop = Arc::new(develop);
    p.edited = Some(now.to_string());
    true
}

impl SidecarData {
    /// Read the label through the catalog's label names: other editors write the label's name
    /// (a custom set's "To Do" as well as "Red"); unknown names clear the label.
    pub fn resolve_label(mut self, cat: &lightcraft_catalog::Catalog) -> Self {
        if let Some(t) = &self.label_text {
            self.label = Some(if t.is_empty() { None } else { cat.label_from_name(t) });
        }
        self
    }
}

/// The sidecar packet for a photo; the colour label is written by its name in `cat` (custom
/// label names included, as other editors do).
pub fn sidecar_packet(p: &Photo, cat: &lightcraft_catalog::Catalog) -> String {
    sidecar_packet_with(p, cat, false)
}

/// [`sidecar_packet`], without any location (GPS, sublocation, city, state, country) when
/// `remove_location` is set (an export with "Remove location info").
pub fn sidecar_packet_with(p: &Photo, cat: &lightcraft_catalog::Catalog, remove_location: bool) -> String {
    let nz = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    let meta = lightcraft_meta::Metadata {
        software: Some("LightCraft".into()),
        title: nz(&p.meta.title),
        caption: nz(&p.meta.caption),
        alt_text: nz(&p.meta.alt_text),
        extended_description: nz(&p.meta.extended_description),
        sublocation: nz(&p.meta.location),
        city: nz(&p.meta.city),
        state: nz(&p.meta.state),
        country: nz(&p.meta.country),
        copyright: nz(&p.meta.copyright),
        copyright_marked: p.meta.copyright_status.marked(),
        usage_terms: nz(&p.meta.usage_terms),
        copyright_url: nz(&p.meta.copyright_url),
        artist: nz(&p.meta.creator),
        keywords: p.meta.keywords.clone(),
        rating: Some(p.rating.min(5) as i8),
        label: p.label.map(|l| cat.label_name(l)),
        capture_time: p.captured.as_deref().and_then(lightcraft_meta::DateTime::parse_iso),
        gps: p.meta.gps.map(|(latitude, longitude)| lightcraft_meta::Gps { latitude, longitude, altitude: None }),
        ..Default::default()
    };
    let mut meta = meta;
    if remove_location {
        (meta.sublocation, meta.city, meta.state, meta.country, meta.gps) = (None, None, None, None, None);
    }
    let settings = serde_json::to_string(&p.develop.to_json()).unwrap_or_default();
    let flag = match p.flag {
        Flag::None => "none",
        Flag::Pick => "pick",
        Flag::Reject => "reject",
    };
    let unsupported = serde_json::to_string(&p.unsupported_edits).unwrap_or_default();
    let mut lc = vec![("settings", settings.as_str()), ("unsupportedEdits", unsupported.as_str()), ("flag", flag)];
    if !remove_location && !p.meta.location.trim().is_empty() {
        lc.push(("location", p.meta.location.as_str()));
    }
    lightcraft_meta::write_xmp_lc(&meta, &lc)
}

/// The properties LightCraft writes and owns in a sidecar: replaced on every save (removed when
/// LightCraft has no value, so clearing a field clears it in the file too). All of them are read
/// back on import and by Read Metadata from File.
pub const OWNED: &[&str] = &[
    "xmp:Rating",
    "xmp:Label",
    "dc:title",
    "dc:description",
    "dc:rights",
    "dc:creator",
    "dc:subject",
    "Iptc4xmpCore:Location",
    "Iptc4xmpCore:AltTextAccessibility",
    "Iptc4xmpCore:ExtDescrAccessibility",
    "photoshop:City",
    "photoshop:State",
    "photoshop:Country",
    "xmpRights:Marked",
    "xmpRights:UsageTerms",
    "xmpRights:WebStatement",
    "lc:*",
];

/// Properties replaced only when LightCraft has a value (capture time; GPS, which LightCraft
/// doesn't read from sidecars): otherwise the sidecar's own value stays. Anything else LightCraft
/// writes (`xmp:CreatorTool`) goes only into new sidecars.
pub const OWNED_IF_STATED: &[&str] = &["exif:DateTimeOriginal", "photoshop:DateCreated", "exif:GPSLatitude", "exif:GPSLongitude"];

/// Write `data` to `path` atomically: a temp file next to it, synced, then renamed over it.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    lightcraft_catalog::safe_file::write_atomic(path, data)
}

/// Copy `path` to a new `<name>.bak-<stamp>` (`-1`, `-2`… if taken; never overwriting).
fn backup(path: &Path, stamp: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    let bytes = std::fs::read(path)?;
    let stamp: String = stamp.chars().filter(char::is_ascii_alphanumeric).collect();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for k in 0..1000 {
        let b = path.with_file_name(if k == 0 { format!("{name}.bak-{stamp}") } else { format!("{name}.bak-{stamp}-{k}") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&b) {
            Ok(mut f) => {
                f.write_all(&bytes)?;
                f.sync_all()?;
                return Ok(b);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free backup name"))
}

/// What [`Session::save_sidecar`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarSaved {
    pub path: PathBuf,
    /// An existing sidecar was merged into (its other contents kept).
    pub merged: bool,
    /// An existing sidecar that couldn't be read as XMP was copied here before being replaced.
    pub backup: Option<PathBuf>,
}

/// The new contents for the sidecar at `path`: `fresh` merged into the existing file, if any.
/// Returns (contents, merged, backup).
fn sidecar_contents(path: &Path, fresh: String, stamp: &str) -> std::io::Result<(String, bool, Option<PathBuf>)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((fresh, false, None)),
        // can't see what is there: never overwrite it blindly
        Err(e) => return Err(e),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((fresh, false, None));
    }
    if let Ok(old) = std::str::from_utf8(&bytes)
        && let Ok(parsed) = lightcraft_meta::parse_xmp(old)
    {
        native_settings(&parsed).map_err(std::io::Error::other)?;
    }
    let rules = lightcraft_meta::MergeRules { owned: OWNED, owned_if_present: OWNED_IF_STATED };
    match std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|old| lightcraft_meta::merge_xmp(old, &fresh, rules).map_err(|e| e.to_string()))
    {
        Ok(merged) => Ok((merged, true, None)),
        Err(why) => {
            let b = backup(path, stamp)?;
            log::warn!("{}: not readable as XMP ({why}); kept a copy as {} before replacing it", path.display(), b.display());
            Ok((fresh, false, Some(b)))
        }
    }
}

/// Key of the stem sidecar a file maps to (case-insensitive).
fn stem_key(original: &str) -> String {
    sidecar_path(original, SidecarNaming::Stem).to_string_lossy().to_lowercase()
}

/// Where a plain path is cut to make its [`stem_key`]: at the dot of its extension, which is
/// what `with_extension` replaces. A plain path is ASCII and ends in its file's name with an
/// extension, which is what a library's paths are; `None` for any other shape.
fn plain_cut(original: &str) -> Option<usize> {
    let path = Path::new(original);
    let (name, stem, ext) = (path.file_name()?.to_str()?, path.file_stem()?.to_str()?, path.extension()?.to_str()?);
    // (the path ends in its file's name, as it stands: no `/.` or separator after it; and the
    // name has a stem and an extension of its own, which `..jpg` and `a.` have not)
    let plain = original.is_ascii() && original.ends_with(name) && !ext.is_empty() && !stem.is_empty() && stem != "." && stem != "..";
    let cut = original.len().checked_sub(ext.len().saturating_add(1))?;
    (plain && original.as_bytes().get(cut) == Some(&b'.')).then_some(cut)
}

/// [`stem_key`] written into `key`, which is reused: nothing is allocated for a plain path
/// ([`plain_cut`]). Any other shape goes the long way.
fn stem_key_into(key: &mut String, original: &str) {
    key.clear();
    match plain_cut(original) {
        Some(cut) => {
            key.push_str(original.get(..cut).unwrap_or_default());
            key.make_ascii_lowercase();
            key.push_str(".xmp");
        }
        None => key.push_str(&stem_key(original)),
    }
}

fn file_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

/// The XMP packet that describes a photo on disk: its sidecar, else (raw/DNG) the embedded XMP.
pub fn read_packet(original: &str, kind: MediaKind, naming: SidecarNaming) -> Option<(String, PathBuf)> {
    read_packet_checked(original, kind, naming).ok().flatten()
}

fn read_packet_checked(original: &str, kind: MediaKind, naming: SidecarNaming) -> std::io::Result<Option<(String, PathBuf)>> {
    if let Some(sc) = find_sidecar(original, naming) {
        return std::fs::read_to_string(&sc).map(|s| Some((s, sc)));
    }
    if kind == MediaKind::Raw {
        let bytes = std::fs::read(original)?;
        return Ok(lightcraft_meta::embedded(&bytes).xmp.map(|x| (x, PathBuf::from(original))));
    }
    Ok(None)
}

/// Photo ids touched by an op (for auto-write).
pub(crate) fn op_photos(op: &Op, out: &mut Vec<PhotoId>) {
    match op {
        Op::SetRating { id, .. }
        | Op::SetFlag { id, .. }
        | Op::SetLabel { id, .. }
        | Op::SetDevelop { id, .. }
        | Op::SetMeta { id, .. }
        | Op::SetUnsupportedEdits { id, .. }
            if !out.contains(id) =>
        {
            out.push(*id);
        }
        Op::Batch { ops } => ops.iter().for_each(|o| op_photos(o, out)),
        _ => {}
    }
}

/// Which photo owns each stem sidecar that several catalogued files map to (see
/// [`Session::sidecar_naming`]).
pub(crate) struct StemOwners(HashMap<String, PhotoId>);

impl StemOwners {
    /// The owners of every shared stem in the library, each file's key made the long way: what
    /// [`StemOwners::among`] must agree with for the photos it is asked about (this is how the
    /// owners were worked out before, for every write).
    #[cfg(test)]
    pub(crate) fn of(cat: &lightcraft_catalog::Catalog) -> StemOwners {
        let mut best: HashMap<String, ((bool, String, u64), PhotoId, usize)> = HashMap::new();
        for p in cat.photos().filter(|p| p.copy_of.is_none()) {
            let Some(path) = file_path(p) else { continue };
            let rank = (p.kind != MediaKind::Raw, path.to_lowercase(), p.id.0);
            let e = best.entry(stem_key(path)).or_insert_with(|| (rank.clone(), p.id, 0));
            e.2 += 1;
            if rank < e.0 {
                (e.0, e.1) = (rank, p.id);
            }
        }
        StemOwners(best.into_iter().filter(|(_, v)| v.2 > 1).map(|(k, v)| (k, v.1)).collect())
    }

    /// The owners of the stems photos `ids` map to: all that naming those photos needs. The
    /// library is passed over once, and only files of those stems are looked at closer, so a
    /// sidecar written for one photo does not cost a table of the whole library.
    pub(crate) fn among(cat: &lightcraft_catalog::Catalog, ids: &[PhotoId]) -> StemOwners {
        let wanted: std::collections::HashSet<String> =
            ids.iter().filter_map(|id| cat.photo(*id)).filter_map(|p| file_path(p)).map(stem_key).collect();
        if wanted.is_empty() {
            return StemOwners(HashMap::new());
        }
        Self::where_wanted(cat, &wanted)
    }

    /// The owners of the shared stems in `wanted`.
    fn where_wanted(cat: &lightcraft_catalog::Catalog, wanted: &std::collections::HashSet<String>) -> StemOwners {
        // per stem: (raw first, then file name, then id) of the best candidate; and how many
        let mut best: HashMap<String, ((bool, String, u64), PhotoId, usize)> = HashMap::new();
        let mut key = String::new();
        for p in cat.photos().filter(|p| p.copy_of.is_none()) {
            let Some(path) = file_path(p) else { continue };
            stem_key_into(&mut key, path);
            if !wanted.contains(key.as_str()) {
                continue;
            }
            let rank = (p.kind != MediaKind::Raw, path.to_lowercase(), p.id.0);
            let e = best.entry(key.clone()).or_insert_with(|| (rank.clone(), p.id, 0));
            e.2 += 1;
            if rank < e.0 {
                (e.0, e.1) = (rank, p.id);
            }
        }
        StemOwners(best.into_iter().filter(|(_, v)| v.2 > 1).map(|(k, v)| (k, v.1)).collect())
    }

    /// `naming` for photo `p`, except that a stem sidecar another file owns becomes Full.
    fn naming(&self, p: &Photo, naming: SidecarNaming) -> SidecarNaming {
        match (naming, file_path(p)) {
            (SidecarNaming::Stem, Some(path)) if self.0.get(&stem_key(path)).is_some_and(|owner| *owner != p.id) => SidecarNaming::Full,
            _ => naming,
        }
    }
}

/// How the sidecar of each of photos `ids` is named in library `cat`, whose naming is `naming`
/// ([`Session::sidecar_namings`], for a thread that has the library and not the session).
pub(crate) fn namings_in(cat: &lightcraft_catalog::Catalog, naming: SidecarNaming, ids: &[PhotoId]) -> Vec<SidecarNaming> {
    let owners = (naming == SidecarNaming::Stem).then(|| StemOwners::among(cat, ids));
    ids.iter()
        .map(|id| match (cat.photo(*id), &owners) {
            (Some(p), Some(o)) => o.naming(p, naming),
            _ => naming,
        })
        .collect()
}

impl Session {
    /// The sidecar naming for photo `id`: the library's, except under Stem naming when another
    /// catalogued file shares the stem (`IMG_0001.CR3` + `IMG_0001.JPG`) and owns the stem
    /// sidecar — a raw first, else the first by file name. The others use Full naming
    /// (`IMG_0001.JPG.xmp`), so their metadata never overwrites each other.
    /// [`Session::sidecar_naming`] for many photos, their stems' owners worked out once (per
    /// photo that is a pass over the whole catalog).
    pub fn sidecar_namings(&self, ids: &[PhotoId]) -> Vec<SidecarNaming> {
        namings_in(&self.catalog, self.xmp.naming, ids)
    }

    pub fn sidecar_naming(&self, id: PhotoId) -> SidecarNaming {
        match self.catalog.photo(id) {
            Some(p) if self.xmp.naming == SidecarNaming::Stem => StemOwners::among(&self.catalog, &[id]).naming(p, self.xmp.naming),
            _ => self.xmp.naming,
        }
    }

    /// Write the photo's XMP sidecar, merged into an existing one (see the module docs).
    pub fn save_sidecar(&mut self, id: PhotoId) -> Result<SidecarSaved> {
        self.save_sidecar_with(id, &StemOwners::among(&self.catalog, &[id]))
    }

    pub(crate) fn save_sidecar_with(&mut self, id: PhotoId, owners: &StemOwners) -> Result<SidecarSaved> {
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        if p.copy_of.is_some() {
            return Err(EngineError::Other(format!("{} is a virtual copy: its settings live only in the library", p.file_name)));
        }
        let orig = file_path(p).ok_or_else(|| EngineError::Other(format!("{} is not a file on disk", p.file_name)))?;
        let naming = owners.naming(p, self.xmp.naming);
        let preferred = sidecar_path(orig, naming);
        let other = if naming == SidecarNaming::Stem { SidecarNaming::Full } else { SidecarNaming::Stem };
        let candidates: Vec<PathBuf> = [naming, other]
            .into_iter()
            .filter(|n| *n != SidecarNaming::Stem || owners.naming(p, SidecarNaming::Stem) == SidecarNaming::Stem)
            .flat_map(|n| {
                let path = sidecar_path(orig, n);
                [path.clone(), path.with_extension("XMP")]
            })
            .collect();
        // A separated packet is still the photo's metadata. Repair it before a fresh write
        // occupies its destination; a failure preserves the queue and both existing packets.
        self.repair_sidecars_for_paths(&candidates, &crate::rename::RealFs)
            .map_err(|e| EngineError::Other(format!("{}: unfinished XMP repair: {e}", preferred.display())))?;
        // Keep an existing packet's spelling/style so foreign properties survive this save and
        // subsequent Undo/Redo writes. Shared stems owned by another photo were excluded above.
        let preferred_missing = !preferred.exists();
        // On a case-insensitive volume `a.xmp` also answers for `a.XMP`: the spelling the folder
        // lists comes first, so the atomic write doesn't rename the packet.
        use crate::rename::MoveFs;
        let existing: Vec<PathBuf> = candidates.into_iter().filter(|path| path.is_file()).collect();
        let listed = existing.iter().find(|path| crate::rename::RealFs.listed(path)).cloned();
        let path = listed.or_else(|| existing.into_iter().next()).unwrap_or_else(|| preferred.clone());
        self.validate_sidecar_packets_for_paths(std::slice::from_ref(&path), &crate::rename::RealFs)?;
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        let orig = file_path(p).ok_or_else(|| EngineError::Other(format!("{} is not a file on disk", p.file_name)))?;
        let err = |e: std::io::Error| EngineError::Other(format!("{}: {e}", path.display()));
        let alternate = if preferred_missing && path.is_file() {
            // Only the ownership-filtered alternate may be adopted, never another photo's stem.
            Some(std::fs::read_to_string(&path).map_err(err)?)
        } else if !path.exists() && p.kind == MediaKind::Raw {
            let bytes = std::fs::read(orig).map_err(err)?;
            lightcraft_meta::embedded(&bytes).xmp
        } else {
            None
        };
        if let Some(packet) = alternate {
            // A new sidecar must not shadow an unreadable native edit in the authoritative packet.
            let parsed = lightcraft_meta::parse_xmp(&packet).map_err(|e| err(std::io::Error::other(e.to_string())))?;
            native_settings(&parsed).map_err(|e| err(std::io::Error::other(e)))?;
        }
        let (data, merged, backup) = sidecar_contents(&path, sidecar_packet(p, &self.catalog), &(self.clock)()).map_err(err)?;
        write_atomic(&path, data.as_bytes()).map_err(err)?;
        self.note_sidecar_write(&path, data.as_bytes());
        Ok(SidecarSaved { path, merged, backup })
    }

    /// The op that applies a photo's sidecar (or embedded XMP) to the catalog, if there is one.
    pub fn read_sidecar_op(&self, id: PhotoId) -> Result<Option<(Op, PathBuf)>> {
        self.read_sidecar_op_with_warnings(id).map(|result| result.map(|(op, from, _)| (op, from)))
    }

    /// Like [`Session::read_sidecar_op`], retaining transient diagnostics for the command report.
    pub fn read_sidecar_op_with_warnings(&self, id: PhotoId) -> Result<Option<(Op, PathBuf, Vec<crate::import::ImportWarning>)>> {
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        let Some(orig) = file_path(p) else { return Ok(None) };
        let Some((packet, from)) =
            read_packet_checked(orig, p.kind, self.sidecar_naming(id)).map_err(|e| EngineError::Other(format!("{orig}: {e}")))?
        else {
            return Ok(None);
        };
        let sc = parse_sidecar(&packet, crate::crs::Target::for_photo(p)).map_err(|e| EngineError::Other(format!("{}: {e}", from.display())))?;
        let mut warnings = sc.warnings.clone();
        for warning in &mut warnings {
            warning.path = orig.to_string();
        }
        let op = self.sidecar_op(id, sc)?;
        Ok(Some((op, from, warnings)))
    }

    /// The op that applies what a sidecar says (already read and parsed, e.g. on a worker
    /// thread) to photo `id`: the sidecar wins. No file is read.
    pub fn sidecar_op(&self, id: PhotoId, sc: SidecarData) -> Result<Op> {
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        let sc = sc.resolve_label(&self.catalog);
        let mut q = (**p).clone();
        let develop_changed = merge_into(&mut q, &sc, &(self.clock)());
        let mut ops = vec![
            Op::SetRating { id, rating: q.rating },
            Op::SetFlag { id, flag: q.flag },
            Op::SetLabel { id, label: q.label },
            Op::SetMeta { id, meta: Box::new(q.meta.clone()) },
        ];
        if q.unsupported_edits != p.unsupported_edits {
            ops.push(Op::SetUnsupportedEdits { id, fields: q.unsupported_edits.clone() });
        }
        if q.captured != p.captured {
            ops.push(Op::SetCaptured { id, captured: q.captured.clone() });
        }
        if develop_changed {
            ops.extend(self.develop_op(id, (*q.develop).clone(), "Read Metadata from File"));
        }
        Ok(Op::Batch { ops })
    }

    /// Auto-write: sidecars for photos changed by `ops` (errors are logged, not returned).
    pub(crate) fn auto_write_sidecars(&mut self, ops: &[Op]) {
        let mut ids = Vec::new();
        ops.iter().for_each(|o| op_photos(o, &mut ids));
        self.auto_write_sidecar_ids(ids);
    }

    /// Gather changed IDs before the saves mutate retained physical repair queues.
    pub(crate) fn auto_write_sidecars_since(&mut self, log_start: usize) {
        let mut ids = Vec::new();
        if let Some(ops) = self.pending_log.get(log_start..) {
            ops.iter().for_each(|o| op_photos(o, &mut ids));
        }
        self.auto_write_sidecar_ids(ids);
    }

    fn auto_write_sidecar_ids(&mut self, ids: Vec<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        let owners = StemOwners::among(&self.catalog, &ids);
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| file_path(p).is_some() && p.copy_of.is_none()) {
                match self.save_sidecar_with(id, &owners) {
                    Ok(_) => {
                        self.unsaved_sidecars.remove(&id);
                    }
                    Err(e) => {
                        // kept for a retry once the file is reachable: a network share that dropped out mustn't leave
                        // the sidecar silently behind the edit
                        log::warn!("auto-write XMP: {e}");
                        self.unsaved_sidecars.insert(id);
                    }
                }
            }
        }
    }

    /// How many photos have an automatic sidecar write waiting (it failed: the folder offline, read-only or full).
    pub fn unsaved_sidecars(&self) -> usize {
        self.unsaved_sidecars.len()
    }

    /// Write the waiting sidecars whose originals are reachable again (the availability cache answers without
    /// touching the disk for the others); photos that are gone, or with auto-write off, are dropped from the wait.
    /// Returns how many were written.
    pub fn retry_unsaved_sidecars(&mut self) -> usize {
        if !self.xmp.auto_write {
            self.unsaved_sidecars.clear();
            return 0;
        }
        let mut ready = Vec::new();
        for id in self.unsaved_sidecars.clone() {
            match self.catalog.photo(id).filter(|p| p.copy_of.is_none()).and_then(|p| file_path(p).map(str::to_owned)) {
                None => {
                    self.unsaved_sidecars.remove(&id);
                }
                Some(path) if self.media.availability.exists(&path) == Some(true) => ready.push(id),
                Some(_) => {}
            }
        }
        if ready.is_empty() {
            return 0;
        }
        let owners = StemOwners::among(&self.catalog, &ready);
        let mut written = 0;
        for id in ready {
            match self.save_sidecar_with(id, &owners) {
                Ok(_) => {
                    self.unsaved_sidecars.remove(&id);
                    written += 1;
                }
                Err(e) => log::debug!("retry XMP: {e}"),
            }
        }
        written
    }
}

#[cfg(test)]
mod tests {
    /// A sidecar written by Lightroom Classic (names flat, paths in `lr:hierarchicalSubject`)
    /// gives the photo its keywords as paths.
    #[test]
    fn sidecar_keywords_keep_their_hierarchy() {
        let m = lightcraft_meta::Metadata {
            keywords: vec!["Lisbon".into(), "Places".into()],
            hierarchical_keywords: vec!["Places|Lisbon".into()],
            ..Default::default()
        };
        let sc = super::parse_sidecar(&lightcraft_meta::write_xmp(&m, None), crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.keywords, Some(vec!["Places|Lisbon".to_string()]));
    }

    use super::*;
    use lightcraft_catalog::Meta;

    fn photo() -> Photo {
        let mut p = Photo::new(PhotoId(7), Source::File { path: "/x/IMG_1.jpg".into() }, "IMG_1.jpg", "JPEG", 40, 30, "2026-01-01T00:00:00");
        p.rating = 4;
        p.flag = Flag::Pick;
        p.label = Some(ColorLabel::Purple);
        p.captured = Some("2025-06-07T08:09:10".into());
        p.meta = Meta {
            title: "Dune & sky".into(),
            caption: "Line <one>\nline two".into(),
            copyright: "© 2026 Me".into(),
            creator: "Ann Lee".into(),
            location: "Namib".into(),
            keywords: vec!["desert".into(), "red".into()],
            gps: Some((-24.75, 15.3)),
            ..Default::default()
        };
        let mut d = DevelopSettings::default();
        d.light.exposure = 0.75;
        d.effects.clarity = 22.0;
        d.crop.geometry.angle = 2.5;
        d.masks.push(lightcraft_develop::Mask { id: 1, ..Default::default() });
        p.develop = Arc::new(d);
        p
    }

    #[test]
    fn naming() {
        assert_eq!(sidecar_path("/a/b/IMG_1.CR3", SidecarNaming::Stem), PathBuf::from("/a/b/IMG_1.xmp"));
        assert_eq!(sidecar_path("/a/b/IMG_1.CR3", SidecarNaming::Full), PathBuf::from("/a/b/IMG_1.CR3.xmp"));
    }

    #[test]
    fn packet_roundtrip_restores_everything() {
        let p = photo();
        let x = sidecar_packet(&p, &lightcraft_catalog::Catalog::new());
        let sc = parse_sidecar(&x, crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.rating, Some(4));
        assert_eq!(sc.flag, Some(Flag::Pick));
        assert_eq!(sc.label, Some(Some(ColorLabel::Purple)));
        let mut q = Photo::new(PhotoId(7), p.source.clone(), "IMG_1.jpg", "JPEG", 40, 30, "2026-01-01T00:00:00");
        q.captured = p.captured.clone();
        q.meta.gps = p.meta.gps;
        assert!(merge_into(&mut q, &sc, "now"));
        assert_eq!(q.develop, p.develop);
        assert_eq!(q.meta, p.meta);
        assert_eq!((q.rating, q.flag, q.label), (p.rating, p.flag, p.label));
        assert_eq!(q.edited.as_deref(), Some("now"));
    }

    #[test]
    fn foreign_sidecar_rating_reject_label_and_crs() {
        let x = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
          <rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
            xmp:Rating="-1" xmp:Label="Green" crs:Exposure2012="-0.40" crs:Vibrance="+12"/></rdf:RDF></x:xmpmeta>"#;
        let sc = parse_sidecar(x, crate::crs::Target::RawAbsolute).unwrap();
        assert_eq!((sc.rating, sc.flag, sc.label), (Some(0), Some(Flag::Reject), Some(Some(ColorLabel::Green))));
        let mut p = photo();
        let before = p.develop.clone();
        assert!(merge_into(&mut p, &sc, "t"));
        assert_eq!(p.develop.light.exposure, -0.4);
        assert_eq!(p.develop.color.vibrance, 12.0);
        assert_eq!(p.develop.effects.clarity, before.effects.clarity, "crs partial keeps unrelated settings");
        assert_eq!(p.meta.title, "Dune & sky", "fields the sidecar doesn't state are kept");
    }

    #[test]
    fn op_photo_collection() {
        let mut v = Vec::new();
        op_photos(
            &Op::Batch {
                ops: vec![
                    Op::SetRating { id: PhotoId(1), rating: 2 },
                    Op::SetFlag { id: PhotoId(1), flag: Flag::Pick },
                    Op::SetLabel { id: PhotoId(3), label: None },
                ],
            },
            &mut v,
        );
        assert_eq!(v, vec![PhotoId(1), PhotoId(3)]);
    }
}

#[cfg(test)]
#[path = "sidecar/tests_repair_saves.rs"]
mod tests_repair_saves;

#[cfg(test)]
#[path = "sidecar/tests_repair_companions.rs"]
mod tests_repair_companions;

#[cfg(test)]
#[path = "sidecar/tests_repair_provenance.rs"]
mod tests_repair_provenance;

#[cfg(test)]
#[path = "sidecar/tests_stem_owners.rs"]
mod tests_stem_owners;

#[cfg(test)]
mod unsupported_edit_tests {
    use super::*;

    #[test]
    fn shared_sidecar_op_carries_diagnostics_without_a_develop_change_and_undo_restores_them() {
        let mut s = Session::with_demo();
        let id = s.active().unwrap();
        let develop = s.catalog.photo(id).unwrap().develop.clone();
        let selected = SidecarData { unsupported_edits: Some(vec!["Look".into()]), ..Default::default() };
        let op = s.sidecar_op(id, selected).unwrap();
        let Op::Batch { ops } = &op else { panic!("expected a batch") };
        assert!(ops.iter().any(|op| matches!(op, Op::SetUnsupportedEdits { .. })));
        assert!(!ops.iter().any(|op| matches!(op, Op::SetDevelop { .. })));
        let mut touched = Vec::new();
        op_photos(&Op::SetUnsupportedEdits { id, fields: vec!["Look".into()] }, &mut touched);
        assert_eq!(touched, [id], "warning-only writes must reach the normal auto-write owner");
        let undo = s.catalog.apply(op).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().unsupported_edits, ["Look"]);
        assert_eq!(s.catalog.photo(id).unwrap().develop, develop);
        let metadata = s.sidecar_op(id, SidecarData::default()).unwrap();
        s.catalog.apply(metadata).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().unsupported_edits, ["Look"], "no develop path preserves diagnostics");
        let clear = s.sidecar_op(id, SidecarData { unsupported_edits: Some(vec![]), ..Default::default() }).unwrap();
        let undo_clear = s.catalog.apply(clear).unwrap();
        assert!(s.catalog.photo(id).unwrap().unsupported_edits.is_empty());
        s.catalog.apply(undo_clear).unwrap();
        assert_eq!(s.catalog.photo(id).unwrap().unsupported_edits, ["Look"]);
        s.catalog.apply(undo).unwrap();
        assert!(s.catalog.photo(id).unwrap().unsupported_edits.is_empty());
        assert_eq!(s.catalog.photo(id).unwrap().develop, develop);
    }

    #[test]
    fn native_sidecar_roundtrip_preserves_diagnostics_and_explicit_empty_clears() {
        let mut photo = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "test", "PNG", 8, 8, "now");
        photo.unsupported_edits = vec!["FilterList".into(), "Look".into()];
        let catalog = lightcraft_catalog::Catalog::new();
        let packet = sidecar_packet(&photo, &catalog);
        let sc = parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.unsupported_edits, Some(photo.unsupported_edits.clone()));
        let mut target = photo.clone();
        target.unsupported_edits = vec!["CameraProfile".into()];
        merge_into(&mut target, &sc, "later");
        assert_eq!(target.unsupported_edits, photo.unsupported_edits);
        photo.unsupported_edits.clear();
        let empty = parse_sidecar(&sidecar_packet(&photo, &catalog), crate::crs::Target::Rendered).unwrap();
        merge_into(&mut target, &empty, "later");
        assert!(target.unsupported_edits.is_empty());
    }

    #[test]
    fn selected_import_path_controls_unsupported_diagnostics() {
        let wrap = |attributes: &str, body: &str| {
            format!(
                r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:lc="http://ns.lightcraft.app/lc/1.0/" {attributes}>{body}</rdf:Description>"#
            )
        };
        let selected = parse_sidecar(
            &wrap(r#"crs:Exposure2012="1" crs:FuturePanelControl="false" crs:FutureEditZero="0""#, ""),
            crate::crs::Target::RawAbsolute,
        )
        .unwrap();
        assert_eq!(selected.unsupported_edits, Some(vec!["FutureEditZero".into(), "FuturePanelControl".into()]));
        let applied =
            parse_sidecar(&wrap(r#"crs:AlreadyApplied="True" crs:CameraProfile="Procedural""#, ""), crate::crs::Target::RawAbsolute).unwrap();
        assert_eq!(applied.unsupported_edits, None);
        let native =
            parse_sidecar(&wrap(r#"crs:CameraProfile="Procedural""#, "<lc:settings>{}</lc:settings>"), crate::crs::Target::RawAbsolute).unwrap();
        assert_eq!(native.unsupported_edits, None);
        let native_known = parse_sidecar(
            &wrap(r#"crs:CameraProfile="Procedural""#, r#"<lc:settings>{}</lc:settings><lc:unsupportedEdits>["FilterList"]</lc:unsupportedEdits>"#),
            crate::crs::Target::RawAbsolute,
        )
        .unwrap();
        assert_eq!(native_known.unsupported_edits, Some(vec!["FilterList".into()]));
        let native_cleared =
            parse_sidecar(&wrap("", "<lc:settings>{}</lc:settings><lc:unsupportedEdits>[]</lc:unsupportedEdits>"), crate::crs::Target::RawAbsolute)
                .unwrap();
        assert_eq!(native_cleared.unsupported_edits, Some(Vec::new()));
        let metadata_only = parse_sidecar(&wrap("", ""), crate::crs::Target::RawAbsolute).unwrap();
        assert_eq!(metadata_only.unsupported_edits, None);
    }
}

#[cfg(test)]
mod diagnostic_bounds_tests {
    use super::*;

    #[test]
    fn maximal_valid_diagnostic_names_round_trip() {
        let fields: Vec<String> = (0..MAX_UNSUPPORTED_NAMES).map(|i| format!("Field{i:03}{}", "A".repeat(MAX_UNSUPPORTED_NAME_BYTES - 8))).collect();
        assert!(fields.iter().all(|name| name.len() == MAX_UNSUPPORTED_NAME_BYTES));
        let text = serde_json::to_string(&fields).unwrap();
        assert_eq!(text.len(), MAX_UNSUPPORTED_JSON_BYTES);
        assert_eq!(native_unsupported_names(&text), Some(fields));
    }

    #[test]
    fn native_diagnostic_names_are_validated_without_truncating_valid_names() {
        let long = "A".repeat(256);
        let values = vec!["FilterList".to_string(), "Mask: AI mask (?)".into(), "Mask: blend mode -7".into(), "未来字段".into(), long.clone()];
        let parsed = native_unsupported_names(&serde_json::to_string(&values).unwrap()).unwrap();
        assert!(parsed.contains(&long) && parsed.contains(&"未来字段".to_string()));
        assert_eq!(parsed.len(), values.len());
        for fields in
            [vec![String::new()], vec!["Bad\nName".into()], vec!["{payload:value}".into()], vec!["A".repeat(257)], vec!["Field".into(); 257]]
        {
            assert!(native_unsupported_names(&serde_json::to_string(&fields).unwrap()).is_none());
        }
        assert!(native_unsupported_names(&" ".repeat(MAX_UNSUPPORTED_JSON_BYTES + 1)).is_none());
        assert_eq!(native_unsupported_names("[]"), Some(vec![]));
    }

    #[test]
    fn invalid_native_diagnostics_preserve_known_warnings_and_warn_on_fresh_import() {
        let packet = r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:lc="http://ns.lightcraft.app/lc/1.0/"><lc:settings>{}</lc:settings><lc:unsupportedEdits>["{payload:value}"]</lc:unsupportedEdits></rdf:Description>"#;
        let sc = parse_sidecar(packet, crate::crs::Target::RawAbsolute).unwrap();
        assert!(sc.unsupported_edits.is_none() && sc.unsupported_edits_invalid);
        let mut photo = Photo::new(PhotoId(1), Source::File { path: "/procedural.dng".into() }, "procedural.dng", "DNG", 8, 8, "2026-01-01");
        photo.unsupported_edits = vec!["FilterList".into()];
        merge_into(&mut photo, &sc, "2026-01-01");
        assert_eq!(photo.unsupported_edits, ["FilterList", INVALID_UNSUPPORTED_DIAGNOSTICS]);
        photo.unsupported_edits.clear();
        merge_into(&mut photo, &sc, "2026-01-01");
        assert_eq!(photo.unsupported_edits, [INVALID_UNSUPPORTED_DIAGNOSTICS]);
        merge_into(&mut photo, &sc, "2026-01-01");
        assert_eq!(photo.unsupported_edits, [INVALID_UNSUPPORTED_DIAGNOSTICS], "the warning is not duplicated");
    }
}

#[cfg(test)]
mod native_authority_tests {
    use super::*;

    fn packet(body: &str) -> String {
        format!(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:lc="http://ns.lightcraft.app/lc/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" crs:Exposure2012="-2">{body}</rdf:Description>"#
        )
    }

    fn rejected_packets() -> Vec<String> {
        let mut packets: Vec<_> = [
            "",
            " ",
            "null",
            "[]",
            "true",
            r#"{"version":999}"#,
            r#"{"version":999,"version":1}"#,
            r#"{"version":1,"version":999}"#,
            r#"{"light":{"exposure":1,"exposure":2}}"#,
            r#"{"light":false}"#,
            "{",
        ]
        .into_iter()
        .map(|json| packet(&format!("<lc:settings>{json}</lc:settings>")))
        .collect();
        packets.extend([
            packet("<lc:settings/><lc:settings>{}</lc:settings>"),
            packet("<lc:settings>{}</lc:settings><lc:settings>{}</lc:settings>"),
            packet(r#"<lc:settings rdf:parseType="Resource"><lc:future>1</lc:future></lc:settings>"#),
            packet(r#"<lc:settings><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></lc:settings>"#),
            packet("<lc:settings>{}</lc:settings>").replace(" crs:Exposure2012", " lc:settings=\"{}\" crs:Exposure2012"),
        ]);
        packets
    }

    fn dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("lc-native-authority-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn unreadable_native_settings_never_fall_through_to_crs_on_any_target() {
        for packet in rejected_packets() {
            for target in [crate::crs::Target::Any, crate::crs::Target::Rendered, crate::crs::Target::RawAbsolute, crate::crs::Target::RawRelative] {
                assert!(parse_sidecar(&packet, target).is_err(), "accepted native packet on {target:?}: {packet}");
            }
        }
    }

    #[test]
    fn supported_native_settings_keep_process_hdr_and_schema_marker() {
        for version in [0, 1] {
            let wire = format!(r#"{{"version":{version},"process":77,"light":{{"exposure":0.4}},"hdr":{{"enabled":true,"max_ev":4.0}}}}"#);
            let sc = parse_sidecar(&packet(&format!("<lc:settings>{wire}</lc:settings>")), crate::crs::Target::RawRelative).unwrap();
            let Some(DevelopPatch::Full(settings)) = sc.develop else { panic!("native settings were not selected") };
            assert_eq!(settings.version, version);
            assert_eq!(settings.process, lightcraft_develop::ProcessVersion(77));
            assert_eq!(settings.light.exposure, 0.4);
            assert!(settings.hdr.enabled);
            assert_eq!(settings.hdr.max_ev, 4.0);
        }
    }

    #[test]
    fn saving_refuses_authoritative_native_errors_before_backup_or_replace() {
        let dir = dir("save");
        let path = dir.join("photo.xmp");
        for packet in rejected_packets() {
            std::fs::write(&path, &packet).unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
            assert!(sidecar_contents(&path, self::packet("<lc:settings>{}</lc:settings>"), "now").is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), packet);
            assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), modified);
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no backup/write is needed for a refused native document");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_or_future_alternate_sidecar_cannot_be_shadowed_by_a_new_preferred_file() {
        let dir = dir("alternate");
        let original = dir.join("photo.png");
        std::fs::write(&original, b"procedural source bytes: metadata save does not decode").unwrap();
        let original_text = original.to_string_lossy().to_string();
        let preferred = sidecar_path(&original_text, SidecarNaming::Stem);
        let alternate = sidecar_path(&original_text, SidecarNaming::Full);
        let mut s = Session::new();
        let id = s.catalog.alloc_photo_id();
        let photo = Photo::new(id, Source::File { path: original_text.clone() }, "photo.png", "PNG", 8, 8, "now");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
        for bytes in [packet(r#"<lc:settings>{"version":999}</lc:settings>"#).into_bytes(), vec![0xff, 0xfe, 0xfd]] {
            std::fs::write(&alternate, &bytes).unwrap();
            let modified = std::fs::metadata(&alternate).unwrap().modified().unwrap();
            assert!(s.read_sidecar_op(id).is_err());
            assert!(s.save_sidecar(id).is_err());
            assert!(!preferred.exists());
            assert_eq!(std::fs::read(&alternate).unwrap(), bytes);
            assert_eq!(std::fs::metadata(&alternate).unwrap().modified().unwrap(), modified);
            assert_eq!(std::fs::read(&original).unwrap(), b"procedural source bytes: metadata save does not decode");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod native_function_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn function_sidecar_declares_schema2_and_keeps_upstream_process_hdr() {
        let mut settings = DevelopSettings::from_json(&json!({
            "version":2,"process":77,"hdr":{"enabled":true,"max_ev":4.0},
            "curve":{"master":{"kind":"function","version":1,"base":{"pieces":[{"end":1.0,"hermite":[0.1,0.9,0.8,0.8]}]},"adjustments":[]}}
        }))
        .unwrap();
        // Even an in-process caller that retained an older marker must write the new boundary.
        settings.version = 1;
        let mut photo = Photo::new(PhotoId(1), Source::Demo { scene: 0 }, "test", "PNG", 8, 8, "now");
        photo.develop = Arc::new(settings.clone());
        photo.unsupported_edits = vec!["Look".into()];
        let packet = sidecar_packet(&photo, &lightcraft_catalog::Catalog::new());
        let sc = parse_sidecar(&packet, crate::crs::Target::Rendered).unwrap();
        let Some(DevelopPatch::Full(loaded)) = sc.develop else { panic!("function settings must remain native") };
        assert_eq!(loaded.version, 2);
        assert_eq!(loaded.curve, settings.curve);
        assert_eq!(loaded.process, settings.process);
        assert_eq!(loaded.hdr, settings.hdr);
        assert_eq!(sc.unsupported_edits, Some(vec!["Look".into()]));
    }
}
