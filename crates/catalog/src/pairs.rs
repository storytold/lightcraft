//! RAW+JPEG pairs: same directory, same filename stem, optional capture-time and camera checks.
//!
//! Detection reads catalog records only. It does not open files. A JPEG in a direct child folder
//! is reported as an export, not joined. A stem the user split stays unpaired until they join
//! those photos again.

use std::collections::HashMap;

use crate::{Catalog, MediaKind, PairRejection, Photo, PhotoId, Source};

/// A raw and JPEG a scan would join.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairCandidate {
    pub raw: PhotoId,
    pub jpeg: PhotoId,
}

/// Same stem, not joined.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairAmbiguous {
    pub directory: String,
    pub stem: String,
    pub reason: &'static str,
    pub ids: Vec<PhotoId>,
}

/// A JPEG in a direct child folder of a raw with the same stem (usually an export).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairExport {
    pub raw: PhotoId,
    pub jpeg: PhotoId,
}

/// One pass over the catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PairScan {
    pub pairs: Vec<PairCandidate>,
    pub ambiguous: Vec<PairAmbiguous>,
    pub exports: Vec<PairExport>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Raw,
    Jpeg,
    Heic,
    Other,
}

struct Item {
    id: PhotoId,
    side: Side,
}

/// Parent directory (slashes normalized) and lower-case stem.
pub fn location_key(path: &str, file_name: &str) -> Option<(String, String)> {
    let stem = file_stem(file_name)?;
    Some((parent_dir(path), stem.to_lowercase()))
}

fn file_stem(name: &str) -> Option<&str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() { None } else { Some(stem) }
}

fn extension(name: &str) -> Option<&str> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.').map(|(_, ext)| ext).filter(|ext| !ext.is_empty())
}

/// Directory key: slashes folded, no trailing slash (`/` stays `/`).
pub fn normalize_dir(dir: &str) -> String {
    let n = dir.replace('\\', "/");
    let trimmed = n.trim_end_matches('/');
    if trimmed.is_empty() && n.starts_with('/') { "/".to_string() } else { trimmed.to_string() }
}

/// Directory portion of a file path, with backslashes folded to slashes.
pub fn parent_dir(path: &str) -> String {
    let n = path.replace('\\', "/");
    match n.rfind('/') {
        Some(0) => "/".to_string(),
        Some(i) => n[..i].trim_end_matches('/').to_string(),
        None => String::new(),
    }
}

fn side_of(p: &Photo) -> Side {
    if p.kind == MediaKind::Raw {
        return Side::Raw;
    }
    let ext = extension(&p.file_name).unwrap_or("");
    let format = p.format.as_str();
    let heic = |s: &str| s.eq_ignore_ascii_case("heic") || s.eq_ignore_ascii_case("heif");
    let jpeg = |s: &str| s.eq_ignore_ascii_case("jpg") || s.eq_ignore_ascii_case("jpeg");
    if heic(ext) || (ext.is_empty() && heic(format)) {
        Side::Heic
    } else if jpeg(ext) || jpeg(format) {
        Side::Jpeg
    } else if heic(format) {
        Side::Heic
    } else {
        Side::Other
    }
}

fn eligible(p: &Photo) -> bool {
    p.copy_of.is_none() && !p.local && matches!(p.source, Source::File { .. }) && p.kind != MediaKind::Video
}

fn file_path(p: &Photo) -> Option<&str> {
    match &p.source {
        Source::File { path } => Some(path),
        Source::Demo { .. } => None,
    }
}

/// Capture times, when both parse, are at most 2 seconds apart, and camera strings match when
/// both are non-empty.
pub fn metadata_agrees(raw: &Photo, jpeg: &Photo) -> bool {
    if let (Some(a), Some(b)) =
        (raw.captured.as_deref().and_then(crate::stacks::iso_seconds), jpeg.captured.as_deref().and_then(crate::stacks::iso_seconds))
        && (a - b).abs() > 2
    {
        return false;
    }
    let ca = raw.meta.camera.trim();
    let cb = jpeg.meta.camera.trim();
    if !ca.is_empty() && !cb.is_empty() && !ca.eq_ignore_ascii_case(cb) {
        return false;
    }
    true
}

/// Whether these photos form a camera pair, including links restored while a member is deleted.
/// Detection excludes deleted records separately; restoring a journaled link must keep them.
pub fn photos_match(raw: &Photo, jpeg: &Photo) -> bool {
    if !eligible(raw) || !eligible(jpeg) || side_of(raw) != Side::Raw || side_of(jpeg) != Side::Jpeg {
        return false;
    }
    let (Some(rp), Some(jp)) = (file_path(raw), file_path(jpeg)) else { return false };
    let (Some((rd, rs)), Some((jd, js))) = (location_key(rp, &raw.file_name), location_key(jp, &jpeg.file_name)) else { return false };
    rd == jd && rs == js && metadata_agrees(raw, jpeg)
}

fn direct_child(parent: &str, child_parent: &str) -> bool {
    if parent.is_empty() || child_parent.len() <= parent.len() {
        return false;
    }
    let Some(rest) = child_parent.strip_prefix(parent) else { return false };
    let Some(rest) = rest.strip_prefix('/') else { return false };
    !rest.is_empty() && !rest.contains('/')
}

/// Classify every eligible file. Rejected stems are listed as ambiguous (`"rejected"`) and are
/// not pairs. `O(photos)`.
pub fn scan(cat: &Catalog) -> PairScan {
    let mut groups: HashMap<(String, String), Vec<Item>> = HashMap::new();
    let mut located: Vec<(PhotoId, String, String, Side)> = Vec::new();
    for p in cat.photos() {
        if p.deleted || !eligible(p) {
            continue;
        }
        let Some(path) = file_path(p) else { continue };
        let Some((dir, stem)) = location_key(path, &p.file_name) else { continue };
        let side = side_of(p);
        groups.entry((dir.clone(), stem.clone())).or_default().push(Item { id: p.id, side });
        located.push((p.id, dir, stem, side));
    }
    let mut out = PairScan::default();
    let mut keys: Vec<(String, String)> = groups.keys().cloned().collect();
    keys.sort();
    for key in keys {
        let Some(items) = groups.get(&key) else { continue };
        let (dir, stem) = &key;
        let mut ids: Vec<PhotoId> = items.iter().map(|i| i.id).collect();
        ids.sort();
        if cat.pair_rejected(dir, stem) {
            out.ambiguous.push(PairAmbiguous { directory: dir.clone(), stem: stem.clone(), reason: "rejected", ids });
            continue;
        }
        let raws: Vec<PhotoId> = items.iter().filter(|i| i.side == Side::Raw).map(|i| i.id).collect();
        let jpegs: Vec<PhotoId> = items.iter().filter(|i| i.side == Side::Jpeg).map(|i| i.id).collect();
        let heics = items.iter().filter(|i| i.side == Side::Heic).count();
        let others = items.iter().filter(|i| i.side == Side::Other).count();
        let ambiguous = |reason: &'static str| PairAmbiguous { directory: dir.clone(), stem: stem.clone(), reason, ids: ids.clone() };
        if raws.len() >= 2 {
            out.ambiguous.push(ambiguous("two raws"));
            continue;
        }
        if jpegs.len() >= 2 {
            out.ambiguous.push(ambiguous("more than one JPEG"));
            continue;
        }
        if raws.len() == 1 && jpegs.is_empty() && heics > 0 {
            out.ambiguous.push(ambiguous("HEIC companion"));
            continue;
        }
        if raws.len() != 1 || jpegs.len() != 1 || heics > 0 || others > 0 {
            if items.len() >= 2 && !(raws.is_empty() && jpegs.len() <= 1 && heics == 0) {
                out.ambiguous.push(ambiguous("not a raw and a JPEG"));
            }
            continue;
        }
        let (Some(raw), Some(jpeg)) = (cat.photo(raws[0]), cat.photo(jpegs[0])) else { continue };
        if !metadata_agrees(raw, jpeg) {
            let reason = if !cameras_agree(raw, jpeg) { "camera mismatch" } else { "capture time mismatch" };
            out.ambiguous.push(ambiguous(reason));
            continue;
        }
        out.pairs.push(PairCandidate { raw: raw.id, jpeg: jpeg.id });
    }
    for (id, dir, stem, side) in &located {
        if *side != Side::Jpeg {
            continue;
        }
        for (raw_id, raw_dir, raw_stem, raw_side) in &located {
            if *raw_side != Side::Raw || raw_stem != stem || !direct_child(raw_dir, dir) {
                continue;
            }
            out.exports.push(PairExport { raw: *raw_id, jpeg: *id });
        }
    }
    out.exports.sort_by_key(|e| (e.raw, e.jpeg));
    out
}

fn cameras_agree(raw: &Photo, jpeg: &Photo) -> bool {
    let ca = raw.meta.camera.trim();
    let cb = jpeg.meta.camera.trim();
    ca.is_empty() || cb.is_empty() || ca.eq_ignore_ascii_case(cb)
}

impl Catalog {
    pub fn pair_rejected(&self, directory: &str, stem: &str) -> bool {
        let key = PairRejection { directory: normalize_dir(directory), stem: stem.to_lowercase() };
        self.rejections().contains(&key)
    }
}
