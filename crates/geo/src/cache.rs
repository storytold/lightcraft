//! The on-disk tile cache: `<dir>/<server id>/<z>/<x>/<y>.tile`, a size limit (oldest files go
//! first) and an expiry (older tiles are refetched, but still shown while offline).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::GeoError;
use crate::mercator::TileKey;

/// Default size limit: 512 MB.
pub const DEFAULT_MAX_BYTES: u64 = 512 * 1024 * 1024;
/// Default expiry: 30 days.
pub const DEFAULT_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);
/// A tile larger than this is not a tile.
pub const MAX_TILE_BYTES: u64 = 4 * 1024 * 1024;

/// A cached tile.
#[derive(Debug, Clone, PartialEq)]
pub struct Cached {
    pub bytes: Vec<u8>,
    /// Older than the expiry: show it, but fetch a fresh copy when online.
    pub stale: bool,
}

#[derive(Debug, Clone)]
pub struct TileCache {
    dir: PathBuf,
    pub max_bytes: u64,
    pub max_age: Duration,
}

impl TileCache {
    pub fn new(dir: impl Into<PathBuf>) -> TileCache {
        TileCache { dir: dir.into(), max_bytes: DEFAULT_MAX_BYTES, max_age: DEFAULT_MAX_AGE }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, server: &str, k: TileKey) -> Option<PathBuf> {
        // the id is validated by TileServer::validate; refuse anything that could leave `dir`
        if server.is_empty() || !server.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return None;
        }
        Some(self.dir.join(server).join(k.z.to_string()).join(k.x.to_string()).join(format!("{}.tile", k.y)))
    }

    pub fn get(&self, server: &str, k: TileKey) -> Option<Cached> {
        let p = self.path(server, k)?;
        let meta = std::fs::metadata(&p).ok()?;
        if meta.len() == 0 || meta.len() > MAX_TILE_BYTES {
            return None;
        }
        let bytes = std::fs::read(&p).ok()?;
        let age = meta.modified().ok().and_then(|m| SystemTime::now().duration_since(m).ok()).unwrap_or_default();
        Some(Cached { bytes, stale: age > self.max_age })
    }

    /// Store a tile (written to a temp file and renamed, so a crash never leaves half a tile).
    pub fn put(&self, server: &str, k: TileKey, bytes: &[u8]) -> Result<(), GeoError> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_TILE_BYTES {
            return Err(GeoError::Invalid("not a tile (empty or too large)".into()));
        }
        let p = self.path(server, k).ok_or_else(|| GeoError::Invalid(format!("bad tile server id `{server}`")))?;
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }

    /// Total bytes and files held.
    pub fn usage(&self) -> (u64, usize) {
        let files = self.files();
        (files.iter().map(|f| f.1).sum(), files.len())
    }

    /// Every tile file: (path, bytes, modified).
    fn files(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![(self.dir.clone(), 0usize)];
        while let Some((d, depth)) = stack.pop() {
            // server/z/x/y: never deeper than 4 levels
            if depth > 4 {
                continue;
            }
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let Ok(m) = e.metadata() else { continue };
                if m.is_dir() {
                    stack.push((e.path(), depth + 1));
                } else if m.is_file() {
                    out.push((e.path(), m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
                }
            }
        }
        out
    }

    /// Delete the oldest tiles until the cache is under `max_bytes`. Returns bytes freed.
    pub fn prune(&self) -> u64 {
        let mut files = self.files();
        let mut total: u64 = files.iter().map(|f| f.1).sum();
        if total <= self.max_bytes {
            return 0;
        }
        files.sort_by_key(|f| f.2);
        let mut freed = 0;
        for (p, len, _) in files {
            if total <= self.max_bytes {
                break;
            }
            if std::fs::remove_file(&p).is_ok() {
                total = total.saturating_sub(len);
                freed += len;
            }
        }
        freed
    }

    /// Remove every cached tile.
    pub fn clear(&self) -> Result<(), GeoError> {
        match std::fs::remove_dir_all(&self.dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dac-geo-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn put_get_prune() {
        let d = tmp("ppp");
        let mut c = TileCache::new(&d);
        let k = TileKey { z: 1, x: 0, y: 1 };
        assert!(c.get("osm", k).is_none());
        c.put("osm", k, b"png").unwrap();
        assert_eq!(c.get("osm", k).unwrap(), Cached { bytes: b"png".to_vec(), stale: false });
        c.max_age = Duration::ZERO;
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get("osm", k).unwrap().stale);
        for y in 0..2 {
            c.put("osm", TileKey { z: 1, x: 1, y }, &[0u8; 100]).unwrap();
        }
        assert_eq!(c.usage(), (203, 3));
        c.max_bytes = 150;
        assert!(c.prune() >= 53);
        assert!(c.usage().0 <= 150);
        // ids that could escape the folder are refused
        assert!(c.put("../x", k, b"a").is_err());
        assert!(c.get("..", k).is_none());
        assert!(c.put("osm", k, b"").is_err());
        c.clear().unwrap();
        assert_eq!(c.usage(), (0, 0));
        c.clear().unwrap();
    }
}
