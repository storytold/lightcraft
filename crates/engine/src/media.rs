//! Source proxies and render jobs.
//!
//! Sources are decoded (or generated) once per level and cached in memory LRUs: `Thumb` (≤ 512 px
//! long edge) for the grid and filmstrip, `Preview` (≤ 2560 px) for the loupe, `Full` (native
//! resolution, the last one only) for exports larger than a preview. A [`RenderJob`] is
//! self-contained and `Send`: frontends run it on a worker thread; if the source wasn't cached yet
//! the job loads it and hands it back in the [`RenderResult`] so the cache can keep it.
//!
//! Rendered thumbnails are cached too ([`lightcraft_preview::PreviewCache`]: memory LRU, plus a
//! disk cache in the library's `thumbs/` folder), keyed by the photo's content hash, the develop
//! settings hash, the size and [`RENDER_CACHE_VERSION`] — so reopening a library shows its grid
//! without decoding a single original, and an edit simply produces a new key.
//!
//! Opening a photo shows something at once ([`QuickJob`]): the loupe's last render for the current
//! settings (kept in the same cache under a size-independent "view" key), else the camera's
//! embedded JPEG for an unedited raw, else a thumbnail-level render — while the real render is
//! prepared. Grid thumbnails of unedited raws likewise start from the embedded preview.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Weak};

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::{Quality, RenderRequest, Rendered, SourceInfo, StageCache};
use lightcraft_preview::{Hash128, Hasher128, Lru, PreviewCache};
use lightcraft_raster::{Histogram, Rgb32f, Rgba8};
use serde::{Deserialize, Serialize};

mod cache;
mod quick;
mod render;
mod session;
mod settings_hash;
mod source;

pub use cache::DenoiseIndex;
pub use quick::{QuickJob, QuickSource, content_key, source_info};
pub use render::{RenderJob, RenderResult, develop, develop_reporting, fit_for_display};
#[cfg(test)]
use settings_hash::SETTINGS_HASH_ENTRIES;
use settings_hash::SettingsHashes;
pub use source::{
    DISK_CACHE_BYTES, DecodedSource, DenoiseSpec, FACE_EDGE_STEP, FileLoader, FileProbe, MakeProduct, PairLoader, PreviewLoader, ProbeInfo,
    RENDER_CACHE_VERSION, ReducedLoader, SourceLevel, SourceRef, THUMB_SIZES, Twin, face_edge, thumb_bucket,
};

pub struct MediaCache {
    settings_hashes: SettingsHashes,
    /// Decoded thumbnail-level sources (LRU by bytes).
    thumbs: Lru<PhotoId, DecodedSource>,
    /// Decoded preview-level sources with their last use ([`lightcraft_preview::next_tick`]).
    previews: Vec<(PhotoId, DecodedSource, u64)>,
    /// The last full-resolution originals (exports, zoomed views; ~300 MB each at 24 MP), with
    /// their last use. Normally one; see [`Self::set_full_capacity`].
    full: Vec<(PhotoId, DecodedSource, u64)>,
    /// How many full-resolution originals to keep (LRU, at least one).
    full_capacity: usize,
    /// The photo the loupe shows ([`Self::set_on_screen`]): its original is not evicted for the budget.
    on_screen: Option<PhotoId>,
    /// How many previews to keep (LRU).
    pub preview_capacity: usize,
    /// Bytes all decoded sources and rendered previews in memory may take together (the cache
    /// share of [`crate::memory::budget`]); the least recently used entry of any of them goes first.
    budget: usize,
    pub file_loader: Option<FileLoader>,
    /// What grid thumbnails load files with (see [`ReducedLoader`]); `None`: [`Self::file_loader`].
    pub reduced_loader: Option<ReducedLoader>,
    pub file_probe: Option<FileProbe>,
    pub preview_loader: Option<PreviewLoader>,
    /// Reads a photo file's bytes (Photo Merge); `None` = the local file system.
    pub file_bytes: Option<crate::merge::ByteReader>,
    /// The library's smart previews folder (originals offline: render from the proxy).
    pub smart_dir: Option<std::path::PathBuf>,
    /// Whether originals (and smart previews) are on disk: cached and checked off the UI thread
    /// in the app (see [`crate::availability`]).
    pub availability: crate::availability::Availability,
    scenes: Vec<lightcraft_scenes::Scene>,
    /// Rendered thumbnails (memory, plus disk once a library is attached).
    pub rendered: Arc<PreviewCache>,
    /// Which photos have a cached denoised picture, and how to read it.
    pub denoise: DenoiseIndex,
}

#[cfg(test)]
mod thumbnail_hash_tests;

#[cfg(test)]
mod tests;
