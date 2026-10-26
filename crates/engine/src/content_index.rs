//! The content index: one image embedding per photo, made by the content model in use, kept in memory as an f16 matrix
//! and cached beside the library, so searching by what a photo shows (`content.search`) is a pass over the matrix.
//!
//! The cache file is `content-embeddings-<model id>.bin` in the library folder, one per model (switching back to an
//! earlier model does not start over). Its header names the model and its weights (`<model id>@<sha256>`): a file made
//! by another model or another version of it, an older format, or a damaged one is ignored and rebuilt. Embeddings are
//! not in the catalog on purpose: they are large, rebuildable and not comparable between models. A library that is not
//! on disk keeps its index in memory only.
//!
//! Layout (little-endian): `"LCCE"`, version `u8` (1), tag length `u16`, the tag, dimension `u32`, element type `u8`
//! (1 = f16); then records of `photo u64`, the photo's content hash (16 bytes) and `dim` f16 values.
//!
//! - Records are appended while the scan runs (saved every few seconds and when the index is dropped). A record torn
//!   by a crash is ignored, and the file is cut back to its last whole record before the next append, so a torn tail
//!   never shifts the records written after it.
//! - A photo indexed again (its original was replaced) or removed from the library leaves a dead record behind; when
//!   more than a fifth of the records are dead the file is rewritten without them (`.part` + rename, atomic).
//! - The content hash is the photo's at the time it was embedded: a record whose hash no longer matches the photo is
//!   stale, and the scan embeds the photo again.
//! - Everything read is bounded: the record count comes from the file's size, the dimension is at most
//!   [`MAX_DIM`], and the matrix never grows past [`MAX_MATRIX_BYTES`] (the index then refuses more, with an error the
//!   status shows).

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};

use half::f16;
use half::slice::HalfFloatSliceExt;
use rayon::prelude::*;

const MAGIC: &[u8; 4] = b"LCCE";
const VERSION: u8 = 1;
/// The element type of the vectors: f16.
const ELEM_F16: u8 = 1;
/// Largest embedding the index holds (the manifests allow no more).
pub(crate) const MAX_DIM: usize = 4096;
/// Longest tag (`<model id>@<sha256>` is under 140 bytes).
const MAX_TAG: usize = 1024;
/// Most bytes of vectors held in memory: 3 GiB is two million photos at 768 dimensions.
pub(crate) const MAX_MATRIX_BYTES: usize = 3 << 30;
/// Most photos the index holds, whatever their dimension.
pub(crate) const MAX_RECORDS: usize = 4_000_000;
/// The file is rewritten without its dead records when more than this share of them is dead…
const COMPACT_SHARE: usize = 5; // (one in five)
/// …and at least this many are.
const COMPACT_MIN: usize = 16;
/// Records read from the file at a time.
const READ_CHUNK: usize = 1024;

/// A photo's content hash as kept in a record.
pub(crate) type Hash = [u8; 16];

/// The cache file of model `id` in the library folder `dir`.
pub(crate) fn file_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("content-embeddings-{id}.bin"))
}

fn record_len(dim: usize) -> usize {
    8 + 16 + dim * 2
}

fn header_of(tag: &str, dim: usize) -> Vec<u8> {
    let tag = tag.as_bytes();
    let mut h = Vec::with_capacity(12 + tag.len());
    h.extend_from_slice(MAGIC);
    h.push(VERSION);
    h.extend_from_slice(&u16::try_from(tag.len()).unwrap_or(0).to_le_bytes());
    h.extend_from_slice(tag);
    h.extend_from_slice(&u32::try_from(dim).unwrap_or(0).to_le_bytes());
    h.push(ELEM_F16);
    h
}

/// The embeddings of one model, for one library.
pub(crate) struct Index {
    /// `<model id>@<sha256 of the weights>`.
    tag: String,
    dim: usize,
    path: Option<PathBuf>,
    /// Row `i` is photo `photos[i]`, embedded when its content hash was `hashes[i]`; its vector is
    /// `data[i * dim .. (i + 1) * dim]`.
    photos: Vec<u64>,
    hashes: Vec<Hash>,
    data: Vec<f16>,
    /// Whether row `i`'s current vector is in the file.
    on_disk: Vec<bool>,
    rows: HashMap<u64, usize>,
    /// Records in the file, and how many of them are dead (superseded or of photos that are gone).
    file_records: usize,
    file_dead: usize,
    /// Bytes of the file that hold whole records (appends start here).
    file_len: u64,
    /// The file on disk is this model's, so records can be appended to it.
    file_matches: bool,
    /// What reading the file found, when it was not simply loaded ("made by another model", "damaged"…).
    pub note: Option<String>,
    /// The last save's failure, until a save works.
    pub save_error: Option<String>,
    /// Bumped by every change of the vectors (a search ranked at one version is still right at it).
    version: u64,
}

impl Index {
    /// An empty index for `tag` (`dim` numbers per photo), kept in memory only.
    pub fn new(tag: &str, dim: usize) -> Index {
        Index {
            tag: tag.to_string(),
            dim: dim.clamp(1, MAX_DIM),
            path: None,
            photos: Vec::new(),
            hashes: Vec::new(),
            data: Vec::new(),
            on_disk: Vec::new(),
            rows: HashMap::new(),
            file_records: 0,
            file_dead: 0,
            file_len: 0,
            file_matches: false,
            note: None,
            save_error: None,
            version: 0,
        }
    }

    /// The index of `tag` cached at `path` (`None`: in memory only): what the file holds for this model, or nothing
    /// when it is missing, another model's, damaged or too large (then it is rebuilt on the first save).
    pub fn open(tag: &str, dim: usize, path: Option<PathBuf>) -> Index {
        let mut index = Index::new(tag, dim);
        index.path = path.clone();
        if let Some(p) = path {
            index.load(&p);
        }
        index
    }

    /// Changes whenever a vector is added, replaced or removed.
    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn tag(&self) -> &str {
        &self.tag
    }

    #[cfg(test)]
    pub fn dim(&self) -> usize {
        self.dim
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Photos indexed.
    pub fn len(&self) -> usize {
        self.photos.len()
    }

    /// The content hash photo `photo` was embedded at.
    pub fn hash(&self, photo: u64) -> Option<Hash> {
        self.rows.get(&photo).and_then(|r| self.hashes.get(*r).copied())
    }

    /// Photo `photo`'s embedding.
    pub fn vector(&self, photo: u64) -> Option<Vec<f32>> {
        let r = *self.rows.get(&photo)?;
        let row = self.data.get(r * self.dim..(r + 1) * self.dim)?;
        let mut out = vec![0.0f32; self.dim];
        row.convert_to_f32_slice(&mut out);
        Some(out)
    }

    /// Every indexed photo with its content hash.
    pub fn entries(&self) -> impl Iterator<Item = (u64, Hash)> + '_ {
        self.photos.iter().copied().zip(self.hashes.iter().copied())
    }

    /// Bytes the vectors take in memory.
    pub fn matrix_bytes(&self) -> usize {
        self.data.len() * 2
    }

    /// Records in the file, and how many of them are dead.
    pub fn file_records(&self) -> (usize, usize) {
        (self.file_records, self.file_dead)
    }

    /// Whether there are embeddings not yet written to the file.
    pub fn has_unsaved(&self) -> bool {
        // (an index with nothing in it leaves another model's file alone until it has something to write)
        // (deletions alone are written once there are enough of them to compact)
        self.path.is_some() && (self.on_disk.iter().any(|d| !d) || (self.file_matches && self.wants_compaction()))
    }

    /// Remember photo `photo`'s embedding (made when its content hash was `hash`), replacing an earlier one. Refused
    /// when the vector is not this index's size or not finite, or when the index is full.
    pub fn insert(&mut self, photo: u64, hash: Hash, v: &[f32]) -> Result<(), String> {
        self.version = self.version.wrapping_add(1);
        if v.len() != self.dim {
            return Err(format!("the model made {} numbers for a photo, not {}", v.len(), self.dim));
        }
        if v.iter().any(|x| !x.is_finite()) {
            return Err("the model made a vector that is not finite".into());
        }
        let row: Vec<f16> = v.iter().map(|x| f16::from_f32(*x)).collect();
        if let Some(&r) = self.rows.get(&photo) {
            let Some(slot) = self.data.get_mut(r * self.dim..(r + 1) * self.dim) else {
                return Err("the index is damaged".into());
            };
            slot.copy_from_slice(&row);
            if let Some(h) = self.hashes.get_mut(r) {
                *h = hash;
            }
            if let Some(d) = self.on_disk.get_mut(r) {
                if *d {
                    self.file_dead += 1;
                }
                *d = false;
            }
            return Ok(());
        }
        if self.photos.len() >= MAX_RECORDS || (self.data.len() + self.dim) * 2 > MAX_MATRIX_BYTES {
            return Err(format!(
                "the content index is full: it holds {} photos, the most LightCraft keeps in memory for this model",
                self.photos.len()
            ));
        }
        self.rows.insert(photo, self.photos.len());
        self.photos.push(photo);
        self.hashes.push(hash);
        self.data.extend_from_slice(&row);
        self.on_disk.push(false);
        Ok(())
    }

    /// Forget photo `photo` (deleted from the library, or to be embedded again). `true` if it was indexed.
    pub fn remove(&mut self, photo: u64) -> bool {
        self.version = self.version.wrapping_add(1);
        let Some(r) = self.rows.remove(&photo) else { return false };
        let last = self.photos.len().saturating_sub(1);
        if self.on_disk.get(r).copied().unwrap_or(false) {
            self.file_dead += 1;
        }
        if r != last {
            // the last row takes the removed one's place
            let (dim, from) = (self.dim, last * self.dim);
            self.data.copy_within(from..from + dim, r * dim);
            if let Some(moved) = self.photos.get(last).copied() {
                self.rows.insert(moved, r);
            }
            self.photos.swap(r, last);
            self.hashes.swap(r, last);
            self.on_disk.swap(r, last);
        }
        self.photos.pop();
        self.hashes.pop();
        self.on_disk.pop();
        self.data.truncate(last * self.dim);
        true
    }

    /// Forget everything, and the file too.
    pub fn clear(&mut self) -> std::io::Result<()> {
        self.version = self.version.wrapping_add(1);
        self.photos.clear();
        self.hashes.clear();
        self.data = Vec::new();
        self.on_disk.clear();
        self.rows.clear();
        self.file_records = 0;
        self.file_dead = 0;
        self.file_len = 0;
        self.file_matches = false;
        self.note = None;
        self.save_error = None;
        match &self.path {
            Some(p) => match std::fs::remove_file(p) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            },
            None => Ok(()),
        }
    }

    /// The cosine of every indexed photo's embedding with `query` (normalised, this index's size), in row order.
    pub fn cosines(&self, query: &[f32]) -> Vec<(u64, f32)> {
        if query.len() != self.dim || self.photos.is_empty() {
            return Vec::new();
        }
        let dim = self.dim;
        // rows in chunks across the threads: 80,000 photos × 768 numbers take a few milliseconds
        self.data
            .par_chunks(dim * 256)
            .enumerate()
            .flat_map_iter(|(chunk, rows)| {
                let mut buf = vec![0.0f32; dim];
                rows.chunks_exact(dim)
                    .enumerate()
                    .map(move |(i, row)| {
                        row.convert_to_f32_slice(&mut buf);
                        let cos: f32 = buf.iter().zip(query).map(|(a, b)| a * b).sum();
                        (chunk * 256 + i, cos)
                    })
                    .collect::<Vec<_>>()
            })
            .filter_map(|(r, cos)| Some((*self.photos.get(r)?, if cos.is_finite() { cos } else { -1.0 })))
            .collect()
    }

    fn load(&mut self, path: &Path) {
        let Ok(meta) = std::fs::metadata(path) else { return };
        let len = meta.len();
        let Ok(file) = std::fs::File::open(path) else {
            self.note = Some("the index file cannot be read: it is rebuilt".into());
            return;
        };
        let mut r = BufReader::with_capacity(1 << 20, file);
        let header = match read_header(&mut r) {
            Ok(h) => h,
            Err(why) => {
                self.note = Some(format!("the index file {why}: it is rebuilt"));
                return;
            }
        };
        if header.tag != self.tag || header.dim != self.dim {
            self.note = Some("the index file was made by another version of the model: it is rebuilt".into());
            return;
        }
        let head_len = header_of(&self.tag, self.dim).len() as u64;
        let rec = record_len(self.dim);
        let records = usize::try_from(len.saturating_sub(head_len) / rec as u64).unwrap_or(usize::MAX);
        if records > MAX_RECORDS || records.saturating_mul(self.dim * 2) > MAX_MATRIX_BYTES {
            self.note = Some(format!("the index file holds {records} records, more than LightCraft keeps in memory: it is rebuilt"));
            return;
        }
        // the hint is the file's own claim; the vectors grow only as records are actually read
        self.photos.reserve(records.min(1 << 16));
        let mut buf = vec![0u8; rec * READ_CHUNK];
        let mut read = 0usize;
        let mut row = vec![f16::ZERO; self.dim];
        while read < records {
            let n = (records - read).min(READ_CHUNK);
            let Some(chunk) = buf.get_mut(..n * rec) else { break };
            if r.read_exact(chunk).is_err() {
                // shorter than its size said (it changed under us): keep what was read
                break;
            }
            for rec_bytes in chunk.chunks_exact(rec) {
                let (Some(id), Some(hash), Some(values)) = (rec_bytes.get(..8), rec_bytes.get(8..24), rec_bytes.get(24..)) else { continue };
                let photo = u64::from_le_bytes(id.try_into().unwrap_or([0; 8]));
                let hash: Hash = hash.try_into().unwrap_or([0; 16]);
                for (dst, src) in row.iter_mut().zip(values.as_chunks::<2>().0) {
                    *dst = f16::from_le_bytes(*src);
                }
                self.file_records += 1;
                if row.iter().any(|v| !v.is_finite()) {
                    // a damaged vector is a dead record (the photo is embedded again)
                    self.file_dead += 1;
                    continue;
                }
                match self.rows.get(&photo) {
                    Some(&at) => {
                        // a later record supersedes the earlier one
                        if let Some(slot) = self.data.get_mut(at * self.dim..(at + 1) * self.dim) {
                            slot.copy_from_slice(&row);
                        }
                        if let Some(h) = self.hashes.get_mut(at) {
                            *h = hash;
                        }
                        self.file_dead += 1;
                    }
                    None => {
                        self.rows.insert(photo, self.photos.len());
                        self.photos.push(photo);
                        self.hashes.push(hash);
                        self.data.extend_from_slice(&row);
                        self.on_disk.push(true);
                    }
                }
            }
            read += n;
        }
        self.file_len = head_len + (self.file_records * rec) as u64;
        self.file_matches = true;
        if len > self.file_len {
            self.note = Some("the index file ended in a torn record (it is ignored)".into());
        }
    }

    fn record(&self, r: usize, out: &mut Vec<u8>) {
        let (Some(photo), Some(hash), Some(row)) = (self.photos.get(r), self.hashes.get(r), self.data.get(r * self.dim..(r + 1) * self.dim)) else {
            return;
        };
        out.extend_from_slice(&photo.to_le_bytes());
        out.extend_from_slice(hash);
        for v in row {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }

    /// Whether the file has so many dead records it should be rewritten.
    fn wants_compaction(&self) -> bool {
        self.file_dead >= COMPACT_MIN && self.file_dead * COMPACT_SHARE > self.file_records
    }

    /// Write what changed since the last save: appended when the file is this model's and not too full of dead
    /// records, else the whole index is written afresh (atomically). On failure the records stay pending and the file
    /// is left as it was (an append that stopped half way is cut back off).
    pub fn save(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else { return Ok(()) };
        let r = if !self.file_matches || self.wants_compaction() { self.rewrite(&path) } else { self.append(&path) };
        self.save_error = r.as_ref().err().map(|e| format!("could not save the content index ({e}): is the disk full or the folder read-only?"));
        r
    }

    fn append(&mut self, path: &Path) -> std::io::Result<()> {
        let pending: Vec<usize> = (0..self.photos.len()).filter(|r| !self.on_disk.get(*r).copied().unwrap_or(true)).collect();
        if pending.is_empty() {
            return Ok(());
        }
        let mut buf = Vec::with_capacity(pending.len() * record_len(self.dim));
        for r in &pending {
            self.record(*r, &mut buf);
        }
        let mut f = match std::fs::OpenOptions::new().write(true).open(path) {
            Ok(f) => f,
            Err(e) => {
                // the file went away (deleted by hand or by a sync tool) or can't be opened: the next save writes it afresh
                self.file_matches = false;
                return Err(e);
            }
        };
        // a torn record (or anything else) after the last whole one is cut off first, so records stay aligned
        if f.metadata()?.len() != self.file_len {
            f.set_len(self.file_len)?;
        }
        use std::io::Seek;
        f.seek(std::io::SeekFrom::Start(self.file_len))?;
        if let Err(e) = f.write_all(&buf).and_then(|()| f.flush()) {
            // half written: cut it back; if even that fails, the next save writes the whole file afresh
            if f.set_len(self.file_len).is_err() {
                self.file_matches = false;
            }
            return Err(e);
        }
        self.file_len += buf.len() as u64;
        self.file_records += pending.len();
        for r in pending {
            if let Some(d) = self.on_disk.get_mut(r) {
                *d = true;
            }
        }
        Ok(())
    }

    fn rewrite(&mut self, path: &Path) -> std::io::Result<()> {
        let mut buf = header_of(&self.tag, self.dim);
        buf.reserve(self.photos.len() * record_len(self.dim));
        for r in 0..self.photos.len() {
            self.record(r, &mut buf);
        }
        let part = path.with_extension("bin.part");
        let written =
            std::fs::File::create(&part).and_then(|mut f| f.write_all(&buf).and_then(|()| f.sync_all())).and_then(|()| std::fs::rename(&part, path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&part);
            return Err(e);
        }
        self.file_matches = true;
        self.file_len = buf.len() as u64;
        self.file_records = self.photos.len();
        self.file_dead = 0;
        self.on_disk.iter_mut().for_each(|d| *d = true);
        self.note = None;
        Ok(())
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        if self.has_unsaved()
            && let Err(e) = self.save()
        {
            log::warn!("content index: {e}");
        }
    }
}

struct Header {
    tag: String,
    dim: usize,
}

fn read_header(r: &mut impl Read) -> Result<Header, &'static str> {
    let mut fixed = [0u8; 7];
    r.read_exact(&mut fixed).map_err(|_| "is damaged (too short)")?;
    if &fixed[..4] != MAGIC {
        return Err("is not a content index");
    }
    if fixed[4] != VERSION {
        return Err("is of another version");
    }
    let tag_len = usize::from(u16::from_le_bytes([fixed[5], fixed[6]]));
    if tag_len == 0 || tag_len > MAX_TAG {
        return Err("is damaged (its model name)");
    }
    let mut tag = vec![0u8; tag_len];
    r.read_exact(&mut tag).map_err(|_| "is damaged (too short)")?;
    let mut rest = [0u8; 5];
    r.read_exact(&mut rest).map_err(|_| "is damaged (too short)")?;
    let dim = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
    if dim == 0 || dim > MAX_DIM {
        return Err("is damaged (its vector size)");
    }
    if rest[4] != ELEM_F16 {
        return Err("stores numbers of another kind");
    }
    let tag = String::from_utf8(tag).map_err(|_| "is damaged (its model name)")?;
    Ok(Header { tag, dim })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lc-contentindex-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join(format!("{name}.bin"));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn unit(dim: usize, hot: usize) -> Vec<f32> {
        (0..dim).map(|i| if i == hot % dim { 1.0 } else { 0.0 }).collect()
    }

    fn h(n: u8) -> Hash {
        [n; 16]
    }

    #[test]
    fn embeddings_survive_a_save_a_reload_and_an_append() {
        let path = temp("roundtrip");
        let mut a = Index::open("m@abc", 4, Some(path.clone()));
        a.insert(7, h(1), &unit(4, 0)).unwrap();
        a.insert(8, h(2), &[0.5, 0.5, 0.5, -0.5]).unwrap();
        a.save().unwrap();
        a.insert(9, h(3), &unit(4, 2)).unwrap();
        a.save().unwrap(); // appended
        assert_eq!(a.file_records(), (3, 0));
        drop(a);
        let b = Index::open("m@abc", 4, Some(path.clone()));
        assert_eq!(b.len(), 3);
        assert_eq!(b.vector(8), Some(vec![0.5, 0.5, 0.5, -0.5]));
        assert_eq!((b.hash(9), b.hash(1)), (Some(h(3)), None));
        assert!(b.note.is_none() && !b.has_unsaved());
        // the size on disk is the header plus one record per photo
        let expected = header_of("m@abc", 4).len() + 3 * record_len(4);
        assert_eq!(std::fs::metadata(&path).unwrap().len() as usize, expected);
    }

    #[test]
    fn an_index_file_deleted_while_open_is_written_afresh_by_the_next_save() {
        let path = temp("deleted");
        let mut a = Index::open("m@abc", 2, Some(path.clone()));
        a.insert(1, h(1), &unit(2, 0)).unwrap();
        a.save().unwrap();
        std::fs::remove_file(&path).unwrap();
        a.insert(2, h(2), &unit(2, 1)).unwrap();
        // the append finds no file: that save fails, the next one writes the whole file
        assert!(a.save().is_err() && a.has_unsaved());
        a.save().unwrap();
        assert!(a.save_error.is_none() && !a.has_unsaved());
        drop(a);
        let b = Index::open("m@abc", 2, Some(path.clone()));
        assert_eq!((b.len(), b.hash(1), b.hash(2)), (2, Some(h(1)), Some(h(2))));
    }

    #[test]
    fn a_new_hash_supersedes_and_deletions_are_compacted_away() {
        let path = temp("compact");
        let mut a = Index::open("m@abc", 2, Some(path.clone()));
        for p in 0..40u64 {
            a.insert(p, h(1), &unit(2, p as usize)).unwrap();
        }
        a.save().unwrap();
        // a replaced original: embedded again, the old record is dead
        a.insert(3, h(9), &unit(2, 1)).unwrap();
        a.save().unwrap();
        assert_eq!(a.file_records(), (41, 1));
        drop(a);
        let mut b = Index::open("m@abc", 2, Some(path.clone()));
        assert_eq!((b.len(), b.hash(3)), (40, Some(h(9))), "the later record wins");
        assert_eq!(b.file_records(), (41, 1));
        // twelve photos deleted: dead records, not yet a fifth
        for p in 10..22 {
            assert!(b.remove(p));
        }
        assert!(!b.remove(10));
        assert_eq!(b.len(), 28);
        assert_eq!(b.vector(39), Some(unit(2, 39)), "rows that moved keep their vector");
        b.insert(100, h(5), &unit(2, 0)).unwrap();
        b.save().unwrap();
        assert_eq!(b.file_records(), (42, 13), "appended");
        // past a fifth dead: the next save rewrites the file without them
        for p in 22..30 {
            b.remove(p);
        }
        b.insert(101, h(5), &unit(2, 0)).unwrap();
        b.save().unwrap();
        assert_eq!(b.file_records(), (22, 0));
        assert!(!path.with_extension("bin.part").exists());
        drop(b);
        let c = Index::open("m@abc", 2, Some(path.clone()));
        assert_eq!(c.len(), 22);
        assert!(c.hash(15).is_none() && c.hash(101).is_some() && c.hash(3) == Some(h(9)));
    }

    #[test]
    fn another_model_a_damaged_file_and_a_torn_tail_are_handled() {
        let path = temp("other");
        let mut a = Index::open("m@abc", 3, Some(path.clone()));
        a.insert(1, h(1), &unit(3, 0)).unwrap();
        a.insert(2, h(2), &unit(3, 1)).unwrap();
        a.save().unwrap();
        drop(a);
        // another model's (or another version's) index: nothing loaded, rebuilt
        let other = Index::open("m@def", 3, Some(path.clone()));
        assert_eq!(other.len(), 0);
        assert!(other.note.as_deref().unwrap().contains("another version"));
        assert!(Index::open("m@abc", 4, Some(path.clone())).len() == 0);
        // a torn last record loses only that record, and the next append is cut back to stay aligned
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        let mut torn = Index::open("m@abc", 3, Some(path.clone()));
        assert_eq!(torn.len(), 1);
        assert!(torn.note.as_deref().unwrap().contains("torn"));
        torn.insert(5, h(5), &unit(3, 2)).unwrap();
        torn.save().unwrap();
        drop(torn);
        let again = Index::open("m@abc", 3, Some(path.clone()));
        assert_eq!(again.len(), 2, "the record appended after the torn one reads back");
        assert_eq!(again.vector(5), Some(unit(3, 2)));
        assert!(again.note.is_none());
    }

    #[test]
    fn hostile_files_are_rebuilt_never_trusted() {
        let path = temp("hostile");
        let good = header_of("m@abc", 3);
        let mut huge_dim = good.clone();
        let at = good.len() - 5;
        huge_dim[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut zero_dim = good.clone();
        zero_dim[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
        let mut wrong_elem = good.clone();
        *wrong_elem.last_mut().unwrap() = 7;
        let mut long_tag = b"LCCE\x01".to_vec();
        long_tag.extend_from_slice(&u16::MAX.to_le_bytes());
        long_tag.extend(vec![b'a'; 100]);
        let mut bad_utf8 = b"LCCE\x01\x02\x00\xff\xfe".to_vec();
        bad_utf8.extend_from_slice(&3u32.to_le_bytes());
        bad_utf8.push(1);
        for junk in [Vec::new(), b"LCCE".to_vec(), vec![0xff; 300], b"LCCE\x09".to_vec(), huge_dim, zero_dim, wrong_elem, long_tag, bad_utf8] {
            std::fs::write(&path, &junk).unwrap();
            let mut j = Index::open("m@abc", 3, Some(path.clone()));
            assert_eq!(j.len(), 0, "{junk:?}");
            // and the next save replaces it with a good file
            j.insert(1, h(1), &unit(3, 0)).unwrap();
            j.save().unwrap();
            drop(j);
            assert_eq!(Index::open("m@abc", 3, Some(path.clone())).len(), 1);
        }
        // vectors that are not numbers are dead records, not embeddings
        let mut file = good.clone();
        file.extend_from_slice(&1u64.to_le_bytes());
        file.extend_from_slice(&h(1));
        for v in [f16::NAN, f16::INFINITY, f16::ONE] {
            file.extend_from_slice(&v.to_le_bytes());
        }
        std::fs::write(&path, &file).unwrap();
        let nan = Index::open("m@abc", 3, Some(path.clone()));
        assert_eq!((nan.len(), nan.file_records()), (0, (1, 1)));
        // a missing file is an empty index, and a directory in its place cannot be read nor written
        let _ = std::fs::remove_file(&path);
        assert_eq!(Index::open("m@abc", 3, Some(path.clone())).len(), 0);
        let dir = path.with_extension("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let mut d = Index::open("m@abc", 3, Some(dir.clone()));
        d.insert(1, h(1), &unit(3, 0)).unwrap();
        assert!(d.save().is_err());
        assert!(d.save_error.as_deref().unwrap().contains("disk full"));
        assert!(d.has_unsaved());
    }

    #[test]
    fn a_file_claiming_more_records_than_memory_allows_is_not_loaded() {
        let path = temp("absurd");
        // a sparse file of 4096-wide records whose size claims more than the cap: nothing is read or allocated
        let mut head = header_of("m@abc", MAX_DIM);
        head.resize(head.len() + record_len(MAX_DIM), 0);
        std::fs::write(&path, &head).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len((MAX_MATRIX_BYTES as u64) * 2).unwrap();
        drop(f);
        let idx = Index::open("m@abc", MAX_DIM, Some(path.clone()));
        assert_eq!(idx.len(), 0);
        assert!(idx.note.as_deref().unwrap().contains("more than"), "{:?}", idx.note);
        drop(idx);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_index_refuses_bad_vectors_and_grows_no_further_than_its_cap() {
        let mut i = Index::new("m@abc", 3);
        assert!(i.insert(1, h(1), &[1.0, 0.0]).is_err());
        assert!(i.insert(1, h(1), &[f32::NAN, 0.0, 0.0]).is_err());
        assert!(i.insert(1, h(1), &[f32::INFINITY, 0.0, 0.0]).is_err());
        assert_eq!(i.len(), 0);
        // a model's dimension is clamped to what the format holds
        assert_eq!(Index::new("m@abc", 1 << 20).dim(), MAX_DIM);
        assert_eq!(Index::new("m@abc", 0).dim(), 1);
        // the cap: a 4096-wide index refuses past MAX_MATRIX_BYTES (checked arithmetically, not by filling 3 GiB)
        const { assert!((MAX_MATRIX_BYTES / (MAX_DIM * 2)) < MAX_RECORDS) };
        // in memory only: saving is a no-op and nothing is pending
        i.insert(1, h(1), &[1.0, 0.0, 0.0]).unwrap();
        assert!(i.save().is_ok() && !i.has_unsaved());
    }

    #[test]
    fn cosines_rank_the_closest_photo_first() {
        let mut i = Index::new("m@abc", 3);
        let n = |v: [f32; 3]| {
            let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.map(|x| x / l)
        };
        i.insert(1, h(1), &n([1.0, 0.0, 0.0])).unwrap();
        i.insert(2, h(1), &n([0.6, 0.8, 0.0])).unwrap();
        i.insert(3, h(1), &n([0.0, 0.0, 1.0])).unwrap();
        // more rows than one chunk, so the parallel pass is exercised
        for p in 10..700 {
            i.insert(p, h(1), &n([0.0, 1.0, 1.0])).unwrap();
        }
        let mut c = i.cosines(&n([0.7, 0.7, 0.0]));
        c.sort_by(|a, b| b.1.total_cmp(&a.1));
        assert_eq!(c.len(), 693);
        assert_eq!(c[0].0, 2);
        assert_eq!(c[1].0, 1);
        assert!(i.cosines(&[1.0]).is_empty(), "a query of another size matches nothing");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_full_disk_on_append_keeps_the_records_pending_and_the_file_aligned() {
        // /dev/full accepts the open and fails every write with "no space left on device"
        let mut i = Index::new("m@abc", 2);
        i.path = Some(PathBuf::from("/dev/full"));
        i.file_matches = true;
        i.insert(1, h(1), &[1.0, 0.0]).unwrap();
        let e = i.save().unwrap_err();
        assert!(i.save_error.is_some(), "{e}");
        assert!(i.has_unsaved(), "nothing is lost");
        // the cut-back failed too: the next save writes the whole file (and fails again, cleanly)
        assert!(i.save().is_err());
        assert_eq!(i.len(), 1);
    }
}
