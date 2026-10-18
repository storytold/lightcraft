use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use lightcraft_catalog::{PhotoId, Source};
use lightcraft_preview::{Hash128, Hasher128};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}
impl Stamp {
    fn of(meta: &Metadata) -> Option<Self> {
        if !meta.is_file() {
            return None;
        }
        Some(Self {
            len: meta.len(),
            modified: meta.modified().ok()?,
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino(), meta.ctime(), meta.ctime_nsec())
            },
        })
    }
}

static READ_HANDLES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[derive(Debug)]
struct ReadProof {
    file: Mutex<Option<File>>,
    #[cfg(windows)]
    identity: (u64, [u8; 16]),
}
impl ReadProof {
    fn new(file: File, share_safe: bool) -> Option<Arc<Self>> {
        if !share_safe {
            return None;
        }
        #[cfg(windows)]
        let identity = physical_identity(&file).ok()?;
        use std::sync::atomic::Ordering::Relaxed;
        let mut count = READ_HANDLES.load(Relaxed);
        loop {
            if count >= 128 {
                return None;
            }
            match READ_HANDLES.compare_exchange_weak(count, count + 1, Relaxed, Relaxed) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        Some(Arc::new(Self {
            file: Mutex::new(Some(file)),
            #[cfg(windows)]
            identity,
        }))
    }
    fn current(&self, path: &str, stamp: &Stamp) -> bool {
        #[cfg(unix)]
        {
            self.matches(stamp) && std::fs::metadata(path).ok().as_ref().and_then(Stamp::of).as_ref() == Some(stamp)
        }
        #[cfg(windows)]
        {
            self.matches_path(path, stamp)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (path, stamp);
            false
        }
    }
    fn release(&self) {
        if self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take().is_some() {
            READ_HANDLES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
    #[cfg(unix)]
    fn matches(&self, stamp: &Stamp) -> bool {
        self.file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|file| file.metadata().ok())
            .as_ref()
            .and_then(Stamp::of)
            .as_ref()
            == Some(stamp)
    }
    #[cfg(windows)]
    fn matches_path(&self, path: &str, stamp: &Stamp) -> bool {
        let held = self.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(held) = held.as_ref() else { return false };
        if held.metadata().ok().as_ref().and_then(Stamp::of).as_ref() != Some(stamp) || physical_identity(held).ok() != Some(self.identity) {
            return false;
        }
        let Ok((current, true)) = open_read(path, true) else { return false };
        current.metadata().ok().as_ref().and_then(Stamp::of).as_ref() == Some(stamp) && physical_identity(&current).ok() == Some(self.identity)
    }
}
impl Drop for ReadProof {
    fn drop(&mut self) {
        self.release();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReaderOrigin(std::sync::Weak<RwLock<State>>);
impl ReaderOrigin {
    fn matches(&self, reader: &Originals) -> bool {
        self.0
            .upgrade()
            .is_some_and(|state| Arc::ptr_eq(&state, &reader.state) && state.read().unwrap_or_else(std::sync::PoisonError::into_inner).active)
    }
}

#[derive(Clone, Debug)]
struct HeaderObservation {
    stamp: Stamp,
    proof: Arc<ReadProof>,
}
impl HeaderObservation {
    fn current(&self, path: &str) -> bool {
        self.proof.current(path, &self.stamp)
    }
    fn same_snapshot(&self, fp: &Fingerprint) -> bool {
        if self.stamp != fp.stamp {
            return false;
        }
        let Some(other) = &fp.proof else { return false };
        if Arc::ptr_eq(&self.proof, other) {
            return true;
        }
        let a = self.proof.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let b = other.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        a.as_ref().zip(b.as_ref()).is_some_and(|(a, b)| same_file_handles(a, b))
    }
}

/// Transient evidence from a native read; never part of the catalog format.
#[derive(Clone, Debug)]
pub struct ProbeEvidence {
    origin: ReaderOrigin,
    path: String,
    fingerprint: Option<Arc<Fingerprint>>,
    metadata_hash: Option<Hash128>,
    header: Option<HeaderObservation>,
    hash: Option<String>,
    size: u64,
    dimensions: (u32, u32),
    preview: Option<String>,
}
impl ProbeEvidence {
    fn same_facts(&self, info: &crate::media::ProbeInfo) -> bool {
        self.size == info.file_size && self.dimensions == (info.width, info.height) && self.preview == info.preview_only
    }
    pub(crate) fn matches(&self, reader: &Originals, path: &str, info: &crate::media::ProbeInfo) -> bool {
        self.origin.matches(reader) && self.path == path && self.hash == info.content_hash && self.same_facts(info)
    }
    pub(crate) fn confirms_hash(&self, reader: &Originals, path: &str, hash: &str) -> bool {
        self.origin.matches(reader)
            && self.path == path
            && self
                .fingerprint
                .as_ref()
                .is_some_and(|fp| self.metadata_hash == Some(fp.hash) && crate::sync::base_hash(hash) == fp.hash.to_string() && fp.current(path))
    }
    pub(crate) fn stale(&self) -> bool {
        self.fingerprint.as_ref().is_some_and(|fp| {
            let live = fp.proof.as_ref().is_some_and(|p| p.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some());
            live && !fp.current(&self.path)
        }) || self.header.as_ref().is_some_and(|header| {
            let live = header.proof.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some();
            live && !header.current(&self.path)
        })
    }
    fn binds(&self, reader: &Originals, path: &str, info: &crate::media::ProbeInfo, fp: &Fingerprint) -> bool {
        self.origin.matches(reader)
            && self.path == path
            && self.same_facts(info)
            && (self.metadata_hash == Some(fp.hash) || self.header.as_ref().is_some_and(|h| h.same_snapshot(fp)))
    }
    pub(crate) fn release(&self) {
        if let Some(fp) = &self.fingerprint {
            fp.release();
        }
        self.release_header();
    }
    fn release_header(&self) {
        if let Some(header) = &self.header {
            header.proof.release();
        }
    }
}
fn evidence(
    reader: &Originals,
    path: &str,
    info: &crate::media::ProbeInfo,
    fp: Option<Fingerprint>,
    metadata_hash: Option<Hash128>,
) -> ProbeEvidence {
    ProbeEvidence {
        origin: reader.origin(),
        path: path.to_owned(),
        fingerprint: fp
            .filter(|fp| {
                metadata_hash == Some(fp.hash)
                    && info.content_hash.as_deref().is_some_and(|h| crate::sync::base_hash(h) == fp.hash.to_string())
                    && fp.len() == info.file_size
            })
            .map(Arc::new),
        metadata_hash,
        header: None,
        hash: info.content_hash.clone(),
        size: info.file_size,
        dimensions: (info.width, info.height),
        preview: info.preview_only.clone(),
    }
}
pub(crate) fn probe_evidence(path: &str, info: &crate::media::ProbeInfo, fp: Option<Fingerprint>) -> Option<ProbeEvidence> {
    let reader = bound_reader()?;
    Some(evidence(&reader, path, info, fp, info.content_hash.as_deref().and_then(Hash128::parse)))
}

pub(crate) struct HeaderRead {
    pub file: File,
    path: String,
    before: Option<Stamp>,
    share_safe: bool,
    reader: Originals,
}
impl HeaderRead {
    pub(crate) fn open(path: &str) -> io::Result<Self> {
        let reader = current_reader()?;
        let (file, share_safe) = open_read(path, true)?;
        let before = file.metadata().ok().as_ref().and_then(Stamp::of);
        Ok(Self { file, path: path.to_owned(), before, share_safe, reader })
    }
    pub(crate) fn finish(self, mut info: crate::media::ProbeInfo) -> io::Result<crate::media::ProbeInfo> {
        let after = self.file.metadata().ok().as_ref().and_then(Stamp::of);
        if self.before != after {
            return Err(io::Error::other("original changed while reading headers"));
        }
        let proof = ReadProof::new(self.file, self.share_safe);
        self.reader.track(&proof);
        let header = self.before.zip(proof).map(|(stamp, proof)| HeaderObservation { stamp, proof });
        let mut native = evidence(&self.reader, &self.path, &info, None, None);
        native.header = header;
        info.native_read = Some(native);
        Ok(info)
    }
}

#[derive(Clone, Debug)]
pub struct Fingerprint {
    pub hash: Hash128,
    stamp: Stamp,
    proof: Option<Arc<ReadProof>>,
    facts: Option<crate::media::ProbeInfo>,
    replaced: bool,
}
impl Fingerprint {
    pub(crate) fn probe_info(&self) -> Option<crate::media::ProbeInfo> {
        self.facts.clone()
    }
    pub fn len(&self) -> u64 {
        self.stamp.len
    }
    pub fn is_empty(&self) -> bool {
        self.stamp.len == 0
    }
    fn observed(&self, path: &str) -> bool {
        std::fs::metadata(path).ok().as_ref().and_then(Stamp::of).as_ref() == Some(&self.stamp)
    }
    pub(crate) fn current(&self, path: &str) -> bool {
        self.proof.as_ref().is_some_and(|proof| proof.current(path, &self.stamp))
    }
    pub(crate) fn release(&self) {
        if let Some(proof) = &self.proof {
            proof.release();
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Migration {
    keys: Vec<String>,
    smart: Option<std::path::PathBuf>,
    denoise: Option<(std::path::PathBuf, String)>,
}
struct Watch {
    migration: Migration,
    ids: Vec<PhotoId>,
    size: u64,
    dimensions: (u32, u32),
    preview: Option<String>,
    known_hash: Option<String>,
    wants: bool,
    quarantined: bool,
    observed: Option<Stamp>,
    ticket: crate::cache_identity::SourceTicket,
}
pub(crate) struct ReadEvent {
    pub path: String,
    pub fingerprint: Fingerprint,
    watch: Arc<Watch>,
}
struct State {
    active: bool,
    wanted: HashMap<String, Arc<Watch>>,
}
#[derive(Clone)]
pub struct Originals {
    state: Arc<RwLock<State>>,
    sink: Arc<Mutex<Vec<ReadEvent>>>,
    candidates: Arc<Mutex<HashMap<String, Fingerprint>>>,
    proofs: Arc<Mutex<Vec<std::sync::Weak<ReadProof>>>>,
    overflow_reported: Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)]
    reads: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    hashes: Arc<std::sync::atomic::AtomicUsize>,
}
impl Default for Originals {
    fn default() -> Self {
        Self {
            state: Arc::new(RwLock::new(State { active: true, wanted: HashMap::new() })),
            sink: Default::default(),
            candidates: Default::default(),
            proofs: Default::default(),
            overflow_reported: Default::default(),
            #[cfg(test)]
            reads: Default::default(),
            #[cfg(test)]
            hashes: Default::default(),
        }
    }
}
thread_local! { static CURRENT: RefCell<Option<Originals>> = const { RefCell::new(None) }; }
thread_local! { static LAST_READ: RefCell<Option<(String, Fingerprint)>> = const { RefCell::new(None) }; }
pub(crate) struct ReadCapture(Option<(String, Fingerprint)>);
impl ReadCapture {
    pub(crate) fn new() -> Self {
        Self(LAST_READ.with(|slot| slot.take()))
    }
    pub(crate) fn fingerprint(&self, path: &str) -> Option<Fingerprint> {
        LAST_READ.with(|slot| slot.borrow().as_ref().filter(|(p, _)| p == path).map(|(_, fp)| fp.clone()))
    }
}
impl Drop for ReadCapture {
    fn drop(&mut self) {
        LAST_READ.with(|slot| slot.replace(self.0.take()));
    }
}
fn remember(path: &str, fp: &Fingerprint) {
    LAST_READ.with(|slot| {
        slot.replace(Some((path.to_owned(), fp.clone())));
    });
}
pub(crate) struct Binding(Option<Originals>);
impl Drop for Binding {
    fn drop(&mut self) {
        CURRENT.with(|slot| {
            slot.replace(self.0.take());
        });
    }
}
impl Originals {
    pub(crate) fn origin(&self) -> ReaderOrigin {
        ReaderOrigin(Arc::downgrade(&self.state))
    }
    pub(crate) fn accepts_origin(&self, origin: &ReaderOrigin) -> bool {
        origin.matches(self)
    }
    pub(crate) fn adopt_probe(&self, s: &crate::Session, id: PhotoId, evidence: &ProbeEvidence) {
        let Some(p) = s.catalog.photo(id) else { return };
        if !evidence.origin.matches(self)
            || !matches!(&p.source, Source::File { path } if path == &evidence.path)
            || (p.width, p.height) != evidence.dimensions
            || p.file_size != evidence.size
            || p.preview_only != evidence.preview
        {
            return;
        }
        evidence.release_header();
        let Some(fp) = evidence.fingerprint.as_deref().filter(|fp| {
            evidence.metadata_hash == Some(fp.hash) && evidence.hash.as_deref().is_some_and(|h| crate::sync::base_hash(h) == fp.hash.to_string())
        }) else {
            return;
        };
        self.report(&evidence.path, fp, self.watch(&evidence.path));
    }
    pub(crate) fn candidate_probe(&self, path: &str, info: &crate::media::ProbeInfo) -> Option<ProbeEvidence> {
        let source = info.native_read.as_ref()?;
        if source.matches(self, path, info) && source.metadata_hash.is_some() {
            return Some(source.clone());
        }
        let fp = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(path).cloned();
        let bound = fp.as_ref().filter(|fp| source.binds(self, path, info, fp)).cloned();
        let hash = bound.as_ref().map(|fp| fp.hash);
        if bound.is_some() {
            source.release_header();
        }
        Some(evidence(self, path, info, bound, hash))
    }
    pub(crate) fn transfer_probe(
        &self,
        source_path: &str,
        source: Option<&ProbeEvidence>,
        path: &str,
        info: &crate::media::ProbeInfo,
        fp: Option<Fingerprint>,
    ) -> ProbeEvidence {
        let source_fp = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(source_path).cloned();
        let fp = fp.or_else(|| self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(path).cloned());
        let hash = fp
            .as_ref()
            .filter(|fp| {
                fp.facts.as_ref().is_some_and(|facts| {
                    (facts.width, facts.height, facts.file_size, &facts.preview_only) == (info.width, info.height, info.file_size, &info.preview_only)
                }) || source.is_none()
                    || source.is_some_and(|source| {
                        source.origin.matches(self)
                            && source.path == source_path
                            && source.same_facts(info)
                            && (source.metadata_hash == Some(fp.hash)
                                || source_fp.as_ref().is_some_and(|read| read.hash == fp.hash && source.binds(self, source_path, info, read)))
                    })
            })
            .map(|fp| fp.hash);
        let native = evidence(self, path, info, fp, hash);
        if let Some(source) = source {
            source.release();
        }
        self.discard_candidate(source_path);
        native
    }
    pub(crate) fn discard_candidate(&self, path: &str) {
        if self.watch_any(path).is_none()
            && let Some(fp) = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(path)
        {
            fp.release();
        }
    }
    pub fn in_scope<T>(&self, f: impl FnOnce() -> T) -> T {
        let _binding = self.bind();
        f()
    }
    pub(crate) fn bind(&self) -> Binding {
        Binding(CURRENT.with(|slot| slot.replace(Some(self.clone()))))
    }
    pub(crate) fn retire(&self) {
        self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner).active = false;
        for proof in self.proofs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain(..).filter_map(|p| p.upgrade()) {
            proof.release();
        }
        self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
    fn track(&self, proof: &Option<Arc<ReadProof>>) {
        let Some(proof) = proof else { return };
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !state.active {
            proof.release();
            return;
        }
        let mut proofs = self.proofs.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        proofs.retain(|weak| weak.upgrade().is_some_and(|proof| proof.file.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()));
        if proofs.len() < 128 && proofs.try_reserve(1).is_ok() {
            proofs.push(Arc::downgrade(proof));
        } else {
            proof.release();
        }
        drop(proofs);
        drop(state);
    }
    pub(crate) fn refresh(&self, s: &crate::Session) {
        let mut state = self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut wanted: HashMap<String, Vec<PhotoId>> = HashMap::new();
        for p in s.catalog.photos().filter(|p| {
            !p.local && !p.deleted && (p.copy_of.is_none() || crate::media::physical_photo(&s.catalog, p.id).is_some_and(|master| master.id == p.id))
        }) {
            let Source::File { path } = &p.source else { continue };
            wanted.entry(path.clone()).or_default().push(p.id);
        }
        let mut next = HashMap::new();
        for (path, mut ids) in wanted {
            ids.sort_unstable();
            let Some(p) = ids.first().and_then(|id| s.catalog.photo(*id)) else { continue };
            let quarantined = s.media.identities.needs_validation(p);
            let wants = p.content_hash.is_none() || quarantined;
            let migration = Migration {
                keys: s.media.content_keys(p),
                smart: s.media.smart_dir.clone(),
                denoise: s.denoise_products_dir().zip(s.denoise.active.as_ref().map(|a| a.fingerprint.clone())),
            };
            let old = state.wanted.get(&path).filter(|w| {
                w.ids == ids
                    && w.size == p.file_size
                    && w.dimensions == (p.width, p.height)
                    && w.preview == p.preview_only
                    && w.known_hash == p.content_hash
                    && w.wants == wants
                    && w.quarantined == quarantined
                    && w.migration == migration
                    && w.ticket.current()
            });
            let watch = old.cloned().unwrap_or_else(|| {
                Arc::new(Watch {
                    migration,
                    ids,
                    size: p.file_size,
                    dimensions: (p.width, p.height),
                    preview: p.preview_only.clone(),
                    known_hash: p.content_hash.clone(),
                    wants,
                    quarantined,
                    observed: wants.then(|| std::fs::metadata(&path).ok().as_ref().and_then(Stamp::of)).flatten(),
                    ticket: s.media.identities.ticket(p),
                })
            });
            next.insert(path, watch);
        }
        state.wanted = next;
    }
    pub(crate) fn drop_membership(&self, ids: &[PhotoId]) {
        self.state.write().unwrap_or_else(std::sync::PoisonError::into_inner).wanted.retain(|_, w| !w.ids.iter().any(|id| ids.contains(id)));
    }
    pub(crate) fn adopt_candidates(&self, s: &crate::Session, ids: &[PhotoId]) {
        for id in ids {
            let Some(p) = s.catalog.photo(*id) else { continue };
            let Source::File { path } = &p.source else { continue };
            let fp = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(path);
            if let Some(fp) =
                fp.filter(|fp| fp.current(path) && p.content_hash.as_deref().is_none_or(|h| crate::sync::base_hash(h) == fp.hash.to_string()))
            {
                self.report(path, &fp, self.watch(path));
            }
        }
    }
    fn watch(&self, path: &str) -> Option<Arc<Watch>> {
        self.watch_any(path).filter(|w| w.wants)
    }
    fn watch_any(&self, path: &str) -> Option<Arc<Watch>> {
        let state = self.state.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active.then(|| state.wanted.get(path).cloned()).flatten()
    }
    fn report(&self, path: &str, fp: &Fingerprint, watch: Option<Arc<Watch>>) {
        let Some(watch) = watch else { return };
        if !fp.current(path) || !watch.ticket.current() {
            return;
        }
        let mut sink = self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if sink.len() < 4096 && sink.try_reserve(1).is_ok() {
            sink.push(ReadEvent { path: path.to_owned(), fingerprint: fp.clone(), watch });
        } else if !self.overflow_reported.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log::warn!("original read hash queue full; a later read can retry learning");
        }
    }
    pub fn read_whole(&self, path: &str) -> io::Result<(Vec<u8>, Option<Fingerprint>)> {
        self.read_buffer(path, false).map(|(bytes, fp, _)| (bytes, fp))
    }
    fn read_buffer(&self, path: &str, force: bool) -> io::Result<(Vec<u8>, Option<Fingerprint>, Option<Hash128>)> {
        let watch = if force { self.watch_any(path) } else { self.watch(path) };
        let (mut file, share_safe) = open_read(path, watch.is_some() || force)?;
        let before = file.metadata().ok().as_ref().and_then(Stamp::of);
        if before.as_ref().is_some_and(|stamp| stamp.len > MAX_ORIGINAL_BYTES as u64) {
            return Err(io::Error::other("original exceeds supported 4 GiB buffer size"));
        }
        let reserve = before.as_ref().and_then(|stamp| usize::try_from(stamp.len).ok()).unwrap_or(0);
        let bytes = read_bounded(&mut file, MAX_ORIGINAL_BYTES, reserve)?;
        #[cfg(test)]
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let hash = if watch.is_some() || force {
            #[cfg(test)]
            self.hashes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Some(lightcraft_preview::hash_bytes(&bytes))
        } else {
            None
        };
        let fp = hash.and_then(|hash| {
            before
                .filter(|b| b.len == bytes.len() as u64 && file.metadata().ok().as_ref().and_then(Stamp::of).as_ref() == Some(b))
                .map(|stamp| {
                    let replaced = watch.as_ref().is_some_and(|w| w.observed.as_ref().is_some_and(|before| before != &stamp));
                    let changed_hash =
                        watch.as_ref().and_then(|w| w.known_hash.as_deref()).is_some_and(|h| crate::sync::base_hash(h) != hash.to_string());
                    let facts = (replaced || changed_hash || watch.as_ref().is_some_and(|w| w.quarantined || w.known_hash.is_none()))
                        .then(|| crate::files::probe_bytes_with(path, &bytes, false).ok())
                        .flatten();
                    Fingerprint { hash, stamp, facts, replaced, proof: ReadProof::new(file, share_safe) }
                })
                .filter(|fp| fp.current(path))
        });
        if let Some(fp) = &fp {
            self.track(&fp.proof);
            remember(path, fp);
            if let Some(watch) = &watch {
                self.migrate(path, fp, watch);
            }
            self.report(path, fp, watch);
        }
        Ok((bytes, fp, hash))
    }
    fn migrate(&self, path: &str, fp: &Fingerprint, watch: &Watch) {
        let hash = fp.hash.to_string();
        if watch.quarantined || fp.replaced || watch.known_hash.as_deref().is_some_and(|h| crate::sync::base_hash(h) != hash) {
            return;
        }
        if fp.facts.as_ref().is_some_and(|facts| {
            facts.file_size != watch.size || (facts.width, facts.height) != watch.dimensions || facts.preview_only != watch.preview
        }) || (watch.known_hash.is_none() && fp.facts.is_none())
        {
            return;
        }
        if let Some(dir) = &watch.migration.smart {
            let target = crate::smart::paths_for_keys(dir, std::slice::from_ref(&hash)).into_iter().next();
            if let Some(target) = target {
                watch.ticket.publish(|| {
                    if !fp.current(path) || crate::smart::is_valid(&target) {
                        return;
                    }
                    let Some(source) = crate::smart::paths_for_keys(dir, &watch.migration.keys).into_iter().find(|p| crate::smart::is_valid(p))
                    else {
                        return;
                    };
                    if let Some(parent) = target.parent()
                        && std::fs::create_dir_all(parent).is_err()
                    {
                        return;
                    }
                    // A hard link publishes an immutable derived product without replacing another writer's file.
                    let _ = std::fs::hard_link(source, target);
                });
            }
        }
        if let Some((dir, model)) = &watch.migration.denoise {
            crate::denoise::migrate_product(dir, model, &watch.migration.keys, &hash, &watch.ticket, || fp.current(path));
        }
    }
    pub fn copy_whole(&self, path: &str, out: &mut impl Write) -> io::Result<Fingerprint> {
        self.copy_stream(path, out, true)
    }
    fn copy_stream(&self, path: &str, out: &mut impl Write, observe: bool) -> io::Result<Fingerprint> {
        let watch = observe.then(|| self.watch(path)).flatten();
        let (mut file, share_safe) = open_read(path, observe)?;
        let before = file.metadata().ok().as_ref().and_then(Stamp::of);
        let mut hash = Hasher128::new();
        let mut buf = vec![0; 1 << 20];
        let mut len = 0u64;
        loop {
            let n = read_retry(&mut file, &mut buf)?;
            if n == 0 {
                break;
            }
            let bytes = buf.get(..n).ok_or_else(|| io::Error::other("invalid file read length"))?;
            out.write_all(bytes)?;
            hash.update(bytes);
            len = len.checked_add(n as u64).ok_or_else(|| io::Error::other("file too large"))?;
        }
        #[cfg(test)]
        self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        self.hashes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let stamp = before
            .filter(|b| b.len == len && file.metadata().ok().as_ref().and_then(Stamp::of).as_ref() == Some(b))
            .ok_or_else(|| io::Error::other("original changed while reading"))?;
        let proof = observe.then(|| ReadProof::new(file, share_safe)).flatten();
        let fp = Fingerprint { hash: hash.finish(), stamp, facts: None, replaced: false, proof };
        if !fp.observed(path) {
            return Err(io::Error::other("original changed while reading"));
        }
        self.track(&fp.proof);
        remember(path, &fp);
        if observe && watch.is_none() && fp.current(path) {
            let mut candidates = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if candidates.len() < 4096 && candidates.try_reserve(1).is_ok() {
                candidates.insert(path.to_owned(), fp.clone());
            }
        }
        self.report(path, &fp, watch);
        Ok(fp)
    }
    pub fn hash_whole(&self, path: &str) -> io::Result<Fingerprint> {
        self.copy_whole(path, &mut io::sink())
    }
    pub(crate) fn drain(&self) -> Vec<ReadEvent> {
        std::mem::take(&mut *self.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }
    pub(crate) fn same_lifecycle(&self, event: &ReadEvent) -> bool {
        self.watch_any(&event.path).is_some_and(|w| Arc::ptr_eq(&w, &event.watch)) && event.watch.ticket.current()
    }
    pub fn reader(&self) -> crate::merge::ByteReader {
        let reader = self.clone();
        Arc::new(move |path| reader.read_whole(path).map(|(b, _)| b).map_err(|e| format!("{path}: {e}")))
    }
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        (self.reads.load(Relaxed), self.hashes.load(Relaxed))
    }
}

pub(crate) fn verify_generated(path: &std::path::Path, data: &[u8]) -> io::Result<Fingerprint> {
    struct Exact<'a> {
        data: &'a [u8],
        at: usize,
    }
    impl Write for Exact<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let end = self.at.checked_add(bytes.len()).ok_or_else(|| io::Error::other("verification offset overflow"))?;
            if self.data.get(self.at..end) != Some(bytes) {
                return Err(io::Error::other("the written file reads back different"));
            }
            self.at = end;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let path = path.to_str().ok_or_else(|| io::Error::other("generated path is not UTF-8"))?;
    let mut out = Exact { data, at: 0 };
    let fp = current_reader()?.copy_whole(path, &mut out)?;
    if out.at != data.len() {
        return Err(io::Error::other("the written file is truncated"));
    }
    Ok(fp)
}
pub(crate) fn same_file_handles(owned: &File, current: &File) -> bool {
    let (Ok(a), Ok(b)) = (owned.metadata(), current.metadata()) else { return false };
    if !a.is_file() || !b.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.ino() != 0 && (a.dev(), a.ino()) == (b.dev(), b.ino())
    }
    #[cfg(windows)]
    {
        physical_identity(owned).ok().zip(physical_identity(current).ok()).is_some_and(|(a, b)| a == b)
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}
pub(crate) fn record_generated(path: &str, bytes: &[u8], mut fp: Fingerprint) -> Result<(), String> {
    if fp.proof.is_some() && !fp.current(path) {
        return Err(format!("{path}: generated file changed after verification; original retained"));
    }
    let mut info = crate::files::probe_bytes_with(path, bytes, false)?;
    info.content_hash = Some(fp.hash.to_string());
    fp.facts = Some(info);
    remember(path, &fp);
    let reader = current_reader().map_err(|e| e.to_string())?;
    if fp.current(path) {
        let mut candidates = reader.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if candidates.len() < 4096 && candidates.try_reserve(1).is_ok() {
            candidates.insert(path.to_owned(), fp);
        }
    }
    Ok(())
}
pub(crate) fn bound_reader() -> Option<Originals> {
    CURRENT.with(|slot| slot.borrow().clone())
}
fn current_reader() -> io::Result<Originals> {
    CURRENT.with(|slot| slot.borrow().clone()).ok_or_else(|| io::Error::other("original read requires a session reader binding"))
}
pub(crate) fn standalone_binding() -> Option<Binding> {
    CURRENT.with(|slot| slot.borrow().is_none()).then(|| Originals::default().bind())
}
pub fn read_whole(path: &str) -> io::Result<Vec<u8>> {
    current_reader()?.read_whole(path).map(|(bytes, _)| bytes)
}
pub(crate) fn read_with_hash(path: &str) -> io::Result<(Vec<u8>, Hash128)> {
    let (bytes, _, hash) = current_reader()?.read_buffer(path, true)?;
    hash.map(|hash| (bytes, hash)).ok_or_else(|| io::Error::other("original fingerprint unavailable"))
}
pub fn hash_whole(path: &str) -> io::Result<Fingerprint> {
    current_reader()?.hash_whole(path)
}
pub fn copy_whole(path: &str, out: &mut impl Write) -> io::Result<Fingerprint> {
    current_reader()?.copy_whole(path, out)
}

const MAX_ORIGINAL_BYTES: usize = u32::MAX as usize;
#[cfg(test)]
thread_local! { static ORIGINAL_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(crate) fn read_bytes() -> u64 {
    ORIGINAL_BYTES.get()
}
#[cfg(all(test, windows))]
thread_local! { static FAIL_ID_QUERY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(windows)]
fn physical_identity(file: &File) -> io::Result<(u64, [u8; 16])> {
    #[cfg(test)]
    if FAIL_ID_QUERY.get() {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "injected unsupported FileIdInfo"));
    }
    lightcraft_file_identity_sys::identity(file)
}

fn open_read(path: &str, observe: bool) -> io::Result<(File, bool)> {
    #[cfg(windows)]
    if observe {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ | FILE_SHARE_DELETE: deny writes while retaining our own rename/unlink path.
        if let Ok(file) = File::options().read(true).share_mode(0x1 | 0x4).open(path) {
            return Ok((file, true));
        }
    }
    let _ = observe;
    Ok((File::open(path)?, cfg!(unix)))
}

fn read_retry(reader: &mut impl Read, out: &mut [u8]) -> io::Result<usize> {
    loop {
        match reader.read(out) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Ok(n) => {
                #[cfg(test)]
                ORIGINAL_BYTES.set(ORIGINAL_BYTES.get().saturating_add(n as u64));
                return Ok(n);
            }
            Err(e) => return Err(e),
        }
    }
}
fn read_bounded(reader: &mut impl Read, limit: usize, reserve: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(reserve.min(limit)).map_err(io::Error::other)?;
    let mut chunk = [0u8; 64 << 10];
    loop {
        let remaining = limit.saturating_sub(bytes.len());
        let take = remaining.min(chunk.len()).max(1);
        let slice = chunk.get_mut(..take).ok_or_else(|| io::Error::other("invalid original read buffer"))?;
        let n = read_retry(reader, slice)?;
        if n == 0 {
            return Ok(bytes);
        }
        if n > remaining {
            return Err(io::Error::other("original exceeds supported buffer size"));
        }
        let data = slice.get(..n).ok_or_else(|| io::Error::other("invalid original read length"))?;
        bytes.try_reserve(n).map_err(io::Error::other)?;
        bytes.extend_from_slice(data);
    }
}

pub(crate) struct NativeHooks {
    loader: crate::media::FileLoader,
    probe: crate::media::FileProbe,
    header: crate::media::FileProbe,
    preview: crate::media::PreviewLoader,
    pair: crate::media::PairLoader,
    bytes: crate::merge::ByteReader,
}
impl NativeHooks {
    fn new(reader: &Originals) -> Self {
        let (loader, probe) = crate::files::fs_hooks();
        let a = reader.clone();
        let loader: crate::media::FileLoader = Arc::new(move |p, e| {
            let _binding = a.bind();
            loader(p, e)
        });
        let a = reader.clone();
        let probe: crate::media::FileProbe = Arc::new(move |p| {
            let _binding = a.bind();
            probe(p)
        });
        let header = crate::files::fs_header_probe();
        let a = reader.clone();
        let header: crate::media::FileProbe = Arc::new(move |p| {
            let _binding = a.bind();
            header(p)
        });
        let preview = crate::files::fs_preview_loader();
        let a = reader.clone();
        let preview: crate::media::PreviewLoader = Arc::new(move |p, e| {
            let _binding = a.bind();
            preview(p, e)
        });
        let pair = crate::files::fs_pair_loader();
        let a = reader.clone();
        let pair: crate::media::PairLoader = Arc::new(move |p, e, s| {
            let _binding = a.bind();
            pair(p, e, s)
        });
        Self { loader, probe, header, preview, pair, bytes: reader.reader() }
    }
    fn install(&self, s: &mut crate::Session, old: Option<&Self>) {
        macro_rules! hook {
            ($field:expr, $name:ident) => {
                if old.is_none_or(|o| $field.as_ref().is_some_and(|f| Arc::ptr_eq(f, &o.$name))) {
                    $field = Some(self.$name.clone());
                }
            };
        }
        hook!(s.media.file_loader, loader);
        hook!(s.media.file_probe, probe);
        hook!(s.media.header_probe, header);
        hook!(s.media.preview_loader, preview);
        hook!(s.media.denoise.loader, pair);
        hook!(s.media.file_bytes, bytes);
    }
}
impl crate::Session {
    pub fn original_reader(&self) -> Originals {
        self.originals.clone()
    }
    pub(crate) fn install_native_hooks(&mut self) {
        self.originals.refresh(self);
        let hooks = NativeHooks::new(&self.originals);
        hooks.install(self, None);
        self.native_hooks = Some(hooks);
    }
    pub(crate) fn reset_originals(&mut self) {
        self.originals.retire();
        self.originals = Originals::default();
        self.originals.refresh(self);
        if let Some(old) = self.native_hooks.take() {
            let hooks = NativeHooks::new(&self.originals);
            hooks.install(self, Some(&old));
            self.native_hooks = Some(hooks);
        }
    }
    pub(crate) fn learn_read_hashes(&mut self) -> crate::Result<()> {
        let mut events = self.originals.drain().into_iter();
        while let Some(event) = events.next() {
            if let Err(error) = self.accept_read_event(&event) {
                self.originals.refresh(self);
                let mut sink = self.originals.sink.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                for event in std::iter::once(event).chain(events) {
                    if sink.len() >= 4096 || sink.try_reserve(1).is_err() {
                        break;
                    }
                    sink.push(event);
                }
                return Err(error);
            }
            event.fingerprint.release();
        }
        Ok(())
    }
    fn accept_read_event(&mut self, event: &ReadEvent) -> crate::Result<()> {
        if !self.originals.same_lifecycle(event) {
            return Ok(());
        }
        if !event.fingerprint.current(&event.path) {
            if let Some(p) = event.watch.ids.first().and_then(|id| self.catalog.photo(*id)).cloned() {
                self.invalidate_cache_source(&p, None, true)?;
                if p.content_hash.is_some() {
                    self.apply_system(lightcraft_catalog::Op::SetContent {
                        id: p.id,
                        width: p.width,
                        height: p.height,
                        file_size: p.file_size,
                        content_hash: None,
                        preview_only: p.preview_only.clone(),
                        preview_checked: p.preview_checked.clone(),
                    })?;
                }
                self.originals.refresh(self);
            }
            return Ok(());
        }
        for id in &event.watch.ids {
            let Some(p) = self.catalog.photo(*id).cloned() else { continue };
            if p.local
                || p.deleted
                || self.physical_photo(p.id).is_none_or(|master| master.id != p.id)
                || !matches!(&p.source, Source::File { path } if path == &event.path)
            {
                continue;
            }
            let hash = event.fingerprint.hash.to_string();
            let same = p.content_hash.as_deref().is_some_and(|h| crate::sync::base_hash(h) == hash);
            let changed = event.fingerprint.replaced || p.content_hash.is_some() && !same;
            if changed && event.fingerprint.facts.is_none() {
                self.invalidate_cache_source(&p, None, true)?;
                continue;
            }
            if !changed && p.file_size != event.fingerprint.len() {
                continue;
            }
            let mut target = (*p).clone();
            if let Some(info) = &event.fingerprint.facts {
                target.width = info.width;
                target.height = info.height;
                target.preview_only = info.preview_only.clone();
                target.preview_checked = lightcraft_catalog::preview_stamp(&info.preview_only);
            }
            target.file_size = event.fingerprint.len();
            target.content_hash = Some(if same {
                p.content_hash.clone().unwrap_or_else(|| hash.clone())
            } else if self.catalog.photos().any(|other| {
                other.id != p.id && other.copy_of.is_none() && other.content_hash.as_deref().is_some_and(|h| crate::sync::base_hash(h) == hash)
            }) {
                format!("{hash}:dup{}", p.id.0)
            } else {
                hash
            });
            let quarantined = self.media.identities.needs_validation(&p);
            let hash_only_reconciliation = quarantined
                && !event.fingerprint.replaced
                && (target.width, target.height, target.file_size, &target.preview_only) == (p.width, p.height, p.file_size, &p.preview_only);
            if !self
                .media
                .identities
                .reconcile(&target, &event.watch.ticket, || event.fingerprint.current(&event.path))
                .map_err(|e| crate::EngineError::Other(format!("can't validate original cache identity: {e}")))?
            {
                continue;
            }
            if event.fingerprint.replaced && (same || p.content_hash.is_none()) {
                self.invalidate_cache_source(&p, crate::cache_identity::PhysicalIdentity::of(&target), true)?;
            }
            let source = target.source.clone();
            if target.content_hash != p.content_hash
                || (target.width, target.height, target.file_size, &target.preview_only) != (p.width, p.height, p.file_size, &p.preview_only)
            {
                let op = lightcraft_catalog::Op::SetContent {
                    id: *id,
                    width: target.width,
                    height: target.height,
                    file_size: target.file_size,
                    content_hash: target.content_hash,
                    preview_only: target.preview_only,
                    preview_checked: target.preview_checked,
                };
                if hash_only_reconciliation {
                    // This source was already retired; its validated read changes bookkeeping, not its lifecycle.
                    self.catalog.apply(op.clone())?;
                    self.pending_log.push(op);
                } else {
                    self.apply_system(op)?;
                }
            }
            self.media.identities.confirm(std::slice::from_ref(&event.path));
            let related: Vec<_> = self.catalog.photos().filter(|other| other.source == source).map(|p| p.id).collect();
            for id in related {
                self.denoise_rebind_learned(id);
            }
            self.originals.refresh(self);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn g_completed_copy_releases_unowned_source_proofs() {
        let dir = crate::tests_xmp::temp_dir("g-copy-proof-release");
        let path = dir.join("source.dng");
        std::fs::write(&path, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()], "mode": "copy", "destination": dir.join("out").to_string_lossy(), "organize": "flat"})).unwrap();
        assert!(
            s.originals.candidates.lock().unwrap().get(path.to_str().unwrap()).is_none_or(|fp| !fp.current(path.to_str().unwrap())),
            "a completed Copy must not keep an unowned source read lease until library close"
        );
        #[cfg(windows)]
        assert!(File::options().write(true).open(&path).is_ok(), "source writes must work after owner completion");
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn g_unbound_read_is_an_actionable_error_and_nested_scope_restores() {
        let dir = crate::tests_xmp::temp_dir("g-reader-binding");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        let path = path.to_str().unwrap();
        assert!(read_whole(path).unwrap_err().to_string().contains("binding"));
        let outer = Originals::default();
        let inner = Originals::default();
        {
            let _outer = outer.bind();
            read_whole(path).unwrap();
            {
                let _inner = inner.bind();
                read_whole(path).unwrap();
            }
            read_whole(path).unwrap();
        }
        assert_eq!(outer.counts().0, 2);
        assert_eq!(inner.counts().0, 1);
        assert!(read_whole(path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn g_retirement_revokes_new_proofs_after_released_fingerprints_fill_the_registry() {
        let dir = crate::tests_xmp::temp_dir("g-released-proof-registry");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("original");
        std::fs::write(&path, b"owned proof bytes").unwrap();
        let reader = Originals::default();
        let mut released = Vec::new();
        for _ in 0..128 {
            let fp = reader.hash_whole(path.to_str().unwrap()).unwrap();
            fp.release();
            released.push(fp);
        }
        let current = reader.hash_whole(path.to_str().unwrap()).unwrap();
        assert!(current.current(path.to_str().unwrap()));
        reader.retire();
        assert!(!current.current(path.to_str().unwrap()), "all live proofs are registered even when released Arcs survive");
        assert!(File::options().write(true).open(&path).is_ok());
        drop((current, released));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn g_concurrent_retirement_revokes_reads_registered_on_either_side() {
        let dir = crate::tests_xmp::temp_dir("g-concurrent-proof-retire");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("original");
        std::fs::write(&path, b"owned proof bytes").unwrap();
        for _ in 0..32 {
            let reader = Originals::default();
            let start = Arc::new(std::sync::Barrier::new(3));
            let (r, gate, file) = (reader.clone(), start.clone(), path.clone());
            let read = std::thread::spawn(move || {
                gate.wait();
                r.hash_whole(file.to_str().unwrap()).unwrap()
            });
            let (r, gate) = (reader.clone(), start.clone());
            let retire = std::thread::spawn(move || {
                gate.wait();
                r.retire();
            });
            start.wait();
            let fp = read.join().unwrap();
            retire.join().unwrap();
            assert!(!fp.current(path.to_str().unwrap()));
            assert!(File::options().write(true).open(&path).is_ok());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn g_bounded_original_retries_interrupted_and_refuses_growth() {
        struct InterruptedOnce(bool);
        impl Read for InterruptedOnce {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                let mut bytes: &[u8] = b"growing original";
                bytes.read(buf)
            }
        }
        assert!(read_bounded(&mut InterruptedOnce(false), 8, usize::MAX).unwrap_err().to_string().contains("buffer size"));
        assert_eq!(read_bounded(&mut &b"small"[..], 5, usize::MAX).unwrap(), b"small");
        assert_eq!(read_bounded(&mut &b""[..], 0, usize::MAX).unwrap(), b"");
    }

    #[test]
    fn g_delayed_read_rejects_replacement_even_with_preserved_mtime() {
        let base = crate::tests_xmp::temp_dir("g-delayed-preserved-time");
        std::fs::create_dir_all(&base).unwrap();
        let path = base.join("a.png");
        let image = lightcraft_raster::Rgba8::from_fn(8, 8, |x, y| [x as u8, y as u8, 10, 255]);
        let bytes =
            crate::export::encode_image(&image, &crate::export::ExportOptions { format: crate::export::ExportFormat::Png, ..Default::default() })
                .unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let p = s.catalog.photos().next().unwrap().clone();
        s.apply_system(lightcraft_catalog::Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: None,
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        let (read, fingerprint) = s.originals.read_whole(path.to_str().unwrap()).unwrap();
        assert_eq!(read, bytes);
        #[cfg(not(any(unix, windows)))]
        assert!(fingerprint.is_none(), "unsupported physical identity: do not confirm");
        #[cfg(any(unix, windows))]
        assert!(fingerprint.is_some());
        let replacement = base.join("b.png");
        let mut other = bytes.clone();
        *other.last_mut().unwrap() ^= 1;
        std::fs::write(&replacement, other).unwrap();
        File::options().write(true).open(&replacement).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        s.persist().unwrap();
        assert!(s.catalog.photo(p.id).unwrap().content_hash.is_none());
        drop(s);
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use lightcraft_catalog::Op;
    fn hashless(tag: &str) -> (crate::Session, std::path::PathBuf, PhotoId) {
        let dir = crate::tests_xmp::temp_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("original.dng");
        std::fs::write(&path, crate::tests_xmp::synthetic_dng_with(None, Default::default())).unwrap();
        let mut s = crate::Session::new().with_fs();
        s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let p = s.catalog.photos().next().unwrap().clone();
        s.apply_system(Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: None,
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
        (s, dir, p.id)
    }
    #[test]
    fn g_deleted_and_readded_photo_rejects_old_read_event() {
        let (mut s, dir, id) = hashless("g-read-reimport");
        let p = s.catalog.photo(id).unwrap().clone();
        s.media.file_bytes.as_ref().unwrap()(dir.join("original.dng").to_str().unwrap()).unwrap();
        s.commit("Replace membership", Op::Batch { ops: vec![Op::RemovePhoto { id }, Op::AddPhoto { photo: Box::new((*p).clone()) }] }).unwrap();
        s.persist().unwrap();
        assert!(s.catalog.photo(id).unwrap().content_hash.is_none(), "old physical read belongs to removed photo lifecycle");
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn g_native_worker_read_uses_captured_session_reader() {
        let (mut s, dir, id) = hashless("g-read-worker");
        let read = s.media.file_bytes.clone().unwrap();
        let before = s.originals.counts();
        let path = dir.join("original.dng");
        std::thread::spawn(move || read(path.to_str().unwrap()).unwrap()).join().unwrap();
        s.persist().unwrap();
        assert!(s.catalog.photo(id).unwrap().content_hash.is_some());
        let after = s.originals.counts();
        assert_eq!((after.0 - before.0, after.1 - before.1), (1, 1));
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn g_library_switch_retires_native_worker_read_and_preserves_custom_hook() {
        let (mut s, dir, _) = hashless("g-read-library-switch");
        let old_reader = s.media.file_bytes.clone().unwrap();
        let custom: crate::media::FileLoader = Arc::new(|_, _| Err("custom decoder".into()));
        s.media.file_loader = Some(custom.clone());
        s.open_library(dir.join("new-library"), false).unwrap();
        assert!(s.media.file_loader.as_ref().is_some_and(|loader| Arc::ptr_eq(loader, &custom)));
        let path = dir.join("original.dng");
        s.execute("library.import", &serde_json::json!({"paths": [path.to_string_lossy()]})).unwrap();
        let p = s.catalog.photos().next().unwrap().clone();
        s.apply_system(Op::SetContent {
            id: p.id,
            width: p.width,
            height: p.height,
            file_size: p.file_size,
            content_hash: None,
            preview_only: p.preview_only.clone(),
            preview_checked: p.preview_checked.clone(),
        })
        .unwrap();
        old_reader(path.to_str().unwrap()).unwrap();
        s.persist().unwrap();
        assert!(s.catalog.photo(p.id).unwrap().content_hash.is_none());
        s.media.file_bytes.as_ref().unwrap()(path.to_str().unwrap()).unwrap();
        s.persist().unwrap();
        assert!(s.catalog.photo(p.id).unwrap().content_hash.is_some());
        drop(s);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
impl Originals {
    pub(crate) fn compare_open(&self, a: &str, b: &str, fa: &mut File, fb: &mut File) -> io::Result<bool> {
        #[cfg(windows)]
        let ((mut leased_a, safe_a), (mut leased_b, safe_b)) = (open_read(a, true)?, open_read(b, true)?);
        #[cfg(windows)]
        let safe_a = safe_a && physical_identity(fa).ok().is_some_and(|id| physical_identity(&leased_a).ok() == Some(id));
        #[cfg(windows)]
        let safe_b = safe_b && physical_identity(fb).ok().is_some_and(|id| physical_identity(&leased_b).ok() == Some(id));
        #[cfg(windows)]
        let (fa, fb) = (if safe_a { &mut leased_a } else { fa }, if safe_b { &mut leased_b } else { fb });
        #[cfg(not(windows))]
        let (safe_a, safe_b) = (cfg!(unix), cfg!(unix));
        let (wa, wb) = (self.watch(a), self.watch(b));
        let (sa, sb) = (fa.metadata().ok().as_ref().and_then(Stamp::of), fb.metadata().ok().as_ref().and_then(Stamp::of));
        let (mut ha, mut hb) = (wa.as_ref().map(|_| Hasher128::new()), Some(Hasher128::new()));
        let (mut ba, mut bb) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
        let (mut la, mut lb) = (0u64, 0u64);
        let equal = loop {
            let (na, nb) = (fill_reader(fa, &mut ba)?, fill_reader(fb, &mut bb)?);
            let ca = ba.get(..na).ok_or_else(|| io::Error::other("invalid original read length"))?;
            let cb = bb.get(..nb).ok_or_else(|| io::Error::other("invalid original read length"))?;
            if let Some(h) = &mut ha {
                h.update(ca);
            }
            if let Some(h) = &mut hb {
                h.update(cb);
            }
            la = la.saturating_add(na as u64);
            lb = lb.saturating_add(nb as u64);
            if ca != cb {
                break false;
            }
            if na == 0 {
                break true;
            }
        };
        for (path, file, stamp, hash, len, watch, safe) in [(a, fa, sa, ha, la, wa, safe_a), (b, fb, sb, hb, lb, wb, safe_b)] {
            #[cfg(test)]
            self.reads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(hash) = hash {
                #[cfg(test)]
                self.hashes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if let Some(stamp) = stamp.filter(|s| s.len == len && file.metadata().ok().as_ref().and_then(Stamp::of).as_ref() == Some(s)) {
                    let proof = file.try_clone().ok().and_then(|file| ReadProof::new(file, safe));
                    let fp = Fingerprint { hash: hash.finish(), stamp, proof, facts: None, replaced: false };
                    self.track(&fp.proof);
                    if equal && watch.is_none() && fp.current(path) {
                        let mut candidates = self.candidates.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        if candidates.len() < 4096 && candidates.try_reserve(1).is_ok() {
                            candidates.insert(path.to_owned(), fp.clone());
                        }
                    }
                    self.report(path, &fp, watch);
                }
            }
        }
        Ok(equal)
    }
}
fn fill_reader(reader: &mut impl Read, bytes: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while let Some(rest) = bytes.get_mut(n..).filter(|s| !s.is_empty()) {
        let read = read_retry(reader, rest)?;
        if read == 0 {
            break;
        }
        n = n.checked_add(read).ok_or_else(|| io::Error::other("original read overflow"))?;
    }
    Ok(n)
}
pub(crate) fn compare_open(a: &str, b: &str, fa: &mut File, fb: &mut File) -> io::Result<bool> {
    current_reader()?.compare_open(a, b, fa, fb)
}
#[cfg(all(test, windows))]
pub(crate) fn verification_hash(path: &str) -> io::Result<Hash128> {
    current_reader()?.copy_stream(path, &mut io::sink(), false).map(|fp| fp.hash)
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    fn source(tag: &str) -> (Originals, std::path::PathBuf, String) {
        let dir = crate::tests_xmp::temp_dir(tag);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source");
        std::fs::write(&path, b"same bytes").unwrap();
        (Originals::default(), dir, path.to_string_lossy().into_owned())
    }
    #[test]
    fn retained_read_denies_writes_allows_rename_and_revokes_every_clone() {
        let (reader, dir, path) = source("g-windows-lease");
        let fp = reader.hash_whole(&path).unwrap();
        assert!(fp.current(&path));
        let clone = fp.clone();
        assert!(File::options().write(true).open(&path).is_err());
        let moved = dir.join("moved");
        std::fs::rename(&path, &moved).unwrap();
        assert!(!fp.current(&path));
        assert!(fp.current(moved.to_str().unwrap()));
        assert!(File::options().write(true).open(&moved).is_err());
        reader.retire();
        assert!(!clone.current(moved.to_str().unwrap()));
        assert!(File::options().write(true).open(&moved).is_ok());
        drop((fp, clone));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn existing_writer_and_unsupported_query_preserve_bytes_and_stream_hash_without_proof() {
        let (reader, dir, path) = source("g-windows-unavailable");
        let writer = File::options().write(true).open(&path).unwrap();
        let mut out = Vec::new();
        let fp = reader.copy_whole(&path, &mut out).unwrap();
        assert_eq!(out, b"same bytes");
        assert_eq!(fp.hash, lightcraft_preview::hash_bytes(&out));
        assert!(!fp.current(&path));
        drop(writer);
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                FAIL_ID_QUERY.set(false);
            }
        }
        let reset = Reset;
        FAIL_ID_QUERY.set(true);
        let mut copied = Vec::new();
        let fp = reader.copy_whole(&path, &mut copied).unwrap();
        assert_eq!(copied, out);
        assert!(!fp.current(&path));
        drop(reset);
        assert!(reader.hash_whole(&path).unwrap().current(&path));
        reader.retire();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn destination_verification_remains_compatible_with_the_synced_writer() {
        use std::io::Write;
        let (reader, dir, path) = source("g-windows-destination-verification");
        let mut writer = File::options().write(true).open(&path).unwrap();
        writer.write_all(b"same bytes").unwrap();
        writer.sync_all().unwrap();
        let hash = reader.in_scope(|| verification_hash(&path)).unwrap();
        assert_eq!(hash, lightcraft_preview::hash_bytes(b"same bytes"));
        drop(writer);
        reader.retire();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn full_identity_rejects_a_difference_only_in_the_high_64_bits() {
        let (_, dir, path) = source("g-windows-high-id");
        let (file, safe) = open_read(&path, true).unwrap();
        let mut proof = ReadProof::new(file, safe).unwrap();
        let stamp = Stamp::of(&File::open(&path).unwrap().metadata().unwrap()).unwrap();
        assert!(proof.matches_path(&path, &stamp));
        Arc::get_mut(&mut proof).unwrap().identity.1[15] ^= 1;
        assert!(!proof.matches_path(&path, &stamp));
        drop(proof);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn junction_retarget_rejects_the_old_fingerprint() {
        let dir = crate::tests_xmp::temp_dir("g-windows-junction");
        let (a, b, link) = (dir.join("a"), dir.join("b"), dir.join("junction"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join("source"), b"same bytes").unwrap();
        std::fs::write(b.join("source"), b"same bytes").unwrap();
        let junction = |target: &std::path::Path| {
            let output = std::process::Command::new("cmd").args(["/D", "/C", "mklink", "/J"]).arg(&link).arg(target).output().unwrap();
            assert!(output.status.success(), "junction creation failed: {}", String::from_utf8_lossy(&output.stderr));
        };
        junction(&a);
        let reader = Originals::default();
        let path = link.join("source");
        let fp = reader.hash_whole(path.to_str().unwrap()).unwrap();
        assert!(fp.current(path.to_str().unwrap()));
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        File::options().write(true).open(b.join("source")).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
        std::fs::remove_dir(&link).unwrap();
        junction(&b);
        assert!(!fp.current(path.to_str().unwrap()));
        reader.retire();
        std::fs::remove_dir(&link).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn ancestor_retarget_rejects_equal_size_and_preserved_time() {
        let dir = crate::tests_xmp::temp_dir("g-windows-ancestor");
        let parent = dir.join("parent");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join("source");
        std::fs::write(&path, b"same bytes").unwrap();
        let reader = Originals::default();
        let fp = reader.hash_whole(path.to_str().unwrap()).unwrap();
        let time = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::rename(&parent, dir.join("retired-parent")).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, b"same bytes").unwrap();
        File::options().write(true).open(&path).unwrap().set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
        assert!(!fp.current(path.to_str().unwrap()));
        reader.retire();
        std::fs::remove_dir_all(dir).unwrap();
    }
}
