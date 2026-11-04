//! Previews: everything that makes a big library show up instantly.
//!
//! - [`hash`]: 128-bit content hashes (file bytes → duplicate detection and cache keys).
//! - [`Lru`]: a cost-bounded memory LRU (decoded sources, rendered thumbnails).
//! - [`DiskCache`]: rendered thumbnails as JPEG files keyed by `(content hash, settings hash,
//!   size, renderer version)`, bounded in bytes (least recently used files are pruned).
//! - [`PreviewCache`]: memory LRU in front of the disk cache; shared by render workers.
//! - [`JobPool`]: worker threads with per-slot de-duplication and priorities (visible thumbnails
//!   before off-screen prefetch); runs jobs inline on wasm.
//!
//! No UI dependencies (L3): the egui frontend, the CLI and the MCP server share it.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod disk;
pub mod hash;
pub mod lru;
pub mod pool;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub use disk::{DiskCache, decode_jpeg, encode_jpeg};
pub use hash::{Hash128, Hasher128, hash_bytes};
use lightcraft_raster::Rgba8;
pub use lru::{Lru, next_tick};
pub use pool::JobPool;

/// Images that deferred disk writes ([`PreviewCache::put_deferred`]) may hold waiting for the writer, in bytes. A
/// write that would go past it is skipped (the image stays in memory; a later render writes it), so renders that
/// arrive faster than the disk takes them can't pile up threads and memory (issue #1522).
#[cfg(not(target_arch = "wasm32"))]
const MAX_QUEUED_BYTES: usize = 128 << 20;

/// A deferred disk write waiting for the cache's writer thread.
#[cfg(not(target_arch = "wasm32"))]
struct Write {
    generation: u64,
    key: Hash128,
    img: Arc<Rgba8>,
    cost: usize,
}

/// Rendered-thumbnail cache: memory LRU over an optional disk cache.
pub struct PreviewCache {
    /// Current generation; `None` once [`Self::retire`]d (nothing is read or written any more).
    generation: RwLock<Option<u64>>,
    mem: Mutex<Lru<Hash128, Arc<Rgba8>>>,
    disk: Option<DiskCache>,
    /// The writer thread's queue (started with the first deferred write), and the bytes waiting in it.
    #[cfg(not(target_arch = "wasm32"))]
    writer: Mutex<Option<std::sync::mpsc::Sender<Write>>>,
    queued: Arc<AtomicUsize>,
}

impl PreviewCache {
    /// Memory-only cache of at most `mem_bytes`.
    pub fn memory(mem_bytes: usize) -> PreviewCache {
        Self::new(mem_bytes, None)
    }

    fn new(mem_bytes: usize, disk: Option<DiskCache>) -> PreviewCache {
        PreviewCache {
            generation: RwLock::new(Some(0)),
            mem: Mutex::new(Lru::new(mem_bytes)),
            disk,
            #[cfg(not(target_arch = "wasm32"))]
            writer: Mutex::new(None),
            queued: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Bytes of images waiting for a deferred disk write.
    pub fn queued_bytes(&self) -> usize {
        self.queued.load(Ordering::Relaxed)
    }

    /// Memory LRU + disk cache in `dir` (at most `disk_bytes`).
    pub fn with_disk(mem_bytes: usize, dir: &Path, disk_bytes: u64) -> PreviewCache {
        Self::new(mem_bytes, Some(DiskCache::new(dir, disk_bytes)))
    }

    pub fn disk(&self) -> Option<&DiskCache> {
        self.disk.as_ref()
    }

    pub fn get(&self, key: Hash128) -> Option<Arc<Rgba8>> {
        self.get_at(self.generation(), key)
    }

    /// Changes when explicitly cleared; jobs captured before that clear cannot repopulate it.
    /// A retired cache reports `u64::MAX`, which no job's generation matches.
    pub fn generation(&self) -> u64 {
        self.generation.read().unwrap_or_else(std::sync::PoisonError::into_inner).unwrap_or(u64::MAX)
    }

    pub fn get_at(&self, generation: u64, key: Hash128) -> Option<Arc<Rgba8>> {
        let current = self.generation.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != Some(generation) {
            return None;
        }
        if let Some(v) = self.mem.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Some(v.clone());
        }
        let img = Arc::new(self.disk.as_ref()?.get(key)?);
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).insert(key, img.clone(), cost(&img));
        Some(img)
    }

    pub fn put(&self, key: Hash128, img: Arc<Rgba8>) {
        self.put_at(self.generation(), key, img);
    }

    pub fn put_at(&self, generation: u64, key: Hash128, img: Arc<Rgba8>) {
        let current = self.generation.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != Some(generation) {
            return;
        }
        if let Some(d) = &self.disk {
            d.put(key, &img);
        }
        let c = cost(&img);
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).insert(key, img, c);
    }

    /// Like [`Self::put`], but the disk write (a JPEG encode) happens on a background thread, so
    /// the caller (a render about to hand its result to the screen) isn't held up.
    pub fn put_deferred(self: &Arc<Self>, key: Hash128, img: Arc<Rgba8>) {
        self.put_deferred_at(self.generation(), key, img);
    }

    pub fn put_deferred_at(self: &Arc<Self>, generation: u64, key: Hash128, img: Arc<Rgba8>) {
        let current = self.generation.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if *current != Some(generation) {
            return;
        }
        let c = cost(&img);
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).insert(key, img.clone(), c);
        drop(current);
        let Some(d) = &self.disk else { return };
        #[cfg(target_arch = "wasm32")]
        {
            d.put(key, &img);
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = d;
            if self.queued.load(Ordering::Relaxed).saturating_add(c) > MAX_QUEUED_BYTES {
                log::debug!("preview cache: disk writes are behind; this one is skipped (the image stays in memory)");
                return;
            }
            let mut writer = self.writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if writer.is_none() {
                *writer = self.start_writer();
            }
            let Some(tx) = writer.as_ref() else { return };
            self.queued.fetch_add(c, Ordering::Relaxed);
            if tx.send(Write { generation, key, img, cost: c }).is_err() {
                self.queued.fetch_sub(c, Ordering::Relaxed);
                *writer = None;
            }
        }
    }

    /// The thread that makes the deferred disk writes, one after another. It holds the cache weakly, so it ends
    /// once the cache is dropped (its queue's sender with it).
    #[cfg(not(target_arch = "wasm32"))]
    fn start_writer(self: &Arc<Self>) -> Option<std::sync::mpsc::Sender<Write>> {
        let (tx, rx) = std::sync::mpsc::channel::<Write>();
        let cache = Arc::downgrade(self);
        let queued = self.queued.clone();
        let spawned = std::thread::Builder::new().name("lc-preview-write".into()).spawn(move || {
            for job in rx {
                if let Some(me) = cache.upgrade() {
                    let current = me.generation.read().unwrap_or_else(std::sync::PoisonError::into_inner);
                    if *current == Some(job.generation)
                        && let Some(d) = &me.disk
                    {
                        d.put(job.key, &job.img);
                    }
                }
                queued.fetch_sub(job.cost, Ordering::Relaxed);
            }
        });
        if spawned.is_err() {
            log::warn!("preview cache: could not start the writer thread");
            return None;
        }
        Some(tx)
    }

    /// Drop everything (memory and disk). Does nothing once [`Self::retire`]d.
    pub fn clear(&self) {
        let mut generation = self.generation.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        // a retired cache no longer owns its directory (its replacement may)
        let Some(g) = generation.as_mut() else { return };
        // (never reaches u64::MAX, which stands for "retired")
        *g = g.wrapping_add(1) % u64::MAX;
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).clear();
        if let Some(d) = &self.disk {
            d.clear();
        }
    }

    /// Stop this cache object for good, leaving its disk files alone: called when another cache
    /// object replaces it (a library reopened or another one opened). Jobs still holding it then
    /// can neither read nor write through it, so a later clear of the replacement's directory
    /// (possibly the same one) can't be repopulated by them. Waits for writes in progress, like
    /// [`Self::clear`].
    pub fn retire(&self) {
        let mut generation = self.generation.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation = None;
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Use tick ([`next_tick`]) of the least recently used image in memory.
    pub fn oldest_tick(&self) -> Option<u64> {
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).oldest_tick()
    }

    /// Drop the least recently used image from memory (it stays on disk); returns its bytes.
    pub fn evict_oldest(&self) -> Option<usize> {
        self.mem.lock().unwrap_or_else(|e| e.into_inner()).pop_oldest()
    }

    /// Change the memory budget (bytes), evicting at once if needed.
    pub fn set_mem_budget(&self, bytes: usize) {
        let mut m = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        m.set_budget(bytes);
        while m.cost() > bytes && m.len() > 1 {
            m.pop_oldest();
        }
    }

    /// (entries, bytes) held in memory.
    pub fn mem_usage(&self) -> (usize, usize) {
        let m = self.mem.lock().unwrap_or_else(|e| e.into_inner());
        (m.len(), m.cost())
    }
}

fn cost(img: &Rgba8) -> usize {
    img.width * img.height * 4 + 64
}

#[cfg(test)]
mod tests;
