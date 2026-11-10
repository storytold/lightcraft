//! RAW+JPEG pairs: a raw and the JPEG (or HEIF) the camera wrote next to it, shown as one photo.
//!
//! A camera set to RAW+JPEG writes `DSC_0001.NEF` and `DSC_0001.JPG` side by side. Imported as
//! they are, the library shows every picture twice. [`Catalog::raw_jpeg_pairs`] finds those pairs
//! and [`Catalog::raw_jpeg_stack_ops`] puts each into a collapsed stack of exactly the two files,
//! the raw on top, so the grid shows one photo and the JPEG is one click away (expand the stack).
//! Stacks are ordinary catalog data, so pairing is undoable, journaled and survives a crash like
//! any other edit, needs no new op and no catalog format change, and Ungroup Stack takes it apart again.
//!
//! The pair rule: both files are in the same folder, have the same file name without the
//! extension (letter case ignored: `dsc_0001.nef` pairs with `DSC_0001.JPG`), and the group of
//! library photos with that name holds exactly one raw and exactly one JPEG/HEIF. Two JPEGs
//! (`.JPG` and `.jpeg`) or two raws (a NEF and a DNG made from it) are ambiguous and stay apart,
//! also when one of them is already in a stack. Other files with the name (a TIFF edited in
//! another app) don't stop the pair and stay outside it. The stem is everything before the last
//! `.`, so `IMG_1.edit.jpg` has the stem `img_1.edit` and does not pair with `IMG_1.NEF`. Stems
//! are compared after [`str::to_lowercase`] only: no Unicode normalisation, so an NFC and an NFD
//! spelling of the same accented name are two names. Photos in Recently Deleted, virtual copies,
//! demo photos and photos only browsed in Local never pair, and a pair one of whose photos is
//! already in a stack is left alone.

use std::collections::{HashMap, HashSet};

use crate::local::folder_of;
use crate::{Catalog, MediaKind, Op, Photo, PhotoId};

/// One RAW+JPEG pair. `jpeg` may also be a HEIF (`.heic`, `.heif`, `.hif`) the camera wrote
/// instead of a JPEG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RawJpegPair {
    pub raw: PhotoId,
    pub jpeg: PhotoId,
}

impl RawJpegPair {
    /// The pair as a stack, top first: the raw, unless the raw can only be shown from its embedded
    /// preview (`preview_only`, e.g. a Nikon High Efficiency NEF this build does not decode). Then
    /// the camera's full-size JPEG is the better photo to look at and edit, and goes on top.
    pub fn stack_order(&self, cat: &Catalog) -> [PhotoId; 2] {
        let raw_decodes = cat.photo(self.raw).is_some_and(|p| p.develops_raw());
        if raw_decodes { [self.raw, self.jpeg] } else { [self.jpeg, self.raw] }
    }
}

/// File extensions (lower case) of the rendered companions a camera writes next to a raw (`.hif`:
/// Nikon's and Canon's HEIF).
const COMPANION_EXTENSIONS: [&str; 5] = ["jpg", "jpeg", "heic", "heif", "hif"];

/// Above this many stems asked about, the catalog scan looks each photo's stem up in a set (one
/// lower-cased copy per photo); below it, it compares against the few stems without allocating.
const FEW_STEMS: usize = 32;

/// A file name's stem and extension as written (`None` without an extension or a stem).
fn split_name(file_name: &str) -> Option<(&str, &str)> {
    let (stem, ext) = file_name.rsplit_once('.')?;
    if stem.is_empty() || ext.is_empty() { None } else { Some((stem, ext)) }
}

/// A file name's stem and extension, both lower case (`None` without an extension or a stem).
pub fn stem_and_extension(file_name: &str) -> Option<(String, String)> {
    let (stem, ext) = split_name(file_name)?;
    Some((stem.to_lowercase(), ext.to_lowercase()))
}

/// Whether `p` is a JPEG or HEIF a camera could have written next to a raw.
pub fn is_companion(p: &Photo) -> bool {
    p.kind == MediaKind::Image && split_name(&p.file_name).is_some_and(|(_, ext)| COMPANION_EXTENSIONS.iter().any(|c| ext.eq_ignore_ascii_case(c)))
}

/// Whether `p` may be one half of a pair at all: a raw or a companion, in the library (not only
/// browsed in Local), not in Recently Deleted, not a virtual copy, and backed by a file.
fn may_pair(p: &Photo) -> bool {
    !p.deleted && !p.local && p.copy_of.is_none() && matches!(p.source, crate::Source::File { .. }) && (p.kind == MediaKind::Raw || is_companion(p))
}

/// The key two photos must share to pair: the photo's folder and its file name's stem (lower
/// case). `None` for photos that never pair.
fn pair_key(p: &Photo) -> Option<(String, String)> {
    if !may_pair(p) {
        return None;
    }
    let folder = folder_of(p)?;
    // the stored path's own file name: a copy is catalogued under its new name
    let (stem, _) = stem_and_extension(&p.file_name)?;
    Some((folder, stem))
}

/// Whether a photo's stem is one of `stems` (lower case), cheaply: the scan runs over the whole
/// catalog for every import batch, so a photo whose stem can't match costs no allocation.
fn stem_wanted(file_name: &str, stems: &HashSet<String>, few: &[&str]) -> bool {
    let Some((stem, _)) = split_name(file_name) else { return false };
    if stems.len() > FEW_STEMS || !stem.is_ascii() {
        return stems.contains(&stem.to_lowercase());
    }
    few.iter().any(|w| w.len() == stem.len() && stem.eq_ignore_ascii_case(w))
}

impl Catalog {
    /// The RAW+JPEG pairs that have at least one member in `ids` (see the module docs for the
    /// rule). The partner may be any photo of the catalog, so a pair whose files arrived in two
    /// import batches, or a JPEG imported next to a raw already in the library, is found too. In
    /// the order of `ids`; each pair once. Returns at once when no photo in `ids` is a raw or a
    /// companion; otherwise one pass over the catalog that only looks closer at photos with a
    /// stem asked about.
    pub fn raw_jpeg_pairs(&self, ids: &[PhotoId]) -> Vec<RawJpegPair> {
        let wanted: HashSet<(String, String)> = ids.iter().filter_map(|id| self.photos.get(id)).filter_map(|p| pair_key(p)).collect();
        if wanted.is_empty() {
            return Vec::new();
        }
        let stems: HashSet<String> = wanted.iter().map(|(_, s)| s.clone()).collect();
        let few: Vec<&str> = stems.iter().map(String::as_str).collect();
        // key → (raws, companions), over every library photo with the key: stacked ones included,
        // so a stacked second JPEG still makes the name ambiguous
        let mut groups: HashMap<(String, String), (Vec<PhotoId>, Vec<PhotoId>)> = HashMap::new();
        for p in self.photos.values() {
            if !may_pair(p) || !stem_wanted(&p.file_name, &stems, &few) {
                continue;
            }
            let Some(key) = pair_key(p).filter(|k| wanted.contains(k)) else { continue };
            let g = groups.entry(key).or_default();
            if p.kind == MediaKind::Raw { g.0.push(p.id) } else { g.1.push(p.id) }
        }
        let stacked = self.stack_index();
        let mut seen: HashSet<PhotoId> = HashSet::new();
        let mut out = Vec::new();
        for id in ids {
            let Some(key) = self.photos.get(id).and_then(|p| pair_key(p)) else { continue };
            let Some((raws, jpegs)) = groups.get(&key) else { continue };
            if let ([raw], [jpeg]) = (raws.as_slice(), jpegs.as_slice())
                && !stacked.contains_key(raw)
                && !stacked.contains_key(jpeg)
                && seen.insert(*raw)
            {
                out.push(RawJpegPair { raw: *raw, jpeg: *jpeg });
            }
        }
        out
    }

    /// Ops that stack each pair (collapsed, top as [`RawJpegPair::stack_order`] says). Pairs whose
    /// photos are already stacked are left alone. Empty when there is nothing to do.
    pub fn raw_jpeg_stack_ops(&mut self, pairs: &[RawJpegPair]) -> Vec<Op> {
        let mut ops = Vec::new();
        for pair in pairs {
            if self.stack_of(pair.raw).is_some() || self.stack_of(pair.jpeg).is_some() {
                continue;
            }
            let [top, other] = pair.stack_order(self);
            ops.extend(self.group_ops(top, &[other], true));
        }
        ops
    }

    /// The RAW+JPEG pair a stack of exactly two photos is: one raw and one companion with the same
    /// folder and stem (`None` for any other stack).
    pub fn raw_jpeg_pair_stack(&self, photo: PhotoId) -> Option<RawJpegPair> {
        let st = self.stack_of(photo)?;
        let [a, b] = st.photos.as_slice() else { return None };
        let (pa, pb) = (self.photos.get(a)?, self.photos.get(b)?);
        if pair_key(pa)? != pair_key(pb)? {
            return None;
        }
        match (pa.kind == MediaKind::Raw, pb.kind == MediaKind::Raw) {
            (true, false) => Some(RawJpegPair { raw: *a, jpeg: *b }),
            (false, true) => Some(RawJpegPair { raw: *b, jpeg: *a }),
            _ => None,
        }
    }

    /// After `photos` were re-probed (a raw now decodes, or only shows its preview): ops that put the
    /// right photo on top of their RAW+JPEG pair stacks again ([`RawJpegPair::stack_order`]). Only
    /// stacks of exactly such a pair are touched; one whose top already is the right one is not.
    pub fn raw_jpeg_retop_ops(&self, photos: &[PhotoId]) -> Vec<Op> {
        let mut done = HashSet::new();
        let mut ops = Vec::new();
        for id in photos {
            let Some(pair) = self.raw_jpeg_pair_stack(*id) else { continue };
            if !done.insert(pair.raw) {
                continue;
            }
            let [top, _] = pair.stack_order(self);
            ops.extend(self.set_top_ops(top));
        }
        ops
    }
}
