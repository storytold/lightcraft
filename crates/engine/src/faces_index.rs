//! The face index: one embedding per face region, kept in memory and cached beside the library.
//!
//! Embedding a face costs a model run (tens to hundreds of milliseconds), so each region is embedded once and
//! remembered, keyed by its photo and its box. The cache file (`face-embeddings-<model id>.bin` in the library folder,
//! one per model, so switching back to an earlier model does not start over) belongs to one model: its header names the
//! model and its file hash, and a file made by another model, an older format, or a damaged one is simply ignored and
//! rebuilt. Embeddings are kept out of the catalog on purpose: they are large, rebuildable, and not comparable between
//! models.
//!
//! Each face also keeps the box to cut its picture from (its "view"): the detector's box when the detector found the same
//! face, so every face is shown equally close however loosely or tightly its own region was drawn (a Lightroom box, an
//! older tool's box), else the region's own box.
//!
//! Layout (little-endian): `"LCFE"`, version `2`, tag length `u16`, the tag (`<model id>@<sha256>`), dimension `u32`,
//! then records of `photo u64`, four `i32` (the region's box in ten-thousandths), four `i32` (the view box likewise), and
//! `dim` `f32`.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dac_geom::Rect;

const MAGIC: &[u8; 4] = b"LCFE";
const VERSION: u8 = 2;
/// Largest cache file we read.
const MAX_FILE: u64 = 1 << 30;

pub(crate) type Key = (u64, [i32; 4]);

/// One face's record: its embedding and its view box (quantized like a key).
type Entry = (Arc<[f32]>, [i32; 4]);

/// A quantized box as a rectangle again.
fn rect_of(k: [i32; 4]) -> Rect {
    let f = |v: i32| f64::from(v) / 10_000.0;
    Rect { x0: f(k[0]), y0: f(k[1]), x1: f(k[2]), y1: f(k[3]) }
}

/// A region's box as the integer key it is cached under.
pub(crate) fn region_key(r: &Rect) -> [i32; 4] {
    let q = |v: f64| if v.is_finite() { (v.clamp(-1e3, 1e3) * 10_000.0).round() as i32 } else { 0 };
    [q(r.x0), q(r.y0), q(r.x1), q(r.y1)]
}

#[derive(Default)]
pub(crate) struct Index {
    /// `<model id>@<sha256>` of the model the embeddings belong to.
    pub tag: String,
    pub dim: usize,
    entries: HashMap<Key, Entry>,
    unsaved: Vec<(Key, Entry)>,
    /// Faces that could not be embedded this session (so they are not retried on every call).
    skipped: std::collections::HashSet<Key>,
    /// The cache file on disk belongs to this model, so new records can be appended to it.
    file_matches: bool,
    pub path: Option<PathBuf>,
}

impl Index {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn get(&self, photo: u64, rect: &Rect) -> Option<&Arc<[f32]>> {
        self.entries.get(&(photo, region_key(rect))).map(|e| &e.0)
    }

    /// The box to cut this face's picture from, once the scan has looked at it.
    pub fn view(&self, photo: u64, rect: &Rect) -> Option<Rect> {
        self.entries.get(&(photo, region_key(rect))).map(|e| rect_of(e.1))
    }

    #[cfg(test)]
    pub fn contains(&self, photo: u64, rect: &Rect) -> bool {
        self.entries.contains_key(&(photo, region_key(rect)))
    }

    /// Whether this face still has to be embedded.
    pub fn needs(&self, photo: u64, rect: &Rect) -> bool {
        let key = (photo, region_key(rect));
        !self.entries.contains_key(&key) && !self.skipped.contains(&key)
    }

    pub fn skip(&mut self, photo: u64, rect: &Rect) {
        self.skipped.insert((photo, region_key(rect)));
    }

    #[cfg(test)]
    pub fn insert(&mut self, photo: u64, rect: &Rect, embedding: Vec<f32>) {
        self.insert_with_view(photo, rect, embedding, rect);
    }

    /// Remember a face's embedding and the box to show it by.
    pub fn insert_with_view(&mut self, photo: u64, rect: &Rect, embedding: Vec<f32>, view: &Rect) {
        let entry: Entry = (embedding.into(), region_key(view));
        let key = (photo, region_key(rect));
        self.entries.insert(key, entry.clone());
        self.unsaved.push((key, entry));
    }

    /// Whether there are embeddings not yet written to the cache file.
    pub fn has_unsaved(&self) -> bool {
        !self.unsaved.is_empty()
    }

    /// Start over for `tag` (`dim` numbers per face), loading what the cache file at `path` holds for it. What the
    /// previous model's index had not written yet is written first, so switching models loses nothing.
    pub fn reset(&mut self, tag: &str, dim: usize, path: Option<PathBuf>) {
        let _ = self.save();
        self.tag = tag.to_string();
        self.dim = dim;
        self.path = path;
        self.entries.clear();
        self.unsaved.clear();
        self.skipped.clear();
        self.file_matches = false;
        if let Some(p) = self.path.clone() {
            self.load(&p);
        }
    }

    fn load(&mut self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        if meta.len() > MAX_FILE {
            return;
        }
        let Ok(bytes) = std::fs::read(path) else { return };
        let Some((tag_len, rest)) = bytes
            .strip_prefix(MAGIC)
            .and_then(|r| r.split_first())
            .filter(|(v, _)| **v == VERSION)
            .and_then(|(_, r)| r.split_at_checked(2))
            .map(|(l, r)| (usize::from(u16::from_le_bytes([l[0], l[1]])), r))
        else {
            return;
        };
        let Some((tag, rest)) = rest.split_at_checked(tag_len) else { return };
        let Some((dim, mut rest)) = rest.split_at_checked(4).map(|(d, r)| (u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize, r)) else { return };
        if tag != self.tag.as_bytes() || dim != self.dim || dim == 0 {
            return;
        }
        self.file_matches = true;
        let record = 8 + 16 + 16 + dim * 4;
        while let Some((rec, tail)) = rest.split_at_checked(record) {
            rest = tail;
            let photo = u64::from_le_bytes([rec[0], rec[1], rec[2], rec[3], rec[4], rec[5], rec[6], rec[7]]);
            let int = |at: usize| i32::from_le_bytes([rec[at], rec[at + 1], rec[at + 2], rec[at + 3]]);
            let key = [int(8), int(12), int(16), int(20)];
            let view = [int(24), int(28), int(32), int(36)];
            let emb: Vec<f32> = rec[40..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
            if emb.iter().all(|v| v.is_finite()) {
                self.entries.insert((photo, key), (emb.into(), view));
            }
        }
    }

    fn header(&self) -> Vec<u8> {
        let tag = self.tag.as_bytes();
        let mut h = Vec::with_capacity(11 + tag.len());
        h.extend_from_slice(MAGIC);
        h.push(VERSION);
        h.extend_from_slice(&u16::try_from(tag.len()).unwrap_or(0).to_le_bytes());
        h.extend_from_slice(tag);
        h.extend_from_slice(&(self.dim as u32).to_le_bytes());
        h
    }

    fn record(&self, key: &Key, entry: &Entry) -> Vec<u8> {
        let mut r = Vec::with_capacity(40 + entry.0.len() * 4);
        r.extend_from_slice(&key.0.to_le_bytes());
        key.1.iter().chain(entry.1.iter()).for_each(|k| r.extend_from_slice(&k.to_le_bytes()));
        entry.0.iter().for_each(|v| r.extend_from_slice(&v.to_le_bytes()));
        r
    }

    /// Write what was added since the last save: appended when the file already belongs to this model, else the
    /// whole index is written fresh (atomically). A failure is returned, the records stay pending.
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            self.unsaved.clear();
            return Ok(());
        };
        if self.unsaved.is_empty() {
            return Ok(());
        }
        if self.file_matches {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
            let mut buf = Vec::new();
            for (k, e) in &self.unsaved {
                buf.extend(self.record(k, e));
            }
            f.write_all(&buf)?;
        } else {
            let mut buf = self.header();
            for (k, e) in &self.entries {
                buf.extend(self.record(k, e));
            }
            let part = path.with_extension("part");
            std::fs::write(&part, &buf)?;
            std::fs::rename(&part, &path)?;
            self.file_matches = true;
        }
        self.unsaved.clear();
        Ok(())
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        let _ = self.save();
    }
}

const SCAN_MAGIC: &[u8; 4] = b"LCFS";
const SCAN_VERSION: u8 = 1;
/// The detector a search was made with: a record made by another one is ignored (a better detector may find more).
pub(crate) const SCAN_TAG: &str = "yunet-2023mar";
/// Largest record file we read.
const SCAN_MAX_FILE: u64 = 64 << 20;

/// The photos the detector has already searched for faces, so each is searched once, however many times the app
/// starts. Kept beside the library (`face-scanned.bin`: a header naming the detector, then photo ids, appended to).
#[derive(Default)]
pub(crate) struct Scanned {
    set: std::collections::HashSet<u64>,
    unsaved: Vec<u64>,
    path: Option<PathBuf>,
    file_matches: bool,
}

impl Scanned {
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn contains(&self, photo: u64) -> bool {
        self.set.contains(&photo)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn insert(&mut self, photo: u64) {
        if self.set.insert(photo) {
            self.unsaved.push(photo);
        }
    }

    pub fn has_unsaved(&self) -> bool {
        !self.unsaved.is_empty()
    }

    /// Start over with the record at `path` (written first: what was not yet saved is not lost).
    pub fn reset(&mut self, path: Option<PathBuf>) {
        let _ = self.save();
        self.set.clear();
        self.unsaved.clear();
        self.file_matches = false;
        self.path = path;
        if let Some(p) = self.path.clone() {
            self.load(&p);
        }
    }

    fn header() -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(SCAN_MAGIC);
        h.push(SCAN_VERSION);
        h.extend_from_slice(&(SCAN_TAG.len() as u16).to_le_bytes());
        h.extend_from_slice(SCAN_TAG.as_bytes());
        h
    }

    fn load(&mut self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        if meta.len() > SCAN_MAX_FILE {
            return;
        }
        let Ok(bytes) = std::fs::read(path) else { return };
        let head = Self::header();
        let Some(rest) = bytes.strip_prefix(head.as_slice()) else { return };
        self.file_matches = true;
        // a half-written last id (a crash while appending) is ignored
        self.set.extend(rest.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)));
    }

    /// Write what was added since the last save: appended when the file is this detector's, else the whole record
    /// is written fresh (atomically). A failure is returned and the ids stay pending.
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            self.unsaved.clear();
            return Ok(());
        };
        if self.unsaved.is_empty() {
            return Ok(());
        }
        if self.file_matches {
            let mut f = std::fs::OpenOptions::new().append(true).open(&path)?;
            let buf: Vec<u8> = self.unsaved.iter().flat_map(|id| id.to_le_bytes()).collect();
            f.write_all(&buf)?;
        } else {
            let mut buf = Self::header();
            buf.extend(self.set.iter().flat_map(|id| id.to_le_bytes()));
            let part = path.with_extension("part");
            std::fs::write(&part, &buf)?;
            std::fs::rename(&part, &path)?;
            self.file_matches = true;
        }
        self.unsaved.clear();
        Ok(())
    }
}

impl Drop for Scanned {
    fn drop(&mut self) {
        let _ = self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64) -> Rect {
        Rect { x0: x, y0: 0.1, x1: x + 0.2, y1: 0.4 }
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lc-faceindex-{name}-{}.bin", std::process::id()))
    }

    #[test]
    fn embeddings_survive_a_save_and_load_and_append() {
        let path = temp("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut a = Index::default();
        a.reset("m@abc", 3, Some(path.clone()));
        a.insert(7, &rect(0.1), vec![1.0, 0.0, 0.5]);
        a.insert(8, &rect(0.3), vec![0.0, 1.0, 0.25]);
        a.save().unwrap();
        a.insert(9, &rect(0.5), vec![0.5, 0.5, 0.5]);
        a.save().unwrap(); // appended
        let mut b = Index::default();
        b.reset("m@abc", 3, Some(path.clone()));
        assert_eq!(b.len(), 3);
        assert_eq!(b.get(8, &rect(0.3)).map(|e| e.to_vec()), Some(vec![0.0, 1.0, 0.25]));
        assert!(b.contains(9, &rect(0.5)) && !b.contains(9, &rect(0.6)) && !b.contains(1, &rect(0.5)));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn another_model_a_damaged_file_or_a_torn_tail_are_handled() {
        let path = temp("other");
        let _ = std::fs::remove_file(&path);
        let mut a = Index::default();
        a.reset("m@abc", 3, Some(path.clone()));
        a.insert(1, &rect(0.1), vec![1.0, 0.0, 0.0]);
        a.insert(2, &rect(0.2), vec![0.0, 1.0, 0.0]);
        a.save().unwrap();
        // another model: nothing is loaded, and saving replaces the file for the new model
        let mut other = Index::default();
        other.reset("n@def", 4, Some(path.clone()));
        assert_eq!(other.len(), 0);
        other.insert(5, &rect(0.1), vec![1.0, 0.0, 0.0, 0.0]);
        other.save().unwrap();
        let mut again = Index::default();
        again.reset("n@def", 4, Some(path.clone()));
        assert_eq!(again.len(), 1);
        let mut old = Index::default();
        old.reset("m@abc", 3, Some(path.clone()));
        assert_eq!(old.len(), 0, "the old model's records are gone");
        // a torn last record loses only that record
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
        let mut torn = Index::default();
        torn.reset("n@def", 4, Some(path.clone()));
        assert_eq!(torn.len(), 0);
        // garbage of every kind is ignored
        for junk in [Vec::new(), b"LCFE".to_vec(), b"LCFE\x01\xff\xff".to_vec(), vec![0xff; 200], b"LCFE\x02\x00\x00".to_vec()] {
            std::fs::write(&path, &junk).unwrap();
            let mut j = Index::default();
            j.reset("n@def", 4, Some(path.clone()));
            assert_eq!(j.len(), 0);
        }
        // and a missing file is fine
        let _ = std::fs::remove_file(&path);
        let mut m = Index::default();
        m.reset("n@def", 4, Some(path.clone()));
        assert_eq!(m.len(), 0);
    }

    #[test]
    fn non_finite_boxes_and_vectors_do_not_break_the_keys() {
        assert_eq!(region_key(&Rect { x0: f64::NAN, y0: f64::INFINITY, x1: 1e300, y1: -1e300 }), [0, 0, 10_000_000, -10_000_000]);
        let path = temp("nan");
        let _ = std::fs::remove_file(&path);
        let mut a = Index::default();
        a.reset("m@abc", 2, Some(path.clone()));
        a.insert(1, &rect(0.1), vec![f32::NAN, 1.0]);
        a.save().unwrap();
        let mut b = Index::default();
        b.reset("m@abc", 2, Some(path.clone()));
        assert_eq!(b.len(), 0, "a non-finite vector is not loaded back");
        let _ = std::fs::remove_file(path);
    }
}

/// What the recognition commands keep between calls: the loaded model and the embeddings made with it.
#[derive(Default)]
pub(crate) struct FacesState {
    /// `<model id>@<sha256>` and the loaded model.
    pub embedder: Option<(String, Arc<dac_faces::runtime::Embedder>)>,
    pub index: Index,
    /// When the models folder was last looked at (the per-frame pump trusts a look for a second).
    pub checked: Option<std::time::Instant>,
    /// Whether recognition was switched on when last looked up, and when.
    pub enabled_seen: Option<(std::time::Instant, bool)>,
    pub worker: Option<crate::faces_worker::Worker>,
    /// After a failure to load the chosen model, the background pump leaves it alone until then.
    pub retry_at: Option<std::time::Instant>,
    /// Photos the worker is busy with.
    pub in_flight: std::collections::HashSet<dac_catalog::PhotoId>,
    /// Photos still to hand to the worker, and the catalog revision this list was made at.
    pub queue: Vec<dac_catalog::PhotoId>,
    pub queue_stamp: Option<u64>,
    /// When the index was last written to its cache file by the background pump.
    pub saved_at: Option<std::time::Instant>,
    /// Bumped when another library is opened: what a worker was making for the old one is dropped.
    pub epoch: u64,
    /// Photos already searched for faces.
    pub scanned: Scanned,
    /// Photos that could not be searched this session (unreadable files): left alone until the next launch.
    pub scan_failed: std::collections::HashSet<u64>,
    /// The scan's row in the activity stack, while photos are left.
    pub task: Option<crate::activity::TaskGuard>,
    /// The most photos left at once since the scan last finished (the row's whole).
    pub peak: u64,
}

impl FacesState {
    /// Another library is being opened: what was learned about the old one is saved, and nothing of it is kept (photo
    /// ids are only unique within a library). The loaded model stays; the index is reloaded for the new library.
    pub fn library_changed(&mut self) {
        self.index.reset("", 0, None);
        self.scanned.reset(None);
        self.scan_failed.clear();
        self.embedder = None;
        self.checked = None;
        self.queue.clear();
        self.queue_stamp = None;
        self.in_flight.clear();
        self.retry_at = None;
        self.saved_at = None;
        self.epoch = self.epoch.wrapping_add(1);
    }
}
