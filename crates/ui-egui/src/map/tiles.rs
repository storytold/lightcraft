//! Map tiles as textures: fetched on worker threads through `dac_geo::fetch` (cache first), a few
//! at a time, only for tiles on screen; decoded off the UI thread; kept as a bounded set of
//! textures (least recently drawn go first). A headless host fetches synchronously so a snapshot
//! shows the finished map. Without a fetcher (the web build) nothing is fetched.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};

use dac_geo::TileKey;
use dac_geo::tiles::TileServer;

/// Concurrent downloads (OSM's policy asks for few connections).
const MAX_INFLIGHT: usize = 4;
/// Textures kept.
const MAX_TEXTURES: usize = 384;
/// Frames before a failed tile is tried again.
const RETRY_FRAMES: u64 = 600;

type Key = (String, TileKey);
type Decoded = Result<(u32, u32, Vec<u8>), String>;

enum Slot {
    Loading,
    Ready(egui::TextureHandle, u64),
    Failed(u64),
}

pub struct TileLoader {
    slots: HashMap<Key, Slot>,
    tx: Sender<(Key, Decoded)>,
    rx: Receiver<(Key, Decoded)>,
    inflight: usize,
    frame: u64,
    #[cfg(not(target_arch = "wasm32"))]
    fetcher: Option<std::sync::Arc<dac_geo::fetch::TileFetcher>>,
    /// The last fetch error (shown in the attribution line).
    pub last_error: Option<String>,
    /// Tiles fetched / failed since start (`map.view` reports them).
    pub loaded: u64,
    pub failed: u64,
}

impl Default for TileLoader {
    fn default() -> Self {
        let (tx, rx) = channel();
        TileLoader {
            slots: HashMap::new(),
            tx,
            rx,
            inflight: 0,
            frame: 0,
            #[cfg(not(target_arch = "wasm32"))]
            fetcher: None,
            last_error: None,
            loaded: 0,
            failed: 0,
        }
    }
}

impl TileLoader {
    /// Set up the fetcher with a disk cache in `cache_dir` (none: memory only).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(&mut self, cache_dir: Option<std::path::PathBuf>) {
        let cache = cache_dir.map(dac_geo::cache::TileCache::new);
        if let Some(c) = cache.clone() {
            // keep the cache under its limit, off the UI thread
            let _ = std::thread::Builder::new().name("tile-cache-prune".into()).spawn(move || c.prune());
        }
        match dac_geo::fetch::TileFetcher::new(cache) {
            Ok(f) => self.fetcher = Some(std::sync::Arc::new(f)),
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }

    pub fn has_fetcher(&self) -> bool {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.fetcher.is_some()
        }
        #[cfg(target_arch = "wasm32")]
        {
            false
        }
    }

    /// Forget every texture (the style changed or the cache was cleared).
    pub fn clear(&mut self) {
        self.slots.clear();
    }

    /// The disk cache's (bytes, files).
    pub fn cache_usage(&self) -> Option<(u64, usize)> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.fetcher.as_ref().and_then(|f| f.cache.as_ref()).map(|c| c.usage())
        }
        #[cfg(target_arch = "wasm32")]
        {
            None
        }
    }

    pub fn clear_cache(&mut self) -> Result<(), String> {
        self.clear();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(c) = self.fetcher.as_ref().and_then(|f| f.cache.as_ref()) {
            return c.clear().map_err(|e| e.to_string());
        }
        Ok(())
    }

    /// Start of a frame: take finished downloads.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok((key, res)) = self.rx.try_recv() {
            self.inflight = self.inflight.saturating_sub(1);
            self.store(ctx, key, res);
        }
        if self.slots.len() > MAX_TEXTURES {
            let mut ready: Vec<(u64, Key)> =
                self.slots.iter().filter_map(|(k, s)| if let Slot::Ready(_, used) = s { Some((*used, k.clone())) } else { None }).collect();
            ready.sort_by_key(|r| r.0);
            let drop = self.slots.len().saturating_sub(MAX_TEXTURES);
            for (_, k) in ready.into_iter().take(drop) {
                self.slots.remove(&k);
            }
        }
    }

    fn store(&mut self, ctx: &egui::Context, key: Key, res: Decoded) {
        match res {
            Ok((w, h, px)) => {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
                let name = format!("tile-{}-{}-{}-{}", key.0, key.1.z, key.1.x, key.1.y);
                let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
                self.slots.insert(key, Slot::Ready(tex, self.frame));
                self.loaded += 1;
                self.last_error = None;
            }
            Err(e) => {
                self.slots.insert(key, Slot::Failed(self.frame));
                self.failed += 1;
                self.last_error = Some(e);
            }
        }
    }

    /// The texture for a tile, if it is loaded (marks it as used).
    pub fn get(&mut self, server: &str, k: TileKey) -> Option<egui::TextureId> {
        let frame = self.frame;
        match self.slots.get_mut(&(server.to_string(), k)) {
            Some(Slot::Ready(t, used)) => {
                *used = frame;
                Some(t.id())
            }
            _ => None,
        }
    }

    /// Ask for a tile that is on screen. `sync`: fetch it now (headless snapshots).
    pub fn want(&mut self, ctx: &egui::Context, server: &TileServer, k: TileKey, sync: bool) {
        let key = (server.id.clone(), k);
        match self.slots.get(&key) {
            Some(Slot::Ready(..)) | Some(Slot::Loading) => return,
            Some(Slot::Failed(at)) if self.frame.saturating_sub(*at) < RETRY_FRAMES => return,
            _ => {}
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(f) = self.fetcher.clone() else { return };
            if sync {
                let res = fetch_decode(&f, server, k);
                self.store(ctx, key, res);
                return;
            }
            if self.inflight >= MAX_INFLIGHT {
                return;
            }
            self.slots.insert(key.clone(), Slot::Loading);
            self.inflight += 1;
            let (tx, server, ctx, again) = (self.tx.clone(), server.clone(), ctx.clone(), key.clone());
            let spawned = std::thread::Builder::new().name("map-tile".into()).spawn(move || {
                let res = fetch_decode(&f, &server, k);
                let _ = tx.send((key, res));
                ctx.request_repaint();
            });
            if spawned.is_err() {
                self.inflight = self.inflight.saturating_sub(1);
                self.slots.insert(again, Slot::Failed(self.frame));
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (ctx, key, sync);
        }
    }

    /// Downloads still running.
    pub fn busy(&self) -> bool {
        self.inflight > 0
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn fetch_decode(f: &dac_geo::fetch::TileFetcher, server: &TileServer, k: TileKey) -> Decoded {
    let (bytes, _) = f.fetch(server, k).map_err(|e| e.to_string())?;
    dac_geo::tiles::decode_tile(&bytes).map_err(|e| e.to_string())
}
