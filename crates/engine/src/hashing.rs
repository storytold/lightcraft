//! Content hashes worked out after a photo joined the library.
//!
//! Browsing a folder (Local) describes raws from the parts of them their headers are in and
//! leaves their content hash unset (see `files::light_probe`): it needs every byte, and a folder on
//! a network share holds thousands of raws. A Local photo is found again by its path. Once one
//! joins the library (Add to My Photos, or an import of its file), it is queued here
//! ([`Session::queue_hash`]) and its hash is worked out: on a worker thread in the app
//! ([`Session::take_hash_jobs`], [`HashJob::run`], [`Session::learn_content_hash`]), or at once
//! where nothing runs in the background (CLI, MCP: [`Session::hash_queued_now`]). Duplicates of it
//! are then recognised on import again.
//!
//! The hash is also what caches are keyed by ([`crate::media::content_key`]: before it, the file's
//! path and size). Caches simply miss once; a smart preview made before it is moved to its new
//! name, so it is not built again or left behind.

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
}

/// The content hash of the file at `path`, as an import works it out; `None` when it can't be read.
pub fn file_hash(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| lightcraft_preview::hash_bytes(&b).to_string())
}

impl HashJob {
    /// Read and hash the file, and move a smart preview made under the old key to the new one.
    /// `None` when the file can't be read or isn't the one queued any more (another size).
    pub fn run(&self) -> Option<HashDone> {
        let Source::File { path } = &self.photo.source else { return None };
        let len = std::fs::metadata(path).ok()?.len();
        if len != self.photo.file_size {
            return None;
        }
        let _permit = crate::memory::work_gate().acquire(usize::try_from(len).unwrap_or(usize::MAX));
        let hash = file_hash(path)?;
        if let Some(dir) = &self.smart_dir {
            let old = crate::smart::file_path(dir, &self.photo);
            let new = crate::smart::sharded_file_path(dir, &Photo { content_hash: Some(hash.clone()), ..self.photo.clone() });
            if old != new && old.is_file() && !new.exists() {
                let moved = new.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::rename(&old, &new));
                if let Err(e) = moved {
                    log::warn!("smart preview of {path}: not renamed after its content hash: {e}");
                }
            }
        }
        Some(HashDone { id: self.id, path: path.clone(), file_size: len, hash })
    }
}

/// Whether photo `p` is a library photo whose hash is still to be worked out from its own file.
fn wants_hash(p: &Photo) -> bool {
    !p.local && !p.deleted && p.copy_of.is_none() && p.content_hash.is_none() && matches!(p.source, Source::File { .. })
}

impl Session {
    /// Queue photos that joined the library without a content hash (see the module docs).
    pub fn queue_hash(&mut self, ids: impl IntoIterator<Item = PhotoId>) {
        for id in ids {
            if self.catalog.photo(id).is_some_and(|p| wants_hash(p)) && !self.unhashed.contains(&id) {
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
        let smart_dir = self.media.smart_dir.clone();
        std::mem::take(&mut self.unhashed)
            .into_iter()
            .filter_map(|id| {
                self.catalog.photo(id).filter(|p| wants_hash(p)).map(|p| HashJob { id, photo: Photo::clone(p), smart_dir: smart_dir.clone() })
            })
            .collect()
    }

    /// Record a worked-out hash when the photo is still the one it was worked out for (same file, same
    /// size, no hash yet): journaled, not an undo step. Whether it was recorded.
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
            Ok(()) => true,
            Err(e) => {
                log::warn!("photo {}: content hash not recorded: {e}", done.id.0);
                false
            }
        }
    }

    /// Work out the queued hashes now (where nothing runs in the background). How many were recorded.
    pub fn hash_queued_now(&mut self) -> usize {
        let jobs = self.take_hash_jobs();
        jobs.iter().filter_map(HashJob::run).map(|d| self.learn_content_hash(d)).filter(|&ok| ok).count()
    }
}
