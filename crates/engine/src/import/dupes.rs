use std::collections::{HashMap, HashSet};

use lightcraft_catalog::{Photo, PhotoId, Source};

use crate::media::ProbeInfo;
use crate::sync::base_hash;

const MAX_SAME_KEY: usize = 256;

type Key = (u64, u32, u32);

fn key(size: u64, width: u32, height: u32) -> Option<Key> {
    (size > 0).then_some((size, width, height))
}

#[derive(Clone, Debug)]
struct Known {
    existing: Option<PhotoId>,
    path: String,
    hash: Option<String>,
    head: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Duplicates {
    by_hash: HashMap<String, Option<PhotoId>>,
    by_key: HashMap<Key, Vec<Known>>,
    heads: HashMap<String, Option<String>>,
    wholes: HashMap<String, Option<String>>,
    filled: Vec<(PhotoId, String)>,
    learned: HashMap<String, String>,
}

impl Duplicates {
    pub(crate) fn add_library(&mut self, p: &Photo, authorized: bool) {
        let hash = p.content_hash.as_ref().filter(|_| authorized);
        if let Some(h) = hash {
            self.by_hash.insert(base_hash(h).to_string(), Some(p.id));
        }
        let Source::File { path } = &p.source else { return };
        if p.copy_of.is_some() {
            return;
        }
        if let Some(k) = key(p.file_size, p.width, p.height) {
            self.by_key.entry(k).or_default().push(Known { existing: Some(p.id), path: path.clone(), hash: hash.cloned(), head: None });
        }
    }

    pub(crate) fn add_library_matching<'a>(
        &mut self,
        library: impl Iterator<Item = &'a std::sync::Arc<Photo>>,
        probes: &[&ProbeInfo],
        authorized: impl Fn(&Photo) -> bool,
    ) {
        let hashes: HashSet<&str> = probes.iter().filter_map(|i| i.content_hash.as_deref()).map(base_hash).collect();
        let keys: HashSet<Key> = probes.iter().filter_map(|i| key(i.file_size, i.width, i.height)).collect();
        if hashes.is_empty() && keys.is_empty() {
            return;
        }
        for p in library {
            let by_hash = p.content_hash.as_deref().is_some_and(|h| hashes.contains(base_hash(h)));
            if by_hash || key(p.file_size, p.width, p.height).is_some_and(|k| keys.contains(&k)) {
                self.add_library(p, authorized(p));
            }
        }
    }

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

    pub(crate) fn find(&mut self, path: &str, info: &mut ProbeInfo) -> Option<Option<PhotoId>> {
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
            if twin.path == path || (probed_hash && twin.hash.is_some()) {
                continue;
            }
            let Some(head) = info.head_hash.clone().or_else(|| self.head(path)) else { continue };
            if twin.head.clone().or_else(|| self.head(&twin.path)) != Some(head) {
                continue;
            }
            let Some(whole) = info.content_hash.clone().or_else(|| self.whole(path)) else { continue };
            info.content_hash.get_or_insert_with(|| whole.clone());
            if let Some(existing) = self.by_hash.get(base_hash(&whole)) {
                return Some(*existing);
            }
            let Some(theirs) = twin.hash.clone().or_else(|| self.whole(&twin.path)) else { continue };
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

    #[cfg(test)]
    pub(crate) fn knows_hash(&self, hash: &str) -> bool {
        self.by_hash.contains_key(base_hash(hash))
    }

    pub(crate) fn take_filled(&mut self) -> Vec<(PhotoId, String)> {
        std::mem::take(&mut self.filled)
    }

    pub(crate) fn learned(&self, path: &str) -> Option<&String> {
        self.learned.get(path)
    }

    pub(crate) fn take_learned(&mut self) -> HashMap<String, String> {
        std::mem::take(&mut self.learned)
    }

    fn learn(&mut self, twin: &Known, hash: &str) {
        self.by_hash.insert(base_hash(hash).to_string(), twin.existing);
        match twin.existing {
            Some(id) => self.filled.push((id, hash.to_string())),
            None => {
                self.learned.insert(twin.path.clone(), hash.to_string());
            }
        }
    }

    fn head(&mut self, path: &str) -> Option<String> {
        self.heads.entry(path.to_string()).or_insert_with(|| read_head(path)).clone()
    }

    fn whole(&mut self, path: &str) -> Option<String> {
        self.wholes.entry(path.to_string()).or_insert_with(|| read_whole(path)).clone()
    }
}

pub(crate) fn could_repeat(a: &ProbeInfo, b: &ProbeInfo) -> bool {
    if let (Some(x), Some(y)) = (&a.content_hash, &b.content_hash) {
        return base_hash(x) == base_hash(y);
    }
    let same_key = key(a.file_size, a.width, a.height).is_some_and(|k| key(b.file_size, b.width, b.height) == Some(k));
    same_key && (a.head_hash.is_none() || b.head_hash.is_none() || a.head_hash == b.head_hash)
}

#[cfg(not(target_arch = "wasm32"))]
fn read_head(path: &str) -> Option<String> {
    crate::files::file_head_hash(path).map_err(|e| log::warn!("import: {path}: {e}; not compared as a duplicate")).ok()
}

#[cfg(target_arch = "wasm32")]
fn read_head(_path: &str) -> Option<String> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn read_whole(path: &str) -> Option<String> {
    let hash = super::hash_file(path);
    if hash.is_none() {
        log::warn!("import: {path} could not be read; not compared as a duplicate");
    }
    hash
}

#[cfg(target_arch = "wasm32")]
fn read_whole(_path: &str) -> Option<String> {
    None
}
