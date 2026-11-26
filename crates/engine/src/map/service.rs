//! The tile fetch queue: background threads that load the current view's tiles from the cache or
//! the tile server, without ever blocking the caller.
//!
//! The map view calls [`TileService::show`] with its [`Viewport`] every frame (cheap when nothing
//! changed), draws the tiles [`TileService::get`] returns, and calls [`TileService::poll`] to learn
//! which tiles arrived (to repaint). Behind that, following the OpenStreetMap tile usage policy:
//!
//! - **Only the view**: the tiles wanted are those drawn plus at most one ring around them, at the
//!   drawn zoom only; no other zooms or areas are ever prefetched and there is no "download area".
//! - **Newest view first**: each `show` replaces the wanted set; queued tiles that left the view
//!   are dropped before they start, the rest are fetched nearest the centre first. (A request
//!   already on the wire finishes and is cached.)
//! - **Gentle**: at most [`MAX_IN_FLIGHT`] requests at a time; after an error, a 429 or a 5xx the
//!   host gets exponential backoff (1 s doubling to 5 min, at least its Retry-After) and then one
//!   probe at a time until a request succeeds, so a failing server sees no retry storm; a tile the server refuses (404, garbage) isn't asked for again
//!   for [`FAILED_RETRY`].
//! - **Cache first**: a fresh cached tile costs no request; an expired one is shown at once and
//!   revalidated with If-None-Match / If-Modified-Since (304 just renews it). Requests carry the
//!   source's User-Agent and never `Cache-Control: no-cache` or `Pragma`.
//! - **Hostile responses**: bodies over [`MAX_TILE_BYTES`], images that aren't PNG/JPEG/WebP of at
//!   most 1024 px, or that fail to decode are errors, never panics.
//!
//! - **Keys stay secret**: a keyed source's API key ([`TileService::with_token`]) is added to the
//!   request URL only; error messages, the cache (keyed by source id and tile) and the activity
//!   row never carry it.
//! - **Credit**: a source with metadata (the Esri layers) has it fetched once, bounded and checked
//!   ([`TileService::attribution`]); until then, or when it fails, the source's fixed credit shows.
//! - **Visible**: while tiles are queued or loading, one "Loading map tiles" row is on the activity
//!   stack (`docs/background-tasks.md`); it goes when the queue is empty.
//!
//! Threads start with the service only when it has something to do (a transport or a cache); on
//! wasm32 there is neither, so the service is inert and tiles never arrive.
//!
//! Ported from the map core of PR #797 (Jason Baldwin) into the engine.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use web_time::Instant;

use super::cache::{TileCache, TileMeta};
use super::mercator::{TileKey, Viewport};
use super::source::{Crop, ESRI_POWERED_BY, TileSource, with_token};
use crate::activity::{Activity, Cancel, TaskGuard};
use crate::map::ApiKey;

/// Most tile requests on the wire at once.
pub const MAX_IN_FLIGHT: usize = 2;
/// Largest tile body accepted.
pub const MAX_TILE_BYTES: usize = 2 << 20;
/// Largest tile edge accepted (256 px tiles; 512 for high-DPI sources).
pub const MAX_TILE_EDGE: u32 = 1024;
/// A tile the server refused is not asked for again before this.
pub const FAILED_RETRY: Duration = Duration::from_secs(10 * 60);
/// Most tiles wanted at once (a 16k-pixel view with its margin stays under this).
pub const MAX_WANTED: usize = 4096;
/// Decoded tiles kept in memory (256 × 256 × 4 bytes each: 64 MB).
pub const MEMORY_TILES: usize = 256;
/// Decoded bytes kept in memory: [`MEMORY_TILES`] ordinary 256 px tiles. A server sending larger
/// tiles (up to [`MAX_TILE_EDGE`], kept whole) gets fewer of them, not more memory.
pub const MEMORY_BYTES: usize = MEMORY_TILES * 256 * 256 * 4;
/// First backoff after an error, and the longest.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Longest Retry-After honoured.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(3600);
/// Largest metadata answer accepted.
pub const MAX_METADATA_BYTES: usize = 64 * 1024;
/// Longest credit text taken from metadata.
const MAX_CREDIT: usize = 400;
/// Server tiles kept a moment after they arrive, so the four 256 px tiles cut from one 512 px tile
/// cost one request.
const RECENT_TILES: usize = 16;
const RECENT_FOR: Duration = Duration::from_secs(120);
/// Worker threads: [`MAX_IN_FLIGHT`] for the network plus one so cache reads never wait behind it.
const WORKERS: usize = MAX_IN_FLIGHT + 1;

/// Starts a named worker thread running the job (`std::thread::Builder` outside tests).
pub(super) type Spawn = dyn Fn(String, Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>;

/// A tile server's answer, as a [`Transport`] returns it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The first header named `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Why a [`Transport`] GET gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransportError {
    /// The body was longer than allowed: the server answered, only that tile is refused (the host
    /// is not backed off for it).
    TooLarge(String),
    /// Connecting, a timeout, a broken connection: the host may be in trouble.
    Failed(String),
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::TooLarge(m) | TransportError::Failed(m) => f.write_str(m),
        }
    }
}

impl From<String> for TransportError {
    fn from(m: String) -> Self {
        TransportError::Failed(m)
    }
}

/// Does one GET. The real one is [`HttpTransport`] (native only); tests use an in-process fake.
pub trait Transport: Send + Sync {
    /// GET `url` with `headers`; a body longer than `max_body` is [`TransportError::TooLarge`].
    /// Must give up within a bounded time (timeouts), and soon after `cancel` is raised.
    fn get(&self, url: &str, headers: &[(&'static str, String)], max_body: usize, cancel: &AtomicBool) -> Result<HttpResponse, TransportError>;
}

/// HTTPS through `lightcraft-fetch` (pure Rust, rustls), with connect and stall timeouts, and a
/// limit on the whole body: the stall timeout restarts with each read, so a server sending a few
/// bytes now and then would otherwise hold one of the two network slots until the size cap.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct HttpTransport {
    pub connect_timeout: Duration,
    pub stall_timeout: Duration,
    /// Longest time spent receiving one body, however often bytes arrive.
    pub body_timeout: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for HttpTransport {
    fn default() -> Self {
        HttpTransport { connect_timeout: Duration::from_secs(10), stall_timeout: Duration::from_secs(15), body_timeout: Duration::from_secs(60) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for HttpTransport {
    fn get(&self, url: &str, headers: &[(&'static str, String)], max_body: usize, cancel: &AtomicBool) -> Result<HttpResponse, TransportError> {
        use lightcraft_fetch::http;
        let url = http::Url::parse(url).map_err(|e| e.to_string())?;
        let limits = http::Limits { connect: self.connect_timeout, stall: self.stall_timeout, cancel };
        let hs: Vec<(&str, String)> = headers.iter().map(|(k, v)| (*k, v.clone())).collect();
        let mut r = http::get(&url, &hs, &limits).map_err(|e| e.to_string())?;
        if r.content_length().is_some_and(|n| n > max_body as u64) {
            return Err(TransportError::TooLarge(format!("tile larger than {max_body} bytes")));
        }
        let mut body = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        let started = Instant::now();
        loop {
            if started.elapsed() >= self.body_timeout {
                return Err(TransportError::Failed(format!("tile not received within {} s", self.body_timeout.as_secs())));
            }
            let n = r.read(&mut buf, &limits).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if body.len().saturating_add(n) > max_body {
                return Err(TransportError::TooLarge(format!("tile larger than {max_body} bytes")));
            }
            body.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
        let headers = kept_headers(|h| r.header_all(h));
        Ok(HttpResponse { status: r.status, headers, body })
    }
}

/// The response headers the tile service reads, from `all` (every value of a header, in order):
/// the `Cache-Control` lines joined into one list (a `no-store` on a second line counts), the
/// others' first value.
#[cfg(any(test, not(target_arch = "wasm32")))]
pub(super) fn kept_headers<'a>(all: impl Fn(&str) -> Vec<&'a str>) -> Vec<(String, String)> {
    ["cache-control", "expires", "date", "etag", "last-modified", "retry-after", "content-type"]
        .iter()
        .filter_map(|h| {
            let values = all(h);
            let v = if *h == "cache-control" { (!values.is_empty()).then(|| values.join(", ")) } else { values.first().map(|v| v.to_string()) };
            v.map(|v| (h.to_string(), v))
        })
        .collect()
}

/// A decoded tile: 8-bit sRGB RGBA, row-major, `width × height × 4` bytes (unpremultiplied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode a tile image (PNG, JPEG or WebP, at most [`MAX_TILE_EDGE`] px a side).
pub fn decode_tile(bytes: &[u8]) -> Result<TileImage, crate::map::MapError> {
    use lightcraft_codecs::{DecodeOptions, Format};
    let bad = |m: String| crate::map::MapError::BadTile(m);
    if bytes.len() > MAX_TILE_BYTES {
        return Err(bad("too large".into()));
    }
    let h = lightcraft_codecs::read_header(bytes).map_err(|e| bad(e.to_string()))?;
    if !matches!(h.format, Format::Png | Format::Jpeg | Format::WebP) {
        return Err(bad(format!("{:?} is not a tile format", h.format)));
    }
    if h.width == 0 || h.height == 0 || h.width > MAX_TILE_EDGE || h.height > MAX_TILE_EDGE {
        return Err(bad(format!("{}×{} is not a tile size", h.width, h.height)));
    }
    let max_pixels = u64::from(MAX_TILE_EDGE) * u64::from(MAX_TILE_EDGE);
    let d = lightcraft_codecs::decode(bytes, DecodeOptions { max_size: None, max_pixels }).map_err(|e| bad(e.to_string()))?;
    let img = d.to_srgb8();
    let (width, height) = (u32::try_from(img.width).unwrap_or(0), u32::try_from(img.height).unwrap_or(0));
    Ok(TileImage { width, height, rgba: img.data.as_flattened().to_vec() })
}

/// The 256 px tile `crop` asks for, cut from (or scaled down from) the server's tile.
pub fn crop_tile(img: TileImage, crop: Crop) -> Result<TileImage, crate::map::MapError> {
    let bad = |m: String| crate::map::MapError::BadTile(m);
    let (w, h) = (img.width as usize, img.height as usize);
    if img.rgba.len() != w.saturating_mul(h).saturating_mul(4) {
        return Err(bad("pixel data doesn't match the size".into()));
    }
    match crop {
        Crop::Whole => Ok(img),
        Crop::Quarter { right, bottom } => {
            if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
                return Err(bad(format!("{w}×{h} can't be cut in quarters")));
            }
            let (qw, qh) = (w / 2, h / 2);
            let (x0, y0) = (if right { qw } else { 0 }, if bottom { qh } else { 0 });
            let mut rgba = Vec::with_capacity(qw * qh * 4);
            for y in y0..y0 + qh {
                let row = (y * w + x0) * 4;
                rgba.extend_from_slice(img.rgba.get(row..row + qw * 4).ok_or_else(|| bad("tile row out of range".into()))?);
            }
            Ok(TileImage { width: qw as u32, height: qh as u32, rgba })
        }
        Crop::Half => {
            if w < 2 || h < 2 || w % 2 != 0 || h % 2 != 0 {
                return Ok(img);
            }
            let (hw, hh) = (w / 2, h / 2);
            let mut rgba = Vec::with_capacity(hw * hh * 4);
            let px = |x: usize, y: usize, c: usize| u32::from(img.rgba.get((y * w + x) * 4 + c).copied().unwrap_or(0));
            for y in 0..hh {
                for x in 0..hw {
                    for c in 0..4 {
                        let sum = px(2 * x, 2 * y, c) + px(2 * x + 1, 2 * y, c) + px(2 * x, 2 * y + 1, c) + px(2 * x + 1, 2 * y + 1, c);
                        rgba.push(((sum + 2) / 4) as u8);
                    }
                }
            }
            Ok(TileImage { width: hw as u32, height: hh as u32, rgba })
        }
    }
}

/// The credit text in a layer's metadata answer: `copyrightText`, one line of plain text (no
/// markup) of at most 400 characters; anything else is refused.
pub fn metadata_credit(body: &[u8]) -> Option<String> {
    if body.len() > MAX_METADATA_BYTES {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let t = v.get("copyrightText")?.as_str()?.trim();
    let ok = !t.is_empty() && t.chars().count() <= MAX_CREDIT && !t.chars().any(|c| c.is_control() || matches!(c, '<' | '>'));
    ok.then(|| t.to_string())
}

/// What [`TileService::poll`] reports.
#[derive(Clone, Debug)]
pub enum TileEvent {
    /// A tile is available (also from [`TileService::get`]). `stale`: from the cache past its
    /// freshness, being revalidated (a fresh copy may follow).
    Ready { key: TileKey, image: Arc<TileImage>, stale: bool },
    /// A tile couldn't be loaded (the view draws its placeholder). Not retried for a while.
    Failed { key: TileKey, error: String },
}

/// Counters for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStats {
    /// Requests sent to the tile server.
    pub requests: u64,
    /// Requests on the wire now, and the most there ever were at once.
    pub in_flight: usize,
    pub max_in_flight: usize,
    /// Answers: 304 Not Modified, errors (transport, 429/5xx, refused or garbage tiles).
    pub not_modified: u64,
    pub errors: u64,
    /// Tiles queued, wanted, and decoded in memory.
    pub queued: usize,
    pub wanted: usize,
    pub in_memory: usize,
    /// Milliseconds until the host may be asked again (0 = not backing off).
    pub backoff_ms: u64,
    /// Server tiles reused from a recent answer instead of a request.
    pub reused: u64,
}

/// Where the source's metadata (its credit) stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Meta {
    #[default]
    NotAsked,
    Queued,
    Running,
    Done,
}

struct MemTile {
    image: Arc<TileImage>,
    /// Fresh until (Unix seconds).
    expires: i64,
    used: u64,
}

#[derive(Default)]
struct State {
    /// The tiles of the newest view, nearest the centre first, and their rank in it.
    wanted: Vec<TileKey>,
    rank: HashMap<TileKey, usize>,
    /// Tiles to look up in the cache, then (if missing or stale) to fetch.
    disk_q: VecDeque<TileKey>,
    net_q: Vec<TileKey>,
    /// Tiles a worker is handling now.
    busy: HashSet<TileKey>,
    net_in_flight: usize,
    /// Tiles not to ask for before the given time (refused or undecodable).
    failed: HashMap<TileKey, Instant>,
    /// Stale tiles already revalidated this session (or being): not again until they expire anew.
    revalidated: HashSet<TileKey>,
    /// The host's backoff: consecutive errors and the time it may be asked again.
    errors_in_row: u32,
    backoff_until: Option<Instant>,
    /// Host failures so far. A request notes it when it is let through; only one let through
    /// after the last failure (the probe) ends the backoff when it succeeds, not one that was
    /// already on the wire when the host began failing.
    failures: u64,
    memory: HashMap<TileKey, MemTile>,
    /// The decoded bytes in `memory` (at most [`MEMORY_BYTES`], beyond the newest tile).
    memory_bytes: usize,
    use_counter: u64,
    events: Vec<TileEvent>,
    stats: ServiceStats,
    shutdown: bool,
    /// The activity row while there is work, and the jobs finished since it appeared.
    row: Option<TaskGuard>,
    row_done: u64,
    activity: Option<Activity>,
    /// The API key added to requests (keyed sources), never anywhere else.
    token: Option<ApiKey>,
    /// The credit from the source's metadata, and where fetching it stands.
    credit: Option<String>,
    meta: Meta,
    /// Recent server answers by URL (without key), newest last.
    recent: VecDeque<(String, Instant, Arc<HttpResponse>)>,
    /// Worker threads that started. None (the OS refused every thread): nothing is queued, since
    /// no one would run it and the view's tiles would stay "loading" forever.
    workers: usize,
}

impl State {
    fn wanted(&self, k: &TileKey) -> bool {
        self.rank.contains_key(k)
    }

    fn remember(&mut self, key: TileKey, image: Arc<TileImage>, expires: i64) {
        self.use_counter += 1;
        self.memory_bytes = self.memory_bytes.saturating_add(image.rgba.len());
        if let Some(old) = self.memory.insert(key, MemTile { image, expires, used: self.use_counter }) {
            self.memory_bytes = self.memory_bytes.saturating_sub(old.image.rgba.len());
        }
        while self.memory.len() > MEMORY_TILES || (self.memory_bytes > MEMORY_BYTES && self.memory.len() > 1) {
            // never drop a tile of the current view for an older one
            let victim = self.memory.iter().filter(|(k, _)| !self.rank.contains_key(k)).min_by_key(|(_, t)| t.used).map(|(k, _)| *k);
            let victim = victim.or_else(|| self.memory.iter().min_by_key(|(_, t)| t.used).map(|(k, _)| *k));
            match victim.and_then(|k| self.memory.remove(&k)) {
                Some(t) => self.memory_bytes = self.memory_bytes.saturating_sub(t.image.rgba.len()),
                None => break,
            }
        }
    }

    /// The host had an error: back off (doubling, at least `retry_after`).
    fn back_off(&mut self, retry_after: Option<Duration>) {
        self.failures = self.failures.wrapping_add(1);
        self.errors_in_row = self.errors_in_row.saturating_add(1);
        let exp = BACKOFF_START.saturating_mul(1u32 << self.errors_in_row.saturating_sub(1).min(16));
        let wait = exp.min(BACKOFF_MAX).max(retry_after.unwrap_or_default().min(RETRY_AFTER_MAX));
        self.backoff_until = Some(Instant::now() + wait);
    }

    /// A request let through at failure count `started` succeeded: the host is fine again, unless
    /// it failed since that request started.
    fn host_ok(&mut self, started: u64) {
        if started == self.failures {
            self.errors_in_row = 0;
            self.backoff_until = None;
        }
    }

    /// Show the activity row while tiles are queued or loading, with how many are done; drop it
    /// when nothing is left.
    fn update_row(&mut self) {
        let left = (self.disk_q.len() + self.net_q.len() + self.busy.len()) as u64;
        if left == 0 {
            self.row = None;
            self.row_done = 0;
            return;
        }
        if self.row.is_none()
            && let Some(a) = &self.activity
        {
            self.row = Some(a.start("download", "Loading map tiles", Cancel::No));
        }
        if let Some(row) = &self.row {
            row.progress(self.row_done, self.row_done.saturating_add(left));
        }
    }
}

struct Shared {
    source: TileSource,
    cache: Option<TileCache>,
    transport: Option<Arc<dyn Transport>>,
    state: Mutex<State>,
    wake: Condvar,
    cancel: AtomicBool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The fetch queue of one tile source (see the module docs). Dropping it stops its threads (a
/// request on the wire is cancelled).
pub struct TileService {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for TileService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileService").field("source", &self.shared.source.id()).finish()
    }
}

enum Job {
    Disk(TileKey),
    /// The tile, and the host's failure count when the request was let through.
    Net(TileKey, u64),
    Meta,
}

impl TileService {
    /// The service for `source`, caching under `cache_root` (none: memory only) capped at
    /// `cache_cap` bytes, fetching with the real HTTPS client (natively, unless the source is
    /// offline). An unusable cache folder is logged and the service runs without it.
    pub fn new(source: TileSource, cache_root: Option<std::path::PathBuf>, cache_cap: u64, activity: Option<Activity>, online: bool) -> TileService {
        let cache = if source.is_offline() {
            None
        } else {
            cache_root.and_then(|root| TileCache::new(root, &source.cache_id(), cache_cap).map_err(|e| log::warn!("map: no tile cache: {e}")).ok())
        };
        #[cfg(not(target_arch = "wasm32"))]
        let transport: Option<Arc<dyn Transport>> =
            (online && !source.is_offline()).then(|| Arc::new(HttpTransport::default()) as Arc<dyn Transport>);
        #[cfg(target_arch = "wasm32")]
        let transport: Option<Arc<dyn Transport>> = {
            let _ = online;
            None
        };
        TileService::with_transport(source, cache, transport).with_activity(activity)
    }

    /// Add `token` (the user's API key) to this source's requests. Only to its tile and metadata
    /// requests: never to messages, logs or the cache.
    pub fn with_token(self, token: Option<ApiKey>) -> TileService {
        self.shared.lock().token = token;
        self
    }

    /// The credit to paint: for a source with metadata, "Powered by Esri" and the layer's own
    /// `copyrightText` once fetched; otherwise (or until then) the source's fixed credit.
    pub fn attribution(&self) -> String {
        match &self.shared.lock().credit {
            Some(c) => format!("{ESRI_POWERED_BY} | {c}"),
            None => self.shared.source.attribution().to_string(),
        }
    }

    /// Report loading on `activity` (one row while tiles are queued or loading).
    pub fn with_activity(self, activity: Option<Activity>) -> TileService {
        self.shared.lock().activity = activity;
        self
    }

    /// The service with an explicit cache and transport (tests use a fake transport). No
    /// transport, or an offline source: nothing is ever fetched.
    pub fn with_transport(source: TileSource, cache: Option<TileCache>, transport: Option<Arc<dyn Transport>>) -> TileService {
        TileService::with_spawner(source, cache, transport, &|name, job| std::thread::Builder::new().name(name).spawn(job).map(drop))
    }

    /// [`TileService::with_transport`] starting its workers with `spawn` (tests make it fail).
    pub(super) fn with_spawner(source: TileSource, cache: Option<TileCache>, transport: Option<Arc<dyn Transport>>, spawn: &Spawn) -> TileService {
        let transport = transport.filter(|_| !source.is_offline());
        let active = transport.is_some() || cache.is_some();
        let shared =
            Arc::new(Shared { source, cache, transport, state: Mutex::new(State::default()), wake: Condvar::new(), cancel: AtomicBool::new(false) });
        let mut workers = 0;
        if active {
            for i in 0..WORKERS {
                let s = Arc::clone(&shared);
                if let Err(e) = spawn(format!("map-tiles-{i}"), Box::new(move || worker(&s))) {
                    log::warn!("map: can't start a tile thread: {e}");
                    break;
                }
                workers += 1;
            }
            if workers == 0 {
                log::warn!("map: no tile thread started; the map shows no tiles");
            }
        }
        shared.lock().workers = workers;
        TileService { shared }
    }

    /// Whether this service can ever deliver a tile: it has a transport or a cache. False on wasm32
    /// (no browser fetch yet) and for the offline source; nothing is queued then, so it stays idle.
    pub fn can_load(&self) -> bool {
        (self.shared.transport.is_some() || self.shared.cache.is_some()) && self.shared.lock().workers > 0
    }

    pub fn source(&self) -> &TileSource {
        &self.shared.source
    }

    /// Load the tiles of `view` (drawn ones plus a one-tile margin), dropping queued tiles it no
    /// longer shows. Call it whenever the view may have changed: it is cheap when it didn't.
    pub fn show(&self, view: &Viewport) {
        self.want(&view.tile_keys(self.shared.source.max_zoom(), 1));
    }

    /// Load exactly `keys` (most important first; at most [`MAX_WANTED`]), replacing what was
    /// wanted before. Keys past the source's max zoom are ignored.
    pub fn want(&self, keys: &[TileKey]) {
        let max_zoom = self.shared.source.max_zoom();
        let mut st = self.shared.lock();
        let mut list: Vec<TileKey> = Vec::with_capacity(keys.len().min(MAX_WANTED));
        let mut rank = HashMap::new();
        for k in keys.iter().filter(|k| k.z <= max_zoom) {
            if list.len() >= MAX_WANTED {
                break;
            }
            if !rank.contains_key(k) {
                rank.insert(*k, list.len());
                list.push(*k);
            }
        }
        let now_unix = crate::map::now_secs();
        let now = Instant::now();
        let stale_in_memory = |st: &State, k: &TileKey| st.memory.get(k).is_some_and(|t| t.expires <= now_unix) && !st.revalidated.contains(k);
        // a tile whose retry delay ran out is work again, though the view didn't change
        let retry_due = |st: &State, k: &TileKey| st.failed.get(k).is_some_and(|until| *until <= now);
        let unchanged = list == st.wanted && !list.iter().any(|k| stale_in_memory(&st, k) || retry_due(&st, k));
        if unchanged {
            return;
        }
        st.wanted = list;
        st.rank = rank;
        // newest view first: what left the view is dropped before it starts
        let State { disk_q, net_q, rank, revalidated, .. } = &mut *st;
        disk_q.retain(|k| rank.contains_key(k));
        net_q.retain(|k| {
            let keep = rank.contains_key(k);
            if !keep {
                // its refresh never started: allow it again when the tile is back in view
                revalidated.remove(k);
            }
            keep
        });
        st.failed.retain(|_, until| *until > now);
        // no worker: queue nothing (see `State::workers`)
        let has_cache = self.shared.cache.is_some() && st.workers > 0;
        let can_fetch = self.shared.transport.is_some() && st.workers > 0;
        // the credit: asked for once, with the first tiles
        if can_fetch && !st.wanted.is_empty() && st.meta == Meta::NotAsked && self.shared.source.metadata_url().is_some() {
            st.meta = Meta::Queued;
        }
        for k in st.wanted.clone() {
            if st.busy.contains(&k) || st.disk_q.contains(&k) || st.net_q.contains(&k) || st.failed.contains_key(&k) {
                continue;
            }
            if st.memory.contains_key(&k) {
                if stale_in_memory(&st, &k) && can_fetch {
                    st.revalidated.insert(k);
                    st.net_q.push(k);
                }
                continue;
            }
            if has_cache {
                st.disk_q.push_back(k);
            } else if can_fetch {
                st.net_q.push(k);
            }
        }
        // the disk queue in the view's order too
        let State { disk_q, rank, .. } = &mut *st;
        disk_q.make_contiguous().sort_by_key(|k| rank.get(k).copied().unwrap_or(usize::MAX));
        st.update_row();
        drop(st);
        self.shared.wake.notify_all();
    }

    /// The decoded tile if it is in memory (never touches disk or network).
    pub fn get(&self, key: TileKey) -> Option<Arc<TileImage>> {
        let mut st = self.shared.lock();
        st.use_counter += 1;
        let n = st.use_counter;
        st.memory.get_mut(&key).map(|t| {
            t.used = n;
            Arc::clone(&t.image)
        })
    }

    /// Tiles that arrived (or failed) since the last call.
    pub fn poll(&self) -> Vec<TileEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }

    pub fn stats(&self) -> ServiceStats {
        let st = self.shared.lock();
        let mut s = st.stats;
        s.queued = st.disk_q.len() + st.net_q.len();
        s.wanted = st.wanted.len();
        s.in_memory = st.memory.len();
        s.in_flight = st.net_in_flight;
        s.backoff_ms = st.backoff_until.map_or(0, |u| u.saturating_duration_since(Instant::now()).as_millis() as u64);
        s
    }

    /// Nothing queued or running (tests wait for this).
    pub fn is_idle(&self) -> bool {
        let st = self.shared.lock();
        st.disk_q.is_empty() && st.net_q.is_empty() && st.busy.is_empty()
    }
}

impl Drop for TileService {
    fn drop(&mut self) {
        let mut st = self.shared.lock();
        st.shutdown = true;
        st.row = None;
        drop(st);
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
    }
}

/// The next job, waiting for one (`None`: shut down).
fn next_job(sh: &Shared) -> Option<Job> {
    let mut st = sh.lock();
    loop {
        if st.shutdown {
            return None;
        }
        while let Some(k) = st.disk_q.pop_front() {
            if st.wanted(&k) && !st.busy.contains(&k) {
                st.busy.insert(k);
                return Some(Job::Disk(k));
            }
        }
        let now = Instant::now();
        let backing_off = st.backoff_until.is_some_and(|u| u > now);
        // while the host is failing, one probe at a time
        let limit = if st.errors_in_row > 0 { 1 } else { MAX_IN_FLIGHT };
        if sh.transport.is_some() && st.meta == Meta::Queued && st.net_in_flight < limit && !backing_off {
            st.meta = Meta::Running;
            st.net_in_flight += 1;
            return Some(Job::Meta);
        }
        if sh.transport.is_some() && !st.net_q.is_empty() && st.net_in_flight < limit && !backing_off {
            // the most central tile of the newest view
            let best = st
                .net_q
                .iter()
                .enumerate()
                .filter(|(_, k)| !st.busy.contains(k))
                .min_by_key(|(_, k)| st.rank.get(k).copied().unwrap_or(usize::MAX))
                .map(|(i, _)| i);
            if let Some(i) = best {
                let k = st.net_q.swap_remove(i);
                if st.wanted(&k) {
                    st.busy.insert(k);
                    st.net_in_flight += 1;
                    st.stats.max_in_flight = st.stats.max_in_flight.max(st.net_in_flight);
                    return Some(Job::Net(k, st.failures));
                }
                continue;
            }
        }
        st = match st.backoff_until.filter(|u| *u > now && !st.net_q.is_empty()) {
            Some(until) => sh.wake.wait_timeout(st, until.saturating_duration_since(now)).unwrap_or_else(PoisonError::into_inner).0,
            None => sh.wake.wait(st).unwrap_or_else(PoisonError::into_inner),
        };
    }
}

fn worker(sh: &Shared) {
    while let Some(job) = next_job(sh) {
        // a panic in a decoder or the transport is that tile's error, not a dead thread
        let key = match job {
            Job::Disk(k) | Job::Net(k, _) => k,
            Job::Meta => {
                let credit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| meta_job(sh))).ok().flatten();
                let mut st = sh.lock();
                st.net_in_flight = st.net_in_flight.saturating_sub(1);
                st.meta = Meta::Done;
                st.credit = credit;
                drop(st);
                sh.wake.notify_all();
                continue;
            }
        };
        let net = matches!(job, Job::Net(..));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            Job::Disk(k) => disk_job(sh, k),
            Job::Net(k, started) => net_job(sh, k, started),
            Job::Meta => {}
        }));
        let mut st = sh.lock();
        st.busy.remove(&key);
        if net {
            st.net_in_flight = st.net_in_flight.saturating_sub(1);
        }
        if r.is_err() {
            st.failed.insert(key, Instant::now() + FAILED_RETRY);
            // a refresh it was running may be tried again after the delay
            st.revalidated.remove(&key);
            st.events.push(TileEvent::Failed { key, error: "internal error loading the tile".into() });
        }
        // a tile counts as done once nothing more is queued for it (a cache miss goes on to the network)
        if !st.net_q.contains(&key) && !st.disk_q.contains(&key) {
            st.row_done = st.row_done.saturating_add(1);
        }
        st.update_row();
        drop(st);
        sh.wake.notify_all();
    }
}

/// Fetch the source's metadata and take its credit (`None`: none usable; the fixed one shows).
fn meta_job(sh: &Shared) -> Option<String> {
    let (transport, url) = (sh.transport.as_ref()?, sh.source.metadata_url()?);
    let token = sh.lock().token.clone();
    let full = match &token {
        Some(t) => with_token(&url, t.secret()),
        None => url.clone(),
    };
    let headers = [("User-Agent", sh.source.user_agent().to_string())];
    match transport.get(&full, &headers, MAX_METADATA_BYTES, &sh.cancel) {
        Ok(r) if r.status == 200 => metadata_credit(&r.body).or_else(|| {
            log::warn!("map: {url}: no usable credit in the layer's metadata; showing the fixed one");
            None
        }),
        Ok(r) => {
            log::warn!("map: {url}: HTTP {} for the layer's metadata; showing the fixed credit", r.status);
            None
        }
        Err(e) => {
            log::warn!("map: {url}: {}", hide(&e.to_string(), token.as_ref()));
            None
        }
    }
}

/// `text` with the API key replaced, for messages (a transport error may quote the URL).
fn hide(text: &str, token: Option<&ApiKey>) -> String {
    match token {
        Some(t) if !t.secret().is_empty() => text.replace(t.secret(), "[key]"),
        _ => text.to_string(),
    }
}

/// Decode a tile of the source and cut the 256 px tile `k` out of it.
fn decode_for(sh: &Shared, k: TileKey, bytes: &[u8]) -> Result<TileImage, crate::map::MapError> {
    crop_tile(decode_tile(bytes)?, sh.source.crop(k))
}

/// Look `k` up in the cache: show it if there (and revalidate it if stale), else fetch it.
fn disk_job(sh: &Shared, k: TileKey) {
    let now = crate::map::now_secs();
    let cached = sh.cache.as_ref().and_then(|c| c.read(k));
    let decoded = cached.and_then(|c| match decode_for(sh, k, &c.bytes) {
        Ok(img) => Some((img, c.meta)),
        Err(e) => {
            log::warn!("map: cached tile {k:?} doesn't decode ({e}); fetching it again");
            if let Some(cache) = &sh.cache {
                cache.remove(k);
            }
            None
        }
    });
    let mut st = sh.lock();
    match decoded {
        Some((img, meta)) => {
            let fresh = meta.is_fresh(now);
            let image = Arc::new(img);
            st.remember(k, Arc::clone(&image), meta.expires);
            st.events.push(TileEvent::Ready { key: k, image, stale: !fresh });
            if !fresh && sh.transport.is_some() && st.wanted(&k) {
                st.revalidated.insert(k);
                st.net_q.push(k);
            }
        }
        None => {
            if sh.transport.is_some() && st.wanted(&k) {
                st.net_q.push(k);
            }
        }
    }
}

/// Parse Retry-After: seconds or an HTTP date.
fn retry_after(r: &HttpResponse) -> Option<Duration> {
    let v = r.header("retry-after")?.trim();
    if let Ok(s) = v.parse::<u64>() {
        return Some(Duration::from_secs(s));
    }
    let at = crate::map::http_date::parse(v)?;
    Some(Duration::from_secs(u64::try_from(at.saturating_sub(crate::map::now_secs())).unwrap_or(0)))
}

/// Fetch `k` from the server (conditionally when a copy is cached); `started`: the host's failure
/// count when the request was let through.
fn net_job(sh: &Shared, k: TileKey, started: u64) {
    let (Some(transport), Some(url)) = (sh.transport.as_ref(), sh.source.url(k)) else { return };
    let cached = sh.cache.as_ref().and_then(|c| c.read(k)).map(|c| c.meta);
    let mut headers: Vec<(&'static str, String)> = vec![("User-Agent", sh.source.user_agent().to_string())];
    if let Some(m) = &cached {
        headers.extend(m.conditional_headers());
    }
    let token = sh.lock().token.clone();
    let cropped = sh.source.crop(k) != Crop::Whole;
    // a server tile that just arrived for a neighbour (a 512 px tile holds four of ours)
    let reuse = if cropped {
        let mut st = sh.lock();
        let now = Instant::now();
        st.recent.retain(|(_, at, _)| now.saturating_duration_since(*at) < RECENT_FOR);
        let hit = st.recent.iter().find(|(u, _, _)| *u == url).map(|(_, _, r)| HttpResponse::clone(r));
        if hit.is_some() {
            st.stats.reused += 1;
        }
        hit
    } else {
        None
    };
    let result = match reuse {
        Some(r) => Ok(r),
        None => {
            sh.lock().stats.requests += 1;
            let full = match &token {
                Some(t) => with_token(&url, t.secret()),
                None => url.clone(),
            };
            let r = transport.get(&full, &headers, MAX_TILE_BYTES, &sh.cancel).map_err(|e| match e {
                TransportError::TooLarge(m) => TransportError::TooLarge(hide(&m, token.as_ref())),
                TransportError::Failed(m) => TransportError::Failed(hide(&m, token.as_ref())),
            });
            if let Ok(resp) = &r
                && resp.status == 200
                && cropped
            {
                let mut st = sh.lock();
                st.recent.push_back((url.clone(), Instant::now(), Arc::new(resp.clone())));
                while st.recent.len() > RECENT_TILES {
                    st.recent.pop_front();
                }
            }
            r
        }
    };
    let now = crate::map::now_secs();
    let fail = |st: &mut State, error: String, retry: bool| {
        st.stats.errors += 1;
        if retry && st.wanted(&k) {
            // the host is in trouble: wait, then try again if still in view
            if !st.net_q.contains(&k) {
                st.net_q.push(k);
            }
        } else {
            if !retry {
                st.failed.insert(k, Instant::now() + FAILED_RETRY);
            }
            // a refresh that didn't happen: allowed again (after the retry delay, or back in view)
            st.revalidated.remove(&k);
        }
        st.events.push(TileEvent::Failed { key: k, error });
    };
    let resp = match result {
        Ok(r) => r,
        Err(e) => {
            let mut st = sh.lock();
            if sh.cancel.load(Ordering::Relaxed) {
                return;
            }
            match e {
                // the server answered with a tile too large: refuse that tile for a while, as a
                // garbage one; the host is fine and the other tiles go on
                TransportError::TooLarge(m) => fail(&mut st, format!("{url}: {m}"), false),
                TransportError::Failed(m) => {
                    st.back_off(None);
                    fail(&mut st, m, true);
                }
            }
            return;
        }
    };
    let header = |n: &str| resp.header(n);
    match resp.status {
        200 => {
            let decoded = decode_for(sh, k, &resp.body);
            let (meta, store) = TileMeta::from_response(now, header);
            let mut st = sh.lock();
            st.host_ok(started);
            match decoded {
                Ok(img) => {
                    drop(st);
                    if store
                        && let Some(c) = &sh.cache
                        && let Err(e) = c.write(k, &meta, &resp.body)
                    {
                        log::warn!("map: {e}");
                    }
                    let mut st = sh.lock();
                    // an answer that is already stale (no-cache, max-age=0) isn't asked for again this session
                    if meta.expires > now {
                        st.revalidated.remove(&k);
                    } else {
                        st.revalidated.insert(k);
                    }
                    let image = Arc::new(img);
                    st.remember(k, Arc::clone(&image), meta.expires);
                    st.events.push(TileEvent::Ready { key: k, image, stale: false });
                }
                Err(e) => fail(&mut st, format!("{url}: {e}"), false),
            }
        }
        304 => {
            let refreshed = match (&cached, &sh.cache) {
                (Some(old), Some(c)) => {
                    let (meta, store) = old.revalidated(now, header);
                    if store {
                        c.refresh(k, &meta).map(|ok| ok.then_some(meta)).unwrap_or_else(|e| {
                            log::warn!("map: {e}");
                            None
                        })
                    } else {
                        // no-store now: off the disk; the copy in memory stays until it expires
                        c.remove(k);
                        Some(meta)
                    }
                }
                _ => None,
            };
            let mut st = sh.lock();
            st.stats.not_modified += 1;
            st.host_ok(started);
            match refreshed {
                Some(meta) => {
                    if let Some(t) = st.memory.get_mut(&k) {
                        t.expires = meta.expires;
                    }
                    if meta.expires > now {
                        st.revalidated.remove(&k);
                    }
                }
                None => fail(&mut st, format!("{url}: 304 for a tile that isn't cached"), false),
            }
        }
        // overloaded or blocked: back off the whole host
        429 | 403 | 500..=599 => {
            let mut st = sh.lock();
            st.back_off(retry_after(&resp));
            let retry = resp.status != 403;
            fail(&mut st, format!("{url}: HTTP {}", resp.status), retry);
        }
        s => {
            let mut st = sh.lock();
            fail(&mut st, format!("{url}: HTTP {s}"), false);
        }
    }
}
