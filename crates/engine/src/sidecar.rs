//! XMP sidecars: `<stem>.xmp` (default) or `<file>.xmp` next to the original.
//!
//! **Write** ([`Session::save_sidecar`], `photo.saveMetadataToFile`, or automatically after every
//! change when [`XmpPrefs::auto_write`] is on): rating, colour label, title, caption, copyright,
//! creator, keywords, capture time and GPS in the standard namespaces, plus our complete develop
//! settings (`lc:settings`, JSON), the pick/reject flag (`lc:flag`) and location (`lc:location`).
//! A sidecar is written only beside an original that is an existing file at an absolute path;
//! otherwise the save fails.
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
//! (`IMG_0001.JPG.xmp`) for writing and reading, so neither overwrites the other. An import ranks
//! the files it is adding against the library the same way ([`ImportStems`]): a JPEG or a TIFF
//! arriving beside its raw's `IMG_0001.xmp` reads its own `<file>.xmp`, never the raw's packet
//! (issue #1905).
//!
//! **Read** (on import, and `photo.readMetadataFromFile`): the sidecar wins for metadata (GPS
//! included: a position in the sidecar replaces the one the camera embedded, as the MWG guidelines
//! have it for XMP over Exif), except the capture time: a time embedded in the file wins, and the sidecar's (`exif:DateTimeOriginal`,
//! else `photoshop:DateCreated`, else `xmp:CreateDate`) fills in only when the file has none; develop
//! settings are restored from `lc:settings` when present, else mapped from interoperable `crs:`
//! fields ([`crate::crs`]). For raw/DNG files without a sidecar the file's embedded XMP is used.
//! Manual reads also accept the first complete standard XMP APP1 in a bounded JPEG header (see `read_jpeg_xmp`).

use std::collections::{HashMap, HashSet};
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct XmpPrefs {
    /// Write the sidecar after every change to a photo's metadata or develop settings.
    pub auto_write: bool,
    pub naming: SidecarNaming,
    /// Read a sidecar another app changed on disk when the photo is unchanged in the catalog
    /// since the last sync (`photo.autoReadMetadata`); both changed stays a conflict. On by
    /// default: it only replaces values the catalog still holds as last synced, as one undo step,
    /// so no work done here is lost (`docs/xmp-interop.md` → Reading other apps' changes).
    pub auto_read: bool,
    /// Auto-write also for photos only browsed in a folder (not added to the library yet). Off by
    /// default: culling and edits made while browsing stay in the library and nothing is written
    /// into the folder (a client's card or archive) until the photos are added.
    pub write_browsed: bool,
}

impl Default for XmpPrefs {
    fn default() -> Self {
        XmpPrefs { auto_write: false, naming: SidecarNaming::default(), auto_read: true, write_browsed: false }
    }
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

/// Find the sidecar for a file an import is adding, by the ownership [`ImportStems`] worked out
/// (issue #1905): [`find_sidecar`] with the library's naming, except that a file which does not
/// own its stem sidecar reads its own `<file>.xmp` (the name it writes) and never the packet of
/// the sibling that owns the stem one.
pub(crate) fn find_sidecar_for_import(stems: &ImportStems, naming: SidecarNaming, original: &str, listing: &mut FolderListing) -> Option<PathBuf> {
    if stems.owns(original) {
        return sidecar_candidates(original, naming).find(|p| listing.is_file(p));
    }
    let shared = xmp_cases(sidecar_path(original, SidecarNaming::Stem));
    sidecar_candidates(original, SidecarNaming::Full).filter(|p| !shared.contains(p)).find(|p| listing.is_file(p))
}

/// The folders an import has listed, so a sidecar name it looks for is answered from one listing
/// per folder instead of one round trip per name (each one costs on a network share, found or
/// not). A listing vouches only for a name it lacks in every case: a name it holds, or holds in
/// another case (a case-insensitive volume answers to that too), is asked of the disk as before,
/// and so is every name in a folder that can't be listed.
#[derive(Default)]
pub(crate) struct FolderListing {
    /// Folder → the lower-cased names in it (`None`: it could not be listed).
    folders: HashMap<PathBuf, Option<HashSet<String>>>,
}

/// The most folders an import keeps listed (a big import walks folder by folder).
const MAX_LISTED_FOLDERS: usize = 256;

/// The most names one folder listing holds; a bigger folder is asked name by name.
const MAX_LISTED_NAMES: usize = 200_000;

impl FolderListing {
    /// Whether `p` is a file, from the listing of its folder where that says it can't be.
    pub(crate) fn is_file(&mut self, p: &Path) -> bool {
        let (Some(dir), Some(name)) = (p.parent(), p.file_name().and_then(|n| n.to_str())) else { return p.is_file() };
        if !self.folders.contains_key(dir) {
            if self.folders.len() >= MAX_LISTED_FOLDERS {
                self.folders.clear();
            }
            let names = std::fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { dir }).ok().and_then(|rd| {
                let mut names = HashSet::new();
                for entry in rd {
                    let entry = entry.ok()?;
                    // a name that isn't Unicode can't be told apart: the folder is asked name by name
                    names.insert(entry.file_name().to_str()?.to_lowercase());
                    if names.len() > MAX_LISTED_NAMES {
                        return None;
                    }
                }
                Some(names)
            });
            self.folders.insert(dir.to_path_buf(), names);
        }
        match self.folders.get(dir) {
            Some(Some(names)) if !names.contains(&name.to_lowercase()) => false,
            _ => p.is_file(),
        }
    }
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
        .flat_map(xmp_cases)
}

/// The three ways one sidecar name can be written: `.xmp`, `.XMP` and `.Xmp`.
fn xmp_cases(p: PathBuf) -> [PathBuf; 3] {
    [p.clone(), p.with_extension("XMP"), p.with_extension("Xmp")]
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
    /// The creator's contact details, when the sidecar states any.
    pub contact: Option<lcc_meta::CreatorContact>,
    pub location: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    /// The ISO 3166-1 alpha-2 country code (`Iptc4xmpCore:CountryCode`).
    pub country_code: Option<String>,
    pub alt_text: Option<String>,
    pub extended_description: Option<String>,
    /// The IPTC Content fields other editors write (headline, description writer, legacy category).
    pub headline: Option<String>,
    pub description_writer: Option<String>,
    pub category: Option<String>,
    /// The supplemental (other) categories (`photoshop:SupplementalCategories`).
    pub supplemental_categories: Vec<String>,
    /// The IPTC subject codes (`Iptc4xmpCore:SubjectCode`).
    pub subject_code: Vec<String>,
    /// `exif:GPSLatitude` / `exif:GPSLongitude` (latitude, longitude): wins over a position embedded in the file.
    pub gps: Option<(f64, f64)>,
    pub keywords: Option<Vec<String>>,
    pub regions: Option<Vec<lcc_meta::Region>>,
    /// The size the regions were drawn on, when they don't say how the photo was turned
    /// ([`lcc_meta::XmpData::regions_drawn_on`]), and how the photo's stored pixels turn upright: the
    /// sidecar's `tiff:Orientation`, or the file's own Exif orientation where the caller knows it. With both,
    /// [`merge_into`] puts boxes drawn on the photo as stored where the upright photo has them (#2674).
    pub regions_drawn_on: Option<(f64, f64)>,
    pub orientation: Option<lcc_meta::Orientation>,
    /// The packet's `tiff:Orientation` when it carries another editor's develop settings (`crs:`): how that editor
    /// turns the stored pixels upright, the file's own orientation and the user's rotation together. The import
    /// turns the photo by what it adds to the file's orientation ([`SidecarData::turn_from`]).
    pub turned_to: Option<lcc_meta::Orientation>,
    /// Capture time (ISO 8601) from `exif:DateTimeOriginal`, `photoshop:DateCreated` or
    /// `xmp:CreateDate` (first found). Used only when the file itself has no capture time.
    pub captured: Option<String>,
    pub develop: Option<DevelopPatch>,
    /// The flat-field map our own settings (`lc:settings`, portable) carried; whoever applies the settings stores it
    /// first ([`SidecarData::vignetting_calibration_ops`]).
    pub vignetting_maps: Vec<(lcc_develop::VignettingMapId, lcc_develop::EmbeddedVignettingMap)>,
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
        contact: (!m.contact.is_empty()).then(|| m.contact.clone()),
        location: lc("location").or_else(|| m.sublocation.clone()),
        city: m.city.clone(),
        state: m.state.clone(),
        country: m.country.clone(),
        country_code: m.country_code.clone(),
        alt_text: m.alt_text.clone(),
        extended_description: m.extended_description.clone(),
        headline: m.headline.clone(),
        description_writer: m.description_writer.clone(),
        category: m.category.clone(),
        supplemental_categories: m.supplemental_categories.clone(),
        subject_code: m.subject_code.clone(),
        gps: m.gps.map(|g| (g.latitude, g.longitude)),
        keywords: (!m.keywords.is_empty() || !m.hierarchical_keywords.is_empty())
            .then(|| lcc_catalog::keywords::from_file(&m.keywords, &m.hierarchical_keywords)),
        // A sidecar that has `mwg-rs:Regions` at all (even an empty list) was written by an app that
        // knows about regions, so it's authoritative: its list, empty or not, replaces the catalog's.
        // One without it (most writers, the app's own included, which keeps another app's
        // `mwg-rs:Regions` byte for byte but never writes one) says nothing about regions, and the
        // photo's are kept.
        regions: d.values.contains_key("mwg-rs:Regions").then(|| m.regions.clone()),
        regions_drawn_on: d.regions_drawn_on(),
        // (only what the regions need: a packet that says nothing else still says nothing)
        orientation: d.regions_drawn_on().and(m.orientation),
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
    let full = d.lc_settings.as_deref().and_then(|j| serde_json::from_str::<Value>(j).ok()).and_then(|mut v| {
        // (a flat-field map comes with them: taken out, the settings reference it)
        out.vignetting_maps.extend(lcc_develop::absorb_portable(&mut v));
        DevelopSettings::from_untrusted_json(&v).ok()
    });
    out.develop = match full {
        Some(s) => Some(DevelopPatch::Full(Box::new(s))),
        None if crate::crs::has_adjustments(&d.properties) => {
            let (partial, unmapped) = crate::crs::to_partial_report(&d.properties, Some(&d.values), target, crate::crs_masks::DEFAULT_ASPECT);
            // Lightroom's default profiles are named by nearly every sidecar; our own starting look stands
            // in for them, so only another profile is worth a warning, and not one an installed calibration
            // has a look for (the photo picks it up through `profile.camera`)
            let profile = crate::crs::lightroom_profile_name(&d.properties)
                .filter(|name| !matches!(name.trim(), "Adobe Standard" | "Adobe Color"))
                .filter(|name| !calibrated_look_for(&d.properties, name))
                .map(str::to_string);
            if let Some(name) = profile {
                out.warnings.push(crate::import::ImportWarning {
                    path: String::new(),
                    code: crate::import::ImportWarningCode::CameraProfile,
                    profile: Some(name),
                    mode: None,
                    fields: Vec::new(),
                    photo: None,
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
                // Lightroom keeps its Upright analysis when Upright is Off but doesn't apply it:
                // nothing is lost then; Auto, Level, Vertical and Full apply Lightroom's own matrix
                // (`geometry.lrc_upright`), so only a mode without a usable one is redone here
                let applied = partial.pointer("/geometry/lrc_upright").is_some_and(|v| !v.is_null());
                if mode != crate::import::ImportUprightMode::Off && !applied {
                    out.warnings.push(crate::import::ImportWarning {
                        path: String::new(),
                        code: crate::import::ImportWarningCode::UprightGeometry,
                        profile: None,
                        mode: Some(mode),
                        fields: Vec::new(),
                        photo: None,
                    });
                }
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
                    photo: None,
                });
            }
            Some(DevelopPatch::Partial(partial))
        }
        None => None,
    };
    if matches!(out.develop, Some(DevelopPatch::Partial(_))) {
        out.turned_to = m.orientation;
    }
    Ok(out)
}

/// Apply sidecar data to a photo record (the sidecar wins, except that a capture time embedded in
/// the file is kept). Returns true if develop changed.
/// Whether the photo's camera has a calibrated look for Lightroom profile `name`: the packet's own camera
/// (`tiff:Make` / `tiff:Model`) when it names one, else any installed calibration (a sidecar is parsed
/// before its photo's camera is known).
fn calibrated_look_for(props: &crate::crs::Props, name: &str) -> bool {
    let id = lcc_camdb::calibration::look_id(name);
    let field = |k: &str| props.get(k).and_then(|v| v.first()).map(|s| s.trim()).filter(|s| !s.is_empty());
    match field("tiff:Model") {
        Some(model) => lcc_raw::dcp::calibrated_for(field("tiff:Make"), model).is_some_and(|c| c.looks.iter().any(|(i, _)| *i == id)),
        None => lcc_raw::dcp::calibrated_look_exists(&id),
    }
}

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
    if let Some(c) = &sc.contact {
        m.contact.clone_from(c);
    }
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
        (&sc.country_code, &mut m.country_code),
        (&sc.alt_text, &mut m.alt_text),
        (&sc.extended_description, &mut m.extended_description),
        (&sc.headline, &mut m.headline),
        (&sc.description_writer, &mut m.description_writer),
        (&sc.category, &mut m.category),
    ] {
        if let Some(v) = src {
            *dst = v.clone();
        }
    }
    if !sc.supplemental_categories.is_empty() {
        m.supplemental_categories = sc.supplemental_categories.clone();
    }
    if !sc.subject_code.is_empty() {
        m.subject_code = sc.subject_code.clone();
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
        // (the photo's size is the upright one)
        if let (Some(drawn_on), Some(o)) = (sc.regions_drawn_on, sc.orientation) {
            let stored = if o.swaps_axes() { (p.height, p.width) } else { (p.width, p.height) };
            lcc_meta::upright_regions(&mut m.regions, drawn_on, o, stored);
        }
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
            let mut out = lcc_develop::apply_partial(&p.develop, &v, 1.0);
            crate::crs::keep_chosen_dcp(&p.develop, &mut out);
            out
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
    /// The ops that store the flat-field map these settings carried in `cat` (none when it has it already): ahead of
    /// the ops that apply them. The map links to no photo; the catalog finds its reference by the raw's SHA-1.
    pub fn vignetting_calibration_ops(&self, cat: &lcc_catalog::Catalog, now: &str) -> Vec<Op> {
        crate::vignetting_calibration_io::add_ops(cat, &self.vignetting_maps, now)
    }

    /// The patch with the rotation [`SidecarData::turned_to`] adds to `file` (the orientation the photo file states,
    /// which the decode applies): Lightroom writes a photo turned in it as the whole orientation. Only another
    /// editor's settings take it; the app's own (`lc:settings`) carry their rotation already.
    pub fn turn_from(mut self, file: Option<lcc_meta::Orientation>) -> Self {
        if let (Some(to), Some(DevelopPatch::Partial(v))) = (self.turned_to, self.develop.as_mut())
            && let Some(o) = v.as_object_mut()
            && let Ok(turn) = serde_json::to_value(file.unwrap_or_default().inverse().then(to))
        {
            o.insert("orientation".into(), turn);
        }
        self
    }

    /// The patch with a Lightroom crop, spots and mask shapes put in the app's frame: `crs:CropLeft` … `CropBottom` are two opposite corners
    /// of the straightened crop on the stored image, before its orientation (the sidecar's whole orientation, else
    /// the file's) and the straighten turn, as in a Lightroom catalog
    /// ([`crate::lightroom_catalog::catalog_crop`]). `upright` is the photo's size turned by the file's orientation.
    /// Only another editor's settings take it; the app's own (`lc:settings`) are in its frame already.
    pub fn crop_from(mut self, file: Option<lcc_meta::Orientation>, upright: (u32, u32)) -> Self {
        let file = file.unwrap_or_default();
        let (w, h) = if file.swaps_axes() { (upright.1, upright.0) } else { upright };
        if let Some(DevelopPatch::Partial(v)) = self.develop.as_mut()
            && w > 0
            && h > 0
        {
            let o = self.turned_to.unwrap_or(file);
            orient_lightroom_patch(v, o, f64::from(w) / f64::from(h), f64::from(upright.0) / f64::from(upright.1));
        }
        self
    }
}

/// Put a Lightroom patch's crop, spots and mask shapes (on the stored image, of aspect `stored`) in the app's frame
/// for orientation `o`. `photo_aspect` is the photo record's width / height: [`merge_into`] refits radials from
/// [`crate::crs_masks::DEFAULT_ASPECT`] to it, so they are left in those terms, fitted to the turned image once it has.
pub(crate) fn orient_lightroom_patch(v: &mut Value, o: lcc_meta::Orientation, stored: f64, photo_aspect: f64) {
    if !(stored.is_finite() && stored > 0.0) {
        return;
    }
    crate::lightroom_catalog::catalog_crop(v, o, stored);
    if o != lcc_meta::Orientation::Normal {
        use crate::crs_masks::{DEFAULT_ASPECT, orient_geometry, refit_radials};
        refit_radials(v, DEFAULT_ASPECT, stored);
        orient_geometry(v, o, stored);
        if photo_aspect.is_finite() && photo_aspect > 0.0 {
            refit_radials(v, photo_aspect, DEFAULT_ASPECT);
        }
    }
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

    /// Take the orientation of the photo file at `original` (the app shows the photo turned by it, and the packet
    /// may not repeat it) when the regions need it: they were drawn on a stated size ([`SidecarData::regions_drawn_on`]).
    /// Only then is the file read ([`file_orientation`]).
    pub fn with_file_orientation(self, original: &str) -> Self {
        if self.regions_drawn_on.is_none() {
            return self;
        }
        SidecarData { orientation: file_orientation(original).or(self.orientation), ..self }
    }
}

/// The orientation the photo file at `path` states, as [`lcc_meta::extract`] reads it from the whole file (the
/// import's): Exif's, else XMP's. A file shorter than [`ORIENTATION_SPAN`] is read whole. Most others state it in the
/// Exif of their first [`ORIENTATION_SPAN`]; when that part doesn't, the Exif may come later ([`late_orientation`]).
/// A WebP's chunks are always walked, since its last EXIF chunk is the one that counts ([`webp_orientation`]).
fn file_orientation(path: &str) -> Option<lcc_meta::Orientation> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    (&mut file).take(ORIENTATION_SPAN).read_to_end(&mut head).ok()?;
    if (head.len() as u64) < ORIENTATION_SPAN {
        return lcc_meta::extract(&head).orientation;
    }
    if head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return webp_orientation(&mut file);
    }
    if let Some(o) = exif_orientation(&head) {
        return Some(o);
    }
    late_orientation(&mut file, &head).or_else(|| lcc_meta::extract(&head).orientation)
}

/// The orientation the Exif in `bytes` (a file, or the start of one) states, leaving out a separate XMP packet (the
/// one the Exif holds in tag 700 counts, as in [`lcc_meta::read_exif`]).
fn exif_orientation(bytes: &[u8]) -> Option<lcc_meta::Orientation> {
    lcc_meta::embedded(bytes).exif.as_deref().map(lcc_meta::read_exif).and_then(|m| m.orientation)
}

/// The orientation of a JPEG or TIFF whose `head` has no Exif stating it, found by the file's structure without reading
/// its image data (a JPEG's Exif can follow other long segments, and many TIFF writers put the first IFD after the
/// strips). As the import does, a JPEG takes its first Exif segment, else its first XMP segment.
fn late_orientation(file: &mut (impl Read + Seek), head: &[u8]) -> Option<lcc_meta::Orientation> {
    let len = file.seek(SeekFrom::End(0)).ok()?;
    file.rewind().ok()?;
    match head.get(..4)? {
        [0xff, 0xd8, ..] => {
            let (mut exif_seen, mut xmp) = (false, None);
            let exif = read_jpeg_app1(file, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS, |segment| {
                let embedded = lcc_meta::embedded(segment);
                if let Some(exif) = embedded.exif
                    && !exif_seen
                {
                    // the first Exif segment ends the walk if it states one; later ones don't count
                    exif_seen = true;
                    return lcc_meta::read_exif(&exif).orientation;
                }
                if xmp.is_none() {
                    xmp = embedded.xmp.map(|x| lcc_meta::parse_xmp(&x).ok().and_then(|x| x.metadata.orientation));
                }
                None
            });
            exif.or(xmp.flatten())
        }
        b"II*\0" | b"MM\0*" => tiff_orientation(file, 0, len),
        _ => None,
    }
}

/// A WebP's orientation as the import takes it: from its last `EXIF` chunk, else its last `XMP ` chunk, in whichever
/// order they come. The chunk headers are walked from the start of `file` (at most [`WEBP_CHUNK_STEPS`], ending at a
/// chunk that runs past the end of the file); then only the first IFD of the EXIF chunk is read, whatever its size
/// ([`tiff_orientation`]), and the XMP chunk if it is at most [`ORIENTATION_SPAN`].
fn webp_orientation(file: &mut (impl Read + Seek)) -> Option<lcc_meta::Orientation> {
    let len = file.seek(SeekFrom::End(0)).ok()?;
    let (mut exif, mut xmp) = (None, None);
    let mut at = 12u64;
    for _ in 0..WEBP_CHUNK_STEPS {
        let mut header = [0; 8];
        if file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut header).is_err() {
            break;
        }
        let [a, b, c, d, s0, s1, s2, s3] = header;
        let size = u64::from(u32::from_le_bytes([s0, s1, s2, s3]));
        let Some(end) = at.checked_add(8 + size).filter(|&end| end <= len) else { break };
        match &[a, b, c, d] {
            b"EXIF" => exif = Some((at + 8, end)),
            b"XMP " => xmp = Some((at + 8, size)),
            _ => {}
        }
        // chunks are padded to an even size
        at = end + (size & 1);
    }
    if let Some((start, end)) = exif {
        // the TIFF stream, after an `Exif\0\0` if the chunk starts with one (as `strip_exif_header` takes it)
        let mut prefix = [0; 5];
        file.seek(SeekFrom::Start(start)).ok()?;
        let prefixed = start + 6 <= end && file.read_exact(&mut prefix).is_ok() && &prefix == b"Exif\0";
        if let Some(o) = tiff_orientation(file, if prefixed { start + 6 } else { start }, end) {
            return Some(o);
        }
    }
    let (start, size) = xmp?;
    xmp_orientation(file, start, size)
}

/// How many chunks [`webp_orientation`] passes over (an animated WebP has one per frame).
const WEBP_CHUNK_STEPS: usize = 4096;

/// The orientation the TIFF stream from `start` to `end` in `file` states in its first IFD (a TIFF file, or the Exif
/// block of a WebP's EXIF chunk; its offsets count from `start`, and nothing past `end` is read). Only the IFD's 12-byte
/// entries are read (at most 65535 of them, under 1 MiB): its orientation tag, else (as [`lcc_meta::read_exif`]
/// does) the XMP packet its tag 700 holds, read if it is at most [`ORIENTATION_SPAN`].
fn tiff_orientation(file: &mut (impl Read + Seek), start: u64, end: u64) -> Option<lcc_meta::Orientation> {
    // where the `len` bytes at `offset` in the stream are in the file, when all of them are in the stream
    let within = |offset: u32, len: u64| {
        let at = start.checked_add(u64::from(offset))?;
        (at.checked_add(len)? <= end).then_some(at)
    };
    file.seek(SeekFrom::Start(within(0, 8)?)).ok()?;
    let mut header = [0; 8];
    file.read_exact(&mut header).ok()?;
    let [b0, b1, b2, b3, i0, i1, i2, i3] = header;
    let le = match &[b0, b1, b2, b3] {
        b"II*\0" => true,
        b"MM\0*" => false,
        _ => return None,
    };
    let u16_of = |b: [u8; 2]| if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) };
    let u32_of = |b: [u8; 4]| if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) };
    let ifd = u32_of([i0, i1, i2, i3]);
    file.seek(SeekFrom::Start(within(ifd, 2)?)).ok()?;
    let mut count = [0; 2];
    file.read_exact(&mut count).ok()?;
    let count = u16_of(count);
    within(ifd, 2 + 12 * u64::from(count))?;
    let mut entries = vec![0; usize::from(count) * 12];
    file.read_exact(&mut entries).ok()?;
    let mut xmp = None;
    for &[t0, t1, y0, y1, c0, c1, c2, c3, v0, v1, v2, v3] in entries.as_chunks::<12>().0 {
        let (tag, kind, count) = (u16_of([t0, t1]), u16_of([y0, y1]), u32_of([c0, c1, c2, c3]));
        match (tag, kind) {
            // exactly one SHORT (with more, the field also holds a second value, or an offset to them all)
            (274, 3) if count == 1 => {
                let v = u16_of([v0, v1]);
                if (1..=8).contains(&v) {
                    return Some(lcc_meta::Orientation::from_exif(v));
                }
            }
            // BYTE or UNDEFINED, at an offset (a packet longer than the field's four bytes)
            (700, 1 | 7) if count > 4 => xmp = Some((u32_of([v0, v1, v2, v3]), u64::from(count))),
            _ => {}
        }
    }
    let (offset, len) = xmp?;
    xmp_orientation(file, within(offset, len)?, len)
}

/// The orientation the XMP packet of `len` bytes at `at` in `file` states, read if it is at most [`ORIENTATION_SPAN`].
fn xmp_orientation(file: &mut (impl Read + Seek), at: u64, len: u64) -> Option<lcc_meta::Orientation> {
    if len > ORIENTATION_SPAN {
        return None;
    }
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut packet = Vec::new();
    (&mut *file).take(len).read_to_end(&mut packet).ok()?;
    lcc_meta::parse_xmp(&String::from_utf8_lossy(&packet)).ok()?.metadata.orientation
}

/// How much of a photo file [`file_orientation`] reads first.
const ORIENTATION_SPAN: u64 = 1 << 20;

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
    // Names in `dc:subject`, paths in `lr:hierarchicalSubject` (`keywords::to_file`).
    let keywords = lcc_catalog::keywords::to_file(&p.meta.keywords);
    let meta = lcc_meta::Metadata {
        software: Some(lcc_brand::DISPLAY_NAME.into()),
        title: nz(&p.meta.title),
        headline: nz(&p.meta.headline),
        caption: nz(&p.meta.caption),
        description_writer: nz(&p.meta.description_writer),
        category: nz(&p.meta.category),
        supplemental_categories: p.meta.supplemental_categories.clone(),
        subject_code: p.meta.subject_code.clone(),
        alt_text: nz(&p.meta.alt_text),
        extended_description: nz(&p.meta.extended_description),
        sublocation: nz(&p.meta.location),
        city: nz(&p.meta.city),
        state: nz(&p.meta.state),
        country: nz(&p.meta.country),
        country_code: nz(&p.meta.country_code),
        copyright: nz(&p.meta.copyright),
        copyright_marked: p.meta.copyright_status.marked(),
        usage_terms: nz(&p.meta.usage_terms),
        copyright_url: nz(&p.meta.copyright_url),
        artist: nz(&p.meta.creator),
        contact: p.meta.contact.clone(),
        keywords: keywords.flat,
        hierarchical_keywords: keywords.hierarchical,
        rating: Some(p.rating.min(5) as i8),
        label: p.label.map(|l| cat.label_name(l)),
        // (never the file's modification time standing in for a capture time nobody stated)
        capture_time: p.captured_stated().and_then(lcc_meta::DateTime::parse_iso),
        gps: p.meta.gps.filter(|_| gps).map(|(latitude, longitude)| lcc_meta::Gps { latitude, longitude, altitude: None }),
        ..Default::default()
    };
    let mut meta = meta;
    if remove_location {
        (meta.sublocation, meta.city, meta.state, meta.country, meta.country_code, meta.gps) = (None, None, None, None, None, None);
    }
    // the pixel-filter stack goes in its own namespace, not in `lc:settings`
    let filters: Vec<lcc_meta::XmpPluginFilter> = p.develop.plugin_filters.iter().map(crate::cmd::plugin_filters::to_xmp).collect();
    // (a lens picked from the lens database stays in the catalog: not written to XMP yet, issue #307)
    let settings = if filters.is_empty() && p.develop.optics.lens.is_none() {
        serde_json::to_string(&*p.develop).unwrap_or_default()
    } else {
        let mut d = (*p.develop).clone();
        d.plugin_filters.clear();
        d.optics.lens = None;
        serde_json::to_string(&d).unwrap_or_default()
    };
    // portable: the flat-field map goes with the settings (another catalog reads it back); settings without one are
    // written exactly as before
    let settings = match p.develop.optics.vignetting_calibration.as_ref().and_then(|id| cat.vignetting_map(id)) {
        Some(r) => {
            let mut v: Value = serde_json::from_str(&settings).unwrap_or(Value::Null);
            lcc_develop::embed_portable(&mut v, |_| Some(r.map.clone()));
            serde_json::to_string(&v).unwrap_or(settings)
        }
        None => settings,
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
    "lr:hierarchicalSubject",
    "Iptc4xmpCore:Location",
    "Iptc4xmpCore:AltTextAccessibility",
    "Iptc4xmpCore:ExtDescrAccessibility",
    "photoshop:City",
    "photoshop:State",
    "photoshop:Country",
    "xmpRights:Marked",
    "xmpRights:UsageTerms",
    "xmpRights:WebStatement",
    "photoshop:AuthorsPosition",
    "Iptc4xmpCore:CreatorContactInfo",
    "lc:*",
    "lcvid:*",
    "lcpx:*",
];

/// Properties replaced only when the app has a value (capture time, GPS): otherwise the
/// sidecar's own value stays. Anything else the app
/// writes (`xmp:CreatorTool`) goes only into new sidecars.
pub const OWNED_IF_STATED: &[&str] = &["exif:DateTimeOriginal", "photoshop:DateCreated", "exif:GPSLatitude", "exif:GPSLongitude"];

/// Merge the app's fresh packet (`fresh`) into an XMP packet another writer produced (`existing`)
/// the way a sidecar save does: the app's properties ([`OWNED`], [`OWNED_IF_STATED`]) replace
/// theirs, everything else — another editor's `crs:` settings, history, unknown namespaces — is
/// kept. An empty `existing` takes `fresh`; an unreadable one is an error, never a silent
/// replacement (the caller leaves the other side as it is).
///
/// Used by IMM-SIDECARS to fill Immich's sidecar with the app's metadata and develop settings
/// without dropping what Immich or another writer already had.
pub fn merge_packet(existing: &str, fresh: &str) -> std::result::Result<String, String> {
    if existing.trim().is_empty() {
        return Ok(fresh.to_string());
    }
    let rules = lcc_meta::MergeRules { owned: OWNED, owned_if_present: OWNED_IF_STATED };
    lcc_meta::merge_xmp(existing, fresh, rules).map_err(|e| e.to_string())
}

/// Write `data` to `path` atomically: a temp file next to it, synced, then renamed over it.
fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    lcc_catalog::safe_file::write_atomic(path, data)
}

/// Copy `path` to a new `<name>.bak-<stamp>` (`-1`, `-2`… if taken; never overwriting).
fn backup(path: &Path, stamp: &str) -> std::io::Result<PathBuf> {
    use std::io::Write;
    // never a link's target: a sidecar linked to a private file would be copied next to the photo (issue #2122)
    let bytes = lcc_catalog::safe_file::read_no_follow(path)?;
    let stamp: String = stamp.chars().filter(char::is_ascii_alphanumeric).collect();
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    for k in 0..1000 {
        let b = path.with_file_name(if k == 0 { format!("{name}.bak-{stamp}") } else { format!("{name}.bak-{stamp}-{k}") });
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&b) {
            Ok(mut f) => {
                f.write_all(&bytes)?;
                lcc_catalog::safe_file::sync_file(&f, false)?;
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

/// The sidecar to write among `candidates` (the spellings an existing packet may have), else `preferred`. An existing
/// packet keeps its spelling and style, so foreign properties survive the save and later Undo / Redo writes. On a
/// case-insensitive volume `a.xmp` also answers for `a.XMP`: the spelling the folder lists comes first, so the atomic
/// write doesn't rename the packet.
fn sidecar_target(candidates: Vec<PathBuf>, preferred: PathBuf) -> PathBuf {
    use crate::rename::MoveFs;
    let existing: Vec<PathBuf> = candidates.into_iter().filter(|path| path.is_file()).collect();
    let listed = existing.iter().find(|path| crate::rename::RealFs.listed(path)).cloned();
    listed.or_else(|| existing.into_iter().next()).unwrap_or(preferred)
}

/// The new contents for photo `p`'s sidecar at `path`: its packet merged into the existing file,
/// if any. Returns (contents, merged, backup).
fn sidecar_contents(path: &Path, p: &Photo, cat: &lcc_catalog::Catalog, stamp: &str) -> std::io::Result<(String, bool, Option<PathBuf>)> {
    merged_contents(path, p, &Packets::of(p, cat), stamp)
}

/// A photo's fresh packets: the whole one, and the one without its GPS position (merged into a sidecar that already
/// states that position, or one cleared here). Made from the catalog, so a background write needs no catalog.
pub(crate) struct Packets {
    pub(crate) with_gps: String,
    pub(crate) without_gps: String,
}

impl Packets {
    pub(crate) fn of(p: &Photo, cat: &lcc_catalog::Catalog) -> Packets {
        Packets { with_gps: packet(p, cat, false, true), without_gps: packet(p, cat, false, false) }
    }
}

/// [`sidecar_contents`] from the photo's packets already made. A capture time of a year or a month alone is stated
/// in `photoshop:DateCreated` only, so an `exif:DateTimeOriginal` already there (an earlier full time) is removed
/// rather than left to outrank it on the next read.
fn merged_contents(path: &Path, p: &Photo, packets: &Packets, stamp: &str) -> std::io::Result<(String, bool, Option<PathBuf>)> {
    let bytes = match read_capped(path, SIDECAR_READ_MAX) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((packets.with_gps.clone(), false, None)),
        // can't see what is there: never overwrite it blindly
        Err(e) => return Err(e),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((packets.with_gps.clone(), false, None));
    }
    let why = match std::str::from_utf8(&bytes) {
        Ok(old) => {
            let gps = GpsOnSave::of(p, old);
            let partial_capture = p.captured.as_deref().and_then(lcc_catalog::dates::partial_capture).is_some();
            let owned: Vec<&str> = OWNED.iter().chain(gps.owned_tags()).copied().chain(partial_capture.then_some("exif:DateTimeOriginal")).collect();
            let rules = lcc_meta::MergeRules { owned: &owned, owned_if_present: OWNED_IF_STATED };
            let fresh = if gps.write() { &packets.with_gps } else { &packets.without_gps };
            match lcc_meta::merge_xmp(old, fresh, rules) {
                Ok(merged) => return Ok((merged, true, None)),
                // a readable sidecar whose merge fails (e.g. the merged packet exceeds the XMP limits), or one
                // that is valid but over the parser's complexity limits: fail the save and keep the file as it
                // is, rather than replace metadata that could be read (upstream #3696). Text with no `rdf:RDF`
                // element parses as empty metadata but holds nothing to keep: it is replaced after a backup,
                // as any sidecar that is not XMP.
                Err(why)
                    if why == lcc_meta::XmpError::TooComplex
                        || (lcc_meta::parse_xmp(old.strip_prefix('\u{feff}').unwrap_or(old)).is_ok()
                            && !matches!(&why, lcc_meta::XmpError::Xml(m) if m.contains("no rdf:RDF element"))) =>
                {
                    return Err(std::io::Error::other(format!("the sidecar is readable but could not be merged ({why}); it was left unchanged")));
                }
                Err(why) => why.to_string(),
            }
        }
        Err(why) => why.to_string(),
    };
    let b = backup(path, stamp)?;
    log::warn!("{}: not readable as XMP ({why}); kept a copy as {} before replacing it", path.display(), b.display());
    Ok((packets.with_gps.clone(), false, Some(b)))
}

/// Make a background sidecar write ([`crate::sidecar_writer`]): the packet merged into an existing one and written
/// atomically. Returns the path and the bytes written; `None` when the original is gone.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn write_job(job: &crate::sidecar_writer::Job) -> std::result::Result<Option<(PathBuf, Vec<u8>)>, String> {
    // the photo's file moved or went after this was queued: its sidecar is written where it is now, by the auto-write
    // the move itself makes
    if !job.original.exists() {
        return Ok(None);
    }
    let path = sidecar_target(job.candidates.clone(), job.preferred.clone());
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let (data, _, _) = merged_contents(&path, &job.photo, &job.packets, &job.stamp).map_err(err)?;
    write_atomic(&path, data.as_bytes()).map_err(err)?;
    Ok(Some((path, data.into_bytes())))
}

/// The GPS tags a sidecar can state, all removed together when a cleared position is saved.
const GPS_TAGS: &[&str] = &["exif:GPSLatitude", "exif:GPSLongitude", "exif:GPSAltitude", "exif:GPSAltitudeRef"];

/// What saving does with the position of a photo whose existing sidecar is `old`.
enum GpsOnSave {
    /// The library's position is written, or none is stated by either.
    Write,
    /// The sidecar already states the library's position (to about a centimetre): it stays as the file writes it, in its
    /// own precision and form, rather than being rewritten from the library's copy, so GPS read from a sidecar goes
    /// back unchanged unless it was changed here.
    Keep,
    /// The position was cleared here and the sidecar still states exactly that one: its GPS tags go. A different position
    /// there was written by another application after the clear, and stays (so does one the library never had).
    Remove,
}

impl GpsOnSave {
    fn of(p: &Photo, old: &str) -> GpsOnSave {
        let stated = lcc_meta::parse_xmp(old).ok().and_then(|d| d.metadata.gps).map(|g| (g.latitude, g.longitude));
        let same = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-7 && (a.1 - b.1).abs() < 1e-7;
        match (p.meta.gps, p.meta.gps_removed, stated) {
            (Some(own), _, Some(theirs)) if same(own, theirs) => GpsOnSave::Keep,
            (None, Some(removed), Some(theirs)) if same(removed, theirs) => GpsOnSave::Remove,
            _ => GpsOnSave::Write,
        }
    }

    /// Whether the fresh packet states the library's position.
    fn write(&self) -> bool {
        matches!(self, GpsOnSave::Write)
    }

    /// The tags the merge replaces from the fresh packet (which states none) on top of the always-owned ones.
    fn owned_tags(&self) -> &'static [&'static str] {
        if matches!(self, GpsOnSave::Remove) { GPS_TAGS } else { &[] }
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

pub(crate) fn file_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

/// The XMP packet embedded in a JPEG's header, read as [`read_packet`] reads it from the file.
pub(crate) fn jpeg_header_xmp(bytes: &[u8]) -> Option<String> {
    read_jpeg_xmp(&mut std::io::Cursor::new(bytes), JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS)
}

/// Most bytes of a sidecar read whole (it must be a regular file).
const SIDECAR_READ_MAX: u64 = 256 << 20;

/// [`crate::bounded_read::read_bounded`] as an `io::Result`, keeping `NotFound` distinguishable.
fn read_capped(path: &Path, max: u64) -> std::io::Result<Vec<u8>> {
    use crate::bounded_read::ReadError;
    crate::bounded_read::read_bounded(path, max).map_err(|e| match e {
        ReadError::Io(e) => e,
        other => std::io::Error::other(other.to_string()),
    })
}

/// The XMP packet that describes a photo on disk: its sidecar, else embedded raw/DNG or JPEG XMP.
pub fn read_packet(original: &str, kind: MediaKind, naming: SidecarNaming) -> Option<(String, PathBuf)> {
    if let Some(sc) = find_sidecar(original, naming) {
        // A discovered sidecar keeps precedence even when reading it fails.
        return read_capped(&sc, SIDECAR_READ_MAX).ok().and_then(|b| String::from_utf8(b).ok()).map(|s| (s, sc));
    }
    if kind == MediaKind::Raw {
        let bytes = read_capped(original.as_ref(), crate::bounded_read::MAX_MEDIA_BYTES).ok()?;
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
    read_jpeg_app1(reader, header_span, marker_steps, |segment| lcc_meta::jpeg_segments(segment).xmp)
}

/// Like [`read_jpeg_xmp`], stopping at the first APP1 segment (in its SOI+APP1 wrapper) that `pick` finds something in.
fn read_jpeg_app1<T>(reader: &mut (impl Read + Seek), header_span: u64, marker_steps: usize, mut pick: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
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
        if let Some(found) = pick(&segment) {
            return Some(found);
        }
    }
    None
}

/// Photo ids touched by an op (for auto-write).
pub(crate) fn op_photos(op: &Op, out: &mut Vec<PhotoId>) {
    match op {
        // every edit the sidecar carries: a kind left out here keeps its old value in the sidecar,
        // and a later folder sync would read that back over the edit
        Op::SetRating { id, .. }
        | Op::SetFlag { id, .. }
        | Op::SetLabel { id, .. }
        | Op::SetDevelop { id, .. }
        | Op::SetMeta { id, .. }
        | Op::SetCaptured { id, .. }
        | Op::SetVideoTrim { id, .. }
        | Op::SetVideoPoster { id, .. }
            if !out.contains(id) =>
        {
            out.push(*id);
        }
        Op::Batch { ops } => ops.iter().for_each(|o| op_photos(o, out)),
        _ => {}
    }
}

/// Where a file stands in the order a shared stem sidecar is owned by: a raw first, then the file
/// name in lower case, then the catalog id (a file an import is adding has no id yet, and ranks
/// last among files of its kind and name: see [`ImportStems`]).
type Rank = (bool, String, u64);

/// The rank of the file at `path`, of kind `kind`, catalogued as `id`.
fn rank_of(path: &str, kind: MediaKind, id: u64) -> Rank {
    (kind != MediaKind::Raw, path.to_lowercase(), id)
}

/// Which photo owns each stem sidecar that several catalogued files map to (see
/// [`Session::sidecar_naming`]).
pub(crate) struct StemOwners(HashMap<String, PhotoId>);

impl StemOwners {
    /// The owners of every shared stem in the library: what [`Session::stem_owners`] keeps built,
    /// so naming one photo is a hash lookup. The library is passed over once; each file's key is
    /// made the long way (only [`StemOwners::where_wanted`] can reuse one buffer, filtering as it
    /// goes). What [`StemOwners::among`] must agree with for the photos it is asked about.
    pub(crate) fn of(cat: &lcc_catalog::Catalog) -> StemOwners {
        let mut best: HashMap<String, (Rank, PhotoId, usize)> = HashMap::new();
        for p in cat.photos().filter(|p| p.copy_of.is_none()) {
            let Some(path) = file_path(p) else { continue };
            let rank = rank_of(path, p.kind, p.id.0);
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
        let mut best: HashMap<String, (Rank, PhotoId, usize)> = HashMap::new();
        let mut key = String::new();
        for p in cat.photos().filter(|p| p.copy_of.is_none()) {
            let Some(path) = file_path(p) else { continue };
            stem_key_into(&mut key, path);
            if !wanted.contains(key.as_str()) {
                continue;
            }
            let rank = rank_of(path, p.kind, p.id.0);
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

/// Which of the files an import is adding may read the shared stem sidecar (`IMG_0001.xmp`) of
/// their stem, and which read their own `<file>.xmp` (`IMG_0001.TIF.xmp`) instead (issue #1905:
/// before, a JPEG or a TIFF imported beside its raw read the raw's packet and took its rating,
/// metadata and develop settings). The order is the one [`StemOwners`] applies to writing, with
/// the arriving files ranked against the library and against each other: a raw first, then the
/// file name in lower case. A file that is not catalogued yet has no id, and the id it draws when
/// the import commits is above every id in the library, so it loses a tie against a catalogued
/// file; between arriving files the order they arrive in stands for it.
pub(crate) struct ImportStems {
    /// The arriving files that own the stem sidecar of their stem, by path as it arrives.
    owners: HashSet<String>,
}

impl ImportStems {
    /// Work out [`ImportStems`] for the files `incoming`, in the order they arrive. `None` for a
    /// library already looked over (a job prepared twice): there is nothing to rank against, so
    /// every arriving file reads as it did before. The library is passed over once, and only the
    /// stems the arriving files map to are looked at closer.
    pub(crate) fn for_import<'a>(cat: Option<&lcc_catalog::Catalog>, incoming: impl Iterator<Item = (&'a str, MediaKind)>) -> ImportStems {
        // per stem: the best of the library's files, and the best arriving file with its path
        let mut best: HashMap<String, (Option<Rank>, Option<(Rank, &'a str)>)> = HashMap::new();
        let mut arriving_paths = HashSet::new();
        for (arriving, (path, kind)) in incoming.enumerate() {
            arriving_paths.insert(path);
            let e = best.entry(stem_key(path)).or_default();
            let rank = rank_of(path, kind, arriving as u64);
            if e.1.as_ref().is_none_or(|(b, _)| rank < *b) {
                e.1 = Some((rank, path));
            }
        }
        if let Some(cat) = cat {
            let mut key = String::new();
            for p in cat.photos().filter(|p| p.copy_of.is_none()) {
                // the arriving file's own record (a trashed photo imported again) is not a rival
                let Some(path) = file_path(p).filter(|path| !arriving_paths.contains(*path)) else { continue };
                stem_key_into(&mut key, path);
                let Some(e) = best.get_mut(key.as_str()) else { continue };
                let rank = rank_of(path, p.kind, p.id.0);
                if e.0.as_ref().is_none_or(|b| rank < *b) {
                    e.0 = Some(rank);
                }
            }
        }
        let owners = best
            .into_values()
            .filter_map(|(in_library, arriving)| {
                let (rank, path) = arriving?;
                let wins = in_library.as_ref().is_none_or(|b| (rank.0, rank.1.as_str()) < (b.0, b.1.as_str()));
                wins.then(|| path.to_string())
            })
            .collect();
        ImportStems { owners }
    }

    /// Whether the arriving file at `original` owns the stem sidecar of its stem.
    pub(crate) fn owns(&self, original: &str) -> bool {
        self.owners.contains(original)
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
    /// The sidecar naming for many photos, their stems' owners worked out in one pass over the
    /// catalog. For a photo asked about often — the grid's badges name a sidecar for every
    /// visible cell, every frame — [`Session::sidecar_naming`] keeps the whole table built
    /// instead.
    pub fn sidecar_namings(&self, ids: &[PhotoId]) -> Vec<SidecarNaming> {
        namings_in(&self.catalog, self.xmp.naming, ids)
    }

    /// The sidecar naming for photo `id`: the library's, except under Stem naming when another
    /// catalogued file shares the stem (`IMG_0001.CR3` + `IMG_0001.JPG`) and owns the stem
    /// sidecar — a raw first, else the first by file name. The others use Full naming
    /// (`IMG_0001.JPG.xmp`), so their metadata never overwrites each other.
    pub fn sidecar_naming(&self, id: PhotoId) -> SidecarNaming {
        match self.catalog.photo(id) {
            Some(p) if self.xmp.naming == SidecarNaming::Stem => self.stem_owners().naming(p, self.xmp.naming),
            _ => self.xmp.naming,
        }
    }

    /// The owners of every shared stem in the library, as the catalog stands at its current
    /// revision: built in one pass the first time, kept until the next applied op bumps the
    /// revision (a library loaded in this process takes a revision range of its own, so nothing
    /// of the last one's survives). Working the owners out is that whole-library pass — the grid's
    /// thumbnail badges made it per visible cell, per frame, which froze the UI on large libraries
    /// (issue #742).
    pub(crate) fn stem_owners(&self) -> std::sync::Arc<StemOwners> {
        let mut cached = self.stems.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((rev, owners)) = cached.as_ref()
            && *rev == self.catalog.revision
        {
            return owners.clone();
        }
        let owners = std::sync::Arc::new(StemOwners::of(&self.catalog));
        *cached = Some((self.catalog.revision, owners.clone()));
        owners
    }

    /// Write the photo's XMP sidecar, merged into an existing one (see the module docs).
    pub fn save_sidecar(&mut self, id: PhotoId) -> Result<SidecarSaved> {
        let owners = self.stem_owners();
        self.save_sidecar_with(id, &owners)
    }

    /// Where photo `id`'s sidecar goes: the preferred path and the spellings an existing packet may have (a shared stem
    /// owned by another photo left out). Catalog only; no file is looked at.
    fn sidecar_places(&self, id: PhotoId, owners: &StemOwners) -> Result<(PathBuf, Vec<PathBuf>)> {
        let places = self.sidecar_paths(id, owners)?;
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
        let orig = file_path(p).unwrap_or_default();
        if !Path::new(orig).is_file() {
            return Err(EngineError::Other(format!("{}: the original {orig} is missing; no XMP sidecar is written", p.file_name)));
        }
        Ok(places)
    }

    /// [`Self::sidecar_places`] from the catalog alone, without looking at the disk: a background write checks the
    /// original on its own thread, so an edit doesn't wait for a slow share.
    fn sidecar_paths(&self, id: PhotoId, owners: &StemOwners) -> Result<(PathBuf, Vec<PathBuf>)> {
        let p = self.catalog.photo(id).ok_or(lcc_catalog::CatalogError::NoPhoto(id))?;
        if p.copy_of.is_some() {
            return Err(EngineError::Other(format!("{} is a virtual copy: its settings live only in the catalog", p.file_name)));
        }
        let orig = file_path(p).ok_or_else(|| EngineError::Other(format!("{} is not a file on disk", p.file_name)))?;
        // A relative path would be taken from the working directory.
        if Path::new(orig).is_relative() {
            return Err(EngineError::Other(format!("{}: the original {orig} is missing; no XMP sidecar is written", p.file_name)));
        }
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
        Ok((preferred, candidates))
    }

    pub(crate) fn save_sidecar_with(&mut self, id: PhotoId, owners: &StemOwners) -> Result<SidecarSaved> {
        let (preferred, candidates) = self.sidecar_places(id, owners)?;
        // A separated packet is still the photo's metadata. Repair it before a fresh write
        // occupies its destination; a failure preserves the queue and both existing packets.
        self.repair_sidecars_for_paths(&candidates, &crate::rename::RealFs)
            .map_err(|e| EngineError::Other(format!("{}: unfinished XMP repair: {e}", preferred.display())))?;
        let path = sidecar_target(candidates, preferred);
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
        let sc = sc.with_file_orientation(orig);
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
        let mut ops = sc.vignetting_calibration_ops(&self.catalog, &(self.clock)());
        ops.extend([
            Op::SetRating { id, rating: q.rating },
            Op::SetFlag { id, flag: q.flag },
            Op::SetLabel { id, label: q.label },
            Op::SetMeta { id, meta: Box::new(q.meta.clone()) },
        ]);
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

    /// Whether the photo's sidecar holds a whole XMP packet (its closing `rdf:RDF` or
    /// `x:xmpmeta` tag is there). An empty or cut-off file, such as one another app is still
    /// writing, is not: automatic reads leave it for a later check rather than take it as "no
    /// metadata".
    pub(crate) fn sidecar_complete(&self, id: PhotoId) -> bool {
        const MAX: u64 = 16 << 20;
        let Some(f) = self.catalog.photo(id).and_then(|p| file_path(p)).and_then(|o| find_sidecar(o, self.sidecar_naming(id))) else {
            return false;
        };
        let Ok(m) = std::fs::metadata(&f) else { return false };
        if m.len() == 0 || m.len() > MAX {
            return false;
        }
        let Ok(bytes) = std::fs::read(&f) else { return false };
        let text = String::from_utf8_lossy(&bytes);
        text.contains("</rdf:RDF>") || text.contains("</x:xmpmeta>")
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
        // photos only browsed (not added) get no sidecar unless asked: looking at a folder changes nothing in it
        let browsed_too = self.xmp.write_browsed;
        #[cfg(not(target_arch = "wasm32"))]
        if self.sidecar_writer.is_some() && self.no_sidecar_repairs() {
            let stamp = (self.clock)();
            for id in ids {
                let Some(p) = self.catalog.photo(id).filter(|p| file_path(p).is_some() && p.copy_of.is_none() && (browsed_too || !p.local)) else {
                    continue;
                };
                let original = PathBuf::from(file_path(p).unwrap_or_default());
                let packets = Packets::of(p, &self.catalog);
                let photo = Box::new((**p).clone());
                match self.sidecar_paths(id, &owners) {
                    Ok((preferred, candidates)) => {
                        if let Some(w) = &self.sidecar_writer {
                            w.submit(crate::sidecar_writer::Job { id, original, preferred, candidates, photo, packets, stamp: stamp.clone() });
                        }
                    }
                    Err(e) => {
                        // the original is away (a share that dropped out): retried once it is back, as a failed write is
                        log::warn!("auto-write XMP: {e}");
                        self.unsaved_sidecars.insert(id);
                    }
                }
            }
            return;
        }
        let mut written = Vec::new();
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| file_path(p).is_some() && p.copy_of.is_none() && (browsed_too || !p.local)) {
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

    /// No sidecar move is waiting to be repaired: what a background write needs, since only the command path keeps the
    /// repairs' bookkeeping in step with a write.
    fn no_sidecar_repairs(&self) -> bool {
        self.pending_sidecar_repairs.is_empty() && self.undo.iter().chain(&self.redo).all(|e| e.retry_sidecars.is_empty())
    }

    /// Make automatic sidecar writes on a thread of their own (`on`), or in the command that changed the photo, as by
    /// default. The desktop app turns this on: on a network share a write is many round trips. Turning it off makes
    /// the writes still queued first.
    pub fn set_background_sidecar_writes(&mut self, on: bool) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if on && self.sidecar_writer.is_none() {
                self.sidecar_writer = crate::sidecar_writer::Writer::start(self.activity.clone());
            } else if !on && self.sidecar_writer.is_some() {
                self.flush_sidecar_writes();
                self.sidecar_writer = None;
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = on;
    }

    /// Background sidecar writes queued or being made.
    pub fn sidecar_writes_pending(&self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        return self.sidecar_writer.as_ref().map_or(0, crate::sidecar_writer::Writer::pending);
        #[cfg(target_arch = "wasm32")]
        0
    }

    /// Take in the background sidecar writes that finished: a failed one waits for a retry, as a failed write in a
    /// command does. Returns how many were written.
    pub fn poll_sidecar_writes(&mut self) -> usize {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(w) = &self.sidecar_writer else { return 0 };
            let mut written = 0;
            let mut stamped = Vec::new();
            for (id, original, result) in w.take_done() {
                match result {
                    Ok(Some((path, bytes))) => {
                        self.note_sidecar_write(&path, &bytes);
                        self.unsaved_sidecars.remove(&id);
                        stamped.push(id);
                        written += 1;
                    }
                    // the original wasn't there: when the photo is still at that path (a share that dropped out, not a
                    // move or a removal made here) the write waits for a retry, as a write made in the command does
                    Ok(None) => {
                        let same = self
                            .catalog
                            .photo(id)
                            .filter(|p| p.copy_of.is_none())
                            .and_then(|p| file_path(p))
                            .is_some_and(|f| Path::new(f) == original);
                        if same {
                            self.unsaved_sidecars.insert(id);
                        }
                    }
                    Err(e) => {
                        log::warn!("auto-write XMP: {e}");
                        self.unsaved_sidecars.insert(id);
                    }
                }
            }
            self.record_xmp_stamps(&stamped, false);
            written
        }
        #[cfg(target_arch = "wasm32")]
        0
    }

    /// Wait for the background sidecar writes queued so far (at most 30 s) and take them in: when the library closes
    /// or another opens, and before a retry. A write still stuck then is logged and left to finish.
    pub fn flush_sidecar_writes(&mut self) {
        if let Err(e) = self.settle_sidecar_writes() {
            log::warn!("{e}");
        }
    }

    /// [`Self::flush_sidecar_writes`] for a file operation the queued writes could race (a photo's file moving, a
    /// sidecar read or saved on request, a change of the sidecar preferences): a write still stuck after the wait (a
    /// network share that stopped answering) fails it, so nothing lands at an old path.
    pub(crate) fn settle_sidecar_writes(&mut self) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            let limit = self.sidecar_flush_limit;
            let stuck = self.sidecar_writer.as_ref().filter(|w| w.pending() > 0 && !w.wait_idle(limit)).map(|w| w.stuck_folder());
            self.poll_sidecar_writes();
            if let Some(folder) = stuck {
                let folder = folder.map_or_else(|| "the photos' folder".to_string(), |f| f.display().to_string());
                return Err(EngineError::Other(format!("XMP writes to {folder} are still running; try again in a moment")));
            }
        }
        Ok(())
    }

    /// How long [`Self::settle_sidecar_writes`] waits (tests make it short).
    #[cfg(all(test, unix))]
    pub(crate) fn set_sidecar_flush_limit(&mut self, limit: std::time::Duration) {
        self.sidecar_flush_limit = limit;
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
    /// Issue #2674: a WebP keeps its EXIF and XMP chunks after the image data, a TIFF's first IFD can follow its strips
    /// and a JPEG's Exif other long segments, so the orientation can be past the first [`super::ORIENTATION_SPAN`] of
    /// the file. Every file gives what the import gives reading it whole: Exif over XMP wherever each is, a WebP's last
    /// EXIF chunk and a JPEG's first Exif segment, and none for a file that states none.
    #[test]
    fn the_file_orientation_is_found_after_the_image_data() {
        let dir = std::env::temp_dir().join(format!("lcc-file-orientation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exif = lcc_meta::write_exif(&lcc_meta::Metadata { orientation: Some(lcc_meta::Orientation::Rotate90), ..Default::default() });
        let span = super::ORIENTATION_SPAN as usize;
        let (o6, o8) = (Some(lcc_meta::Orientation::Rotate90), Some(lcc_meta::Orientation::Rotate270));
        // a WebP: VP8X, then `chunks`
        let webp = |chunks: &[(&[u8], Vec<u8>)]| {
            let mut w = b"RIFF\0\0\0\0WEBP".to_vec();
            for (id, data) in [(&b"VP8X"[..], vec![0; 10])].iter().chain(chunks) {
                w.extend_from_slice(id);
                w.extend_from_slice(&(data.len() as u32).to_le_bytes());
                w.extend_from_slice(data);
                if data.len() % 2 == 1 {
                    w.push(0);
                }
            }
            w
        };
        let image = |len: usize| (&b"VP8L"[..], vec![0; len]);
        let exif_chunk = (&b"EXIF"[..], [&b"Exif\0\0"[..], &exif].concat());
        // an EXIF chunk over the span (orientation 6 too), one stating no orientation, and an XMP packet stating 8
        let huge_exif = (&b"EXIF"[..], [&b"Exif\0\0"[..], &exif, &vec![0; span]].concat());
        let plain = lcc_meta::write_exif(&lcc_meta::Metadata::default());
        let plain_chunk = (&b"EXIF"[..], [&b"Exif\0\0"[..], &plain].concat());
        let packet = lcc_meta::write_xmp(&lcc_meta::Metadata { orientation: o8, ..Default::default() }, None);
        let xmp_chunk = (&b"XMP "[..], packet.clone().into_bytes());
        // the XMP packet padded with whitespace to `len` bytes
        let padded_xmp = |len: usize| {
            let mut p = packet.clone().into_bytes();
            p.resize(len, b' ');
            (&b"XMP "[..], p)
        };
        // an EXIF chunk holding only a TIFF header, whose IFD offset (16) points past the chunk's end into the next
        // chunk's data, where an IFD stating orientation 6 sits
        let stray_exif = (&b"EXIF"[..], b"II*\0\x10\0\0\0".to_vec());
        let stray_ifd = (&b"JUNK"[..], vec![1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        // a TIFF whose first IFD follows more than the first MiB of image data: an orientation tag of `values` values
        // whose field starts with `value` (with more than two, it is an offset), a tag pointing elsewhere, and an XMP
        // packet (tag 700) stored after the IFD
        let tiff = |le: bool, orientation: Option<(u32, u16)>, xmp: Option<&str>| {
            let (u16_of, u32_of): (fn(u16) -> [u8; 2], fn(u32) -> [u8; 4]) =
                if le { (u16::to_le_bytes, u32::to_le_bytes) } else { (u16::to_be_bytes, u32::to_be_bytes) };
            let strips = span + 64;
            let mut entries = Vec::new();
            if let Some((values, value)) = orientation {
                entries.push((274u16, 3u16, values, [u16_of(value), [0, 0]].concat()));
            }
            entries.push((305, 2, 64, u32_of(4).to_vec()));
            let packet_at = 8 + strips + 2 + 12 * (entries.len() + usize::from(xmp.is_some())) + 4;
            if let Some(x) = xmp {
                entries.push((700, 7, x.len() as u32, u32_of(packet_at as u32).to_vec()));
            }
            let mut t = if le { b"II*\0".to_vec() } else { b"MM\0*".to_vec() };
            t.extend_from_slice(&u32_of(8 + strips as u32));
            t.resize(8 + strips, 0);
            t.extend_from_slice(&u16_of(entries.len() as u16));
            for (tag, kind, count, value) in entries {
                t.extend_from_slice(&u16_of(tag));
                t.extend_from_slice(&u16_of(kind));
                t.extend_from_slice(&u32_of(count));
                t.extend_from_slice(&value);
            }
            t.extend_from_slice(&[0; 4]);
            t.extend_from_slice(xmp.unwrap_or_default().as_bytes());
            t
        };
        // a JPEG: the APP1 payloads `early`, more than the first MiB of other segments, `late`, the image data
        let jpeg = |early: &[Vec<u8>], late: &[Vec<u8>]| {
            let app1 = |j: &mut Vec<u8>, payload: &Vec<u8>| {
                j.extend_from_slice(&[0xff, 0xe1]);
                j.extend_from_slice(&(payload.len() as u16 + 2).to_be_bytes());
                j.extend_from_slice(payload);
            };
            let mut j = vec![0xff, 0xd8];
            early.iter().for_each(|p| app1(&mut j, p));
            for _ in 0..17 {
                j.extend_from_slice(&[0xff, 0xef, 0xff, 0xff]);
                j.resize(j.len() + 0xfffd, 0);
            }
            late.iter().for_each(|p| app1(&mut j, p));
            j.extend_from_slice(&[0xff, 0xda, 0, 2, 0xff, 0xd9]);
            j
        };
        let exif_app1 = [&b"Exif\0\0"[..], &exif].concat();
        let plain_app1 = [&b"Exif\0\0"[..], &plain].concat();
        let xmp_app1 = [&b"http://ns.adobe.com/xap/1.0/\0"[..], packet.as_bytes()].concat();
        for (name, bytes, want) in [
            ("small.webp", webp(&[image(64), exif_chunk.clone()]), o6),
            ("large.webp", webp(&[image(span + 64), exif_chunk.clone(), xmp_chunk.clone()]), o6),
            // EXIF wins in whichever order the chunks come, and an XMP in the first MiB doesn't stop the search
            ("reversed.webp", webp(&[image(span + 64), xmp_chunk.clone(), exif_chunk.clone()]), o6),
            ("xmp-first.webp", webp(&[xmp_chunk.clone(), image(span + 64), exif_chunk.clone()]), o6),
            ("none.webp", webp(&[image(span + 64), exif_chunk.clone()])[..span + 100].to_vec(), None),
            ("xmp.webp", webp(&[image(span + 64), xmp_chunk.clone()]), o8),
            // an EXIF chunk of any size is read by its IFD: one over the span, one (without `Exif\0\0`, big-endian)
            // whose IFD is more than the span into it
            ("huge.webp", webp(&[image(span + 64), huge_exif, xmp_chunk.clone()]), o6),
            ("late-ifd.webp", webp(&[image(span + 64), (&b"EXIF"[..], tiff(false, Some((1, 6)), None)), xmp_chunk.clone()]), o6),
            // nothing outside the EXIF chunk is read for its IFD
            ("outside.webp", webp(&[image(span + 64), stray_exif, stray_ifd, xmp_chunk.clone()]), o8),
            // the last EXIF chunk counts, here a late one stating no orientation after one in the first MiB
            ("last-exif.webp", webp(&[exif_chunk, image(span + 64), plain_chunk, xmp_chunk]), o8),
            // an XMP chunk of the span is read
            ("span-xmp.webp", webp(&[image(64), padded_xmp(span)]), o8),
            ("le.tif", tiff(true, Some((1, 8)), None), o8),
            ("be.tif", tiff(false, Some((1, 8)), None), o8),
            ("offset.tif", tiff(true, Some((3, 8)), None), None),
            // the tag wins over the IFD's XMP; XMP fills in for no tag or an invalid one
            ("both.tif", tiff(true, Some((1, 6)), Some(&packet)), o6),
            ("xmp.tif", tiff(false, None, Some(&packet)), o8),
            ("invalid.tif", tiff(true, Some((1, 0)), Some(&packet)), o8),
            ("none.jpg", jpeg(&[], &[]), None),
            ("xmp-first.jpg", jpeg(std::slice::from_ref(&xmp_app1), std::slice::from_ref(&exif_app1)), o6),
            ("xmp-only.jpg", jpeg(&[], std::slice::from_ref(&xmp_app1)), o8),
            // the first Exif segment counts, though it states no orientation
            ("two-exif.jpg", jpeg(&[plain_app1], &[exif_app1, xmp_app1]), o8),
        ] {
            assert_eq!(lcc_meta::extract(&bytes).orientation, want, "import: {name}");
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(super::file_orientation(&path.to_string_lossy()), want, "{name}");
        }
        // a larger XMP chunk isn't read, where the import (reading the whole file) takes its orientation
        let over = webp(&[image(64), padded_xmp(span + 1)]);
        assert_eq!(lcc_meta::extract(&over).orientation, o8);
        std::fs::write(dir.join("over-xmp.webp"), over).unwrap();
        assert_eq!(super::file_orientation(&dir.join("over-xmp.webp").to_string_lossy()), None);
        assert_eq!(super::file_orientation(&dir.join("missing.webp").to_string_lossy()), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A folder listing answers as asking the disk does: present names (in any case the volume
    /// answers to) are found, absent ones are not, and an unlisted folder is asked name by name.
    #[test]
    fn folder_listing_answers_as_the_disk() {
        let dir = std::env::temp_dir().join(format!("sidecar-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub.xmp")).unwrap();
        std::fs::write(dir.join("IMG_1.XMP"), b"x").unwrap();
        std::fs::write(dir.join("img_2.xmp"), b"x").unwrap();
        let mut listing = FolderListing::default();
        for name in ["IMG_1.XMP", "IMG_1.xmp", "img_1.xmp", "img_2.xmp", "IMG_2.XMP", "IMG_3.xmp", "sub.xmp", "none"] {
            let p = dir.join(name);
            assert_eq!(listing.is_file(&p), p.is_file(), "{name}");
        }
        let missing = dir.join("gone").join("a.xmp");
        assert!(!listing.is_file(&missing));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sidecars_are_written_only_beside_an_existing_original() {
        let dir = std::env::temp_dir().join(format!("app-sidecar-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("IMG_1.jpg"), b"original").unwrap();
        let originals = [
            (r"\\192.0.2.1\share\IMG_1.jpg".to_string(), false),
            (dir.join("IMG_2.jpg").to_string_lossy().to_string(), false),
            (dir.join("IMG_1.jpg").to_string_lossy().to_string(), true),
        ];
        for (original, written) in originals {
            let _ = std::fs::remove_file(dir.join("IMG_1.xmp"));
            let mut s = Session::new();
            let p = Photo::new(PhotoId(1), Source::File { path: original.clone() }, "IMG_1.jpg", "JPEG", 4, 3, "2026-01-01T00:00:00");
            s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
            assert_eq!(s.save_sidecar(PhotoId(1)).is_ok(), written, "{original}");
            assert_eq!(dir.join("IMG_1.xmp").is_file(), written, "{original}");
            assert!(!dir.join("IMG_2.xmp").exists(), "{original}");
            assert!(!Path::new(r"\\192.0.2.1\share\IMG_1.xmp").exists(), "{original}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

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

    #[test]
    fn a_profile_with_a_calibrated_look_is_not_warned_about() {
        let xmp = |model: &str| {
            format!(
                r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
                <rdf:Description rdf:about="" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/"
                  tiff:Make="LOOKTEST" tiff:Model="{model}" crs:Exposure2012="+0.5" crs:CameraProfile="Adobe Standard">
                <crs:Look rdf:parseType="Resource"><crs:Name>Adobe Vivid</crs:Name><crs:Amount>1</crs:Amount></crs:Look>
                </rdf:Description></rdf:RDF></x:xmpmeta>"#
            )
        };
        let warned = |model: &str| {
            parse_sidecar(&xmp(model), crate::crs::Target::RawAbsolute)
                .unwrap()
                .warnings
                .iter()
                .any(|w| w.code == crate::import::ImportWarningCode::CameraProfile && w.profile.as_deref() == Some("Adobe Vivid"))
        };
        // add a calibrated camera to whatever is installed (the registry is process-wide), and take it out again
        let dcp = std::sync::Arc::new(lcc_raw::dcp::Dcp {
            model: "LOOKTEST ONE".into(),
            name: "Adobe Vivid".into(),
            copyright: String::new(),
            color: lcc_raw::ColorData::default(),
        });
        let others = || lcc_raw::dcp::calibrated().iter().filter(|c| c.make != "LOOKTEST").map(|c| (**c).clone()).collect::<Vec<_>>();
        let mut list = others();
        list.push(lcc_raw::dcp::Calibrated {
            make: "LOOKTEST".into(),
            names: vec!["LOOKTEST ONE".into()],
            lens: None,
            looks: vec![("cal:adobe-vivid".into(), dcp)],
            default: Some("cal:adobe-vivid".into()),
        });
        lcc_raw::dcp::install_calibrated(list);
        let (calibrated, other_camera) = (warned("ONE"), warned("TWO"));
        lcc_raw::dcp::install_calibrated(others());
        assert!(!calibrated, "the camera has a calibrated Adobe Vivid");
        assert!(other_camera, "another camera has none: the stand-in is reported as before");
    }

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

    /// A sidecar that is a link to a private file is not copied into a `.bak` next to the photo
    /// (issue #2122); a real sidecar is.
    #[cfg(unix)]
    #[test]
    fn a_linked_sidecar_is_never_backed_up() {
        let dir = std::env::temp_dir().join(format!("lcc-sidecar-backup-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("secret");
        std::fs::write(&secret, b"private").unwrap();
        let linked = dir.join("IMG_1.xmp");
        std::os::unix::fs::symlink(&secret, &linked).unwrap();
        assert!(super::backup(&linked, "20261017").is_err());
        let copies = std::fs::read_dir(&dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains(".bak-")).count();
        assert_eq!(copies, 0);
        let real = dir.join("IMG_2.xmp");
        std::fs::write(&real, b"<x/>").unwrap();
        let b = super::backup(&real, "20261017").unwrap();
        assert_eq!(std::fs::read(b).unwrap(), b"<x/>");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Photos on an SMB share mounted by macOS (issue #1407), where the full flush fails with
    /// ENOTSUP: an import copy, a move to another volume and a sidecar backup flush with a plain
    /// `fsync` instead of failing.
    #[test]
    fn durable_writes_reach_a_share_without_a_full_flush() {
        use crate::rename::MoveFs;
        use lcc_catalog::safe_file::{full_sync_unsupported, plain_fsyncs_on_this_thread};
        let d = crate::tests_xmp::temp_dir("no-full-flush");
        let src = d.join("DSC_0001.NEF");
        std::fs::write(&src, b"raw bytes").unwrap();
        let before = plain_fsyncs_on_this_thread();
        {
            let _smb = full_sync_unsupported();
            crate::import_move::copy_verified(&src, &d.join("imported.NEF"), None).unwrap();
            crate::rename::RealFs.copy_no_replace(&src, &d.join("moved.NEF")).unwrap();
            let b = backup(&src, "20261015").unwrap();
            assert_eq!(std::fs::read(b).unwrap(), b"raw bytes");
        }
        assert_eq!(plain_fsyncs_on_this_thread() - before, 3, "each write falls back to fsync");
        assert_eq!(std::fs::read(d.join("imported.NEF")).unwrap(), b"raw bytes");
        assert_eq!(std::fs::read(d.join("moved.NEF")).unwrap(), b"raw bytes");
        let _ = std::fs::remove_dir_all(&d);
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

    /// Capture-time, trim and poster edits reach the auto-write, alone and inside a batch, so the
    /// sidecar doesn't keep the old value for a later folder sync to read back over the edit.
    #[test]
    fn capture_time_trim_and_poster_edits_are_auto_written() {
        use super::{Op, PhotoId, op_photos};
        let (a, b, c) = (PhotoId(1), PhotoId(2), PhotoId(3));
        let ops = [
            Op::SetCaptured { id: a, captured: Some("2024-05-01T10:00:00".into()) },
            Op::Batch { ops: vec![Op::SetVideoTrim { id: b, trim_in: 1.0, trim_out: Some(2.0) }, Op::SetVideoPoster { id: c, time: Some(1.5) }] },
            Op::SetCaptured { id: a, captured: None },
        ];
        let mut ids = Vec::new();
        ops.iter().for_each(|o| op_photos(o, &mut ids));
        assert_eq!(ids, vec![a, b, c]);
    }

    /// IMM-SIDECARS: filling Immich's sidecar keeps what another writer put there, while the
    /// app's properties and develop settings replace theirs.
    #[test]
    fn merge_packet_keeps_foreign_xmp_and_takes_ours() {
        let existing = r#"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
<rdf:Description rdf:about='' xmlns:dc='http://purl.org/dc/elements/1.1/' xmlns:xmp='http://ns.adobe.com/xap/1.0/'
 xmlns:crs='http://ns.adobe.com/camera-raw-settings/1.0/' xmlns:other='http://example.invalid/ns/'>
<dc:description><rdf:Alt><rdf:li xml:lang='x-default'>Immich copy</rdf:li></rdf:Alt></dc:description>
<xmp:Rating>2</xmp:Rating><crs:Exposure2012='+0.60' other:Secret='keep me'/></rdf:Description></rdf:RDF></x:xmpmeta>"#;
        let fresh = format!(
            r#"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#'>
<rdf:Description rdf:about='' xmlns:dc='http://purl.org/dc/elements/1.1/' xmlns:xmp='http://ns.adobe.com/xap/1.0/'
 xmlns:lc='{}'>
<dc:description><rdf:Alt><rdf:li xml:lang='x-default'>From the catalog</rdf:li></rdf:Alt></dc:description>
<xmp:Rating>5</xmp:Rating><lc:settings>{{"version":1}}</lc:settings></rdf:Description></rdf:RDF></x:xmpmeta>"#,
            lcc_meta::LC_NS
        );
        let out = super::merge_packet(existing, &fresh).unwrap();
        let d = lcc_meta::parse_xmp(&out).unwrap();
        assert_eq!(d.metadata.rating, Some(5), "ours replaces theirs");
        assert_eq!(d.metadata.caption.as_deref(), Some("From the catalog"));
        assert!(out.contains("crs:Exposure2012='+0.60'"), "another app's settings stay:\n{out}");
        assert!(out.contains("other:Secret='keep me'"), "unknown namespaces stay:\n{out}");
        assert!(d.lc_settings.is_some(), "the develop settings are there");
        // an empty sidecar takes the fresh packet; an unreadable one is refused, not replaced
        assert_eq!(super::merge_packet("", &fresh).unwrap(), fresh);
        assert!(super::merge_packet("<not xmp", &fresh).is_err());
    }
}
