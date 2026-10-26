//! The fetch queue against an in-process fake transport and a localhost server (never the
//! internet): viewport-only requests, newest view first, at most two in flight, backoff, the
//! User-Agent, conditional refreshes and hostile responses.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cache::{TileCache, TileMeta};
use crate::mercator::{LatLon, TileKey, Viewport};
use crate::service::*;
use crate::source::{TileSource, USER_AGENT};

type Handler = Box<dyn Fn(&str, &[(&'static str, String)]) -> Result<HttpResponse, String> + Send + Sync>;

struct Fake {
    log: Mutex<Vec<(String, Vec<(&'static str, String)>)>>,
    active: AtomicUsize,
    max_active: AtomicUsize,
    handler: Handler,
}

impl Fake {
    fn new(handler: impl Fn(&str, &[(&'static str, String)]) -> Result<HttpResponse, String> + Send + Sync + 'static) -> Arc<Fake> {
        Arc::new(Fake { log: Mutex::new(Vec::new()), active: AtomicUsize::new(0), max_active: AtomicUsize::new(0), handler: Box::new(handler) })
    }
    fn urls(&self) -> Vec<String> {
        self.log.lock().unwrap().iter().map(|(u, _)| u.clone()).collect()
    }
    fn keys(&self) -> Vec<TileKey> {
        self.urls().iter().map(|u| key_of(u)).collect()
    }
}

impl Transport for Fake {
    fn get(&self, url: &str, headers: &[(&'static str, String)], _max: usize, _cancel: &AtomicBool) -> Result<HttpResponse, String> {
        self.log.lock().unwrap().push((url.to_string(), headers.to_vec()));
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(n, Ordering::SeqCst);
        let r = (self.handler)(url, headers);
        self.active.fetch_sub(1, Ordering::SeqCst);
        r
    }
}

fn source() -> TileSource {
    TileSource::custom(Some("test"), "https://tiles.test/{z}/{x}/{y}.png", Some("© test"), Some(19)).unwrap()
}

fn key_of(url: &str) -> TileKey {
    let p: Vec<u32> = url.trim_start_matches("https://tiles.test/").trim_end_matches(".png").split('/').map(|s| s.parse().unwrap()).collect();
    TileKey::new(p[0] as u8, p[1], p[2]).unwrap()
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut wr = e.write_header().unwrap();
        let data: Vec<u8> = (0..w * h).flat_map(|i| [(i % 256) as u8, 100, 200, 255]).collect();
        wr.write_image_data(&data).unwrap();
    }
    out
}

fn ok_png(cache_control: &str) -> HttpResponse {
    HttpResponse { status: 200, headers: vec![("Cache-Control".into(), cache_control.into()), ("ETag".into(), "\"v1\"".into())], body: png(256, 256) }
}

fn wait_until(what: &str, secs: u64, f: impl Fn() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lightcraft-map-svc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn paris() -> Viewport {
    Viewport::new(LatLon::new(48.8566, 2.3522).unwrap(), 12.0, 800.0, 600.0)
}

#[test]
fn only_the_views_tiles_are_requested_with_our_user_agent() {
    let fake = Fake::new(|_, _| Ok(ok_png("max-age=3600")));
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    let view = paris();
    svc.show(&view);
    let want: HashSet<TileKey> = view.tile_keys(19, 1).into_iter().collect();
    wait_until("all tiles", 20, || svc.is_idle() && fake.urls().len() >= want.len());
    let got: Vec<TileKey> = fake.keys();
    assert_eq!(got.len(), want.len(), "each tile once");
    assert!(got.iter().all(|k| want.contains(k) && k.z == 12), "only the view's tiles at its zoom");
    // the drawn tiles plus at most one ring: never more than (cols + 2) × (rows + 2)
    let drawn: HashSet<TileKey> = view.tiles(19).iter().map(|t| t.key).collect();
    assert!(want.is_superset(&drawn));
    for (_, headers) in fake.log.lock().unwrap().iter() {
        let ua: Vec<_> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("user-agent")).collect();
        assert_eq!(ua.len(), 1);
        assert_eq!(ua[0].1, USER_AGENT);
        assert!(
            !headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("pragma") || (k.eq_ignore_ascii_case("cache-control") && v.contains("no-cache")))
        );
    }
    let events = svc.poll();
    let ready: HashSet<TileKey> =
        events.iter().filter_map(|e| if let TileEvent::Ready { key, stale: false, .. } = e { Some(*key) } else { None }).collect();
    assert_eq!(ready, want);
    let img = svc.get(*drawn.iter().next().unwrap()).unwrap();
    assert_eq!((img.width, img.height, img.rgba.len()), (256, 256, 256 * 256 * 4));
    assert_eq!(&img.rgba[..4], &[0, 100, 200, 255]);
    // the same view again: nothing new
    svc.show(&view);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.urls().len(), want.len());
}

#[test]
fn at_most_two_requests_at_a_time() {
    let fake = Fake::new(|_, _| {
        std::thread::sleep(Duration::from_millis(15));
        Ok(ok_png("max-age=60"))
    });
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    let keys: Vec<TileKey> = (0..12).map(|x| TileKey::new(5, x, 3).unwrap()).collect();
    svc.want(&keys);
    wait_until("12 tiles", 20, || svc.is_idle() && fake.urls().len() == 12);
    assert_eq!(fake.max_active.load(Ordering::SeqCst), 2);
    assert_eq!(svc.stats().max_in_flight, 2);
    // nearest the centre (first in the list) first
    assert_eq!(fake.keys()[..2].iter().map(|k| k.x).collect::<HashSet<_>>(), [0, 1].into_iter().collect());
}

#[test]
fn tiles_that_left_the_view_are_dropped_before_they_start() {
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let g = gate.clone();
    let fake = Fake::new(move |_, _| {
        let (open, cv) = &*g;
        let mut o = open.lock().unwrap();
        while !*o {
            o = cv.wait(o).unwrap();
        }
        Ok(ok_png("max-age=60"))
    });
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    let a = paris();
    let b = Viewport::new(LatLon::new(-33.86, 151.21).unwrap(), 12.0, 800.0, 600.0);
    svc.show(&a);
    wait_until("two requests on the wire", 10, || fake.active.load(Ordering::SeqCst) == 2);
    svc.show(&b);
    {
        let (open, cv) = &*gate;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
    let in_b: HashSet<TileKey> = b.tile_keys(19, 1).into_iter().collect();
    wait_until("view B", 20, || svc.is_idle() && fake.urls().len() == 2 + in_b.len());
    let keys = fake.keys();
    let in_a: HashSet<TileKey> = a.tile_keys(19, 1).into_iter().collect();
    assert!(keys[..2].iter().all(|k| in_a.contains(k)), "the two on the wire were A's");
    assert!(keys[2..].iter().all(|k| in_b.contains(k)), "then only B's: {:?}", &keys[2..]);
    assert_eq!(keys[2..].iter().collect::<HashSet<_>>().len(), in_b.len());
}

#[test]
fn a_429_backs_off_the_host_instead_of_retrying() {
    let fake = Fake::new(|_, _| Ok(HttpResponse { status: 429, headers: vec![], body: b"slow down".to_vec() }));
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    let keys: Vec<TileKey> = (0..6).map(|x| TileKey::new(4, x, 4).unwrap()).collect();
    svc.want(&keys);
    wait_until("the first answers", 10, || svc.stats().errors >= 1);
    std::thread::sleep(Duration::from_millis(400));
    // at most the two that were already on the wire; then silence while backing off
    assert!(fake.urls().len() <= 2, "{:?}", fake.urls());
    let s = svc.stats();
    assert!(s.backoff_ms > 0, "{s:?}");
    assert!(svc.poll().iter().any(|e| matches!(e, TileEvent::Failed { .. })));
    // after the backoff the host is asked again (one request at a time while it keeps failing)
    wait_until("a retry", 10, || fake.urls().len() > 2);
    std::thread::sleep(Duration::from_millis(300));
    assert!(fake.urls().len() <= 4, "no retry storm: {}", fake.urls().len());
}

#[test]
fn retry_after_is_honoured() {
    let fake = Fake::new(|_, _| Ok(HttpResponse { status: 503, headers: vec![("Retry-After".into(), "120".into())], body: vec![] }));
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    svc.want(&[TileKey::new(1, 0, 0).unwrap()]);
    wait_until("the answer", 10, || svc.stats().errors == 1);
    assert!(svc.stats().backoff_ms > 100_000, "{:?}", svc.stats());
}

#[test]
fn cached_tiles_need_no_request_and_stale_ones_are_revalidated() {
    let root = tmp("revalidate");
    let fresh_key = TileKey::new(6, 10, 20).unwrap();
    let stale_key = TileKey::new(6, 11, 20).unwrap();
    let cache = TileCache::new(&root, "test", 100 << 20).unwrap();
    let now = crate::now_secs();
    cache.write(fresh_key, &TileMeta { fetched: now, expires: now + 3600, etag: Some("\"f\"".into()), last_modified: None }, &png(256, 256)).unwrap();
    let old = TileMeta {
        fetched: now - 900_000,
        expires: now - 10,
        etag: Some("\"v1\"".into()),
        last_modified: Some("Sun, 06 Nov 1994 08:49:37 GMT".into()),
    };
    cache.write(stale_key, &old, &png(256, 256)).unwrap();
    let fake = Fake::new(|_, _| Ok(HttpResponse { status: 304, headers: vec![("Cache-Control".into(), "max-age=3600".into())], body: vec![] }));
    let svc = TileService::with_transport(source(), Some(TileCache::new(&root, "test", 100 << 20).unwrap()), Some(fake.clone()));
    svc.want(&[fresh_key, stale_key]);
    wait_until("both", 10, || svc.is_idle() && svc.stats().not_modified == 1);
    let events = svc.poll();
    assert!(events.iter().any(|e| matches!(e, TileEvent::Ready { key, stale: false, .. } if *key == fresh_key)));
    assert!(events.iter().any(|e| matches!(e, TileEvent::Ready { key, stale: true, .. } if *key == stale_key)), "shown at once while refreshing");
    // only the stale one was asked for, conditionally
    let log = fake.log.lock().unwrap().clone();
    assert_eq!(log.len(), 1);
    assert_eq!(key_of(&log[0].0), stale_key);
    assert!(log[0].1.contains(&("If-None-Match", "\"v1\"".to_string())));
    assert!(log[0].1.contains(&("If-Modified-Since", "Sun, 06 Nov 1994 08:49:37 GMT".to_string())));
    // the 304 renewed it on disk, keeping its validators and bytes
    let c = TileCache::new(&root, "test", 100 << 20).unwrap().read(stale_key).unwrap();
    assert!(c.meta.expires >= now + 3500 && c.meta.fetched >= now);
    assert_eq!(c.meta.etag.as_deref(), Some("\"v1\""));
    assert_eq!(c.bytes, png(256, 256));
    // showing the same tiles again asks for nothing
    svc.want(&[stale_key, fresh_key]);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.urls().len(), 1);
    drop(svc);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_stale_tile_whose_refresh_was_dropped_is_refreshed_when_back_in_view() {
    let root = tmp("revalidate-dropped");
    let stale_key = TileKey::new(6, 11, 20).unwrap();
    let busy = [TileKey::new(6, 1, 1).unwrap(), TileKey::new(6, 2, 2).unwrap()];
    let now = crate::now_secs();
    let old = TileMeta { fetched: now - 900_000, expires: now - 10, etag: Some("\"v1\"".into()), last_modified: None };
    TileCache::new(&root, "test", 100 << 20).unwrap().write(stale_key, &old, &png(256, 256)).unwrap();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let g = gate.clone();
    let fake = Fake::new(move |_, _| {
        let (open, cv) = &*g;
        let mut o = open.lock().unwrap();
        while !*o {
            o = cv.wait(o).unwrap();
        }
        Ok(ok_png("max-age=60"))
    });
    let svc = TileService::with_transport(source(), Some(TileCache::new(&root, "test", 100 << 20).unwrap()), Some(fake.clone()));
    // both request slots held, so the stale tile's refresh can only wait in the queue
    svc.want(&busy);
    wait_until("two requests on the wire", 10, || fake.active.load(Ordering::SeqCst) == 2);
    svc.want(&[busy[0], busy[1], stale_key]);
    wait_until("the stale tile from disk", 10, || svc.stats().in_memory >= 1);
    // it leaves the view before its refresh starts, then comes back
    svc.want(&busy);
    svc.want(&[busy[0], busy[1], stale_key]);
    {
        let (open, cv) = &*gate;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
    wait_until("the refresh", 10, || svc.is_idle() && fake.keys().contains(&stale_key));
    drop(svc);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn tiles_that_arrive_already_stale_are_not_refetched_every_frame() {
    for cache_control in ["no-cache", "max-age=0"] {
        let fake = Fake::new(move |_, _| Ok(ok_png(cache_control)));
        let svc = TileService::with_transport(source(), None, Some(fake.clone()));
        let keys = [TileKey::new(5, 1, 1).unwrap(), TileKey::new(5, 2, 1).unwrap()];
        svc.want(&keys);
        wait_until("both tiles", 10, || svc.is_idle() && svc.stats().in_memory == 2);
        // the UI shows the same view every frame
        for _ in 0..20 {
            svc.want(&keys);
            std::thread::sleep(Duration::from_millis(5));
        }
        wait_until("idle", 10, || svc.is_idle());
        assert_eq!(fake.urls().len(), 2, "{cache_control}: one request per tile, not one per frame");
    }
}

#[test]
fn fetched_tiles_are_cached_and_damaged_entries_refetched() {
    let root = tmp("store");
    let fake = Fake::new(|_, _| Ok(ok_png("max-age=600")));
    let k = TileKey::new(3, 4, 5).unwrap();
    let file = root.join("test/3/4/5.tile");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"LCTILE1\ncorrupt").unwrap();
    let svc = TileService::with_transport(source(), Some(TileCache::new(&root, "test", 100 << 20).unwrap()), Some(fake.clone()));
    svc.want(&[k]);
    wait_until("the tile", 10, || svc.get(k).is_some() && svc.is_idle());
    assert_eq!(fake.urls().len(), 1, "the damaged entry was fetched again");
    let c = TileCache::new(&root, "test", 100 << 20).unwrap().read(k).unwrap();
    assert_eq!(c.meta.etag.as_deref(), Some("\"v1\""));
    assert!(c.meta.expires > crate::now_secs() + 500);
    // a cached file that isn't an image is deleted and fetched again too
    drop(svc);
    TileCache::new(&root, "test", 100 << 20).unwrap().write(k, &TileMeta { expires: i64::MAX, ..Default::default() }, b"not a png").unwrap();
    let svc = TileService::with_transport(source(), Some(TileCache::new(&root, "test", 100 << 20).unwrap()), Some(fake.clone()));
    svc.want(&[k]);
    wait_until("the tile again", 10, || svc.get(k).is_some() && svc.is_idle());
    assert_eq!(fake.urls().len(), 2);
    drop(svc);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn hostile_responses_fail_cleanly_and_are_not_retried() {
    let fake = Fake::new(|url, _| {
        let k = key_of(url);
        Ok(match k.x {
            0 => HttpResponse { status: 200, headers: vec![], body: b"<html>captive portal</html>".to_vec() },
            1 => HttpResponse { status: 200, headers: vec![], body: png(2048, 4) },
            2 => HttpResponse { status: 404, headers: vec![], body: vec![] },
            3 => HttpResponse { status: 200, headers: vec![], body: png(256, 256)[..300].to_vec() },
            4 => HttpResponse { status: 304, headers: vec![], body: vec![] },
            _ => panic!("a transport bug"),
        })
    });
    let svc = TileService::with_transport(source(), None, Some(fake.clone()));
    let keys: Vec<TileKey> = (0..6).map(|x| TileKey::new(3, x, 0).unwrap()).collect();
    svc.want(&keys);
    wait_until("all answers", 10, || svc.is_idle() && fake.urls().len() == 6);
    let failed: HashSet<TileKey> =
        svc.poll().into_iter().filter_map(|e| if let TileEvent::Failed { key, .. } = e { Some(key) } else { None }).collect();
    assert_eq!(failed, keys.iter().copied().collect());
    assert!(keys.iter().all(|k| svc.get(*k).is_none()));
    assert_eq!(svc.stats().backoff_ms, 0, "a refused tile is not the host's trouble");
    // asked again (a new view with the same tiles): not retried for a while
    svc.want(&keys[..3]);
    svc.want(&keys);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.urls().len(), 6);
}

#[test]
fn offline_and_transportless_services_never_fetch() {
    let fake = Fake::new(|_, _| Ok(ok_png("max-age=60")));
    let svc = TileService::with_transport(TileSource::offline(), None, Some(fake.clone()));
    svc.show(&paris());
    let inert = TileService::with_transport(source(), None, None);
    inert.show(&paris());
    std::thread::sleep(Duration::from_millis(50));
    assert!(fake.urls().is_empty());
    assert!(svc.poll().is_empty() && inert.poll().is_empty());
    // the offline source has no cache either
    let svc = TileService::new(TileSource::offline(), Some(tmp("offline")), 1 << 20);
    svc.show(&paris());
    assert!(svc.is_idle());
}

/// The real HTTP client against a localhost server: our User-Agent (once), conditional headers,
/// no `no-cache`, 304s, and bodies over the cap refused.
#[test]
fn the_http_transport_sends_the_right_headers_and_caps_bodies() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let tile = png(256, 256);
    let body = tile.clone();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for i in 0..4 {
            let (mut s, _) = listener.accept().unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = Vec::new();
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap() == 0 || line.trim().is_empty() {
                    break;
                }
                head.push(line.trim().to_string());
            }
            requests.push(head);
            match i {
                0 => {
                    s.write_all(
                        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nCache-Control: max-age=60\r\nETag: \"t\"\r\n\r\n", body.len()).as_bytes(),
                    )
                    .unwrap();
                    s.write_all(&body).unwrap();
                }
                1 => s.write_all(b"HTTP/1.1 304 Not Modified\r\nCache-Control: max-age=60\r\n\r\n").unwrap(),
                2 => s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 3000000\r\n\r\n").unwrap(),
                _ => {
                    // chunked, past the cap without announcing it
                    s.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
                    let chunk = vec![b'x'; 64 * 1024];
                    for _ in 0..40 {
                        if s.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).is_err()
                            || s.write_all(&chunk).is_err()
                            || s.write_all(b"\r\n").is_err()
                        {
                            break;
                        }
                    }
                }
            }
        }
        requests
    });
    let t = HttpTransport::default();
    let cancel = AtomicBool::new(false);
    let ua = vec![("User-Agent", USER_AGENT.to_string())];
    let r = t.get(&format!("{base}/1/0/0.png"), &ua, MAX_TILE_BYTES, &cancel).unwrap();
    assert_eq!((r.status, r.body.len(), r.header("etag")), (200, tile.len(), Some("\"t\"")));
    assert!(decode_tile(&r.body).is_ok());
    let mut cond = ua.clone();
    cond.extend(TileMeta { etag: Some("\"t\"".into()), ..Default::default() }.conditional_headers());
    let r = t.get(&format!("{base}/1/0/0.png"), &cond, MAX_TILE_BYTES, &cancel).unwrap();
    assert_eq!((r.status, r.body.len()), (304, 0));
    assert!(t.get(&format!("{base}/1/0/1.png"), &ua, MAX_TILE_BYTES, &cancel).unwrap_err().contains("larger"));
    assert!(t.get(&format!("{base}/1/1/1.png"), &ua, MAX_TILE_BYTES, &cancel).unwrap_err().contains("larger"));
    let requests = server.join().unwrap();
    for head in &requests {
        let agents: Vec<_> = head.iter().filter(|l| l.to_ascii_lowercase().starts_with("user-agent:")).collect();
        assert_eq!(agents, vec![&format!("User-Agent: {USER_AGENT}")], "{head:?}");
        assert!(
            !head.iter().any(|l| l.to_ascii_lowercase().starts_with("pragma") || l.to_ascii_lowercase().starts_with("cache-control")),
            "{head:?}"
        );
    }
    assert!(requests[1].contains(&"If-None-Match: \"t\"".to_string()));
}
