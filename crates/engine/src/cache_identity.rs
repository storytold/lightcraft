use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use lightcraft_catalog::{Photo, Source};
use lightcraft_preview::{Hash128, Hasher128};

#[derive(Clone)]
pub struct CacheIdentity(Arc<RwLock<State>>);

struct State {
    invalid: HashMap<Hash128, Option<PhysicalIdentity>>,
    epochs: HashMap<String, u64>,
    pending: HashSet<String>,
    dir: Option<PathBuf>,
    uncertain: bool,
    active: bool,
    nonce: u128,
}

impl Default for CacheIdentity {
    fn default() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map_or(0, |t| t.as_nanos())
            ^ u128::from(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        Self(Arc::new(RwLock::new(State {
            invalid: HashMap::new(),
            epochs: HashMap::new(),
            pending: HashSet::new(),
            dir: None,
            uncertain: false,
            active: true,
            nonce,
        })))
    }
}

pub fn path_key(p: &Photo) -> Option<String> {
    match &p.source {
        Source::File { path } => Some(format!("file:{path}:{}", p.file_size)),
        _ => None,
    }
}

fn alias_hash(key: &str) -> Hash128 {
    Hasher128::new().str(key).finish()
}

#[cfg(all(test, unix))]
thread_local! { static DIRECTORY_FAILURE: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) }; }

#[cfg(all(test, unix))]
pub(crate) struct DirectoryFault(Option<usize>);
#[cfg(all(test, unix))]
impl Drop for DirectoryFault {
    fn drop(&mut self) {
        DIRECTORY_FAILURE.set(self.0);
    }
}
#[cfg(all(test, unix))]
pub(crate) fn fail_directory_after(steps: usize) -> DirectoryFault {
    DirectoryFault(DIRECTORY_FAILURE.replace(Some(steps)))
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PhysicalIdentity {
    source: String,
    width: u32,
    height: u32,
    file_size: u64,
    content_hash: String,
    preview_only: Option<String>,
}

impl PhysicalIdentity {
    pub(crate) fn of(p: &Photo) -> Option<Self> {
        let Source::File { path } = &p.source else { return None };
        Some(Self {
            source: path.clone(),
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: crate::sync::base_hash(p.content_hash.as_deref()?).to_string(),
            preview_only: p.preview_only.clone(),
        })
    }
}

const MAX_MARKER_BYTES: usize = 16 << 10;

fn read_expected(path: &Path) -> (Option<PhysicalIdentity>, usize) {
    let read = || -> std::io::Result<Vec<u8>> {
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_file() || meta.len() > MAX_MARKER_BYTES as u64 {
            return Err(std::io::Error::other("unreadable source invalidation"));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take((MAX_MARKER_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let Ok(bytes) = read() else { return (None, 0) };
    let expected = if bytes.len() <= MAX_MARKER_BYTES {
        bytes.strip_prefix(b"LCI1\n").and_then(|body| serde_json::from_slice::<Option<PhysicalIdentity>>(body).ok()).flatten()
    } else {
        None
    };
    (expected, bytes.len())
}

impl CacheIdentity {
    pub fn open(dir: &Path) -> Self {
        let cache = Self::default();
        let mut state = cache.0.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.dir = Some(dir.to_path_buf());
        let mut loaded_bytes = 0usize;
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for entry in entries.take(1_000_001) {
                    let Some(name) = entry.ok().and_then(|e| e.file_name().to_str().map(str::to_owned)) else {
                        state.uncertain = true;
                        continue;
                    };
                    if let Some(hash) = name.strip_suffix(".invalid").and_then(|s| u128::from_str_radix(s, 16).ok()) {
                        let (expected, bytes) = read_expected(&dir.join(name));
                        loaded_bytes = loaded_bytes.saturating_add(bytes);
                        if loaded_bytes > 64 << 20 {
                            state.uncertain = true;
                            break;
                        }
                        state.invalid.insert(Hash128(hash), expected);
                    } else {
                        state.uncertain = true;
                    }
                    if state.invalid.len() > 1_000_000 {
                        state.uncertain = true;
                        break;
                    }
                }
                state.uncertain |= state.invalid.is_empty();
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => state.uncertain = true,
        }
        if let Some(parent) = dir.parent() {
            match std::fs::symlink_metadata(parent.join("source-invalidations-enabled")) {
                Ok(meta) => state.uncertain |= !meta.is_file() || state.invalid.is_empty(),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => state.uncertain = true,
            }
        }
        drop(state);
        cache
    }

    pub fn retire(&self) {
        self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner).active = false;
    }

    pub fn keys(&self, p: &Photo) -> Vec<String> {
        let state = self.0.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut keys = crate::media::content_keys(p);
        let pending = matches!(&p.source, Source::File { path } if state.pending.contains(path));
        if let Some(alias) = path_key(p)
            && (pending || state.uncertain || state.invalid.contains_key(&alias_hash(&alias)))
        {
            let expected = state.invalid.get(&alias_hash(&alias)).and_then(Option::as_ref);
            if pending || expected.is_none() || expected != PhysicalIdentity::of(p).as_ref() {
                keys.clear();
            } else {
                keys.retain(|k| k != &alias);
            }
            let epoch = match &p.source {
                Source::File { path } => state.epochs.get(path).copied().unwrap_or(0),
                _ => 0,
            };
            let transient = format!("uncached:{}:{epoch}:{alias}", state.nonce);
            if !keys.contains(&transient) {
                keys.push(transient);
            }
        }
        keys
    }

    pub(crate) fn needs_validation(&self, p: &Photo) -> bool {
        !self.keys(p).contains(&crate::media::content_key(p))
    }

    pub(crate) fn reconcile(&self, p: &Photo, ticket: &SourceTicket, validate: impl FnOnce() -> bool) -> std::io::Result<bool> {
        let mut state = self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !Arc::ptr_eq(&self.0, &ticket.cache.0) || !state.active || state.epochs.get(&ticket.path).copied().unwrap_or(0) != ticket.epoch {
            return Ok(false);
        }
        if !validate() {
            return Ok(false);
        }
        let Some(alias) = path_key(p) else { return Ok(false) };
        let hash = alias_hash(&alias);
        if state.uncertain || state.pending.contains(&ticket.path) || state.invalid.contains_key(&hash) {
            let expected = PhysicalIdentity::of(p);
            state.invalid.insert(hash, expected.clone());
            state.pending.insert(ticket.path.clone());
            if let Some(dir) = &state.dir {
                std::fs::create_dir_all(dir)?;
                if state.uncertain {
                    write_marker(&dir.join("uncertain"), b"source invalidation history unavailable\n")?;
                }
                let mut body = b"LCI1\n".to_vec();
                body.extend(serde_json::to_vec(&expected).map_err(std::io::Error::other)?);
                if body.len() > MAX_MARKER_BYTES {
                    return Err(std::io::Error::other("source cache identity is too long"));
                }
                write_marker(&dir.join(format!("{hash}.invalid")), &body)?;
                if let Some(parent) = dir.parent() {
                    write_marker(&parent.join("source-invalidations-enabled"), b"1\n")?;
                }
            }
            state.pending.remove(&ticket.path);
        }
        Ok(true)
    }

    pub fn tag(&self, p: &Photo, key: &str) -> String {
        let state = self.0.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        let epoch = match &p.source {
            Source::File { path } => state.epochs.get(path).copied().unwrap_or(0),
            _ => 0,
        };
        let path = match &p.source {
            Source::File { path } => path.as_str(),
            _ => "",
        };
        format!("{key}:source-life:{}:source-path:{path}:source-epoch:{epoch}", state.nonce)
    }

    pub fn ticket(&self, p: &Photo) -> SourceTicket {
        let path = match &p.source {
            Source::File { path } => path.clone(),
            _ => String::new(),
        };
        let state = self.0.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        SourceTicket { cache: self.clone(), epoch: state.epochs.get(&path).copied().unwrap_or(0), path }
    }

    pub fn confirm(&self, sources: &[String]) {
        let mut state = self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        for source in sources {
            state.pending.remove(source);
        }
    }

    pub fn invalidate(&self, p: &Photo, expected: Option<PhysicalIdentity>, fence: impl FnOnce()) -> std::io::Result<()> {
        let Some(alias) = path_key(p) else { return Ok(()) };
        let mut state = self.0.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let hash = alias_hash(&alias);
        state.invalid.insert(hash, expected.clone());
        if let Source::File { path } = &p.source {
            state.pending.insert(path.clone());
            let epoch = state.epochs.entry(path.clone()).or_default();
            *epoch = epoch.wrapping_add(1);
        }
        fence();
        if let Some(dir) = &state.dir {
            std::fs::create_dir_all(dir)?;
            if state.uncertain {
                write_marker(&dir.join("uncertain"), b"source invalidation history unavailable\n")?;
            }
            let mut body = b"LCI1\n".to_vec();
            body.extend(serde_json::to_vec(&expected).map_err(std::io::Error::other)?);
            if body.len() > MAX_MARKER_BYTES {
                return Err(std::io::Error::other("source cache identity is too long"));
            }
            write_marker(&dir.join(format!("{hash}.invalid")), &body)?;
            if let Some(parent) = dir.parent() {
                write_marker(&parent.join("source-invalidations-enabled"), b"1\n")?;
            }
        }
        Ok(())
    }
}

fn write_marker(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    lightcraft_catalog::safe_file::write_atomic(path, bytes)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        #[cfg(test)]
        directory_step()?;
        let dir = std::fs::File::open(parent)?;
        #[cfg(test)]
        directory_step()?;
        dir.sync_all()?;
    }
    Ok(())
}

#[cfg(all(test, unix))]
fn directory_step() -> std::io::Result<()> {
    DIRECTORY_FAILURE.with(|fault| match fault.get() {
        Some(0) => Err(std::io::Error::from_raw_os_error(5)),
        Some(n) => {
            fault.set(Some(n - 1));
            Ok(())
        }
        None => Ok(()),
    })
}

#[derive(Clone)]
pub struct SourceTicket {
    cache: CacheIdentity,
    path: String,
    epoch: u64,
}

impl SourceTicket {
    pub fn publish<T>(&self, f: impl FnOnce() -> T) -> Option<T> {
        let state = self.cache.0.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        (state.active && state.epochs.get(&self.path).copied().unwrap_or(0) == self.epoch).then(f)
    }

    pub fn current(&self) -> bool {
        self.publish(|| ()).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f_repair_legacy_or_unknown_marker_body_never_authorizes_primary() {
        for (case, body) in [("legacy", "invalid source alias\n"), ("empty", ""), ("future", "LCI9\nunknown\n")] {
            let dir = crate::tests_xmp::temp_dir(&format!("unresolved-marker-{case}"));
            let markers = dir.join("source-invalidations");
            std::fs::create_dir_all(&markers).unwrap();
            let mut p = Photo::new(lightcraft_catalog::PhotoId(1), Source::File { path: "a.jpg".into() }, "a.jpg", "JPEG", 8, 8, "");
            p.content_hash = Some("old".into());
            let name = format!("{}.invalid", alias_hash(&path_key(&p).unwrap()));
            std::fs::write(markers.join(name), body).unwrap();
            std::fs::write(dir.join("source-invalidations-enabled"), "1\n").unwrap();
            assert!(CacheIdentity::open(&markers).keys(&p).iter().all(|key| key.starts_with("uncached:")), "{case}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn f_repair_uncertain_store_stays_uncertain_after_another_invalidation() {
        for fail_first in [false, true] {
            let dir = crate::tests_xmp::temp_dir(&format!("uncertain-continuity-{fail_first}"));
            let markers = dir.join("source-invalidations");
            let p = Photo::new(lightcraft_catalog::PhotoId(1), Source::File { path: "a.jpg".into() }, "a.jpg", "JPEG", 8, 8, "");
            let mut other = p.clone();
            other.source = Source::File { path: "b.jpg".into() };
            CacheIdentity::open(&markers).invalidate(&p, None, || ()).unwrap();
            std::fs::remove_dir_all(&markers).unwrap();
            std::fs::create_dir(&markers).unwrap();
            let cache = CacheIdentity::open(&markers);
            if fail_first {
                let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
                assert!(cache.invalidate(&other, None, || ()).is_err());
                drop(fault);
                assert_eq!(std::fs::read_dir(&markers).unwrap().count(), 0);
                assert!(!CacheIdentity::open(&markers).keys(&p).contains(&path_key(&p).unwrap()));
            }
            cache.invalidate(&other, None, || ()).unwrap();
            assert!(!CacheIdentity::open(&markers).keys(&p).contains(&path_key(&p).unwrap()), "lost uncertainty after invalidating another photo");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn f_repair_directory_durability_errors_refuse_success_after_rename() {
        for stage in 0..4 {
            let dir = crate::tests_xmp::temp_dir(&format!("directory-durability-{stage}"));
            let markers = dir.join("source-invalidations");
            let p = Photo::new(lightcraft_catalog::PhotoId(1), Source::File { path: "a.jpg".into() }, "a.jpg", "JPEG", 8, 8, "");
            let cache = CacheIdentity::open(&markers);
            let ticket = cache.ticket(&p);
            let _fault = fail_directory_after(stage);
            assert!(cache.invalidate(&p, None, || ()).is_err(), "stage={stage}");
            assert!(!ticket.current());
            let mut copy = p.clone();
            copy.file_size = p.file_size.saturating_add(1);
            copy.content_hash = Some("old".into());
            assert!(cache.keys(&copy).iter().all(|key| key.starts_with("uncached:")), "source quarantine must cover copies with older catalog sizes");
            assert!(markers.join(format!("{}.invalid", alias_hash(&path_key(&p).unwrap()))).is_file(), "rename preceded error");
            assert!(!cache.keys(&p).contains(&path_key(&p).unwrap()));
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn f_repair_empty_marker_store_with_sentinel_stays_closed() {
        for recreate in [false, true] {
            let dir = crate::tests_xmp::temp_dir(&format!("empty-invalidation-{recreate}"));
            let markers = dir.join("source-invalidations");
            let p = Photo::new(lightcraft_catalog::PhotoId(1), Source::File { path: "a.jpg".into() }, "a.jpg", "JPEG", 8, 8, "");
            let cache = CacheIdentity::open(&markers);
            cache.invalidate(&p, None, || ()).unwrap();
            if recreate {
                std::fs::remove_dir_all(&markers).unwrap();
                std::fs::create_dir(&markers).unwrap();
            } else {
                for entry in std::fs::read_dir(&markers).unwrap() {
                    std::fs::remove_file(entry.unwrap().path()).unwrap();
                }
            }
            assert!(!CacheIdentity::open(&markers).keys(&p).contains(&path_key(&p).unwrap()), "recreate={recreate}");
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_missing_marker_store_cannot_revive_a_previously_invalidated_alias() {
        let dir = crate::tests_xmp::temp_dir("missing-invalidation-store");
        let markers = dir.join("source-invalidations");
        let p = Photo::new(lightcraft_catalog::PhotoId(1), Source::File { path: "a.jpg".into() }, "a.jpg", "JPEG", 8, 8, "");
        let cache = CacheIdentity::open(&markers);
        let alias = path_key(&p).unwrap();
        assert!(cache.keys(&p).contains(&alias));
        cache.invalidate(&p, None, || ()).unwrap();
        std::fs::remove_dir_all(&markers).unwrap();
        let reopened = CacheIdentity::open(&markers);
        assert!(!reopened.keys(&p).contains(&alias));
        let _ = std::fs::remove_dir_all(dir);
    }
}
