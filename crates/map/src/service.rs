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
//! Threads start with the service only when it has something to do (a transport or a cache); on
//! wasm32 there is neither, so the service is inert and tiles never arrive.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use web_time::Instant;

use crate::cache::{TileCache, TileMeta};
use crate::mercator::{TileKey, Viewport};
use crate::source::TileSource;

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
/// First backoff after an error, and the longest.
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Longest Retry-After honoured.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(3600);
/// Worker threads: [`MAX_IN_FLIGHT`] for the network plus one so cache reads never wait behind it.
const WORKERS: usize = MAX_IN_FLIGHT + 1;

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

/// Does one GET. The real one is [`HttpTransport`] (native only); tests use an in-process fake.
pub trait Transport: Send + Sync {
    /// GET `url` with `headers`; a body longer than `max_body` is an error. Must give up within a
    /// bounded time (timeouts), and soon after `cancel` is raised.
    fn get(&self, url: &str, headers: &[(&'static str, String)], max_body: usize, cancel: &AtomicBool) -> Result<HttpResponse, String>;
}

/// HTTPS through `lightcraft-fetch` (pure Rust, rustls), with connect and stall timeouts.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct HttpTransport {
    pub connect_timeout: Duration,
    pub stall_timeout: Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for HttpTransport {
    fn default() -> Self {
        HttpTransport { connect_timeout: Duration::from_secs(10), stall_timeout: Duration::from_secs(15) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Transport for HttpTransport {
    fn get(&self, url: &str, headers: &[(&'static str, String)], max_body: usize, cancel: &AtomicBool) -> Result<HttpResponse, String> {
        use lightcraft_fetch::http;
        let url = http::Url::parse(url).map_err(|e| e.to_string())?;
        let limits = http::Limits { connect: self.connect_timeout, stall: self.stall_timeout, cancel };
        let hs: Vec<(&str, String)> = headers.iter().map(|(k, v)| (*k, v.clone())).collect();
        let mut r = http::get(&url, &hs, &limits).map_err(|e| e.to_string())?;
        if r.content_length().is_some_and(|n| n > max_body as u64) {
            return Err(format!("tile larger than {max_body} bytes"));
        }
        let mut body = Vec::new();
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = r.read(&mut buf, &limits).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if body.len().saturating_add(n) > max_body {
                return Err(format!("tile larger than {max_body} bytes"));
            }
            body.extend_from_slice(buf.get(..n).unwrap_or_default());
        }
        let headers = ["cache-control", "expires", "date", "etag", "last-modified", "retry-after", "content-type"]
            .iter()
            .filter_map(|h| r.header(h).map(|v| (h.to_string(), v.to_string())))
            .collect();
        Ok(HttpResponse { status: r.status, headers, body })
    }
}

/// A decoded tile: 8-bit sRGB RGBA, row-major, `width × height × 4` bytes (unpremultiplied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TileImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode a tile image (PNG, JPEG or WebP, at most [`MAX_TILE_EDGE`] px a side).
pub fn decode_tile(bytes: &[u8]) -> Result<TileImage, crate::MapError> {
    use lightcraft_codecs::{DecodeOptions, Format};
    let bad = |m: String| crate::MapError::BadTile(m);
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
    memory: HashMap<TileKey, MemTile>,
    use_counter: u64,
    events: Vec<TileEvent>,
    stats: ServiceStats,
    shutdown: bool,
}

impl State {
    fn wanted(&self, k: &TileKey) -> bool {
        self.rank.contains_key(k)
    }

    fn remember(&mut self, key: TileKey, image: Arc<TileImage>, expires: i64) {
        self.use_counter += 1;
        self.memory.insert(key, MemTile { image, expires, used: self.use_counter });
        while self.memory.len() > MEMORY_TILES {
            // never drop a tile of the current view for an older one
            let victim = self.memory.iter().filter(|(k, _)| !self.rank.contains_key(k)).min_by_key(|(_, t)| t.used).map(|(k, _)| *k);
            let victim = victim.or_else(|| self.memory.iter().min_by_key(|(_, t)| t.used).map(|(k, _)| *k));
            match victim {
                Some(k) => self.memory.remove(&k),
                None => break,
            };
        }
    }

    /// The host had an error: back off (doubling, at least `retry_after`).
    fn back_off(&mut self, retry_after: Option<Duration>) {
        self.errors_in_row = self.errors_in_row.saturating_add(1);
        let exp = BACKOFF_START.saturating_mul(1u32 << self.errors_in_row.saturating_sub(1).min(16));
        let wait = exp.min(BACKOFF_MAX).max(retry_after.unwrap_or_default().min(RETRY_AFTER_MAX));
        self.backoff_until = Some(Instant::now() + wait);
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
        f.debug_struct("TileService").field("source", &self.shared.source.id).finish()
    }
}

enum Job {
    Disk(TileKey),
    Net(TileKey),
}

impl TileService {
    /// The service for `source`, caching under `cache_root` (none: memory only) capped at
    /// `cache_cap` bytes, fetching with the real HTTPS client (natively, unless the source is
    /// offline). An unusable cache folder is logged and the service runs without it.
    pub fn new(source: TileSource, cache_root: Option<std::path::PathBuf>, cache_cap: u64) -> TileService {
        let cache = if source.is_offline() {
            None
        } else {
            cache_root.and_then(|root| TileCache::new(root, &source.id, cache_cap).map_err(|e| log::warn!("map: no tile cache: {e}")).ok())
        };
        #[cfg(not(target_arch = "wasm32"))]
        let transport: Option<Arc<dyn Transport>> = (!source.is_offline()).then(|| Arc::new(HttpTransport::default()) as Arc<dyn Transport>);
        #[cfg(target_arch = "wasm32")]
        let transport: Option<Arc<dyn Transport>> = None;
        TileService::with_transport(source, cache, transport)
    }

    /// The service with an explicit cache and transport (tests use a fake transport). No
    /// transport, or an offline source: nothing is ever fetched.
    pub fn with_transport(source: TileSource, cache: Option<TileCache>, transport: Option<Arc<dyn Transport>>) -> TileService {
        let transport = transport.filter(|_| !source.is_offline());
        let active = transport.is_some() || cache.is_some();
        let shared =
            Arc::new(Shared { source, cache, transport, state: Mutex::new(State::default()), wake: Condvar::new(), cancel: AtomicBool::new(false) });
        if active {
            for i in 0..WORKERS {
                let s = Arc::clone(&shared);
                if let Err(e) = std::thread::Builder::new().name(format!("map-tiles-{i}")).spawn(move || worker(&s)) {
                    log::warn!("map: can't start a tile thread: {e}");
                    break;
                }
            }
        }
        TileService { shared }
    }

    pub fn source(&self) -> &TileSource {
        &self.shared.source
    }

    /// Load the tiles of `view` (drawn ones plus a one-tile margin), dropping queued tiles it no
    /// longer shows. Call it whenever the view may have changed: it is cheap when it didn't.
    pub fn show(&self, view: &Viewport) {
        self.want(&view.tile_keys(self.shared.source.max_zoom, 1));
    }

    /// Load exactly `keys` (most important first; at most [`MAX_WANTED`]), replacing what was
    /// wanted before. Keys past the source's max zoom are ignored.
    pub fn want(&self, keys: &[TileKey]) {
        let max_zoom = self.shared.source.max_zoom;
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
        let now_unix = crate::now_secs();
        let now = Instant::now();
        let stale_in_memory = |st: &State, k: &TileKey| st.memory.get(k).is_some_and(|t| t.expires <= now_unix) && !st.revalidated.contains(k);
        let unchanged = list == st.wanted && !list.iter().any(|k| stale_in_memory(&st, k));
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
        let has_cache = self.shared.cache.is_some();
        let can_fetch = self.shared.transport.is_some();
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
        self.shared.lock().shutdown = true;
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
                    return Some(Job::Net(k));
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
            Job::Disk(k) | Job::Net(k) => k,
        };
        let net = matches!(job, Job::Net(_));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match job {
            Job::Disk(k) => disk_job(sh, k),
            Job::Net(k) => net_job(sh, k),
        }));
        let mut st = sh.lock();
        st.busy.remove(&key);
        if net {
            st.net_in_flight = st.net_in_flight.saturating_sub(1);
        }
        if r.is_err() {
            st.failed.insert(key, Instant::now() + FAILED_RETRY);
            st.events.push(TileEvent::Failed { key, error: "internal error loading the tile".into() });
        }
        drop(st);
        sh.wake.notify_all();
    }
}

/// Look `k` up in the cache: show it if there (and revalidate it if stale), else fetch it.
fn disk_job(sh: &Shared, k: TileKey) {
    let now = crate::now_secs();
    let cached = sh.cache.as_ref().and_then(|c| c.read(k));
    let decoded = cached.and_then(|c| match decode_tile(&c.bytes) {
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
    let at = crate::http_date::parse(v)?;
    Some(Duration::from_secs(u64::try_from(at.saturating_sub(crate::now_secs())).unwrap_or(0)))
}

/// Fetch `k` from the server (conditionally when a copy is cached).
fn net_job(sh: &Shared, k: TileKey) {
    let (Some(transport), Some(url)) = (sh.transport.as_ref(), sh.source.url(k)) else { return };
    let cached = sh.cache.as_ref().and_then(|c| c.read(k)).map(|c| c.meta);
    let mut headers: Vec<(&'static str, String)> = vec![("User-Agent", sh.source.user_agent.clone())];
    if let Some(m) = &cached {
        headers.extend(m.conditional_headers());
    }
    sh.lock().stats.requests += 1;
    let result = transport.get(&url, &headers, MAX_TILE_BYTES, &sh.cancel);
    let now = crate::now_secs();
    let fail = |st: &mut State, error: String, retry: bool| {
        st.stats.errors += 1;
        if retry {
            // the host is in trouble: wait, then try again if still in view
            if st.wanted(&k) && !st.net_q.contains(&k) {
                st.net_q.push(k);
            }
        } else {
            st.failed.insert(k, Instant::now() + FAILED_RETRY);
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
            st.back_off(None);
            fail(&mut st, e, true);
            return;
        }
    };
    let header = |n: &str| resp.header(n);
    match resp.status {
        200 => {
            let decoded = decode_tile(&resp.body);
            let (meta, store) = TileMeta::from_response(now, header);
            let mut st = sh.lock();
            st.errors_in_row = 0;
            st.backoff_until = None;
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
                    let meta = old.revalidated(now, header);
                    c.refresh(k, &meta).map(|ok| ok.then_some(meta)).unwrap_or_else(|e| {
                        log::warn!("map: {e}");
                        None
                    })
                }
                _ => None,
            };
            let mut st = sh.lock();
            st.stats.not_modified += 1;
            st.errors_in_row = 0;
            st.backoff_until = None;
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
