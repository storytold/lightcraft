//! Disposable search cache, independent of catalog.snap/catalog.log and their format version.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::sync::Arc;

use lightcraft_catalog::{Catalog, Photo, PhotoId, Source, Store};
use lightcraft_meta::SearchMetadata;
use serde::{Deserialize, Serialize};

pub(crate) const FILE: &str = "metadata-search-cache.jsonl";
const HEADER: &[u8] = b"{\"version\":1}\n";
const MAX_BYTES: usize = 256 * 1024 * 1024;
const MAX_RECORDS: usize = 100_000;
const MAX_LINE: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    id: PhotoId,
    path: String,
    hash: Option<String>,
    index: Arc<SearchMetadata>,
}

impl Entry {
    fn matches(&self, photo: &Photo) -> bool {
        matches!(&photo.source, Source::File { path } if path == &self.path) && photo.content_hash == self.hash
    }
}

#[derive(Default)]
pub(crate) struct MetadataCache {
    entries: BTreeMap<PhotoId, Entry>,
    dirty: BTreeSet<PhotoId>,
    bytes: usize,
    rewrite: bool,
}

impl MetadataCache {
    pub(crate) fn load(store: &mut dyn Store, catalog: &mut Catalog) -> Self {
        let mut cache = Self::default();
        let bytes = match store.read_limited(FILE, MAX_BYTES) {
            Ok(Some(b)) => b,
            Ok(None) => return cache,
            Err(e) => {
                log::warn!("metadata search cache ignored (Reload from Disk rebuilds it): {e}");
                cache.rewrite = true;
                return cache;
            }
        };
        cache.bytes = bytes.len();
        let mut lines = bytes.split(|b| *b == b'\n');
        if lines.next() != Some(HEADER.strip_suffix(b"\n").unwrap_or(HEADER)) {
            cache.rewrite = true;
            return cache;
        }
        for line in lines.filter(|l| !l.is_empty()) {
            if line.len() > MAX_LINE {
                cache.rewrite = true;
                continue;
            }
            let Ok(entry) = serde_json::from_slice::<Entry>(line) else {
                cache.rewrite = true;
                continue;
            };
            if !entry.index.valid() || entry.path.len() > 32_768 {
                cache.rewrite = true;
                continue;
            }
            if cache.entries.len() < MAX_RECORDS || cache.entries.contains_key(&entry.id) {
                cache.entries.insert(entry.id, entry);
            }
        }
        // Opening even a network library reads only the cache and catalog, never every original.
        // External file changes are picked up by the existing explicit metadata refresh commands.
        cache.entries.retain(|id, entry| catalog.photo(*id).is_some_and(|photo| entry.matches(photo)));
        for (id, entry) in &cache.entries {
            let _ = catalog.set_metadata_search(*id, Some(entry.index.clone()));
        }
        cache
    }

    pub(crate) fn save(&mut self, store: &mut dyn Store, catalog: &Catalog) -> io::Result<()> {
        if self.dirty.is_empty() {
            return Ok(());
        }
        // Append just changed entries during import; compact periodically instead of rewriting
        // the entire library's metadata after each small import batch.
        let compact = self.rewrite
            || self.bytes == 0
            || self.bytes > (self.entries.len().saturating_mul(4096)).max(8 * 1024 * 1024)
            || self.dirty.len() > 256
            || self.bytes.saturating_add(self.dirty.len().saturating_mul(MAX_LINE)) > MAX_BYTES;
        if compact {
            self.entries.retain(|id, entry| catalog.photo(*id).is_some_and(|p| entry.matches(p)));
            self.bytes = store.write_atomic_with(FILE, &mut |w| {
                w.write_all(HEADER)?;
                let mut total = HEADER.len();
                for entry in self.entries.values() {
                    let line = serde_json::to_vec(entry).map_err(io::Error::other)?;
                    if total.saturating_add(line.len() + 1) > MAX_BYTES {
                        break;
                    }
                    w.write_all(&line)?;
                    w.write_all(b"\n")?;
                    total += line.len() + 1;
                }
                Ok(())
            })? as usize;
        } else {
            // A newline also separates an interrupted append from subsequent valid records.
            let mut bytes = vec![b'\n'];
            for id in &self.dirty {
                if let Some(entry) = self.entries.get(id) {
                    serde_json::to_writer(&mut bytes, entry).map_err(io::Error::other)?;
                    bytes.push(b'\n');
                }
            }
            if let Err(e) = store.append(FILE, &bytes) {
                self.rewrite = true;
                return Err(e);
            }
            self.bytes = self.bytes.saturating_add(bytes.len());
        }
        self.dirty.clear();
        self.rewrite = false;
        Ok(())
    }
}

impl crate::Session {
    pub(crate) fn remember_metadata_search(&mut self, id: PhotoId) {
        let entry = self.catalog.photo(id).and_then(|p| {
            let Source::File { path } = &p.source else { return None };
            Some(Entry { id, path: path.clone(), hash: p.content_hash.clone(), index: p.metadata_search.clone()? })
        });
        if let Some(entry) = entry {
            if self.metadata_cache.entries.get(&id) != Some(&entry) {
                self.metadata_cache.entries.insert(id, entry);
                self.metadata_cache.dirty.insert(id);
            }
        } else if self.metadata_cache.entries.remove(&id).is_some() {
            // Removing a record needs compaction, otherwise an old appended entry could return.
            self.metadata_cache.dirty.insert(id);
            self.metadata_cache.rewrite = true;
        }
    }

    /// Refresh derived data separately from undoable catalog edits. Virtual copies share a source.
    pub(crate) fn index_metadata_search(&mut self, id: PhotoId, info: &crate::media::ProbeInfo, naming: crate::sidecar::SidecarNaming) -> bool {
        let Some(mut index) = info.metadata_search.clone() else { return false };
        if let Some(p) = self.catalog.photo(id)
            && let Source::File { path } = &p.source
        {
            let packet = read_sidecar(path, naming);
            Arc::make_mut(&mut index).replace_sidecar(packet.as_deref());
        }
        let changed = self.catalog.photo(id).is_some_and(|p| p.metadata_search.as_ref() != Some(&index));
        let _ = self.catalog.set_metadata_search(id, Some(index));
        self.remember_metadata_search(id);
        changed
    }

    pub(crate) fn refresh_sidecar_search(&mut self, id: PhotoId, naming: crate::sidecar::SidecarNaming) {
        let Some(p) = self.catalog.photo(id) else { return };
        let Source::File { path } = &p.source else { return };
        let packet = read_sidecar(path, naming);
        let mut index = p.metadata_search.clone().unwrap_or_default();
        Arc::make_mut(&mut index).replace_sidecar(packet.as_deref());
        let _ = self.catalog.set_metadata_search(id, Some(index));
        self.remember_metadata_search(id);
    }
}

fn read_sidecar(path: &str, naming: crate::sidecar::SidecarNaming) -> Option<String> {
    let path = crate::sidecar::find_sidecar(path, naming)?;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    String::from_utf8(bytes).ok()
}
