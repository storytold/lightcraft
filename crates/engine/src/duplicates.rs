//! The duplicate finder ([`crate::cmd::duplicates`], `library.findDuplicates`): photos whose
//! file's bytes another photo of the library already has (re-imported copies), and, one scan
//! deeper, copies that differ — resized, re-compressed, renamed — found by comparing a perceptual
//! hash of each photo's picture ([`crate::phash`]): a raw by its camera JPEG, an image file by
//! itself, a clip by a poster frame. Raw + JPEG pairs of the same shot group too; they are their
//! own kind (`sameShot`), something to stack, not to delete.
//!
//! The scan is [`crate::cmd::missing`]'s shape: [`candidates`] snapshots the library on the
//! caller's thread, then [`scan`] touches only the disk and the caches — never the session — so
//! the app runs it on a worker with a cancel and progress, and the command runs it inline. A
//! photo's content hash, when its record has none (an import from before content hashes, a
//! Lightroom catalog import), is read from its file during the similar scan; it is not written
//! back. A clip that cannot be decoded is reported as failed, not hashed: the poster the render
//! path would draw for it ([`crate::media::video_poster`]) is the same tile for every such clip,
//! and hashing that tile would call them all duplicates of one another.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use lcc_catalog::{Catalog, MediaKind, Photo, PhotoId};
use lcc_preview::PreviewCache;
use serde_json::{Value, json};

use crate::eyes::{EyeSource, load as load_picture};
use crate::phash::{MATCH_DISTANCE, hash_rgb32f, hash_rgba8};

/// One library photo as the finder sees it, snapshotted on the caller's thread.
#[derive(Clone, Debug)]
pub struct DupCandidate {
    pub id: PhotoId,
    /// The photo's file, when it has one (a demo photo has none).
    pub path: Option<String>,
    pub kind: MediaKind,
    pub width: u32,
    pub height: u32,
    pub file_size: u64,
    pub imported: String,
    /// The record's content hash without the `:dup<id>` a duplicate's carries; `None` when the
    /// record never had one.
    pub hash: Option<String>,
}

/// Whether `p` is one the finder looks at: a library photo (not in Recently Deleted, not one only
/// browsed in Local) and not a virtual copy (the same photo as its master).
fn in_scope(p: &Photo) -> bool {
    p.in_library() && p.copy_of.is_none()
}

/// Every in-scope photo, in id order.
pub fn candidates(cat: &Catalog) -> Vec<DupCandidate> {
    cat.photos()
        .filter(|p| in_scope(p))
        .map(|p| DupCandidate {
            id: p.id,
            path: match &p.source {
                lcc_catalog::Source::File { path } => Some(path.clone()),
                _ => None,
            },
            kind: p.kind,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            imported: p.imported.clone(),
            hash: p.content_hash.as_deref().map(crate::sync::base_hash).map(str::to_string),
        })
        .collect()
}

/// What makes a group of photos duplicates of one another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GroupKind {
    /// The same file bytes (a re-imported copy).
    Exact,
    /// Copies that differ (resized, re-compressed, renamed) but show the same picture.
    Similar,
    /// A raw and its JPEG pair of the same shot: something to stack, not to delete.
    SameShot,
}

impl GroupKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GroupKind::Exact => "exact",
            GroupKind::Similar => "similar",
            GroupKind::SameShot => "sameShot",
        }
    }
}

/// A group of photos showing one picture: the best copy to keep first ([`rank`]), each with
/// whether its file's bytes are that copy's.
#[derive(Clone, Debug)]
pub struct DupGroup {
    pub kind: GroupKind,
    /// (id, identical to the first member's bytes).
    pub members: Vec<(PhotoId, bool)>,
}

/// How a scan went.
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub groups: Vec<DupGroup>,
    /// Pictures looked at for the similar pass.
    pub scanned: usize,
    /// Photos whose record has no content hash.
    pub unhashed: usize,
    /// Pictures that could not be read.
    pub failed: usize,
    /// Why the first few reads failed.
    pub failures: Vec<(PhotoId, String)>,
    pub cancelled: bool,
}

/// The scan itself. `loader` reads a raw's camera JPEG; without it raws count as failed. `cache`
/// keeps perceptual hashes on disk ([`crate::phash_store`]); without it they live for this scan
/// only. `progress` hears (done, total) of the similar pass. Never touches the session.
pub fn scan(
    candidates: &[DupCandidate],
    loader: Option<&crate::media::PreviewLoader>,
    cache: Option<&PreviewCache>,
    similar: bool,
    cancel: &AtomicBool,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> ScanReport {
    let mut report = ScanReport { unhashed: candidates.iter().filter(|c| c.hash.is_none()).count(), ..ScanReport::default() };
    let mut hashes: Vec<Option<String>> = candidates.iter().map(|c| c.hash.clone()).collect();
    let mut phashes: Vec<Option<u64>> = vec![None; candidates.len()];

    if similar {
        use rayon::prelude::*;
        // what to look at: every photo with a file — its stored perceptual hash, else its picture
        let targets: Vec<usize> = candidates.iter().enumerate().filter_map(|(i, c)| c.path.is_some().then_some(i)).collect();
        let total = targets.len();
        let done = std::sync::atomic::AtomicUsize::new(0);
        let results: Vec<Option<(usize, Option<String>, Option<u64>, Option<String>)>> = targets
            .into_par_iter()
            .map(|i| {
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                let path = candidates[i].path.clone().unwrap_or_default();
                let (hash, phash, why) = look_at(&candidates[i], &path, loader, cache);
                progress(done.fetch_add(1, Ordering::Relaxed) + 1, total);
                Some((i, hash, phash, why))
            })
            .collect();
        for r in results.into_iter().flatten() {
            let (i, hash, phash, why) = r;
            if let Some(h) = hash {
                hashes[i] = Some(h);
            }
            phashes[i] = phash;
            if let Some(w) = why {
                report.failed += 1;
                if report.failures.len() < 20 {
                    report.failures.push((candidates[i].id, w));
                }
            }
        }
        report.scanned = total;
        report.cancelled = cancel.load(Ordering::Relaxed);
    }

    report.groups = group(candidates, &hashes, &phashes);
    report
}

/// The groups photos fall into by their content hashes and perceptual hashes: same bytes, or
/// hashes within [`MATCH_DISTANCE`] bits of each other (a clip only with clips: its poster is not
/// the same picture as a still). Two photos whose records have no hash never count as exact.
/// A hash's `:dup<id>` suffix (a duplicate's own key) is stripped here as well, so a caller that
/// passes stored hashes unstripped groups them with their base.
fn group(candidates: &[DupCandidate], hashes: &[Option<String>], phashes: &[Option<u64>]) -> Vec<DupGroup> {
    let hashes: Vec<Option<String>> = hashes.iter().map(|h| h.as_deref().map(crate::sync::base_hash).map(str::to_string)).collect();
    let mut uf = UnionFind::new(candidates.len());
    let mut by_hash: HashMap<&str, usize> = HashMap::new();
    for (i, h) in hashes.iter().enumerate() {
        if let Some(h) = h {
            if let Some(j) = by_hash.get(h.as_str()) {
                uf.union(*j, i);
            } else {
                by_hash.insert(h.as_str(), i);
            }
        }
    }
    pair_similar(phashes, candidates, |a, b| uf.union(a, b));

    let mut groups: Vec<DupGroup> = Vec::new();
    for (_, members) in uf.groups() {
        let mut ranked: Vec<usize> = members;
        ranked.sort_by(|&a, &b| rank(&candidates[b], &hashes[b]).cmp(&rank(&candidates[a], &hashes[a])));
        let best = ranked.first().and_then(|&i| hashes.get(i)).cloned().flatten();
        let (has_raw, has_image) =
            ranked.iter().fold((false, false), |(r, i), &k| (r || candidates[k].kind == MediaKind::Raw, i || candidates[k].kind == MediaKind::Image));
        let all_one_hash = best.is_some() && ranked.iter().all(|&i| hashes.get(i).and_then(|h| h.as_deref()) == best.as_deref());
        let kind = if all_one_hash {
            GroupKind::Exact
        } else if has_raw && has_image {
            GroupKind::SameShot
        } else {
            GroupKind::Similar
        };
        let members = ranked
            .into_iter()
            .map(|i| (candidates[i].id, hashes.get(i).and_then(|h| h.as_deref()).is_some_and(|h| best.as_deref() == Some(h))))
            .collect();
        groups.push(DupGroup { kind, members });
    }
    groups.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| b.members.len().cmp(&a.members.len())).then_with(|| first_id(a).cmp(&first_id(b))));
    groups
}

/// A photo's missing content hash (read from its file) and its perceptual hash (from the store,
/// else its picture), with why the picture could not be read when it could not.
fn look_at(
    c: &DupCandidate,
    path: &str,
    loader: Option<&crate::media::PreviewLoader>,
    cache: Option<&PreviewCache>,
) -> (Option<String>, Option<u64>, Option<String>) {
    let hash = if c.hash.is_some() { None } else { crate::cmd::missing::file_hash(path) };
    let key = if cache.is_some() { crate::phash_store::file_key(path) } else { None };
    let stored = match (cache, key) {
        (Some(cache), Some(key)) => crate::phash_store::read(cache, key),
        _ => None,
    };
    let phash = match (stored, cache, key) {
        (Some(h), _, _) => Some(h),
        (None, Some(cache), Some(key)) => match picture_hash(c, path, loader) {
            Some(h) => {
                crate::phash_store::write(cache, key, h);
                Some(h)
            }
            None => None,
        },
        (None, _, _) => picture_hash(c, path, loader),
    };
    let why = if phash.is_none() {
        Some(format!("{path}: no picture to compare (no camera JPEG, an unreadable file or an unsupported clip)"))
    } else {
        None
    };
    (hash, phash, why)
}

/// The picture's perceptual hash: a raw by its camera JPEG, an image file by itself, a clip by a
/// poster frame of its own (`None`: no picture to compare).
fn picture_hash(c: &DupCandidate, path: &str, loader: Option<&crate::media::PreviewLoader>) -> Option<u64> {
    match c.kind {
        MediaKind::Raw => loader?(path, 256).map(|img| hash_rgba8(&img)),
        MediaKind::Image => load_picture(&EyeSource::File(path.to_string())).map(|img| hash_rgba8(&img)),
        MediaKind::Video => {
            let frame = lcc_video::poster(std::path::Path::new(path), None, 256).ok()?;
            (frame.image.width > 0 && frame.image.height > 0).then(|| hash_rgb32f(&frame.image))
        }
    }
}

/// Pair every two photos whose hashes sit within [`MATCH_DISTANCE`] bits, by comparing only
/// photos that share a 16-bit band of their 64 (multi-index hashing), and only stills with stills,
/// clips with clips. `pair` is told each such (index, index), the lower first.
fn pair_similar(phashes: &[Option<u64>], candidates: &[DupCandidate], mut pair: impl FnMut(usize, usize)) {
    const BANDS: u32 = 4;
    let mut by_band: Vec<HashMap<u16, Vec<usize>>> = vec![HashMap::new(); BANDS as usize];
    for (i, h) in phashes.iter().enumerate() {
        if let Some(h) = h {
            for b in 0..BANDS {
                let band = ((h >> (b * 16)) & 0xffff) as u16;
                by_band[b as usize].entry(band).or_default().push(i);
            }
        }
    }
    let same_family = |a: usize, b: usize| (candidates[a].kind == MediaKind::Video) == (candidates[b].kind == MediaKind::Video);
    let mut seen: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for (i, h) in phashes.iter().enumerate() {
        let Some(h) = *h else { continue };
        for b in 0..BANDS {
            let band = ((h >> (b * 16)) & 0xffff) as u16;
            if let Some(others) = by_band[b as usize].get(&band) {
                for &j in others {
                    if j <= i || !same_family(i, j) || !seen.insert((i, j)) {
                        continue;
                    }
                    if let Some(hj) = phashes.get(j).copied().flatten()
                        && crate::phash::distance(h, hj) <= MATCH_DISTANCE
                    {
                        pair(i, j);
                    }
                }
            }
        }
    }
}

/// What makes one copy the best to keep, most distinguishing first: the most pixels, then the
/// largest file, then the record that knows its content hash, then the one imported first (the
/// `Reverse` keeps the date ascending inside the descending sort).
fn rank<'a>(c: &'a DupCandidate, hash: &Option<String>) -> (u64, u64, bool, std::cmp::Reverse<&'a str>) {
    let pixels = u64::from(c.width).saturating_mul(u64::from(c.height));
    (pixels, c.file_size, hash.is_some(), std::cmp::Reverse(c.imported.as_str()))
}

fn first_id(g: &DupGroup) -> PhotoId {
    g.members.first().map_or(PhotoId(0), |m| m.0)
}

/// Groups of equal things (the equal photos share a root).
struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> UnionFind {
        UnionFind { parent: (0..n).collect() }
    }
    fn find(&mut self, i: usize) -> usize {
        let mut root = i;
        while let Some(&p) = self.parent.get(root) {
            if p == root {
                break;
            }
            root = p;
        }
        let mut j = i;
        while let Some(&p) = self.parent.get(j) {
            if p == j {
                break;
            }
            if let Some(slot) = self.parent.get_mut(j) {
                *slot = root;
            }
            j = p;
        }
        root
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            let (lo, hi) = if ra < rb { (ra, rb) } else { (rb, ra) };
            if let Some(p) = self.parent.get_mut(hi) {
                *p = lo;
            }
        }
    }
    /// Every member by root, roots in order, only groups of two or more.
    fn groups(&mut self) -> Vec<(usize, Vec<usize>)> {
        let mut by_root: HashMap<usize, Vec<usize>> = HashMap::new();
        for i in 0..self.parent.len() {
            by_root.entry(self.find(i)).or_default().push(i);
        }
        let mut out: Vec<(usize, Vec<usize>)> = by_root.into_iter().filter(|(_, m)| m.len() > 1).collect();
        out.sort_by_key(|(root, _)| *root);
        out
    }
}

/// The report as `library.findDuplicates` returns it: each member's facts come from the live
/// catalog, so the caller sees name, path and size as they are now.
pub fn report_to_json(cat: &Catalog, report: &ScanReport) -> Value {
    let member = |id: PhotoId, identical: bool| -> Value {
        let p = cat.photo(id);
        json!({
            "id": id.0,
            "name": p.map(|p| p.file_name.clone()).unwrap_or_default(),
            "path": p.and_then(|p| match &p.source { lcc_catalog::Source::File { path } => Some(path.clone()), _ => None }).unwrap_or_default(),
            "kind": p.map(|p| p.kind).unwrap_or_default(),
            "format": p.map(|p| p.format.clone()).unwrap_or_default(),
            "width": p.map(|p| p.width).unwrap_or_default(),
            "height": p.map(|p| p.height).unwrap_or_default(),
            "fileSize": p.map(|p| p.file_size).unwrap_or_default(),
            "imported": p.map(|p| p.imported.clone()).unwrap_or_default(),
            "captured": p.and_then(|p| p.captured.clone()),
            "identical": identical,
        })
    };
    let groups: Vec<Value> = report
        .groups
        .iter()
        .map(|g| {
            json!({
                "kind": g.kind.as_str(),
                "photos": g.members.iter().map(|&(id, identical)| member(id, identical)).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "groups": groups,
        "scanned": report.scanned,
        "unhashed": report.unhashed,
        "failed": report.failed,
        "failures": report.failures.iter().map(|(id, why)| json!({"id": id.0, "why": why})).collect::<Vec<_>>(),
        "cancelled": report.cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, hash: Option<&str>, kind: MediaKind, pixels: u64, size: u64) -> DupCandidate {
        DupCandidate {
            id: PhotoId(id),
            path: None,
            kind,
            width: u32::try_from(pixels.max(1)).unwrap_or(1),
            height: 1,
            file_size: size,
            imported: format!("2026-01-{id:02}"),
            hash: hash.map(str::to_string),
        }
    }

    #[test]
    fn same_content_groups_and_the_best_copy_comes_first() {
        let cs = vec![
            cand(1, Some("aa"), MediaKind::Image, 100, 10),
            cand(2, Some("aa"), MediaKind::Image, 400, 10),
            cand(3, Some("aa"), MediaKind::Image, 400, 20),
            cand(4, Some("bb"), MediaKind::Image, 100, 10),
        ];
        let groups = group(&cs, &cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>(), &[None, None, None, None]);
        assert_eq!(groups.len(), 1, "{groups:?}");
        let g = &groups[0];
        assert_eq!(g.kind, GroupKind::Exact);
        // most pixels, then the larger file
        assert_eq!(g.members[0], (PhotoId(3), true));
        assert_eq!(g.members[1], (PhotoId(2), true));
        assert_eq!(g.members[2], (PhotoId(1), true));
    }

    #[test]
    fn dup_suffixed_hashes_group_with_their_base() {
        let cs = vec![
            cand(1, Some("aa"), MediaKind::Image, 100, 10),
            cand(2, Some("aa:dup7"), MediaKind::Image, 100, 10),
            cand(3, Some("bb"), MediaKind::Image, 100, 10),
        ];
        let hashes = cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>();
        let groups = group(&cs, &hashes, &[None, None, None]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members.len(), 2);
    }

    #[test]
    fn photos_without_hashes_never_count_as_exact() {
        // two records with no hash at all share nothing until the similar scan says so
        let cs = vec![cand(1, None, MediaKind::Image, 100, 10), cand(2, None, MediaKind::Image, 100, 10)];
        let groups = group(&cs, &[None, None], &[None, None]);
        assert!(groups.is_empty());
        // and when their pictures match, the group is similar, not exact
        let a = 0x00ff_00ff_00ff_00ff;
        let groups = group(&cs, &[None, None], &[Some(a), Some(a ^ 3)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, GroupKind::Similar);
    }

    #[test]
    fn similar_hashes_group_within_the_match_distance() {
        let a = 0x00ff_00ff_00ff_00ff;
        let cs = vec![
            cand(1, Some("aa"), MediaKind::Image, 100, 10),
            cand(2, Some("bb"), MediaKind::Image, 100, 10),
            cand(3, Some("cc"), MediaKind::Image, 100, 10),
        ];
        let hashes = cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>();
        // 2 within 10 bits of 1, 3 far away
        let groups = group(&cs, &hashes, &[Some(a), Some(a ^ 3), Some(!a)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, GroupKind::Similar);
        assert_eq!(groups[0].members.len(), 2);
        // the keeper is identical only to itself; the other copy differs in bytes
        assert!(groups[0].members[0].1, "the keeper equals its own bytes");
        assert!(!groups[0].members[1].1, "the other copy's bytes differ");
    }

    #[test]
    fn a_raw_and_its_jpeg_are_a_same_shot_group() {
        let a = 0x0f0f_0f0f_0f0f_0f0f;
        let cs = vec![cand(1, Some("aa"), MediaKind::Raw, 2400, 24_000_000), cand(2, Some("bb"), MediaKind::Image, 600, 8_000_000)];
        let hashes = cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>();
        let groups = group(&cs, &hashes, &[Some(a), Some(a ^ 1)]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].kind, GroupKind::SameShot);
        // the raw is the better copy to keep
        assert_eq!(groups[0].members[0].0, PhotoId(1));
    }

    #[test]
    fn clips_only_pair_with_clips() {
        let a = 0xffff_0000_ffff_0000;
        let cs = vec![
            cand(1, Some("aa"), MediaKind::Video, 100, 10),
            cand(2, Some("bb"), MediaKind::Image, 100, 10),
            cand(3, Some("cc"), MediaKind::Video, 100, 10),
        ];
        let hashes = cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>();
        let groups = group(&cs, &hashes, &[Some(a), Some(a), Some(a ^ 2)]);
        // the still's hash equals the clips' but they never pair; the two clips do
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].members, vec![(PhotoId(1), true), (PhotoId(3), false)]);
    }

    #[test]
    fn groups_list_exact_first_then_bigger_groups() {
        let a = 0x00ff_00ff_00ff_00ff;
        let b = 0xf00f_f00f_f00f_f00f;
        let cs = vec![
            cand(1, Some("aa"), MediaKind::Image, 100, 10),
            cand(2, Some("aa"), MediaKind::Image, 100, 10),
            cand(3, Some("cc"), MediaKind::Image, 100, 10),
            cand(4, Some("dd"), MediaKind::Image, 100, 10),
            cand(5, Some("ee"), MediaKind::Image, 100, 10),
        ];
        let hashes = cs.iter().map(|c| c.hash.clone()).collect::<Vec<_>>();
        // 1 and 2 share bytes; 3, 4 and 5 share only their picture
        let groups = group(&cs, &hashes, &[Some(a), Some(a), Some(b), Some(b ^ 3), Some(b ^ 5)]);
        assert_eq!(groups.len(), 2);
        // the exact pair, then the similar triple (bigger than the pair, but exact comes first)
        assert_eq!(groups[0].kind, GroupKind::Exact);
        assert_eq!(groups[0].members.len(), 2);
        assert_eq!(groups[1].kind, GroupKind::Similar);
        assert_eq!(groups[1].members.len(), 3);
    }

    #[test]
    fn an_empty_scan_reports_nothing() {
        let r = scan(&[], None, None, true, &AtomicBool::new(false), &|_, _| {});
        assert!(r.groups.is_empty());
        assert_eq!(r.scanned, 0);
    }
}
