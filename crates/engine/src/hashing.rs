//! Content hashes worked out after a photo joined the library.
//!
//! Browsing a folder (Local) describes raws from the parts of them their headers are in and
//! leaves their content hash unset (see `files::light_probe`): it needs every byte, and a folder on
//! a network share holds thousands of raws. A Local photo is found again by its path. Once one
//! joins the library (Add to My Photos, or an import of its file), it is queued here
//! ([`Session::queue_hash`]; an undo or redo that brings it back queues it again) and its hash is worked out: on a worker thread in the app
//! ([`Session::take_hash_jobs`], [`HashJob::run`], [`Session::learn_content_hash`]), or at once
//! where nothing runs in the background (CLI, MCP: [`Session::hash_queued_now`]). Duplicates of it
//! are then recognised on import again.
//!
//! The hash is also what caches are keyed by ([`crate::media::content_key`]: before it, the file's
//! path and size). Caches simply miss once; a smart preview made before it is moved to its new
//! name, so it is not built again or left behind. The move waits until the session records the
//! hash ([`Session::learn_content_hash`]): a result that no longer applies (Add to My Photos undone
//! meanwhile) leaves the preview where the photo's current key finds it.

use std::collections::HashSet;
use std::path::PathBuf;

use lightcraft_catalog::{Op, Photo, PhotoId, Source};

use crate::Session;

/// One photo's hash to work out, detached from the session (the file can be on a slow drive).
#[derive(Clone, Debug)]
pub struct HashJob {
    pub id: PhotoId,
    /// The photo as it was queued: its file, its size, and the cache key its smart preview has.
    photo: Photo,
    smart_dir: Option<PathBuf>,
}

/// A worked-out hash, for [`Session::learn_content_hash`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HashDone {
    pub id: PhotoId,
    path: String,
    file_size: u64,
    hash: String,
    /// A smart preview under the queued key (from, to): moved once the hash is recorded.
    smart_move: Option<(PathBuf, PathBuf)>,
}

/// The content hash of the file at `path`, as an import works it out; `None` when it can't be read.
/// Read in pieces: the file is whatever is on the disk, of any size.
pub fn file_hash(path: &str) -> Option<String> {
    hash_reader(std::fs::File::open(path).ok()?)
}

/// The content hash of what `r` gives, read in pieces (the hash import computes of the whole).
fn hash_reader(mut r: impl std::io::Read) -> Option<String> {
    let mut hasher = lightcraft_preview::Hasher128::new();
    let mut piece = vec![0u8; 1 << 16];
    loop {
        match r.read(&mut piece) {
            Ok(0) => return Some(hasher.finish().to_string()),
            Ok(n) => {
                hasher.update(piece.get(..n)?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
}

/// What tells a file's versions apart without reading it: its size and modification time (`None`
/// where the file system keeps none).
fn revision(path: &str) -> Option<(u64, Option<std::time::SystemTime>)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

impl HashJob {
    /// Read and hash the file, and find a smart preview made under the old key (it moves to the new
    /// one when the session records the hash). `None` when the file can't be read, isn't the one
    /// queued any more (another size), or changed while it was read (the hash would be of neither
    /// version): the job is then tried again later.
    pub fn run(&self) -> Option<HashDone> {
        self.run_reading(|path| std::fs::read(path).ok())
    }

    /// [`Self::run`] with the file read by `read` (tests change the file while it is read).
    pub(crate) fn run_reading(&self, read: impl FnOnce(&str) -> Option<Vec<u8>>) -> Option<HashDone> {
        let Source::File { path } = &self.photo.source else { return None };
        let before = revision(path)?;
        let len = before.0;
        if len != self.photo.file_size {
            return None;
        }
        let _permit = crate::memory::work_gate().acquire(usize::try_from(len).unwrap_or(usize::MAX));
        let bytes = read(path)?;
        if revision(path)? != before {
            return None;
        }
        let hash = lightcraft_preview::hash_bytes(&bytes).to_string();
        drop(bytes);
        // Only looked up here, off the UI thread: the photo keeps its old key until the hash is recorded.
        let smart_move = self.smart_dir.as_ref().and_then(|dir| {
            let old = crate::smart::file_path(dir, &self.photo);
            let new = crate::smart::sharded_file_path(dir, &Photo { content_hash: Some(hash.clone()), ..self.photo.clone() });
            (old != new && old.is_file() && !new.exists()).then_some((old, new))
        });
        Some(HashDone { id: self.id, path: path.clone(), file_size: len, hash, smart_move })
    }
}

/// Move a smart preview to the name the photo's recorded hash gives it (one rename in the cache
/// folder); a preview gone or replaced meanwhile is left alone.
fn move_smart_preview(old: &std::path::Path, new: &std::path::Path) {
    if new.exists() {
        return;
    }
    let moved = new.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::rename(old, new));
    match moved {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            log::warn!("smart preview {}: not renamed after its content hash: {e}", old.display());
        }
        _ => {}
    }
}

/// Photos `op` brings into the library (`SetLocal` to false, in batches too).
pub(crate) fn joined_library(op: &Op) -> Vec<PhotoId> {
    let mut ids = Vec::new();
    let mut pending = vec![op];
    while let Some(op) = pending.pop() {
        match op {
            Op::SetLocal { id, local: false } => ids.push(*id),
            Op::Batch { ops } => pending.extend(ops),
            _ => {}
        }
    }
    ids
}

/// Whether photo `p` is a library photo whose hash is still to be worked out from its own file.
fn wants_hash(p: &Photo) -> bool {
    !p.local && !p.deleted && p.copy_of.is_none() && p.content_hash.is_none() && matches!(p.source, Source::File { .. })
}

impl Session {
    /// Queue photos that joined the library without a content hash (see the module docs).
    pub fn queue_hash(&mut self, ids: impl IntoIterator<Item = PhotoId>) {
        let mut queued: HashSet<PhotoId> = self.unhashed.iter().copied().collect();
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| wants_hash(p)) && queued.insert(id) {
                self.unhashed.push(id);
            }
        }
    }

    /// Whether hashes are queued ([`Self::take_hash_jobs`] has work).
    pub fn hashes_queued(&self) -> bool {
        !self.unhashed.is_empty()
    }

    /// The queued photos' jobs (the queue is emptied), for a worker thread.
    pub fn take_hash_jobs(&mut self) -> Vec<HashJob> {
        let ids = std::mem::take(&mut self.unhashed);
        self.hash_jobs(ids)
    }

    fn hash_jobs(&self, ids: Vec<PhotoId>) -> Vec<HashJob> {
        let smart_dir = &self.media.smart_dir;
        ids.into_iter()
            .filter_map(|id| {
                self.catalog.photo(id).filter(|p| wants_hash(p)).map(|p| HashJob { id, photo: Photo::clone(p), smart_dir: smart_dir.clone() })
            })
            .collect()
    }

    /// Photos that just joined the library (Add to My Photos, an import, an undo or redo of
    /// either): queued, and where nothing runs in the background, their hashes are worked out now.
    /// Only theirs: hashes queued earlier wait for the next drain, so a browse that promotes nothing
    /// reads no whole files.
    pub(crate) fn hash_joined(&mut self, ids: impl IntoIterator<Item = PhotoId>) {
        let ids: Vec<PhotoId> = ids.into_iter().collect();
        let joined: HashSet<PhotoId> = ids.iter().copied().collect();
        self.queue_hash(ids);
        if self.hash_in_background {
            return;
        }
        // (both keep the queue's order)
        let (mine, rest) = std::mem::take(&mut self.unhashed).into_iter().partition(|id| joined.contains(id));
        self.unhashed = rest;
        let jobs = self.hash_jobs(mine);
        self.run_hash_jobs(jobs);
    }

    /// Record a worked-out hash when the photo is still the one it was worked out for (same file, same
    /// size, no hash yet): journaled, not an undo step. A smart preview made under the old key moves
    /// to the new one only then. Whether it was recorded.
    pub fn learn_content_hash(&mut self, done: HashDone) -> bool {
        let Some(p) = self.catalog.photo(done.id) else { return false };
        let same = matches!(&p.source, Source::File { path } if *path == done.path) && p.file_size == done.file_size;
        if !same || !wants_hash(p) {
            return false;
        }
        let op = Op::SetContent {
            id: done.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: Some(done.hash),
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        };
        match self.apply_system(op) {
            Ok(()) => {
                if let Some((old, new)) = &done.smart_move {
                    move_smart_preview(old, new);
                }
                true
            }
            Err(e) => {
                log::warn!("photo {}: content hash not recorded: {e}", done.id.0);
                false
            }
        }
    }

    /// Work out the queued hashes now (where nothing runs in the background). How many were recorded.
    pub fn hash_queued_now(&mut self) -> usize {
        let jobs = self.take_hash_jobs();
        self.run_hash_jobs(jobs)
    }

    /// Run `jobs` here; one that fails (the file can't be read now) stays queued for a later try,
    /// as long as its photo still wants a hash. How many were recorded.
    fn run_hash_jobs(&mut self, jobs: Vec<HashJob>) -> usize {
        let mut recorded = 0;
        let mut failed = Vec::new();
        for job in jobs {
            if job.run().is_some_and(|d| self.learn_content_hash(d)) {
                recorded += 1;
            } else {
                failed.push(job.id);
            }
        }
        self.queue_hash(failed);
        recorded
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that gives its bytes a few at a time, and an interruption in between.
    struct Trickle<'a>(&'a [u8], bool);

    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.1 = !self.1;
            if self.1 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            let n = self.0.len().min(buf.len()).min(7);
            buf[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }

    #[test]
    fn a_hash_read_in_pieces_is_the_hash_of_the_whole() {
        // longer than one piece, so the read loop runs more than once
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let whole = lightcraft_preview::hash_bytes(&bytes).to_string();
        assert_eq!(hash_reader(bytes.as_slice()).as_deref(), Some(whole.as_str()));
        assert_eq!(hash_reader(Trickle(&bytes[..1000], false)).as_deref(), Some(lightcraft_preview::hash_bytes(&bytes[..1000]).to_string().as_str()));
        assert_eq!(hash_reader(std::io::empty()).as_deref(), Some(lightcraft_preview::hash_bytes(&[]).to_string().as_str()));
    }

    /// A scratch folder that goes away with the test, however it ends.
    struct Scratch(std::path::PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_hash_is_the_import_hash_and_a_missing_file_has_none() {
        let dir = Scratch(std::env::temp_dir().join(format!("lc-file-hash-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir.0);
        std::fs::create_dir_all(&dir.0).unwrap();
        let f = dir.0.join("a.bin");
        std::fs::write(&f, b"pixels").unwrap();
        assert_eq!(file_hash(f.to_str().unwrap()), Some(lightcraft_preview::hash_bytes(b"pixels").to_string()));
        assert_eq!(file_hash(dir.0.join("nope").to_str().unwrap()), None);
    }
}
