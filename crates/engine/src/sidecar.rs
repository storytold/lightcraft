//! XMP sidecars: `<stem>.xmp` (default) or `<file>.xmp` next to the original.
//!
//! **Write** ([`Session::save_sidecar`], `photo.saveMetadataToFile`, or automatically after every
//! change when [`XmpPrefs::auto_write`] is on): rating, colour label, title, caption, copyright,
//! creator, keywords, capture time and GPS in the standard namespaces, plus our complete develop
//! settings (`lc:settings`, JSON), the pick/reject flag (`lc:flag`) and location (`lc:location`).
//!
//! An existing sidecar is never replaced wholesale (issue #92): The app's properties
//! ([`OWNED`], [`OWNED_IF_STATED`]) are merged into it ([`lcc_meta::merge_xmp`]) and
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
//! **Read** (on import, and `photo.readMetadataFromFile`): the sidecar wins for metadata (GPS
//! included: a position in the sidecar replaces the one the camera embedded, as the MWG guidelines
//! have it for XMP over Exif), except the capture time: a time embedded in the file wins, and the sidecar's (`exif:DateTimeOriginal`,
//! else `photoshop:DateCreated`, else `xmp:CreateDate`) fills in only when the file has none; develop
//! settings are restored from `lc:settings` when present, else mapped from interoperable `crs:`
//! fields ([`crate::crs`]). For raw/DNG files without a sidecar the file's embedded XMP is used.
//! Manual reads also accept the first complete standard XMP APP1 in a bounded JPEG header (see `read_jpeg_xmp`).

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lcc_catalog::{ColorLabel, Flag, MediaKind, Op, Photo, PhotoId, SidecarStat, Source, XmpStatus};
use lcc_develop::DevelopSettings;
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
    /// `exif:GPSLatitude` / `exif:GPSLongitude` (latitude, longitude): wins over a position embedded in the file.
    pub gps: Option<(f64, f64)>,
    pub keywords: Option<Vec<String>>,
    pub regions: Option<Vec<lcc_meta::Region>>,
    /// Capture time (ISO 8601) from `exif:DateTimeOriginal`, `photoshop:DateCreated` or
    /// `xmp:CreateDate` (first found). Used only when the file itself has no capture time.
    pub captured: Option<String>,
    pub develop: Option<DevelopPatch>,
    /// A video clip's trim (`lcvid:trimIn`, `lcvid:trimOut`; seconds, `None` out = the end), when stated.
    pub video_trim: Option<(f64, Option<f64>)>,
    /// A video clip's poster frame time (`lcvid:posterTime`), when stated.
    pub video_poster: Option<f64>,
    /// Actionable diagnostics for foreign develop settings that were not fully carried over.
    /// These are surfaced in the photo import report and never persisted in the catalog.
    pub warnings: Vec<crate::import::ImportWarning>,
    /// Plug-in custom metadata fields (`lcpf:fields`): `(plug-in id, key, value as text)`. They go back
    /// to the plug-ins' own data ([`crate::cmd::plugins::restore_fields`]), not into the photo record.
    pub plugin_fields: Vec<(String, String, String)>,
    /// The pixel-filter plug-in stack (`lcpx:filters`), checked, uids reassigned; `None` when the sidecar has none
    /// (then `lc:settings`, if any, decides).
    pub plugin_filters: Option<Vec<lcc_develop::PluginFilter>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DevelopPatch {
    /// Our own complete settings (`lc:settings`).
    Full(Box<DevelopSettings>),
    /// Mapped `crs:` fields (partial JSON, merged like a preset).
    Partial(Value),
}

/// Parse a sidecar / XMP packet. `target` selects how a `crs:` white balance is read
/// ([`crate::crs::Target`]: absolute Kelvin for raws with a measured illuminant, a shift from the
/// as-shot white for everything developed relative to it).
pub fn parse_sidecar(xmp: &str, target: crate::crs::Target) -> std::result::Result<SidecarData, String> {
    let d = lcc_meta::parse_xmp(xmp).map_err(|e| e.to_string())?;
    let m = &d.metadata;
    let lc = |k: &str| d.properties.get(&format!("lc:{k}")).and_then(|v| v.first()).cloned();
    let mut out = SidecarData {
        plugin_fields: lcc_meta::plugin_fields(&d),
        // read a few more than fit, so skipped malformed entries don't hide good ones after them
        plugin_filters: lcc_meta::plugin_filters(&d, lcc_develop::MAX_PLUGIN_FILTERS.saturating_mul(4))
            .map(|e| crate::cmd::plugin_filters::stack_from_xmp(&e)),
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
        gps: m.gps.map(|g| (g.latitude, g.longitude)),
        keywords: (!m.keywords.is_empty() || !m.hierarchical_keywords.is_empty())
            .then(|| lcc_catalog::keywords::from_file(&m.keywords, &m.hierarchical_keywords)),
        // A sidecar that has `mwg-rs:Regions` at all (even an empty list) was written by an app that
        // knows about regions, so it's authoritative: its list, empty or not, replaces the catalog's.
        // One without it (most writers, the app's own included, which keeps another app's
        // `mwg-rs:Regions` byte for byte but never writes one) says nothing about regions, and the
        // photo's are kept.
        regions: d.values.contains_key("mwg-rs:Regions").then(|| m.regions.clone()),
        captured: m.capture_time.map(|d| d.to_iso()),
        ..Default::default()
    };
    let vid = |k: &str| {
        d.properties
            .get(&format!("{}:{k}", lcc_meta::VIDEO_PREFIX))
            .and_then(|v| v.first())
            .and_then(|t| t.trim().parse::<f64>().ok())
            .filter(|t| t.is_finite())
    };
    if let Some(t_in) = vid("trimIn") {
        out.video_trim = Some((t_in, vid("trimOut")));
    }
    out.video_poster = vid("posterTime");
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
    let full = d.lc_settings.as_deref().and_then(|j| serde_json::from_str::<Value>(j).ok()).and_then(|v| DevelopSettings::from_json(&v).ok());
    out.develop = match full {
        Some(s) => Some(DevelopPatch::Full(Box::new(s))),
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
            Some(DevelopPatch::Partial(partial))
        }
        None => None,
    };
    Ok(out)
}

/// Apply sidecar data to a photo record (the sidecar wins, except that a capture time embedded in
/// the file is kept). Returns true if develop changed.
pub fn merge_into(p: &mut Photo, sc: &SidecarData, now: &str) -> bool {
    // (a date import guessed from the file's modification time is no capture time: the sidecar's wins)
    if p.captured_stated().is_none() && sc.captured.is_some() {
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
    if let Some(gps) = sc.gps {
        m.gps = Some(gps);
    }
    if let Some(marked) = sc.copyright_marked {
        m.copyright_status = lcc_catalog::CopyrightStatus::from_marked(Some(marked));
    }
    if let Some(k) = &sc.keywords {
        m.keywords = k.clone();
    }
    if let Some(r) = &sc.regions {
        m.regions = r.clone();
    }
    // a clip's trim and poster, when the photo is a probed clip and the values fit it (bad ones are ignored)
    if let Some(v) = p.video.as_mut() {
        if let Some((t_in, t_out)) = sc.video_trim
            && lcc_catalog::validate_trim(v.duration, t_in, t_out).is_ok()
        {
            (v.trim_in, v.trim_out) = (t_in, t_out);
        }
        if let Some(t) = sc.video_poster
            && lcc_catalog::validate_poster(v.duration, Some(t)).is_ok()
        {
            v.poster_time = Some(t);
        }
    }
    let mut develop = match &sc.develop {
        Some(DevelopPatch::Full(s)) => (**s).clone(),
        Some(DevelopPatch::Partial(v)) => {
            let mut v = v.clone();
            if p.width > 0 && p.height > 0 {
                crate::crs_masks::refit_radials(&mut v, crate::crs_masks::DEFAULT_ASPECT, p.width as f64 / p.height as f64);
            }
            lcc_develop::apply_partial(&p.develop, &v, 1.0)
        }
        None if sc.plugin_filters.is_some() => (*p.develop).clone(),
        None => return false,
    };
    if let Some(f) = &sc.plugin_filters {
        develop.plugin_filters = f.clone();
    }
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
    pub fn resolve_label(mut self, cat: &lcc_catalog::Catalog) -> Self {
        if let Some(t) = &self.label_text {
            self.label = Some(if t.is_empty() { None } else { cat.label_from_name(t) });
        }
        self
    }
}

/// The sidecar packet for a photo; the colour label is written by its name in `cat` (custom
/// label names included, as other editors do).
pub fn sidecar_packet(p: &Photo, cat: &lcc_catalog::Catalog) -> String {
    sidecar_packet_with(p, cat, false)
}

/// [`sidecar_packet`], without any location (GPS, sublocation, city, state, country) when
/// `remove_location` is set (an export with "Remove location info").
pub fn sidecar_packet_with(p: &Photo, cat: &lcc_catalog::Catalog, remove_location: bool) -> String {
    packet(p, cat, remove_location, true)
}

/// [`sidecar_packet_with`]; the GPS position is left out unless `gps` is set.
fn packet(p: &Photo, cat: &lcc_catalog::Catalog, remove_location: bool, gps: bool) -> String {
    let nz = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    let meta = lcc_meta::Metadata {
        software: Some(lcc_brand::DISPLAY_NAME.into()),
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
        // (never the file's modification time standing in for a capture time nobody stated)
        capture_time: p.captured_stated().and_then(lcc_meta::DateTime::parse_iso),
        gps: p.meta.gps.filter(|_| gps).map(|(latitude, longitude)| lcc_meta::Gps { latitude, longitude, altitude: None }),
        ..Default::default()
    };
    let mut meta = meta;
    if remove_location {
        (meta.sublocation, meta.city, meta.state, meta.country, meta.gps) = (None, None, None, None, None);
    }
    // the pixel-filter stack goes in its own namespace, not in `lc:settings`
    let filters: Vec<lcc_meta::XmpPluginFilter> = p.develop.plugin_filters.iter().map(crate::cmd::plugin_filters::to_xmp).collect();
    let settings = if filters.is_empty() {
        serde_json::to_string(&*p.develop).unwrap_or_default()
    } else {
        let mut d = (*p.develop).clone();
        d.plugin_filters.clear();
        serde_json::to_string(&d).unwrap_or_default()
    };
    let flag = match p.flag {
        Flag::None => "none",
        Flag::Pick => "pick",
        Flag::Reject => "reject",
    };
    let mut lc = vec![("settings", settings.as_str()), ("flag", flag)];
    if !remove_location && !p.meta.location.trim().is_empty() {
        lc.push(("location", p.meta.location.as_str()));
    }
    // plug-in custom metadata fields, under their own namespace
    let fields: Vec<(String, String, String)> = p
        .plugin_fields
        .iter()
        .filter_map(|(k, v)| k.split_once('/').map(|(plugin, key)| (plugin.to_string(), key.to_string(), lcc_catalog::rules::plugin_value_text(v))))
        .collect();
    let fields: Vec<(&str, &str, &str)> = fields.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    // a video clip's trim and poster, in the fork's video namespace
    let mut video: Vec<(&str, String)> = Vec::new();
    if let Some(v) = &p.video {
        video.push(("trimIn", v.trim_in.to_string()));
        if let Some(o) = v.trim_out {
            video.push(("trimOut", o.to_string()));
        }
        if let Some(t) = v.poster_time {
            video.push(("posterTime", t.to_string()));
        }
    }
    let video: Vec<(&str, &str)> = video.iter().map(|(k, v)| (*k, v.as_str())).collect();
    lcc_meta::write_xmp_filters(&meta, &lc, &fields, &video, &filters)
}

/// The properties the app writes and owns in a sidecar: replaced on every save (removed when
/// The app has no value, so clearing a field clears it in the file too). All of them are read
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
    "lcvid:*",
    "lcpx:*",
];

/// Properties replaced only when the app has a value (capture time, GPS): otherwise the
/// sidecar's own value stays. Anything else the app
/// writes (`xmp:CreatorTool`) goes only into new sidecars.
pub const OWNED_IF_STATED: &[&str] = &["exif:DateTimeOriginal", "photoshop:DateCreated", "exif:GPSLatitude", "exif:GPSLongitude"];

/// Write `data` to `path` atomically: a temp file next to it, synced, then renamed over it.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    lcc_catalog::safe_file::write_atomic(path, data)
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

/// The new contents for photo `p`'s sidecar at `path`: its packet merged into the existing file,
/// if any. Returns (contents, merged, backup).
fn sidecar_contents(path: &Path, p: &Photo, cat: &lcc_catalog::Catalog, stamp: &str) -> std::io::Result<(String, bool, Option<PathBuf>)> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((sidecar_packet(p, cat), false, None)),
        // can't see what is there: never overwrite it blindly
        Err(e) => return Err(e),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((sidecar_packet(p, cat), false, None));
    }
    let rules = lcc_meta::MergeRules { owned: OWNED, owned_if_present: OWNED_IF_STATED };
    match std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|old| lcc_meta::merge_xmp(old, &packet(p, cat, false, !states_gps(old, p.meta.gps)), rules).map_err(|e| e.to_string()))
    {
        Ok(merged) => Ok((merged, true, None)),
        Err(why) => {
            let b = backup(path, stamp)?;
            log::warn!("{}: not readable as XMP ({why}); kept a copy as {} before replacing it", path.display(), b.display());
            Ok((sidecar_packet(p, cat), false, Some(b)))
        }
    }
}

/// Whether packet `old` already states position `gps` (to about a centimetre). Saving leaves such a
/// position as the file writes it, in its own precision and form, rather than rewriting it from the
/// library's copy: GPS read from a sidecar goes back unchanged unless it was changed here.
fn states_gps(old: &str, gps: Option<(f64, f64)>) -> bool {
    let (Some((lat, lon)), Some(g)) = (gps, lcc_meta::parse_xmp(old).ok().and_then(|d| d.metadata.gps)) else { return false };
    (g.latitude - lat).abs() < 1e-7 && (g.longitude - lon).abs() < 1e-7
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

pub(crate) fn file_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

/// The XMP packet that describes a photo on disk: its sidecar, else embedded raw/DNG or JPEG XMP.
pub fn read_packet(original: &str, kind: MediaKind, naming: SidecarNaming) -> Option<(String, PathBuf)> {
    if let Some(sc) = find_sidecar(original, naming) {
        // A discovered sidecar keeps precedence even when reading it fails.
        return std::fs::read_to_string(&sc).ok().map(|s| (s, sc));
    }
    if kind == MediaKind::Raw {
        let bytes = std::fs::read(original).ok()?;
        let x = lcc_meta::embedded(&bytes).xmp?;
        return Some((x, PathBuf::from(original)));
    }
    if kind == MediaKind::Image {
        let mut file = std::fs::File::open(original).ok()?;
        let mut soi = [0; 2];
        file.read_exact(&mut soi).ok()?;
        file.seek(SeekFrom::Start(0)).ok()?;
        // A JPEG is read only up to its first XMP segment; other images (TIFF, PNG, ...) whole.
        let x = if soi == [0xff, 0xd8] {
            read_jpeg_xmp(&mut file, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS)?
        } else {
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).ok()?;
            lcc_meta::embedded(&bytes).xmp?
        };
        return Some((x, PathBuf::from(original)));
    }
    None
}

const JPEG_XMP_HEADER_SPAN: u64 = 16 * 1024 * 1024;
const JPEG_XMP_HEADER_STEPS: usize = 4096;

/// Read from SOI at the current position, stopping at the first standard XMP packet or SOS/EOI.
/// The span includes SOI, segment headers and skipped payloads; each marker/fill step after SOI
/// consumes one iteration. Only a complete APP1 payload (at most 65533 bytes) reaches the shared
/// parser, in a single SOI+APP1 wrapper (at most 65539 bytes). This does not validate the whole JPEG.
fn read_jpeg_xmp(reader: &mut (impl Read + Seek), header_span: u64, marker_steps: usize) -> Option<String> {
    let mut header = reader.take(header_span);
    let mut soi = [0; 2];
    header.read_exact(&mut soi).ok()?;
    if soi != [0xff, 0xd8] {
        return None;
    }
    let mut filled = false;
    for _ in 0..marker_steps {
        if !filled {
            let mut prefix = [0];
            header.read_exact(&mut prefix).ok()?;
            if prefix != [0xff] {
                return None;
            }
        }
        let mut code = [0];
        header.read_exact(&mut code).ok()?;
        let [marker] = code;
        filled = marker == 0xff;
        match marker {
            0xff | 0xd8 | 0xd0..=0xd7 | 0x01 => continue,
            0xd9 | 0xda => return None,
            _ => {}
        }
        let mut length = [0; 2];
        header.read_exact(&mut length).ok()?;
        let payload_len = u16::from_be_bytes(length).checked_sub(2)?;
        let remaining = header.limit().checked_sub(u64::from(payload_len))?;
        if marker != 0xe1 {
            // Seeking must spend the same span budget as reading, before the seek is attempted.
            header.set_limit(remaining);
            header.get_mut().seek(SeekFrom::Current(i64::from(payload_len))).ok()?;
            continue;
        }
        let segment_len = 6usize.checked_add(usize::from(payload_len))?;
        let [hi, lo] = length;
        let mut segment = Vec::with_capacity(segment_len);
        segment.extend_from_slice(&[0xff, 0xd8, 0xff, 0xe1, hi, lo]);
        segment.resize(segment_len, 0);
        header.read_exact(segment.get_mut(6..)?).ok()?;
        if let Some(xmp) = lcc_meta::jpeg_segments(&segment).xmp {
            return Some(xmp);
        }
    }
    None
}

/// Photo ids touched by an op (for auto-write).
pub(crate) fn op_photos(op: &Op, out: &mut Vec<PhotoId>) {
    match op {
        Op::SetRating { id, .. } | Op::SetFlag { id, .. } | Op::SetLabel { id, .. } | Op::SetDevelop { id, .. } | Op::SetMeta { id, .. }
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
    pub(crate) fn of(cat: &lcc_catalog::Catalog) -> StemOwners {
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
    pub(crate) fn among(cat: &lcc_catalog::Catalog, ids: &[PhotoId]) -> StemOwners {
        let wanted: std::collections::HashSet<String> =
            ids.iter().filter_map(|id| cat.photo(*id)).filter_map(|p| file_path(p)).map(stem_key).collect();
        if wanted.is_empty() {
            return StemOwners(HashMap::new());
        }
        Self::where_wanted(cat, &wanted)
    }

    /// The owners of the shared stems in `wanted`.
    fn where_wanted(cat: &lcc_catalog::Catalog, wanted: &std::collections::HashSet<String>) -> StemOwners {
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
pub(crate) fn namings_in(cat: &lcc_catalog::Catalog, naming: SidecarNaming, ids: &[PhotoId]) -> Vec<SidecarNaming> {
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
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
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
        // On a case-insensitive volume `a.xmp` also answers for `a.XMP`: the spelling the folder
        // lists comes first, so the atomic write doesn't rename the packet.
        use crate::rename::MoveFs;
        let existing: Vec<PathBuf> = candidates.into_iter().filter(|path| path.is_file()).collect();
        let listed = existing.iter().find(|path| crate::rename::RealFs.listed(path)).cloned();
        let path = listed.or_else(|| existing.into_iter().next()).unwrap_or(preferred);
        self.validate_sidecar_packets_for_paths(std::slice::from_ref(&path), &crate::rename::RealFs)?;
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
        let err = |e: std::io::Error| EngineError::Other(format!("{}: {e}", path.display()));
        let (data, merged, backup) = sidecar_contents(&path, p, &self.catalog, &(self.clock)()).map_err(err)?;
        write_atomic(&path, data.as_bytes()).map_err(err)?;
        self.note_sidecar_write(&path, data.as_bytes());
        Ok(SidecarSaved { path, merged, backup })
    }

    /// The op that applies a photo's sidecar (or embedded XMP) to the catalog, if there is one.
    pub fn read_sidecar_op(&self, id: PhotoId) -> Result<Option<(Op, PathBuf)>> {
        self.read_sidecar_op_with_warnings(id).map(|result| result.map(|(op, from, _, _)| (op, from)))
    }

    /// Like [`Session::read_sidecar_op`], retaining transient diagnostics for the command report.
    /// The sidecar's plug-in fields come back too (see [`SidecarData::plugin_fields`]).
    #[allow(clippy::type_complexity)]
    pub fn read_sidecar_op_with_warnings(
        &self,
        id: PhotoId,
    ) -> Result<Option<(Op, PathBuf, Vec<crate::import::ImportWarning>, Vec<(String, String, String)>)>> {
        self.read_sidecar_op_named(id, self.sidecar_naming(id))
    }

    /// [`Session::read_sidecar_op_with_warnings`] with the photo's sidecar naming already worked
    /// out ([`Session::sidecar_namings`] once for many photos: per photo it is quadratic).
    #[allow(clippy::type_complexity)]
    pub(crate) fn read_sidecar_op_named(
        &self,
        id: PhotoId,
        naming: SidecarNaming,
    ) -> Result<Option<(Op, PathBuf, Vec<crate::import::ImportWarning>, Vec<(String, String, String)>)>> {
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
        let Some(orig) = file_path(p) else { return Ok(None) };
        let Some((packet, from)) = read_packet(orig, p.kind, naming) else { return Ok(None) };
        let sc = parse_sidecar(&packet, crate::crs::Target::for_photo(p)).map_err(|e| EngineError::Other(format!("{}: {e}", from.display())))?;
        let mut warnings = sc.warnings.clone();
        for warning in &mut warnings {
            warning.path = orig.to_string();
        }
        let fields = sc.plugin_fields.clone();
        let op = self.sidecar_op(id, sc)?;
        Ok(Some((op, from, warnings, fields)))
    }

    /// The op that applies what a sidecar says (already read and parsed, e.g. on a worker
    /// thread) to photo `id`: the sidecar wins. No file is read.
    pub fn sidecar_op(&self, id: PhotoId, sc: SidecarData) -> Result<Op> {
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
        let sc = sc.resolve_label(&self.catalog);
        let mut q = (**p).clone();
        let develop_changed = merge_into(&mut q, &sc, &(self.clock)());
        let mut ops = vec![
            Op::SetRating { id, rating: q.rating },
            Op::SetFlag { id, flag: q.flag },
            Op::SetLabel { id, label: q.label },
            Op::SetMeta { id, meta: Box::new(q.meta.clone()) },
        ];
        if q.captured != p.captured {
            ops.push(Op::SetCaptured { id, captured: q.captured.clone() });
        }
        if develop_changed {
            ops.extend(self.develop_op(id, (*q.develop).clone(), "Read Metadata from File"));
        }
        Ok(Op::Batch { ops })
    }

    /// What the file system says about photo `id`'s sidecar now (`None`: it has none, or isn't a file).
    pub fn sidecar_stat(&self, id: PhotoId) -> Option<SidecarStat> {
        self.sidecar_stat_named(id, self.sidecar_naming(id))
    }

    /// [`Session::sidecar_stat`] with the photo's sidecar naming already worked out (working it
    /// out is a pass over the whole catalog: per photo of a large import it is quadratic).
    fn sidecar_stat_named(&self, id: PhotoId, naming: SidecarNaming) -> Option<SidecarStat> {
        let p = self.catalog.photo(id)?;
        let orig = file_path(p)?;
        let f = find_sidecar(orig, naming)?;
        let m = std::fs::metadata(f).ok()?;
        let mtime =
            m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0);
        Some(SidecarStat { mtime, size: m.len() })
    }

    /// The metadata-vs-XMP state of photo `id` (see [`lcc_catalog::xmp_state`]).
    pub fn xmp_status(&self, id: PhotoId) -> XmpStatus {
        match self.catalog.photo(id) {
            Some(p) => p.xmp_status(self.sidecar_stat(id)),
            None => XmpStatus::Unknown,
        }
    }

    /// Record that the photos' catalog state and their sidecars are in step now (right after a
    /// read from or write to the sidecar): app bookkeeping, journaled but not an undo step.
    /// With `only_with_sidecar`, photos without a sidecar file are skipped.
    pub(crate) fn record_xmp_stamps(&mut self, ids: &[PhotoId], only_with_sidecar: bool) {
        let namings = self.sidecar_namings(ids);
        for (id, naming) in ids.iter().zip(namings) {
            let stat = self.sidecar_stat_named(*id, naming);
            if only_with_sidecar && stat.is_none() {
                continue;
            }
            let Some(p) = self.catalog.photo(*id) else { continue };
            let stamp = Some(p.xmp_stamp_now(stat));
            if p.xmp == stamp {
                continue;
            }
            let op = Op::SetXmpStamp { id: *id, stamp };
            if self.catalog.apply(op.clone()).is_ok() {
                self.pending_log.push(op);
            }
        }
    }

    /// Auto-write: sidecars for photos changed by `ops` (errors are logged, not returned).
    pub(crate) fn auto_write_sidecars(&mut self, ops: &[Op]) {
        let mut ids = Vec::new();
        ops.iter().for_each(|o| op_photos(o, &mut ids));
        self.auto_write_sidecars_of(ids);
    }

    /// Gather changed IDs before the saves mutate retained physical repair queues.
    pub(crate) fn auto_write_sidecars_since(&mut self, log_start: usize) {
        let mut ids = Vec::new();
        if let Some(ops) = self.pending_log.get(log_start..) {
            ops.iter().for_each(|o| op_photos(o, &mut ids));
        }
        self.auto_write_sidecars_of(ids);
    }

    /// [`Session::auto_write_sidecars`] for photos changed outside ops (plug-in fields).
    pub(crate) fn auto_write_sidecars_of(&mut self, ids: Vec<PhotoId>) {
        if ids.is_empty() {
            return;
        }
        let owners = StemOwners::among(&self.catalog, &ids);
        let mut written = Vec::new();
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| file_path(p).is_some() && p.copy_of.is_none()) {
                match self.save_sidecar_with(id, &owners) {
                    Ok(_) => {
                        self.unsaved_sidecars.remove(&id);
                        written.push(id);
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
        self.record_xmp_stamps(&written, false);
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
        let m = lcc_meta::Metadata {
            keywords: vec!["Lisbon".into(), "Places".into()],
            hierarchical_keywords: vec!["Places|Lisbon".into()],
            ..Default::default()
        };
        let sc = super::parse_sidecar(&lcc_meta::write_xmp(&m, None), crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.keywords, Some(vec!["Places|Lisbon".to_string()]));
    }

    use super::*;
    use lcc_catalog::Meta;

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
        d.masks.push(lcc_develop::Mask { id: 1, ..Default::default() });
        p.develop = Arc::new(d);
        p
    }

    /// A clip's trim and poster go out under the fork's video namespace and come back; values that don't
    /// fit the clip are ignored on read; a still photo writes none.
    #[test]
    fn video_trim_and_poster_roundtrip() {
        let mut p = Photo::new(PhotoId(3), Source::File { path: "/v/c.mp4".into() }, "c.mp4", "MP4", 1, 1, "2026-01-01T00:00:00");
        p.kind = lcc_catalog::MediaKind::Video;
        let v = lcc_catalog::VideoMeta {
            duration: 12.0,
            fps_num: 25,
            fps_den: 1,
            width: 1280,
            height: 720,
            codec: "h264".into(),
            has_audio: false,
            trim_in: 1.25,
            trim_out: Some(9.5),
            poster_time: Some(3.0),
        };
        p.video = Some(v.clone());
        let x = sidecar_packet(&p, &lcc_catalog::Catalog::new());
        assert!(x.contains(&lcc_meta::video_ns()) && x.contains("<lcvid:trimIn>1.25</lcvid:trimIn>"), "{x}");
        let sc = parse_sidecar(&x, crate::crs::Target::Rendered).unwrap();
        assert_eq!((sc.video_trim, sc.video_poster), (Some((1.25, Some(9.5))), Some(3.0)));
        let mut q = p.clone();
        q.video = Some(lcc_catalog::VideoMeta { trim_in: 0.0, trim_out: None, poster_time: None, ..v.clone() });
        merge_into(&mut q, &sc, "now");
        assert_eq!(q.video, Some(v.clone()));
        // a sidecar written for a longer clip: out of range, ignored
        let other = x.replace("9.5</lcvid:trimOut>", "99</lcvid:trimOut>").replace(">3</lcvid:posterTime>", ">-4</lcvid:posterTime>");
        let mut r = p.clone();
        merge_into(&mut r, &parse_sidecar(&other, crate::crs::Target::Rendered).unwrap(), "now");
        assert_eq!(r.video, Some(v));
        // a still photo has no video properties
        assert!(!sidecar_packet(&photo(), &lcc_catalog::Catalog::new()).contains("lcvid"));
    }

    fn filter(plugin: &str, stage: lcc_develop::FilterStage) -> lcc_develop::PluginFilter {
        let mut params = serde_json::Map::new();
        params.insert("strength".into(), serde_json::json!(0.5));
        params.insert("mode".into(), serde_json::json!("a<b & \"c\""));
        lcc_develop::PluginFilter {
            uid: 40,
            plugin: plugin.into(),
            module_hash: "abc123".into(),
            name: "Neg & <Pos>".into(),
            params,
            enabled: false,
            stage,
            analysis: Some(serde_json::json!({"base": [0.1, 0.2, 0.3]})),
        }
    }

    /// The pixel-filter stack goes out under its own namespace (not in `lc:settings`), in order, and comes back
    /// with fresh uids, through a merge into another writer's sidecar too.
    #[test]
    fn plugin_filter_stack_roundtrip() {
        let mut p = photo();
        let mut d = (*p.develop).clone();
        d.plugin_filters = vec![filter("x.neg", lcc_develop::FilterStage::Source), filter("x.sharp", lcc_develop::FilterStage::Display)];
        d.plugin_filters[1].enabled = true;
        d.plugin_filters[1].analysis = None;
        p.develop = Arc::new(d);
        let x = sidecar_packet(&p, &lcc_catalog::Catalog::new());
        assert!(x.contains(&lcc_meta::plugin_filters_ns()) && x.contains("<lcpx:filters>"), "{x}");
        let sc = parse_sidecar(&x, crate::crs::Target::Rendered).unwrap();
        let Some(DevelopPatch::Full(full)) = &sc.develop else { panic!("no lc:settings") };
        assert!(full.plugin_filters.is_empty(), "the stack is not duplicated in lc:settings");
        let got = sc.plugin_filters.clone().unwrap();
        let plugins: Vec<&str> = got.iter().map(|f| f.plugin.as_str()).collect();
        assert_eq!(plugins, ["x.neg", "x.sharp"]);
        assert_eq!(got.iter().map(|f| f.uid).collect::<Vec<_>>(), [1, 2]);
        let mut q = photo();
        merge_into(&mut q, &sc, "now");
        let strip = |v: &[lcc_develop::PluginFilter]| v.iter().map(|f| lcc_develop::PluginFilter { uid: 0, ..f.clone() }).collect::<Vec<_>>();
        assert_eq!(strip(&q.develop.plugin_filters), strip(&p.develop.plugin_filters));
        // merged into a sidecar another app wrote, the stack survives and a later empty stack removes it
        let foreign = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description rdf:about=\"\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmp:Rating=\"2\"/></rdf:RDF></x:xmpmeta>";
        let rules = lcc_meta::MergeRules { owned: OWNED, owned_if_present: OWNED_IF_STATED };
        let merged = lcc_meta::merge_xmp(foreign, &x, rules).unwrap();
        let back = parse_sidecar(&merged, crate::crs::Target::Rendered).unwrap().plugin_filters.unwrap();
        assert_eq!(strip(&back), strip(&p.develop.plugin_filters));
        let cleared = lcc_meta::merge_xmp(&merged, &sidecar_packet(&photo(), &lcc_catalog::Catalog::new()), rules).unwrap();
        assert!(!cleared.contains("lcpx:filters"), "{cleared}");
        let mut r = p.clone();
        merge_into(&mut r, &parse_sidecar(&cleared, crate::crs::Target::Rendered).unwrap(), "now");
        assert!(r.develop.plugin_filters.is_empty());
    }

    /// No stack: nothing of the namespace is written and `lc:settings` is the settings JSON as before.
    #[test]
    fn empty_plugin_filter_stack_writes_nothing() {
        let p = photo();
        let x = sidecar_packet(&p, &lcc_catalog::Catalog::new());
        assert!(!x.contains("lcpx") && !x.contains(&lcc_meta::plugin_filters_ns()), "{x}");
        let d = lcc_meta::parse_xmp(&x).unwrap();
        assert_eq!(d.lc_settings.as_deref(), Some(serde_json::to_string(&*p.develop).unwrap().as_str()));
        assert_eq!(parse_sidecar(&x, crate::crs::Target::Rendered).unwrap().plugin_filters, None);
    }

    /// Hostile stack entries are skipped (never a panic); the good ones around them are kept, and the count is capped.
    #[test]
    fn hostile_plugin_filter_entries_are_skipped() {
        let ns = lcc_meta::plugin_filters_ns();
        let li = |body: &str| format!("<rdf:li rdf:parseType=\"Resource\">{body}</rdf:li>");
        let good = li("<p:plugin>ok.one</p:plugin><p:moduleHash>h</p:moduleHash><p:name>One</p:name>");
        let big = "x".repeat(300);
        let huge = "y".repeat(lcc_meta::MAX_FILTER_PART + 1);
        let bad: Vec<String> = vec![
            li("<p:moduleHash>h</p:moduleHash><p:name>no plugin</p:name>"),
            li("<p:plugin>a</p:plugin><p:name>no hash</p:name>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash>"),
            li(&format!("<p:plugin>{big}</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name>")),
            li(&format!("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>{huge}</p:name>")),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:stage>middle</p:stage>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:enabled>maybe</p:enabled>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:params>{not json</p:params>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:params>[1,2]</p:params>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:params>{\"k\":{\"nested\":1}}</p:params>"),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:params>{\"k\":1e999}</p:params>"),
            li(&format!("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:analysis>{}</p:analysis>", "[".repeat(5000))),
            li("<p:plugin>a</p:plugin><p:moduleHash>h</p:moduleHash><p:name>n</p:name><p:analysis>nope</p:analysis>"),
            li("<p:plugin><rdf:Bag><rdf:li>a</rdf:li></rdf:Bag></p:plugin>"),
            "<rdf:li>just text</rdf:li>".into(),
        ];
        for b in &bad {
            let x = format!(
                "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:p=\"{ns}\"><p:filters><rdf:Seq>{good}{b}{good}</rdf:Seq></p:filters></rdf:Description></rdf:RDF></x:xmpmeta>"
            );
            let got = parse_sidecar(&x, crate::crs::Target::Rendered).unwrap().plugin_filters.unwrap();
            assert_eq!(got.len(), 2, "{b}");
            assert_eq!(got.iter().map(|f| f.uid).collect::<Vec<_>>(), [1, 2]);
        }
        // a lone value instead of a seq, and far too many entries
        let many = good.repeat(lcc_develop::MAX_PLUGIN_FILTERS * 10);
        for body in ["<p:filters>text</p:filters>".to_string(), format!("<p:filters><rdf:Seq>{many}</rdf:Seq></p:filters>")] {
            let x = format!(
                "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:p=\"{ns}\">{body}</rdf:Description></rdf:RDF></x:xmpmeta>"
            );
            let got = parse_sidecar(&x, crate::crs::Target::Rendered).unwrap().plugin_filters.unwrap_or_default();
            assert!(got.len() <= lcc_develop::MAX_PLUGIN_FILTERS);
        }
    }

    #[test]
    fn naming() {
        assert_eq!(sidecar_path("/a/b/IMG_1.CR3", SidecarNaming::Stem), PathBuf::from("/a/b/IMG_1.xmp"));
        assert_eq!(sidecar_path("/a/b/IMG_1.CR3", SidecarNaming::Full), PathBuf::from("/a/b/IMG_1.CR3.xmp"));
    }

    #[test]
    fn packet_roundtrip_restores_everything() {
        let p = photo();
        let x = sidecar_packet(&p, &lcc_catalog::Catalog::new());
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

    /// Sidecars as other apps write them (issue #43): the rating and crop arrive whatever the
    /// writer's prefixes, attribute/element form, `True`/`true` spelling or Description split.
    #[test]
    fn rating_and_crop_from_sidecars_of_other_writers() {
        const RDF: &str = r#"xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#""#;
        const XMP: &str = r#"xmlns:xmp="http://ns.adobe.com/xap/1.0/""#;
        const CRS: &str = r#"xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/""#;
        let wrap = |body: &str| {
            format!(
                r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF {RDF}>{body}</rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#
            )
        };
        let crop = r#"crs:HasCrop="true" crs:CropTop="0.1" crs:CropLeft="0.2" crs:CropBottom="0.9" crs:CropRight="0.8" crs:CropAngle="-3.5" crs:CropConstrainToWarp="1""#;
        let cases: Vec<(&str, String, u8, bool)> = vec![
            // darktable: `photo.NEF.xmp`, its own history namespace (crop lives there, not in crs)
            (
                "darktable",
                wrap(&format!(
                    r#"<rdf:Description rdf:about="" {XMP} xmlns:darktable="http://darktable.sf.net/" xmp:Rating="3" darktable:xmp_version="5" darktable:history_end="2"><darktable:history><rdf:Seq><rdf:li darktable:operation="clipping" darktable:enabled="1"/></rdf:Seq></darktable:history></rdf:Description>"#
                )),
                3,
                false,
            ),
            // element-form rating, lower-case `true`, older `xap` prefix
            (
                "element",
                wrap(&format!(
                    r#"<rdf:Description rdf:about="" xmlns:xap="http://ns.adobe.com/xap/1.0/" {CRS} {crop}><xap:Rating>4</xap:Rating></rdf:Description>"#
                )),
                4,
                true,
            ),
            // ExifTool / Capture One / digiKam: one Description per namespace, rating as `5.0`
            (
                "split",
                wrap(&format!(
                    r#"<rdf:Description rdf:about="" xmlns:MicrosoftPhoto="http://ns.microsoft.com/photo/1.0/" MicrosoftPhoto:Rating="99"/><rdf:Description rdf:about="" {XMP} xmp:Rating=" 5.0 "/><rdf:Description rdf:about="" {CRS} {crop}/>"#
                )),
                5,
                true,
            ),
            // Bridge / Camera Raw from Photoshop: crs elements rather than attributes
            (
                "acr-elements",
                wrap(&format!(
                    r#"<rdf:Description rdf:about="" {XMP} {CRS}><xmp:Rating>2</xmp:Rating><crs:HasCrop>True</crs:HasCrop><crs:CropLeft>0.2</crs:CropLeft><crs:CropTop>0.1</crs:CropTop><crs:CropRight>0.8</crs:CropRight><crs:CropBottom>0.9</crs:CropBottom><crs:CropAngle>-3.5</crs:CropAngle></rdf:Description>"#
                )),
                2,
                true,
            ),
        ];
        for (name, x, rating, has_crop) in cases {
            let sc = parse_sidecar(&x, crate::crs::Target::RawAbsolute).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(sc.rating, Some(rating), "{name}");
            let mut p = photo();
            merge_into(&mut p, &sc, "t");
            assert_eq!(p.rating, rating, "{name}");
            let g = p.develop.to_json()["crop"]["geometry"].clone();
            if has_crop {
                assert!((g["rect"]["x0"].as_f64().unwrap_or(0.0) - 0.2).abs() < 1e-6, "{name}: {g}");
                assert!((g["angle"].as_f64().unwrap_or(0.0) + 3.5).abs() < 1e-6, "{name}: {g}");
            }
        }
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
mod jpeg_header_tests {
    use super::{JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS, read_jpeg_xmp};
    use std::io::{self, Cursor, Read, Seek, SeekFrom};

    struct CountedReader {
        cursor: Cursor<Vec<u8>>,
        bytes_read: usize,
        largest_read: usize,
        seeks: usize,
        chunk: usize,
        read_error_at: Option<u64>,
        seek_error: bool,
    }

    impl CountedReader {
        fn new(bytes: Vec<u8>) -> Self {
            Self { cursor: Cursor::new(bytes), bytes_read: 0, largest_read: 0, seeks: 0, chunk: usize::MAX, read_error_at: None, seek_error: false }
        }
    }

    impl Read for CountedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.largest_read = self.largest_read.max(buf.len());
            if self.read_error_at.is_some_and(|at| self.cursor.position() >= at) {
                return Err(io::Error::other("injected header read failure"));
            }
            let len = buf.len().min(self.chunk);
            let read = self.cursor.read(&mut buf[..len])?;
            self.bytes_read += read;
            Ok(read)
        }
    }

    impl Seek for CountedReader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            if self.seek_error {
                return Err(io::Error::other("injected header seek failure"));
            }
            self.cursor.seek(pos)
        }
    }

    fn segment(marker: u8, payload: &[u8]) -> Vec<u8> {
        let len = u16::try_from(payload.len() + 2).unwrap();
        let mut bytes = vec![0xff, marker];
        bytes.extend_from_slice(&len.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn packet(rating: i8) -> String {
        lcc_meta::write_xmp(&lcc_meta::Metadata { rating: Some(rating), ..Default::default() }, None)
    }

    fn xmp_segment(packet: &str) -> Vec<u8> {
        let mut payload = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
        payload.extend_from_slice(packet.as_bytes());
        segment(0xe1, &payload)
    }

    fn jpeg(packet: &str) -> Vec<u8> {
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(xmp_segment(packet));
        bytes
    }

    #[test]
    fn first_standard_packet_skips_other_segments_and_stops_before_tail() {
        let first = packet(2);
        let first_payload_len = xmp_segment(&first).len() - 4;
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(segment(0xe0, &[7; 17]));
        bytes.extend(segment(0xe1, b"Exif\0\0tiny"));
        let mut extended = b"http://ns.adobe.com/xmp/extension/\0".to_vec();
        extended.extend_from_slice(&[0; 40]);
        extended.extend_from_slice(b"not a standard packet");
        bytes.extend(segment(0xe1, &extended));
        bytes.extend(xmp_segment(&first));
        let end = bytes.len();
        bytes.extend(xmp_segment(&packet(5)));
        bytes.extend_from_slice(&[0xff, 0xda, 0, 2]);
        bytes.extend_from_slice(&[3; 128]);
        let mut reader = CountedReader::new(bytes);
        reader.read_error_at = Some(end as u64);
        assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), Some(first));
        assert_eq!(reader.cursor.position(), end as u64, "later packets and image bytes are not read");
        assert_eq!(reader.bytes_read, end - 17, "APP0 payload is skipped, not buffered");
        assert_eq!(reader.seeks, 1);
        assert_eq!(reader.largest_read, first_payload_len, "reads at most one complete APP1 payload at a time");
    }

    #[test]
    fn stops_at_sos_or_eoi_without_reading_later_xmp() {
        for stop in [0xda, 0xd9] {
            let mut bytes = vec![0xff, 0xd8, 0xff, stop];
            bytes.extend(xmp_segment(&packet(3)));
            let mut reader = CountedReader::new(bytes);
            reader.read_error_at = Some(4);
            assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), None);
            assert_eq!((reader.bytes_read, reader.cursor.position(), reader.seeks), (4, 4, 0));
        }
    }

    #[test]
    fn rejects_incomplete_app1_even_when_partial_xmp_parses() {
        let expected = packet(4);
        let mut bytes = jpeg(&expected);
        let declared = u16::from_be_bytes([bytes[4], bytes[5]]) + 7;
        bytes[4..6].copy_from_slice(&declared.to_be_bytes());
        let partial = lcc_meta::jpeg_segments(&bytes).xmp.unwrap();
        assert_eq!(partial, expected, "the shared extractor tolerates a truncated declared payload");
        assert_eq!(super::parse_sidecar(&partial, crate::crs::Target::Rendered).unwrap().rating, Some(4));
        let len = bytes.len();
        let mut reader = CountedReader::new(bytes);
        assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), None);
        assert_eq!(reader.bytes_read, len);
    }

    #[test]
    fn span_budget_counts_soi_headers_and_skipped_payloads() {
        let expected = packet(3);
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(segment(0xe0, &[9; 23]));
        bytes.extend(xmp_segment(&expected));
        let end = bytes.len();
        let mut exact = CountedReader::new(bytes.clone());
        assert_eq!(read_jpeg_xmp(&mut exact, end as u64, JPEG_XMP_HEADER_STEPS), Some(expected));
        assert_eq!(exact.cursor.position(), end as u64, "a complete early APP1 does not require EOI");
        assert_eq!(exact.bytes_read, end - 23);
        let mut short = CountedReader::new(bytes);
        assert_eq!(read_jpeg_xmp(&mut short, (end - 1) as u64, JPEG_XMP_HEADER_STEPS), None);
        assert_eq!((short.bytes_read, short.cursor.position(), short.seeks), (10, 33, 1), "reject before APP1 payload read");
        assert_eq!(short.largest_read, 2);
    }

    #[test]
    fn span_budget_rejects_skip_before_seeking() {
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(segment(0xe0, &[8; 18]));
        bytes.extend(xmp_segment(&packet(1)));
        let mut reader = CountedReader::new(bytes);
        assert_eq!(read_jpeg_xmp(&mut reader, 6, JPEG_XMP_HEADER_STEPS), None);
        assert_eq!((reader.bytes_read, reader.cursor.position(), reader.seeks), (6, 6, 0));
    }

    #[test]
    fn marker_budget_counts_fill_and_standalone_markers() {
        let expected = packet(1);
        // Two fill steps, SOI, TEM, RST3, then APP1: six steps after the initial SOI.
        let mut bytes = vec![0xff, 0xd8, 0xff, 0xff, 0xff, 0xd8, 0xff, 0x01, 0xff, 0xd3];
        bytes.extend(xmp_segment(&expected));
        for (steps, end) in [(0, 2), (5, 10)] {
            let mut reader = CountedReader::new(bytes.clone());
            assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, steps), None);
            assert_eq!((reader.bytes_read, reader.cursor.position()), (end, end as u64));
        }
        let mut reader = CountedReader::new(bytes);
        assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, 6), Some(expected));
    }

    #[test]
    fn rejects_invalid_headers_and_segment_lengths() {
        for bytes in [
            vec![],
            vec![0xff],
            b"\x89PNG\r\n\x1a\n".to_vec(),
            vec![0xff, 0xd8, 0, 0xe1, 0, 2],
            vec![0xff, 0xd8, 0xff, 0xe1, 0],
            vec![0xff, 0xd8, 0xff, 0xe1, 0, 0],
            vec![0xff, 0xd8, 0xff, 0xe1, 0, 1],
            vec![0xff, 0xd8, 0xff, 0xe1, 0, 3],
        ] {
            let len = bytes.len();
            let mut reader = CountedReader::new(bytes);
            assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), None);
            assert!(reader.bytes_read <= len);
        }
    }

    #[test]
    fn propagates_read_and_seek_failures() {
        for at in [0, 6] {
            let mut reader = CountedReader::new(jpeg(&packet(4)));
            reader.read_error_at = Some(at);
            assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), None);
            assert_eq!(reader.bytes_read as u64, at);
        }
        let mut bytes = vec![0xff, 0xd8];
        bytes.extend(segment(0xe0, &[1]));
        bytes.extend(xmp_segment(&packet(4)));
        let mut reader = CountedReader::new(bytes);
        reader.seek_error = true;
        assert_eq!(read_jpeg_xmp(&mut reader, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS), None);
        assert_eq!((reader.bytes_read, reader.seeks), (6, 1));
    }

    #[test]
    fn handles_short_reads_without_bypassing_span_limit() {
        let expected = packet(2);
        let bytes = jpeg(&expected);
        let end = bytes.len();
        let mut exact = CountedReader::new(bytes.clone());
        exact.chunk = 1;
        assert_eq!(read_jpeg_xmp(&mut exact, end as u64, JPEG_XMP_HEADER_STEPS), Some(expected));
        assert_eq!(exact.bytes_read, end);
        for (span, read) in [(0, 0), (1, 1), (end - 1, 6)] {
            let mut reader = CountedReader::new(bytes.clone());
            reader.chunk = 1;
            assert_eq!(read_jpeg_xmp(&mut reader, span as u64, JPEG_XMP_HEADER_STEPS), None);
            assert_eq!(reader.bytes_read, read);
            assert!(reader.cursor.position() <= span as u64);
        }
    }
}
