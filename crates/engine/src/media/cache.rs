use super::settings_hash::SettingsHashes;
use super::source::{DecodedSource, DenoiseSpec, PairLoader, RENDERED_MEM_BYTES, SourceLevel, SourceRef, THUMB_SOURCE_BYTES};
use super::{Arc, DevelopSettings, Hasher128, Lru, MediaCache, Photo, PhotoId, PreviewCache, Rgb32f, Source, SourceInfo};

/// The photos that have a cached denoised picture for the denoise model in use. Only memory: nothing here touches the
/// file system, so it can be asked on the UI thread.
#[derive(Default)]
pub struct DenoiseIndex {
    ready: std::collections::HashMap<PhotoId, DenoiseSpec>,
    /// Reads the pictures (`None`: this build cannot, and nothing is ever ready).
    pub loader: Option<PairLoader>,
}

impl DenoiseIndex {
    /// What to load for `id` besides its plain picture, and how.
    pub fn get(&self, id: PhotoId) -> Option<(&DenoiseSpec, &PairLoader)> {
        self.ready.get(&id).zip(self.loader.as_ref())
    }

    pub fn spec(&self, id: PhotoId) -> Option<&DenoiseSpec> {
        self.ready.get(&id)
    }

    pub fn set(&mut self, id: PhotoId, spec: DenoiseSpec) {
        self.ready.insert(id, spec);
    }

    pub fn remove(&mut self, id: PhotoId) -> bool {
        self.ready.remove(&id).is_some()
    }

    pub fn clear(&mut self) {
        self.ready.clear();
    }

    pub fn len(&self) -> usize {
        self.ready.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    pub fn ids(&self) -> Vec<PhotoId> {
        let mut v: Vec<PhotoId> = self.ready.keys().copied().collect();
        v.sort();
        v
    }
}

impl Default for MediaCache {
    fn default() -> Self {
        let budget = crate::memory::cache_share(crate::memory::budget());
        MediaCache {
            settings_hashes: SettingsHashes::default(),
            thumbs: Lru::new(THUMB_SOURCE_BYTES.min(budget)),
            previews: Vec::new(),
            full: Vec::new(),
            full_capacity: 1,
            on_screen: None,
            preview_capacity: 0,
            budget,
            file_loader: None,
            reduced_loader: None,
            file_probe: None,
            header_probe: None,
            preview_loader: None,
            file_bytes: None,
            smart_dir: None,
            availability: Default::default(),
            scenes: Vec::new(),
            rendered: Arc::new(PreviewCache::memory(rendered_budget(budget))),
            denoise: DenoiseIndex::default(),
        }
    }
}

/// Memory for rendered previews out of the cache share.
fn rendered_budget(share: usize) -> usize {
    RENDERED_MEM_BYTES.min(share / 4)
}

impl MediaCache {
    /// Keep rendered thumbnails on disk in `dir` as well. The cache this replaces is retired
    /// (its files stay): render jobs still holding it can't write through it any more, even
    /// into a directory a later clear emptied (`dir` may be the same one).
    pub fn attach_disk_cache(&mut self, dir: &std::path::Path, disk_bytes: u64) {
        self.rendered.retire();
        self.rendered = Arc::new(PreviewCache::with_disk(rendered_budget(self.budget), dir, disk_bytes));
    }

    /// Change the disk budget of the cache in `dir`: in place when that cache is attached (its
    /// thumbnails are keyed by content, so they and the textures shown from them stay valid,
    /// and the same cache object keeps its generation), else attach it.
    pub fn set_disk_cache_bytes(&mut self, dir: &std::path::Path, disk_bytes: u64) {
        match self.rendered.disk().filter(|d| d.dir() == dir) {
            Some(d) => d.set_budget(disk_bytes),
            None => self.attach_disk_cache(dir, disk_bytes),
        }
    }

    /// Forget every decoded source (photo ids changed meaning, e.g. another library was opened).
    pub fn clear_sources(&mut self) {
        self.settings_hashes = SettingsHashes::default();
        self.thumbs = Lru::new(THUMB_SOURCE_BYTES.min(self.budget));
        self.previews.clear();
        self.full.clear();
    }

    /// How many full-resolution originals to keep at once (at least one). Compare and Reference
    /// views zoomed to 1:1 cut windows from two photos' originals: with room for one, every window
    /// render of one side would evict the other side's original and decode its own again. Lowering
    /// it drops the least recently used originals at once.
    pub fn set_full_capacity(&mut self, n: usize) {
        self.full_capacity = n.max(1);
        self.trim_full();
    }

    /// The photo the loupe shows (the frontend sets it every frame; `None` in other views). Its original stays
    /// even over budget: a 64 MP original (797 MB as linear float) alone exceeds the default share, and evicting it
    /// made every pan of a zoomed view decode the raw again. Showing another photo lets it go at once.
    pub fn set_on_screen(&mut self, id: Option<PhotoId>) {
        if self.on_screen != id {
            self.on_screen = id;
            self.enforce_budget();
        }
    }

    fn trim_full(&mut self) {
        while self.full.len() > self.full_capacity {
            let oldest = self.full.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i).unwrap_or(0);
            self.full.remove(oldest);
        }
    }

    /// Bytes the caches may hold together.
    pub fn budget(&self) -> usize {
        self.budget
    }

    /// Change the bytes the caches may hold together, evicting at once if needed.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = bytes;
        self.thumbs.set_budget(THUMB_SOURCE_BYTES.min(bytes));
        self.rendered.set_mem_budget(rendered_budget(bytes));
        self.enforce_budget();
    }

    /// Bytes held by decoded sources and rendered previews in memory.
    pub fn held(&self) -> usize {
        let (t, p, f) = self.usage();
        t.bytes + p.bytes + f.bytes + self.rendered.mem_usage().1
    }

    /// Evict least recently used entries across the source caches and the rendered previews
    /// until they fit the budget. The newest preview-level source (the photo on screen, or the
    /// one just decoded) is never evicted here, nor is the original of the photo on screen
    /// ([`Self::set_on_screen`]).
    pub fn enforce_budget(&mut self) {
        // The on-screen original is never evicted here, so it does not count either: counted, a 64 MP original (760 MiB)
        // nearly fills the default share (768 MiB) alone and everything else went instead — the photo's preview and
        // thumbnail sources and the neighbours' were decoded again from the raw on every edit, zoom-out and step.
        let pinned: usize = self.full.iter().filter(|e| Some(e.0) == self.on_screen).map(|e| e.1.bytes()).sum();
        let mut held = self.held().saturating_sub(pinned);
        while held > self.budget {
            let newest_preview = self.previews.iter().map(|e| e.2).max();
            let candidates = [
                self.thumbs.oldest_tick().map(|t| (t, 0)),
                self.previews.iter().filter(|e| Some(e.2) != newest_preview).map(|e| e.2).min().map(|t| (t, 1)),
                self.full.iter().filter(|e| Some(e.0) != self.on_screen).map(|e| e.2).min().map(|t| (t, 2)),
                self.rendered.oldest_tick().map(|t| (t, 3)),
            ];
            let Some((tick, which)) = candidates.into_iter().flatten().min() else { break };
            let freed = match which {
                0 => self.thumbs.pop_oldest(),
                1 => self.previews.iter().position(|e| e.2 == tick).map(|i| self.previews.remove(i).1.bytes()),
                2 => self.full.iter().position(|e| e.2 == tick).map(|i| self.full.remove(i).1.bytes()),
                _ => self.rendered.evict_oldest(),
            };
            match freed {
                Some(b) if b > 0 => held = held.saturating_sub(b),
                _ => break,
            }
        }
    }

    pub fn get(&mut self, id: PhotoId, level: SourceLevel) -> Option<Arc<Rgb32f>> {
        self.get_source(id, level).map(|s| s.image)
    }

    pub(super) fn get_source(&mut self, id: PhotoId, level: SourceLevel) -> Option<DecodedSource> {
        match level {
            SourceLevel::Thumb => self.thumbs.get(&id).cloned(),
            SourceLevel::Preview => self.previews.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = lightcraft_preview::next_tick();
                e.1.clone()
            }),
            SourceLevel::Full => self.full.iter_mut().find(|e| e.0 == id).map(|e| {
                e.2 = lightcraft_preview::next_tick();
                e.1.clone()
            }),
        }
    }

    /// The facts of photo `id`'s cached sources: the decoder's when one has them, else `header`
    /// (with a smart preview's stored camera tone curve).
    pub(super) fn source_facts(&self, id: PhotoId, header: SourceInfo) -> SourceInfo {
        let cached = || {
            self.thumbs
                .peek(&id)
                .into_iter()
                .chain(self.previews.iter().filter(|e| e.0 == id).map(|e| &e.1))
                .chain(self.full.iter().filter(|e| e.0 == id).map(|e| &e.1))
        };
        match cached().find(|s| s.info.is_some()).or_else(|| cached().next()) {
            Some(s) => s.info_or(header),
            None => header,
        }
    }

    pub fn insert(&mut self, id: PhotoId, level: SourceLevel, img: Arc<Rgb32f>) {
        self.insert_source(id, level, DecodedSource::new(img, None));
    }

    pub(super) fn insert_source(&mut self, id: PhotoId, level: SourceLevel, img: DecodedSource) {
        let tick = lightcraft_preview::next_tick();
        match level {
            SourceLevel::Thumb => {
                let cost = img.bytes();
                self.thumbs.insert(id, img, cost);
            }
            SourceLevel::Preview => {
                self.previews.retain(|e| e.0 != id);
                self.previews.push((id, img, tick));
                let cap = if self.preview_capacity == 0 { 4 } else { self.preview_capacity };
                while self.previews.len() > cap {
                    let oldest = self.previews.iter().enumerate().min_by_key(|(_, e)| e.2).map(|(i, _)| i).unwrap_or(0);
                    self.previews.remove(oldest);
                }
            }
            SourceLevel::Full => {
                self.full.retain(|e| e.0 != id);
                self.full.push((id, img, tick));
                self.trim_full();
            }
        }
        self.enforce_budget();
    }

    /// Forget a photo's decoded sources (e.g. after its file changed).
    pub fn forget(&mut self, id: PhotoId) {
        self.thumbs.remove(&id);
        self.previews.retain(|e| e.0 != id);
        self.full.retain(|e| e.0 != id);
    }

    /// (decoded thumbnail sources, bytes).
    pub fn source_usage(&self) -> (usize, usize) {
        (self.thumbs.len(), self.thumbs.cost())
    }

    /// Whether the decoded source of `id` at `level` is held.
    pub fn has_source(&self, id: PhotoId, level: SourceLevel) -> bool {
        match level {
            SourceLevel::Thumb => self.thumbs.contains(&id),
            SourceLevel::Preview => self.previews.iter().any(|e| e.0 == id),
            SourceLevel::Full => self.full.iter().any(|e| e.0 == id),
        }
    }

    /// Decoded sources held: (thumbnail level, preview level, full size).
    pub fn usage(&self) -> (crate::memory::Usage, crate::memory::Usage, crate::memory::Usage) {
        use crate::memory::Usage;
        let previews = Usage::new(self.previews.len(), self.previews.iter().map(|e| e.1.bytes()).sum());
        let full = Usage::new(self.full.len(), self.full.iter().map(|e| e.1.bytes()).sum());
        (Usage::new(self.thumbs.len(), self.thumbs.cost()), previews, full)
    }

    /// What a render of photo `id` with settings `s` adds to its identity for the denoised picture it will use: 0 unless
    /// it uses one (the Denoise amount is above 0 and the photo has a cached denoised picture).
    pub fn denoise_salt(&self, id: PhotoId, s: &DevelopSettings) -> u64 {
        match self.denoise.spec(id) {
            Some(spec) if s.denoise_amount() > 0.0 => Hasher128::new().str(&spec.key).finish().0 as u64 | 1,
            _ => 0,
        }
    }

    /// How to load photo `p` at `level`: from the cache when its source is there, else from its file (with its denoised
    /// picture beside it when it has one), its smart preview or the demo scene.
    pub fn source_ref(&mut self, p: &Photo, level: SourceLevel) -> SourceRef {
        self.source_ref_with(p, level, true)
    }

    /// [`Self::source_ref`] that never takes a decoded source from memory: for a job that needs something the cached
    /// one lacks (an export, whose denoised picture is made when it runs).
    pub fn source_ref_uncached(&mut self, p: &Photo, level: SourceLevel) -> SourceRef {
        self.source_ref_with(p, level, false)
    }

    fn source_ref_with(&mut self, p: &Photo, level: SourceLevel, cached: bool) -> SourceRef {
        let mut r = self.source_ref_plain(p, level, cached);
        if let (SourceRef::File { denoise, .. }, Some((spec, loader))) = (&mut r, self.denoise.get(p.id)) {
            *denoise = Some((spec.clone(), loader.clone()));
        }
        r
    }

    fn source_ref_plain(&mut self, p: &Photo, level: SourceLevel, cached: bool) -> SourceRef {
        if cached && let Some(a) = self.get_source(p.id, level) {
            return SourceRef::Loaded(Box::new(a));
        }
        // a thumbnail of a photo whose preview is decoded (the one in the loupe) is cut from it; not
        // with a denoised picture, which the thumbnail would lose
        if cached
            && level == SourceLevel::Thumb
            && self.denoise.spec(p.id).is_none()
            && let Some(big) = self.get_source(p.id, SourceLevel::Preview)
        {
            return SourceRef::Fit { from: Box::new(big), max_edge: level.max_edge() };
        }
        // Procedural scenes have a nominal size: "full" is that size, not unbounded.
        let max_edge = level.max_edge().min(match (&p.source, level) {
            (Source::Demo { .. }, SourceLevel::Full) => p.width.max(p.height).max(1) as usize,
            _ => usize::MAX,
        });
        // the original is offline: its smart preview, when there is one. Only cached answers are
        // used here (no file-system call on the UI thread in the app); an original not known to
        // be offline is tried first and its smart preview is the render worker's fallback.
        #[cfg(not(target_arch = "wasm32"))]
        if let (Source::File { path }, Some(dir)) = (&p.source, &self.smart_dir) {
            let sharded = crate::smart::sharded_file_path(dir, p);
            let legacy = dir.join(crate::smart::file_name(p));
            let sp = if self.availability.exists(&sharded.to_string_lossy()) == Some(true) { sharded } else { legacy };
            // (a proxy cut short fails to load, like the offline original; the worker's fallback
            // in `SourceRef::load` only uses a proxy that passes `smart::is_valid`)
            if self.availability.is_offline(path) && self.availability.exists(&sp.to_string_lossy()) == Some(true) {
                return SourceRef::Smart { path: sp };
            }
            let mut r = self.origin_ref(&p.source, max_edge);
            if let SourceRef::File { fallback, .. } = &mut r {
                *fallback = Some(sp);
            }
            return r;
        }
        self.origin_ref(&p.source, max_edge)
    }

    /// How to load `origin` at most `max_edge` pixels long, ignoring decoded sources in memory (a
    /// render worker in another wasm instance builds its sources from this).
    pub fn origin_ref(&mut self, origin: &Source, max_edge: usize) -> SourceRef {
        match origin {
            Source::Demo { scene } => {
                if self.scenes.is_empty() {
                    self.scenes = lightcraft_scenes::demo_library();
                }
                match self.scenes.iter().find(|s| s.id == *scene) {
                    Some(s) => SourceRef::Demo { scene: Box::new(s.clone()), max_edge },
                    None => SourceRef::File { path: format!("demo:{scene}"), max_edge, loader: None, fallback: None, denoise: None, reduced: None },
                }
            }
            Source::File { path } => {
                SourceRef::File { path: path.clone(), max_edge, loader: self.file_loader.clone(), fallback: None, denoise: None, reduced: None }
            }
        }
    }
}
