//! XMP sidecars: `<stem>.xmp` (default) or `<file>.xmp` next to the original.
//!
//! **Write** ([`Session::save_sidecar`], `photo.saveMetadataToFile`, or automatically after every
//! change when [`XmpPrefs::auto_write`] is on): rating, colour label, title, caption, copyright,
//! creator, keywords, capture time and GPS in the standard namespaces, plus our complete develop
//! settings (`lc:settings`, JSON), the pick/reject flag (`lc:flag`) and location (`lc:location`).
//! A sidecar is written only beside an original that is an existing file at an absolute path;
//! otherwise the save fails.
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
//! (`IMG_0001.JPG.xmp`) for writing and reading, so neither overwrites the other. An import ranks
//! the files it is adding against the library the same way ([`ImportStems`]): a JPEG or a TIFF
//! arriving beside its raw's `IMG_0001.xmp` reads its own `<file>.xmp`, never the raw's packet
//! (issue #1905).
//!
//! **Read** (on import, and `photo.readMetadataFromFile`): the sidecar wins for metadata, except
//! the capture time: a time embedded in the file wins, and the sidecar's (`exif:DateTimeOriginal`,
//! else `photoshop:DateCreated`, else `xmp:CreateDate`) fills in only when the file has none; develop
//! settings are restored from `lc:settings` when present, else mapped from interoperable `crs:`
//! fields ([`crate::crs`]). For raw/DNG files without a sidecar the file's embedded XMP is used.
//! Manual reads also accept the first complete standard XMP APP1 in a bounded JPEG header (see `read_jpeg_xmp`).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
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

/// Where to look for a catalogued photo's sidecar: how the library names it, and whether the photo
/// may read the shared stem sidecar (`IMG_0001.xmp`) of its stem. A photo that shares its stem
/// with a file that owns that sidecar (see [`Session::sidecar_naming`]) reads its own `<file>.xmp`
/// and never the packet of the sibling that owns the stem one (issue #1523).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SidecarLookup {
    pub naming: SidecarNaming,
    /// False when another file with the same stem owns the stem sidecar.
    pub owns_stem: bool,
}

impl SidecarLookup {
    /// The names a sidecar for `original` can have, in the order they are looked for: those of
    /// [`sidecar_candidates`], without the shared stem sidecar when the photo does not own it.
    pub(crate) fn candidates(self, original: &str) -> impl Iterator<Item = PathBuf> {
        let shared = (!self.owns_stem).then(|| xmp_cases(sidecar_path(original, SidecarNaming::Stem)));
        sidecar_candidates(original, self.naming).filter(move |p| shared.as_ref().is_none_or(|shared| !shared.contains(p)))
    }

    /// [`find_sidecar`], except that a photo that does not own its stem sidecar never reads it.
    pub fn find(self, original: &str) -> Option<PathBuf> {
        self.candidates(original).find(|p| p.is_file())
    }
}

/// Find the sidecar for a file an import is adding, by the ownership [`ImportStems`] worked out
/// (issue #1905): [`find_sidecar`] with the library's naming, except that a file which does not
/// own its stem sidecar reads its own `<file>.xmp` (the name it writes) and never the packet of
/// the sibling that owns the stem one.
pub(crate) fn find_sidecar_for_import(stems: &ImportStems, naming: SidecarNaming, original: &str) -> Option<PathBuf> {
    SidecarLookup { naming, owns_stem: stems.owns(original) }.find(original)
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
    pub location: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub alt_text: Option<String>,
    pub extended_description: Option<String>,
    pub keywords: Option<Vec<String>>,
    pub regions: Option<Vec<lightcraft_meta::Region>>,
    /// The size the regions were drawn on, when they don't say how the photo was turned
    /// ([`lightcraft_meta::XmpData::regions_drawn_on`]), and how the photo's stored pixels turn upright: the
    /// sidecar's `tiff:Orientation`, or the file's own Exif orientation where the caller knows it. With both,
    /// [`merge_into`] puts boxes drawn on the photo as stored where the upright photo has them (#2674).
    pub regions_drawn_on: Option<(f64, f64)>,
    pub orientation: Option<lightcraft_meta::Orientation>,
    /// Capture time (ISO 8601) from `exif:DateTimeOriginal`, `photoshop:DateCreated` or
    /// `xmp:CreateDate` (first found). Used only when the file itself has no capture time.
    pub captured: Option<String>,
    pub develop: Option<DevelopPatch>,
    /// Actionable diagnostics for foreign develop settings that were not fully carried over.
    /// These are surfaced in the photo import report and never persisted in the catalog.
    pub warnings: Vec<crate::import::ImportWarning>,
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
        regions_drawn_on: d.regions_drawn_on(),
        // (only what the regions need: a packet that says nothing else still says nothing)
        orientation: d.regions_drawn_on().and(m.orientation),
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
    let full =
        d.lc_settings.as_deref().and_then(|j| serde_json::from_str::<Value>(j).ok()).and_then(|v| DevelopSettings::from_untrusted_json(&v).ok());
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
        // (the photo's size is the upright one)
        if let (Some(drawn_on), Some(o)) = (sc.regions_drawn_on, sc.orientation) {
            let stored = if o.swaps_axes() { (p.height, p.width) } else { (p.width, p.height) };
            lightcraft_meta::upright_regions(&mut m.regions, drawn_on, o, stored);
        }
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

    /// Take the orientation of the photo file at `original` (LightCraft shows the photo turned by it, and the packet
    /// may not repeat it) when the regions need it: they were drawn on a stated size ([`SidecarData::regions_drawn_on`]).
    /// Only then is the file read ([`file_orientation`]).
    pub fn with_file_orientation(self, original: &str) -> Self {
        if self.regions_drawn_on.is_none() {
            return self;
        }
        SidecarData { orientation: file_orientation(original).or(self.orientation), ..self }
    }
}

/// The orientation the photo file at `path` states, as [`lightcraft_meta::extract`] reads it from the whole file (the
/// import's): Exif's, else XMP's. A file shorter than [`ORIENTATION_SPAN`] is read whole. Most others state it in the
/// Exif of their first [`ORIENTATION_SPAN`]; when that part doesn't, the Exif may come later ([`late_orientation`]).
/// A WebP's or PNG's chunks are always walked, since the last Exif chunk is the one that counts ([`webp_orientation`],
/// [`png_orientation`]).
fn file_orientation(path: &str) -> Option<lightcraft_meta::Orientation> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    (&mut file).take(ORIENTATION_SPAN).read_to_end(&mut head).ok()?;
    if (head.len() as u64) < ORIENTATION_SPAN {
        return lightcraft_meta::extract(&head).orientation;
    }
    if head.get(..4) == Some(b"RIFF") && head.get(8..12) == Some(b"WEBP") {
        return webp_orientation(&mut file);
    }
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        return png_orientation(&mut file);
    }
    if let Some(o) = exif_orientation(&head) {
        return Some(o);
    }
    late_orientation(&mut file, &head).or_else(|| lightcraft_meta::extract(&head).orientation)
}

/// The orientation the Exif in `bytes` (a file, or the start of one) states, leaving out a separate XMP packet (the
/// one the Exif holds in tag 700 counts, as in [`lightcraft_meta::read_exif`]).
fn exif_orientation(bytes: &[u8]) -> Option<lightcraft_meta::Orientation> {
    lightcraft_meta::embedded(bytes).exif.as_deref().map(lightcraft_meta::read_exif).and_then(|m| m.orientation)
}

/// The orientation of a JPEG, TIFF or PSD whose `head` has no Exif stating it, found by the file's structure without
/// reading its image data (a JPEG's Exif can follow other long segments, many TIFF writers put the first IFD after the
/// strips, and a PSD's image resources can outgrow `head`). As the import does, a JPEG takes its first Exif segment,
/// else its first XMP segment.
fn late_orientation(file: &mut (impl Read + Seek), head: &[u8]) -> Option<lightcraft_meta::Orientation> {
    let len = file.seek(SeekFrom::End(0)).ok()?;
    file.rewind().ok()?;
    match head.get(..4)? {
        [0xff, 0xd8, ..] => {
            let (mut exif_seen, mut xmp) = (false, None);
            let exif = read_jpeg_app1(file, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS, |segment| {
                let embedded = lightcraft_meta::embedded(segment);
                if let Some(exif) = embedded.exif
                    && !exif_seen
                {
                    // the first Exif segment ends the walk if it states one; later ones don't count
                    exif_seen = true;
                    return lightcraft_meta::read_exif(&exif).orientation;
                }
                if xmp.is_none() {
                    xmp = embedded.xmp.map(|x| lightcraft_meta::parse_xmp(&x).ok().and_then(|x| x.metadata.orientation));
                }
                None
            });
            exif.or(xmp.flatten())
        }
        b"II*\0" | b"MM\0*" => tiff_orientation(file, 0, len),
        b"8BPS" => psd_orientation(file),
        _ => None,
    }
}

/// A WebP's orientation as the import takes it: from its last `EXIF` chunk, else its last `XMP ` chunk, in whichever
/// order they come. The chunk headers are walked from the start of `file` (at most [`WEBP_CHUNK_STEPS`], ending at a
/// chunk that runs past the end of the file); then only the first IFD of the EXIF chunk is read, whatever its size
/// ([`tiff_orientation`]), and the XMP chunk if it is at most [`ORIENTATION_SPAN`].
fn webp_orientation(file: &mut (impl Read + Seek)) -> Option<lightcraft_meta::Orientation> {
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
    if let Some(o) = exif.and_then(|(start, end)| exif_chunk_orientation(file, start, end)) {
        return Some(o);
    }
    let (start, size) = xmp?;
    xmp_orientation(file, start, size)
}

/// How many chunks [`webp_orientation`] passes over (an animated WebP has one per frame).
const WEBP_CHUNK_STEPS: usize = 4096;

/// The orientation the Exif chunk from `start` to `end` in `file` states: its TIFF stream comes after an `Exif\0\0` if
/// the chunk starts with one (as `strip_exif_header` takes it).
fn exif_chunk_orientation(file: &mut (impl Read + Seek), start: u64, end: u64) -> Option<lightcraft_meta::Orientation> {
    let mut prefix = [0; 5];
    file.seek(SeekFrom::Start(start)).ok()?;
    let prefixed = start + 6 <= end && file.read_exact(&mut prefix).is_ok() && &prefix == b"Exif\0";
    tiff_orientation(file, if prefixed { start + 6 } else { start }, end)
}

/// A PNG's orientation as the import takes it: from its last `eXIf` chunk, else its last uncompressed XMP `iTXt` chunk,
/// wherever they come (a writer may put them after the image data). The chunk headers are walked from the start of
/// `file` to `IEND` (at most [`png_chunk_steps`], ending at a chunk that runs past the end of the file); then only the
/// first IFD of the `eXIf` chunk is read, whatever its size ([`tiff_orientation`]), and the `iTXt` chunk if it is at
/// most [`ORIENTATION_SPAN`].
fn png_orientation(file: &mut (impl Read + Seek)) -> Option<lightcraft_meta::Orientation> {
    let len = file.seek(SeekFrom::End(0)).ok()?;
    let (mut exif, mut xmp) = (None, None);
    let mut at = 8u64;
    for _ in 0..png_chunk_steps(len) {
        let (mut header, mut key) = ([0; 8], [0; 19]);
        if at + 12 > len || file.seek(SeekFrom::Start(at)).is_err() || file.read_exact(&mut header).is_err() {
            break;
        }
        let [s0, s1, s2, s3, a, b, c, d] = header;
        let size = u64::from(u32::from_be_bytes([s0, s1, s2, s3]));
        let Some(end) = at.checked_add(8 + size).filter(|&end| end <= len) else { break };
        match &[a, b, c, d] {
            b"eXIf" => exif = Some((at + 8, end)),
            // the keyword, then compression flag 0
            b"iTXt" if size >= 20 && file.read_exact(&mut key).is_ok() && &key == b"XML:com.adobe.xmp\0\0" => xmp = Some((at + 8, size)),
            b"IEND" => break,
            _ => {}
        }
        // past the chunk's CRC
        at = end + 4;
    }
    if let Some(o) = exif.and_then(|(start, end)| exif_chunk_orientation(file, start, end)) {
        return Some(o);
    }
    let (start, size) = xmp?;
    let chunk = read_within_span(file, start, size)?;
    // keyword\0 compression-flag compression-method language\0 translated\0 text
    let text = chunk.get(20..)?.splitn(3, |&c| c == 0).nth(2)?;
    lightcraft_meta::parse_xmp(&String::from_utf8_lossy(text)).ok()?.metadata.orientation
}

/// How many chunks [`png_orientation`] passes over in a file of `len` bytes: 4096 and one per KiB of the file, at most
/// 2^20. Image data is often split into many `IDAT` chunks (libpng writes 8 KiB ones by default), so the limit grows
/// with the file; each chunk costs two system calls, so a short file of empty chunks can't hold a worker for seconds.
fn png_chunk_steps(len: u64) -> u64 {
    (4096 + len / 1024).min(1 << 20)
}

/// A PSD's or PSB's orientation as the import takes it, from its image resources: the file's start up to their end is
/// read if it is at most [`PSD_RESOURCES_SPAN`]. They come before the layers and the image data, after the 26-byte
/// header and the colour-mode data; each of those two sections starts with its length.
fn psd_orientation(file: &mut (impl Read + Seek)) -> Option<lightcraft_meta::Orientation> {
    let mut length = [0; 4];
    file.seek(SeekFrom::Start(26)).ok()?;
    file.read_exact(&mut length).ok()?;
    let resources = 30 + u64::from(u32::from_be_bytes(length));
    file.seek(SeekFrom::Start(resources)).ok()?;
    file.read_exact(&mut length).ok()?;
    let end = resources + 4 + u64::from(u32::from_be_bytes(length));
    if end > PSD_RESOURCES_SPAN {
        return None;
    }
    file.rewind().ok()?;
    let mut bytes = Vec::new();
    (&mut *file).take(end).read_to_end(&mut bytes).ok()?;
    lightcraft_meta::extract(&bytes).orientation
}

/// How far into a PSD or PSB [`psd_orientation`] reads.
const PSD_RESOURCES_SPAN: u64 = 16 * 1024 * 1024;

/// The orientation the TIFF stream from `start` to `end` in `file` states in its first IFD (a TIFF file, or the Exif
/// block of a WebP's EXIF chunk; its offsets count from `start`, and nothing past `end` is read). Only the IFD's 12-byte
/// entries are read (at most 65535 of them, under 1 MiB): its orientation tag, else (as [`lightcraft_meta::read_exif`]
/// does) the XMP packet its tag 700 holds, read if it is at most [`ORIENTATION_SPAN`].
fn tiff_orientation(file: &mut (impl Read + Seek), start: u64, end: u64) -> Option<lightcraft_meta::Orientation> {
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
                    return Some(lightcraft_meta::Orientation::from_exif(v));
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
fn xmp_orientation(file: &mut (impl Read + Seek), at: u64, len: u64) -> Option<lightcraft_meta::Orientation> {
    let packet = read_within_span(file, at, len)?;
    lightcraft_meta::parse_xmp(&String::from_utf8_lossy(&packet)).ok()?.metadata.orientation
}

/// The `len` bytes at `at` in `file`, read if they are at most [`ORIENTATION_SPAN`].
fn read_within_span(file: &mut (impl Read + Seek), at: u64, len: u64) -> Option<Vec<u8>> {
    if len > ORIENTATION_SPAN {
        return None;
    }
    file.seek(SeekFrom::Start(at)).ok()?;
    let mut bytes = Vec::new();
    (&mut *file).take(len).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// How much of a photo file [`file_orientation`] reads first.
const ORIENTATION_SPAN: u64 = 1 << 20;

/// The sidecar packet for a photo; the colour label is written by its name in `cat` (custom
/// label names included, as other editors do).
pub fn sidecar_packet(p: &Photo, cat: &lightcraft_catalog::Catalog) -> String {
    sidecar_packet_with(p, cat, false)
}

/// [`sidecar_packet`], without any location (GPS, sublocation, city, state, country) when
/// `remove_location` is set (an export with "Remove location info").
pub fn sidecar_packet_with(p: &Photo, cat: &lightcraft_catalog::Catalog, remove_location: bool) -> String {
    let nz = |s: &str| (!s.trim().is_empty()).then(|| s.to_string());
    // Names in `dc:subject`, paths in `lr:hierarchicalSubject` (`keywords::to_file`).
    let keywords = lightcraft_catalog::keywords::to_file(&p.meta.keywords);
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
        keywords: keywords.flat,
        hierarchical_keywords: keywords.hierarchical,
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
    let settings = serde_json::to_string(&*p.develop).unwrap_or_default();
    let flag = match p.flag {
        Flag::None => "none",
        Flag::Pick => "pick",
        Flag::Reject => "reject",
    };
    let mut lc = vec![("settings", settings.as_str()), ("flag", flag)];
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

/// The XMP packet that describes a photo on disk: its sidecar, else embedded raw/DNG or JPEG XMP.
pub fn read_packet(original: &str, kind: MediaKind, lookup: SidecarLookup) -> Option<(String, PathBuf)> {
    if let Some(sc) = lookup.find(original) {
        // A discovered sidecar keeps precedence even when reading it fails.
        return std::fs::read_to_string(&sc).ok().map(|s| (s, sc));
    }
    if kind == MediaKind::Raw {
        let bytes = std::fs::read(original).ok()?;
        let x = lightcraft_meta::embedded(&bytes).xmp?;
        return Some((x, PathBuf::from(original)));
    }
    if kind == MediaKind::Image {
        let mut file = std::fs::File::open(original).ok()?;
        let x = read_jpeg_xmp(&mut file, JPEG_XMP_HEADER_SPAN, JPEG_XMP_HEADER_STEPS)?;
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
    read_jpeg_app1(reader, header_span, marker_steps, |segment| lightcraft_meta::jpeg_segments(segment).xmp)
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
        Op::SetRating { id, .. } | Op::SetFlag { id, .. } | Op::SetLabel { id, .. } | Op::SetDevelop { id, .. } | Op::SetMeta { id, .. }
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
    /// The owners of every shared stem in the library, each file's key made the long way: what
    /// [`StemOwners::among`] must agree with for the photos it is asked about (this is how the
    /// owners were worked out before, for every write).
    #[cfg(test)]
    pub(crate) fn of(cat: &lightcraft_catalog::Catalog) -> StemOwners {
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

    /// Whether photo `p` may read the shared stem sidecar of its stem: no other file owns it.
    fn owns(&self, p: &Photo) -> bool {
        file_path(p).is_none_or(|path| self.0.get(&stem_key(path)).is_none_or(|owner| *owner == p.id))
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
    pub(crate) fn for_import<'a>(cat: Option<&lightcraft_catalog::Catalog>, incoming: impl Iterator<Item = (&'a str, MediaKind)>) -> ImportStems {
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
pub(crate) fn namings_in(cat: &lightcraft_catalog::Catalog, naming: SidecarNaming, ids: &[PhotoId]) -> Vec<SidecarNaming> {
    lookups_in(cat, naming, ids).into_iter().map(|l| l.naming).collect()
}

/// [`namings_in`], with whether each photo may read the stem sidecar of its stem.
pub(crate) fn lookups_in(cat: &lightcraft_catalog::Catalog, naming: SidecarNaming, ids: &[PhotoId]) -> Vec<SidecarLookup> {
    let owners = StemOwners::among(cat, ids);
    ids.iter()
        .map(|id| match cat.photo(*id) {
            Some(p) => SidecarLookup { naming: owners.naming(p, naming), owns_stem: owners.owns(p) },
            None => SidecarLookup { naming, owns_stem: true },
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

    /// [`Session::sidecar_naming`] and the stem-sidecar ownership of photo `id`, for reading its sidecar.
    pub fn sidecar_lookup(&self, id: PhotoId) -> SidecarLookup {
        lookups_in(&self.catalog, self.xmp.naming, &[id]).into_iter().next().unwrap_or(SidecarLookup { naming: self.xmp.naming, owns_stem: true })
    }

    /// [`Session::sidecar_lookup`] for many photos, their stems' owners worked out once.
    pub fn sidecar_lookups(&self, ids: &[PhotoId]) -> Vec<SidecarLookup> {
        lookups_in(&self.catalog, self.xmp.naming, ids)
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
        // A relative path would be taken from the working directory.
        if Path::new(orig).is_relative() || !Path::new(orig).is_file() {
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
        let p = self.catalog.photo(id).ok_or(lightcraft_catalog::CatalogError::NoPhoto(id))?;
        let err = |e: std::io::Error| EngineError::Other(format!("{}: {e}", path.display()));
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
        let Some((packet, from)) = read_packet(orig, p.kind, self.sidecar_lookup(id)) else { return Ok(None) };
        let sc = parse_sidecar(&packet, crate::crs::Target::for_photo(p)).map_err(|e| EngineError::Other(format!("{}: {e}", from.display())))?;
        let sc = sc.with_file_orientation(orig);
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
    /// Issue #2674: a WebP keeps its EXIF and XMP chunks after the image data, a TIFF's first IFD can follow its strips
    /// and a JPEG's Exif other long segments, so the orientation can be past the first [`super::ORIENTATION_SPAN`] of
    /// the file. Every file gives what the import gives reading it whole: Exif over XMP wherever each is, a WebP's last
    /// EXIF chunk and a JPEG's first Exif segment, and none for a file that states none.
    #[test]
    fn the_file_orientation_is_found_after_the_image_data() {
        let dir = std::env::temp_dir().join(format!("lightcraft-file-orientation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exif = lightcraft_meta::write_exif(&lightcraft_meta::Metadata {
            orientation: Some(lightcraft_meta::Orientation::Rotate90),
            ..Default::default()
        });
        let span = super::ORIENTATION_SPAN as usize;
        let (o6, o8) = (Some(lightcraft_meta::Orientation::Rotate90), Some(lightcraft_meta::Orientation::Rotate270));
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
        let plain = lightcraft_meta::write_exif(&lightcraft_meta::Metadata::default());
        let plain_chunk = (&b"EXIF"[..], [&b"Exif\0\0"[..], &plain].concat());
        let packet = lightcraft_meta::write_xmp(&lightcraft_meta::Metadata { orientation: o8, ..Default::default() }, None);
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
            assert_eq!(lightcraft_meta::extract(&bytes).orientation, want, "import: {name}");
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(super::file_orientation(&path.to_string_lossy()), want, "{name}");
        }
        // a larger XMP chunk isn't read, where the import (reading the whole file) takes its orientation
        let over = webp(&[image(64), padded_xmp(span + 1)]);
        assert_eq!(lightcraft_meta::extract(&over).orientation, o8);
        std::fs::write(dir.join("over-xmp.webp"), over).unwrap();
        assert_eq!(super::file_orientation(&dir.join("over-xmp.webp").to_string_lossy()), None);
        assert_eq!(super::file_orientation(&dir.join("missing.webp").to_string_lossy()), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Issue #2674: the same for a PNG, whose `eXIf` and XMP `iTXt` chunks can follow the image data, and a PSD, whose
    /// image resources can outgrow the first [`super::ORIENTATION_SPAN`].
    #[test]
    fn a_png_or_psd_gives_the_orientation_the_import_gives() {
        let dir = std::env::temp_dir().join(format!("lightcraft-png-psd-orientation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let span = super::ORIENTATION_SPAN as usize;
        let (o6, o8) = (Some(lightcraft_meta::Orientation::Rotate90), Some(lightcraft_meta::Orientation::Rotate270));
        let exif = lightcraft_meta::write_exif(&lightcraft_meta::Metadata { orientation: o6, ..Default::default() });
        let plain = lightcraft_meta::write_exif(&lightcraft_meta::Metadata::default());
        let packet = lightcraft_meta::write_xmp(&lightcraft_meta::Metadata { orientation: o8, ..Default::default() }, None);
        // a PNG: its IHDR, then `chunks`, each with a CRC (nothing checks it)
        let png = |chunks: &[(&[u8], Vec<u8>)]| {
            let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
            for (id, data) in [(&b"IHDR"[..], vec![0; 13])].iter().chain(chunks) {
                p.extend_from_slice(&(data.len() as u32).to_be_bytes());
                p.extend_from_slice(id);
                p.extend_from_slice(data);
                p.extend_from_slice(&[0; 4]);
            }
            p
        };
        let idat = |len: usize| (&b"IDAT"[..], vec![0; len]);
        let iend = (&b"IEND"[..], Vec::new());
        let exif_chunk = (&b"eXIf"[..], exif.clone());
        let plain_chunk = (&b"eXIf"[..], plain.clone());
        // an `iTXt` chunk: the keyword, compression flag `flag`, method, no language, no translated keyword, the text
        let itxt = |keyword: &[u8], flag: u8, text: &[u8]| (&b"iTXt"[..], [keyword, &[0, flag, 0, 0, 0], text].concat());
        let xmp_key = &b"XML:com.adobe.xmp"[..];
        let xmp_chunk = itxt(xmp_key, 0, packet.as_bytes());
        // the XMP packet padded with whitespace, in a chunk of `len` bytes
        let padded_xmp = |len: usize| {
            let mut p = packet.clone().into_bytes();
            p.resize(len - 22, b' ');
            itxt(xmp_key, 0, &p)
        };
        // a big-endian TIFF stream whose IFD, stating orientation 6, is more than the span into it
        let ifd = [0u8, 1, 1, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, 0, 0, 0, 0];
        let late_ifd = [&b"MM\0*"[..], &(8 + span as u32).to_be_bytes(), &vec![0; span], &ifd].concat();
        // an `eXIf` holding only a TIFF header, whose IFD offset (20) points past the chunk's end into the next chunk's
        // data, where an IFD stating orientation 6 sits
        let stray_exif = (&b"eXIf"[..], b"II*\0\x14\0\0\0".to_vec());
        let stray_ifd = (&b"juNK"[..], vec![1, 0, 0x12, 1, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]);
        // a PSD: its header, `colour` bytes of colour-mode data, the image resources (id, data), `pixels` other bytes
        let psd = |colour: usize, resources: &[(u16, &[u8])], pixels: usize| {
            let mut section = Vec::new();
            for (id, data) in resources {
                section.extend_from_slice(b"8BIM");
                section.extend_from_slice(&id.to_be_bytes());
                section.extend_from_slice(&[0, 0]);
                section.extend_from_slice(&(data.len() as u32).to_be_bytes());
                section.extend_from_slice(data);
                if data.len() % 2 == 1 {
                    section.push(0);
                }
            }
            let mut p = b"8BPS\0\x01".to_vec();
            p.resize(26, 0);
            p.extend_from_slice(&(colour as u32).to_be_bytes());
            p.resize(p.len() + colour, 0);
            p.extend_from_slice(&(section.len() as u32).to_be_bytes());
            p.extend_from_slice(&section);
            p.resize(p.len() + pixels, 0);
            p
        };
        let long = vec![0; span + 64];
        let (exif_res, plain_res, xmp_res, long_res) = ((1058, &exif[..]), (1058, &plain[..]), (1060, packet.as_bytes()), (1039, &long[..]));
        let late_png = png(&[idat(span + 64), exif_chunk.clone()]);
        let late_psd = psd(0, &[long_res, exif_res], 0);
        for (name, bytes, want) in [
            ("early.png", png(&[exif_chunk.clone(), idat(span + 64), iend.clone()]), o6),
            ("late.png", png(&[idat(span + 64), exif_chunk.clone(), xmp_chunk.clone(), iend.clone()]), o6),
            // the `eXIf` wins in whichever order the chunks come, and an XMP in the first MiB doesn't stop the search
            ("reversed.png", png(&[idat(span + 64), xmp_chunk.clone(), exif_chunk.clone(), iend.clone()]), o6),
            ("xmp-first.png", png(&[xmp_chunk.clone(), idat(span + 64), exif_chunk.clone(), iend.clone()]), o6),
            ("xmp.png", png(&[idat(span + 64), xmp_chunk.clone(), iend.clone()]), o8),
            ("none.png", png(&[idat(span + 64), iend.clone()]), None),
            // the `eXIf` is cut short, or comes after the end of the image
            ("cut.png", late_png[..late_png.len() - 6].to_vec(), None),
            ("after-iend.png", png(&[idat(span + 64), iend.clone(), exif_chunk.clone()]), None),
            // an `eXIf` of any size is read by its IFD: with `Exif\0\0`, over the span, and with its IFD past the span
            ("prefixed.png", png(&[idat(span + 64), (&b"eXIf"[..], [&b"Exif\0\0"[..], &exif].concat())]), o6),
            ("huge.png", png(&[idat(64), (&b"eXIf"[..], [&exif[..], &vec![0; span]].concat()), xmp_chunk.clone()]), o6),
            ("late-ifd.png", png(&[idat(span + 64), (&b"eXIf"[..], late_ifd), xmp_chunk.clone()]), o6),
            // nothing outside the `eXIf` is read for its IFD
            ("outside.png", png(&[idat(span + 64), stray_exif, stray_ifd, xmp_chunk.clone()]), o8),
            // the last `eXIf` counts, here a late one stating no orientation after one in the first MiB
            ("last-exif.png", png(&[exif_chunk.clone(), idat(span + 64), plain_chunk, xmp_chunk.clone()]), o8),
            // a compressed packet or another keyword isn't the XMP; a chunk of the span is read
            ("compressed.png", png(&[idat(span + 64), xmp_chunk.clone(), itxt(xmp_key, 1, packet.as_bytes())]), o8),
            ("only-compressed.png", png(&[idat(span + 64), itxt(xmp_key, 1, packet.as_bytes())]), None),
            ("other-text.png", png(&[idat(span + 64), itxt(b"Comment", 0, packet.as_bytes())]), None),
            ("short-text.png", png(&[idat(span + 64), xmp_chunk.clone(), (&b"iTXt"[..], b"XML:com.adobe.xmp\0\0".to_vec())]), o8),
            ("span-xmp.png", png(&[idat(64), padded_xmp(span)]), o8),
            ("late.psd", late_psd.clone(), o6),
            ("early.psd", psd(0, &[exif_res], span + 64), o6),
            // Exif wins in whichever order the resources come; XMP fills in for none, or none stating an orientation
            ("both.psd", psd(0, &[xmp_res, long_res, exif_res], 0), o6),
            ("xmp.psd", psd(768, &[long_res, xmp_res], 0), o8),
            ("plain.psd", psd(0, &[long_res, plain_res, xmp_res], 0), o8),
            ("none.psd", psd(0, &[long_res], span), None),
            // as at import, nothing is taken from resources that are cut short or state their Exif twice
            ("cut.psd", late_psd[..late_psd.len() - 1].to_vec(), None),
            ("two-exif.psd", psd(0, &[long_res, exif_res, exif_res], 0), None),
        ] {
            assert_eq!(lightcraft_meta::extract(&bytes).orientation, want, "import: {name}");
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            assert_eq!(super::file_orientation(&path.to_string_lossy()), want, "{name}");
        }
        // a larger XMP chunk isn't read, where the import (reading the whole file) takes its orientation
        let over = png(&[idat(64), padded_xmp(span + 1)]);
        assert_eq!(lightcraft_meta::extract(&over).orientation, o8);
        std::fs::write(dir.join("over-xmp.png"), over).unwrap();
        assert_eq!(super::file_orientation(&dir.join("over-xmp.png").to_string_lossy()), None);
        std::fs::remove_dir_all(dir).unwrap();

        // the `eXIf` as the last chunk walked, and as the first one not: IHDR, `chunks` of empty `IDAT`, the `eXIf`
        let steps = super::png_chunk_steps;
        let split = |chunks: usize| png(&[vec![idat(0); chunks], vec![exif_chunk.clone()]].concat());
        let len = |chunks: usize| (8 + 25 + 12 * chunks + 12 + exif.len()) as u64;
        let walked = (0..).find(|&n| n as u64 + 2 == steps(len(n)) && n as u64 + 3 > steps(len(n + 1))).unwrap();
        let (last, past) = (split(walked), split(walked + 1));
        assert_eq!(last.len() as u64, len(walked));
        assert_eq!(super::png_orientation(&mut std::io::Cursor::new(&last)), o6);
        assert_eq!(lightcraft_meta::extract(&past).orientation, o6);
        assert_eq!(super::png_orientation(&mut std::io::Cursor::new(&past)), None);
        // image data split into chunks of a KiB is walked whatever its length, up to the limit
        let kib_chunks = png(&[vec![idat(1012); 6000], vec![exif_chunk.clone()]].concat());
        assert_eq!(super::png_orientation(&mut std::io::Cursor::new(&kib_chunks)), o6);
        assert_eq!(steps(u64::MAX), 1 << 20);

        // image resources that end exactly at the limit, and a byte past it
        let limit = super::PSD_RESOURCES_SPAN as usize;
        let padding = vec![0; limit - 58 - exif.len().next_multiple_of(2)];
        let within = psd(0, &[(1039, &padding[..]), exif_res], 0);
        assert_eq!(within.len(), limit);
        assert_eq!(super::psd_orientation(&mut std::io::Cursor::new(&within)), o6);
        let past = psd(1, &[(1039, &padding[..]), exif_res], 0);
        assert_eq!(lightcraft_meta::extract(&past).orientation, o6);
        assert_eq!(super::psd_orientation(&mut std::io::Cursor::new(&past)), None);
    }

    #[test]
    fn sidecars_are_written_only_beside_an_existing_original() {
        let dir = std::env::temp_dir().join(format!("lightcraft-sidecar-local-{}", std::process::id()));
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
        let m = lightcraft_meta::Metadata {
            keywords: vec!["Lisbon".into(), "Places".into()],
            hierarchical_keywords: vec!["Places|Lisbon".into()],
            ..Default::default()
        };
        let sc = super::parse_sidecar(&lightcraft_meta::write_xmp(&m, None), crate::crs::Target::Rendered).unwrap();
        assert_eq!(sc.keywords, Some(vec!["Places|Lisbon".to_string()]));
    }

    /// Issue #2556: a sidecar's `lc:settings` colour-range mask keeps at most `MAX_COLOR_RANGE_SAMPLES` samples.
    #[test]
    fn sidecar_color_range_samples_are_capped() {
        use lightcraft_develop::{MAX_COLOR_RANGE_SAMPLES, Mask, MaskComponent, MaskOp, MaskShape};
        let samples: Vec<[f64; 3]> = (0..1000).map(|i| [i as f64 / 1000.0, 0.1, -0.1]).collect();
        let mut d = DevelopSettings::default();
        let shape = MaskShape::ColorRange { samples: samples.clone(), refine: 50.0 };
        d.masks.push(Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], ..Default::default() });
        let xmp = lightcraft_meta::write_xmp(&lightcraft_meta::Metadata::default(), Some(&d.to_json().to_string()));
        let sc = super::parse_sidecar(&xmp, crate::crs::Target::Rendered).unwrap();
        let Some(DevelopPatch::Full(s)) = sc.develop else { panic!("expected full settings") };
        let Some(MaskShape::ColorRange { samples: got, .. }) = s.masks.first().and_then(|m| m.components.first()).map(|c| &c.shape) else {
            panic!("expected a colour range")
        };
        assert_eq!(got.as_slice(), &samples[..MAX_COLOR_RANGE_SAMPLES]);
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
        lightcraft_meta::write_xmp(&lightcraft_meta::Metadata { rating: Some(rating), ..Default::default() }, None)
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
        let partial = lightcraft_meta::jpeg_segments(&bytes).xmp.unwrap();
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
