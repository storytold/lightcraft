use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use lightcraft_catalog::{Photo, PhotoId, Source};

use crate::media::ProbeInfo;
use crate::sync::base_hash;

/// Files of one size and dimensions compared with a new file at most; beyond it the file is not compared.
const MAX_SAME_KEY: usize = 256;

/// File size, width and height.
type Key = (u64, u32, u32);

/// A file's [`Key`]; `None` for an empty or unknown size.
fn key(size: u64, width: u32, height: u32) -> Option<Key> {
    (size > 0).then_some((size, width, height))
}

/// A file of a [`Key`]: a library photo's (`existing`) or one this import added, with what is
/// known of its content so far.
#[derive(Clone, Debug)]
struct Known {
    existing: Option<PhotoId>,
    path: String,
    hash: Option<String>,
    head: Option<String>,
}

/// The contents an import or a scan knows, to find duplicates by: by content hash, and for files
/// without one, by size and dimensions, then a hash of their first 512 KiB, then a hash of all
/// their bytes, each read once.
#[derive(Debug, Default)]
pub(crate) struct Duplicates {
    /// Base content hash → the library photo that has it (`None`: a file this import added).
    by_hash: HashMap<String, Option<PhotoId>>,
    by_key: HashMap<Key, Vec<Known>>,
    /// Head hashes read so far, by path (`None`: unreadable).
    heads: HashMap<String, Option<String>>,
    /// Whole-file hashes read so far, by path (`None`: unreadable).
    wholes: HashMap<String, Option<String>>,
    /// Library photos hashed to compare them, with their hashes.
    filled: Vec<(PhotoId, String)>,
    /// Files this import added that were hashed to compare them: path → hash.
    learned: HashMap<String, String>,
}

impl Duplicates {
    /// Index library photo `p` by its content and, when it owns its file, by its size and
    /// dimensions. `physical` is the photo whose file `p` shows ([`crate::media::physical_photo`]):
    /// a virtual copy's own hash counts only while it is that photo's, and a copy left on its own
    /// file (its master relinked) is compared like any other photo.
    pub(crate) fn add_library(&mut self, p: &Photo, physical: Option<&Photo>, authorized: bool) {
        let owns_file = physical.is_some_and(|m| m.id == p.id);
        let same_content = |h: &String| owns_file || physical.and_then(|m| m.content_hash.as_deref()).is_some_and(|m| base_hash(m) == base_hash(h));
        let hash = p.content_hash.as_ref().filter(|_| authorized).filter(|h| same_content(h));
        if let Some(h) = hash {
            self.by_hash.insert(base_hash(h).to_string(), Some(p.id));
        }
        let Source::File { path } = &p.source else { return };
        if !owns_file {
            return;
        }
        if let Some(k) = key(p.file_size, p.width, p.height) {
            self.by_key.entry(k).or_default().push(Known { existing: Some(p.id), path: path.clone(), hash: hash.cloned(), head: None });
        }
    }

    /// [`Duplicates::add_library`] for the library photos that could match one of `probes` (by
    /// content hash or by size and dimensions). `physical` gives a photo's
    /// [`crate::media::physical_photo`] and whether its hash is trusted.
    pub(crate) fn add_library_matching<'a>(
        &mut self,
        library: impl Iterator<Item = &'a std::sync::Arc<Photo>>,
        probes: &[&ProbeInfo],
        physical: impl Fn(&Photo) -> Option<(std::sync::Arc<Photo>, bool)>,
    ) {
        let hashes: HashSet<&str> = probes.iter().filter_map(|i| i.content_hash.as_deref()).map(base_hash).collect();
        let keys: HashSet<Key> = probes.iter().filter_map(|i| key(i.file_size, i.width, i.height)).collect();
        if hashes.is_empty() && keys.is_empty() {
            return;
        }
        for p in library {
            let by_hash = p.content_hash.as_deref().is_some_and(|h| hashes.contains(base_hash(h)));
            if by_hash || key(p.file_size, p.width, p.height).is_some_and(|k| keys.contains(&k)) {
                let (owner, authorized) = physical(p).map_or((None, false), |(owner, authorized)| (Some(owner), authorized));
                self.add_library(p, owner.as_deref(), authorized);
            }
        }
    }

    /// Index a file this import adds (or a scan lists) at `path`, as probed.
    pub(crate) fn add_new(&mut self, path: &str, info: &ProbeInfo) {
        if let Some(h) = &info.content_hash {
            self.by_hash.insert(base_hash(h).to_string(), None);
        }
        if let Some(k) = key(info.file_size, info.width, info.height) {
            let entries = self.by_key.entry(k).or_default();
            if let Some(known) = entries.iter_mut().find(|known| known.path == path) {
                known.hash = info.content_hash.clone().or_else(|| known.hash.clone());
                known.head = info.head_hash.clone().or_else(|| known.head.clone());
            } else {
                entries.push(Known { existing: None, path: path.to_string(), hash: info.content_hash.clone(), head: info.head_hash.clone() });
            }
        }
    }

    /// The file at `path` repeats a known one: `Some(Some(photo))` a library photo,
    /// `Some(None)` a file added earlier in this import, `None` none. A file without a hash is
    /// compared with those of its size and dimensions by head, then whole: a whole hash read for
    /// it is put in `info`. Gives up (`None`) once `cancel` is set.
    pub(crate) fn find(&mut self, path: &str, info: &mut ProbeInfo, cancel: &AtomicBool) -> Option<Option<PhotoId>> {
        if let Some(existing) = info.content_hash.as_deref().and_then(|h| self.by_hash.get(base_hash(h))) {
            return Some(*existing);
        }
        let k = key(info.file_size, info.width, info.height)?;
        let twins = self.by_key.get(&k)?;
        if twins.len() > MAX_SAME_KEY {
            log::warn!("import {path}: {} photos share its size and dimensions; not compared with them", twins.len());
            return None;
        }
        let twins = twins.clone();
        let probed_hash = info.content_hash.is_some();
        for twin in twins {
            if cancel.load(Relaxed) {
                return None;
            }
            if twin.path == path || (probed_hash && twin.hash.is_some()) {
                continue;
            }
            let Some(head) = info.head_hash.clone().or_else(|| self.head(path)) else { continue };
            if twin.head.clone().or_else(|| self.head(&twin.path)) != Some(head) {
                continue;
            }
            let Some(whole) = info.content_hash.clone().or_else(|| self.whole(path, k.0, cancel)) else { continue };
            info.content_hash.get_or_insert_with(|| whole.clone());
            if let Some(existing) = self.by_hash.get(base_hash(&whole)) {
                return Some(*existing);
            }
            let Some(theirs) = twin.hash.clone().or_else(|| self.whole(&twin.path, k.0, cancel)) else { continue };
            if twin.hash.is_none() {
                self.learn(&twin, &theirs);
            }
            if base_hash(&theirs) == base_hash(&whole) {
                return Some(twin.existing);
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn hash_count(&self) -> usize {
        self.by_hash.len()
    }

    /// Whether a known file has this content.
    pub(crate) fn knows_hash(&self, hash: &str) -> bool {
        self.by_hash.contains_key(base_hash(hash))
    }

    /// The library photos hashed to compare them since the last call, with their hashes.
    pub(crate) fn take_filled(&mut self) -> Vec<(PhotoId, String)> {
        std::mem::take(&mut self.filled)
    }

    /// The hash read for a file this import added at `path`, when it was hashed to compare it.
    pub(crate) fn learned(&self, path: &str) -> Option<&String> {
        self.learned.get(path)
    }

    /// The hashes read for files this import added (path → hash) since the last call.
    pub(crate) fn take_learned(&mut self) -> HashMap<String, String> {
        std::mem::take(&mut self.learned)
    }

    /// Keep `hash`, read for `twin`, for the photo or the file it belongs to.
    fn learn(&mut self, twin: &Known, hash: &str) {
        self.by_hash.insert(base_hash(hash).to_string(), twin.existing);
        match twin.existing {
            Some(id) => self.filled.push((id, hash.to_string())),
            None => {
                self.learned.insert(twin.path.clone(), hash.to_string());
            }
        }
    }

    /// The hash of the first 512 KiB of `path`, read once.
    fn head(&mut self, path: &str) -> Option<String> {
        self.heads.entry(path.to_string()).or_insert_with(|| read_head(path)).clone()
    }

    /// The hash of all of `path` (`size` bytes), read once; not kept when `cancel` stopped it.
    fn whole(&mut self, path: &str, size: u64, cancel: &AtomicBool) -> Option<String> {
        if let Some(known) = self.wholes.get(path) {
            return known.clone();
        }
        let hash = read_whole(path, size, cancel);
        if !cancel.load(Relaxed) {
            self.wholes.insert(path.to_string(), hash.clone());
        }
        hash
    }
}

/// Whether `b` may have `a`'s content: the same hash, or, without both hashes, the same size,
/// dimensions and head where known.
pub(crate) fn could_repeat(a: &ProbeInfo, b: &ProbeInfo) -> bool {
    if let (Some(x), Some(y)) = (&a.content_hash, &b.content_hash) {
        return base_hash(x) == base_hash(y);
    }
    let same_key = key(a.file_size, a.width, a.height).is_some_and(|k| key(b.file_size, b.width, b.height) == Some(k));
    same_key && (a.head_hash.is_none() || b.head_hash.is_none() || a.head_hash == b.head_hash)
}

/// The head hash of `path` ([`crate::files::file_head_hash`]); `None`, logged, when it can't be read.
#[cfg(not(target_arch = "wasm32"))]
fn read_head(path: &str) -> Option<String> {
    crate::files::file_head_hash(path).map_err(|e| log::warn!("import: {path}: {e}; not compared as a duplicate")).ok()
}

#[cfg(target_arch = "wasm32")]
fn read_head(_path: &str) -> Option<String> {
    None
}

/// The content hash of `path` when it still has `size` bytes; `None`, logged, when it can't be read.
#[cfg(not(target_arch = "wasm32"))]
fn read_whole(path: &str, size: u64, cancel: &AtomicBool) -> Option<String> {
    let hash = super::hash_file(path, size, cancel);
    if hash.is_none() && !cancel.load(Relaxed) {
        log::warn!("import: {path} could not be read; not compared as a duplicate");
    }
    hash
}

#[cfg(target_arch = "wasm32")]
fn read_whole(_path: &str, _size: u64, _cancel: &AtomicBool) -> Option<String> {
    None
}
