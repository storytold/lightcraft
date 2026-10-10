//! Tile downloads (native): the cache first, then the server through `dac-net` (which sends the
//! brand's User-Agent). Only tiles on screen are ever requested — no bulk prefetch, as the
//! OpenStreetMap tile usage policy asks. A stale cached tile is returned when the server can't be
//! reached, so the map keeps working offline.

use std::time::Duration;

use dac_net::{Client, ClientConfig};

use crate::GeoError;
use crate::cache::{MAX_TILE_BYTES, TileCache};
use crate::mercator::TileKey;
use crate::tiles::TileServer;

/// Where a tile came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cache,
    Network,
    /// The server failed; an expired cached copy was used.
    StaleCache,
}

#[derive(Debug, Clone)]
pub struct TileFetcher {
    client: Client,
    pub cache: Option<TileCache>,
    /// Never touch the network (cache only).
    pub offline: bool,
}

impl TileFetcher {
    pub fn new(cache: Option<TileCache>) -> Result<TileFetcher, GeoError> {
        let client = Client::new(ClientConfig {
            connect_timeout: Duration::from_secs(8),
            stall_timeout: Duration::from_secs(15),
            max_body: MAX_TILE_BYTES,
            ..ClientConfig::default()
        })
        .map_err(|e| GeoError::Net(e.to_string()))?;
        Ok(TileFetcher { client, cache, offline: false })
    }

    /// One tile's encoded image (PNG / JPEG / WebP).
    pub fn fetch(&self, server: &TileServer, k: TileKey) -> Result<(Vec<u8>, Source), GeoError> {
        if k.z > server.max_zoom {
            return Err(GeoError::Invalid(format!("{} stops at zoom {}", server.name, server.max_zoom)));
        }
        let cached = self.cache.as_ref().and_then(|c| c.get(&server.id, k));
        let stale = match cached {
            Some(c) if !c.stale => return Ok((c.bytes, Source::Cache)),
            Some(c) => Some(c.bytes),
            None => None,
        };
        if self.offline {
            return stale.map(|b| (b, Source::StaleCache)).ok_or_else(|| GeoError::Net("offline: tile not cached".into()));
        }
        let got = self
            .client
            .get(&server.tile_url(k))
            .header("Accept", "image/png,image/jpeg,image/webp,image/*;q=0.8")
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes());
        match got {
            Ok(bytes) if !bytes.is_empty() => {
                if let Some(c) = &self.cache
                    && let Err(e) = c.put(&server.id, k, &bytes)
                {
                    log::debug!("tile cache: {e}");
                }
                Ok((bytes, Source::Network))
            }
            Ok(_) => stale.map(|b| (b, Source::StaleCache)).ok_or_else(|| GeoError::Net("the tile server sent an empty tile".into())),
            Err(e) => stale.map(|b| (b, Source::StaleCache)).ok_or_else(|| GeoError::Net(format!("{}: {e}", server.name))),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use super::*;

    /// A fake tile server on 127.0.0.1: answers `n` requests with the path as the body (or 500).
    pub(crate) fn fake_server(n: usize, fail: bool) -> (u16, Arc<Mutex<Vec<String>>>, std::thread::JoinHandle<()>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        let h = std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((mut s, _)) = l.accept() else { return };
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut ua = String::new();
                loop {
                    let mut h = String::new();
                    r.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if h.to_ascii_lowercase().starts_with("user-agent:") {
                        ua = h[11..].trim().to_string();
                    }
                }
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                s2.lock().unwrap().push(format!("{path} {ua}"));
                let resp = if fail {
                    "HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n".to_string()
                } else {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{path}", path.len())
                };
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (port, seen, h)
    }

    #[test]
    fn fetches_through_cache_and_survives_outages() {
        let dir = std::env::temp_dir().join(format!("dac-geo-fetch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (port, seen, h) = fake_server(1, false);
        let server =
            TileServer::custom("fake", "Fake", &format!("http://127.0.0.1:{port}/{{z}}/{{x}}/{{y}}.png"), "test", 19, Default::default()).unwrap();
        let mut f = TileFetcher::new(Some(TileCache::new(&dir))).unwrap();
        let k = TileKey { z: 2, x: 1, y: 3 };
        assert_eq!(f.fetch(&server, k).unwrap(), (b"/2/1/3.png".to_vec(), Source::Network));
        h.join().unwrap();
        let req = seen.lock().unwrap()[0].clone();
        assert!(req.starts_with("/2/1/3.png "), "{req}");
        assert!(req.contains('/'), "a User-Agent is sent: {req}");
        // second time: from the cache, no request
        assert_eq!(f.fetch(&server, k).unwrap().1, Source::Cache);
        // expired + server down → the stale copy
        f.cache.as_mut().unwrap().max_age = Duration::ZERO;
        std::thread::sleep(Duration::from_millis(5));
        let (port2, _, h2) = fake_server(1, true);
        let down = TileServer { url: format!("http://127.0.0.1:{port2}/{{z}}/{{x}}/{{y}}.png"), ..server.clone() };
        assert_eq!(f.fetch(&down, k).unwrap().1, Source::StaleCache);
        h2.join().unwrap();
        // offline, uncached → an error, not a panic
        f.offline = true;
        assert!(f.fetch(&server, TileKey { z: 2, x: 0, y: 0 }).is_err());
        assert!(f.fetch(&server, TileKey { z: 20, x: 0, y: 0 }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
