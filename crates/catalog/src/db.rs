//! Catalog storage v4 (native): a [redb](https://docs.rs/redb) database, `catalog.redb`.
//!
//! The op log (`catalog.log`, see [`crate::journal`]) is unchanged: it is still the undo journal,
//! the change feed and the crash-safe record of every op. What changes is the snapshot: instead
//! of rewriting the whole catalog as one JSON file, a **checkpoint** writes only what changed
//! since the last one (photos whose `Arc` changed, albums, stacks, links and preview entries that
//! differ) in one ACID transaction, together with the `seq` of the last op it holds. Loading reads
//! the tables in parallel and replays the log records after that `seq`.
//!
//! Tables:
//!
//! | table | key → value |
//! |---|---|
//! | `meta` | `format`, `version`, `seq`, `head` (ids counters, label names, browse times) |
//! | `photos` | photo id → photo JSON, develop settings as references ([`crate::settings_ref`]) |
//! | `develop_settings` | settings hash → settings JSON (develop, History steps, Versions; shared) |
//! | `collections` | album id → album JSON (albums, smart albums, folders of albums) |
//! | `stacks` | stack id → stack JSON |
//! | `previews` | photo id → [`PreviewEntry`] JSON |
//! | `remote_identity` | (photo id, service, account) → [`RemoteIdentity`] JSON |
//! | `remote_identity_by_remote` | (service, account, remote id) → photo id |
//! | `folders`, `keywords`, `idx_camera`, `idx_captured_day` | value → photo ids (multimap): secondary indexes (rating and flag have too few values for an index to beat a scan) |
//!
//! [`CatalogDb`] also reads single photos and pages of ids straight from the file (lazily,
//! without loading the catalog), for tools and the paging grid.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use dac_develop::DevelopSettings;
use redb::{Database, MultimapTableDefinition, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};

use crate::remote::{PreviewEntry, RemoteIdentity, RemoteTable};
use crate::settings_ref::{self, Resolver};
use crate::{Album, AlbumId, Catalog, CatalogError, ColorLabel, Photo, PhotoId, Result, Stack, StackId};

pub const DB_FILE: &str = "catalog.redb";
const FORMAT: &str = "dac-catalog-db";

const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");
const PHOTOS: TableDefinition<u64, &[u8]> = TableDefinition::new("photos");
const SETTINGS: TableDefinition<u128, &[u8]> = TableDefinition::new("develop_settings");
const COLLECTIONS: TableDefinition<u64, &[u8]> = TableDefinition::new("collections");
const STACKS: TableDefinition<u64, &[u8]> = TableDefinition::new("stacks");
const PREVIEWS: TableDefinition<u64, &[u8]> = TableDefinition::new("previews");
const REMOTE: TableDefinition<(u64, &str, &str), &[u8]> = TableDefinition::new("remote_identity");
const REMOTE_BY_REMOTE: TableDefinition<(&str, &str, &str), u64> = TableDefinition::new("remote_identity_by_remote");
const FOLDERS: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("folders");
const KEYWORDS: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("keywords");
const IDX_CAMERA: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("idx_camera");
const IDX_DAY: MultimapTableDefinition<&str, u64> = MultimapTableDefinition::new("idx_captured_day");
const STR_INDEXES: [MultimapTableDefinition<&str, u64>; 4] = [FOLDERS, KEYWORDS, IDX_CAMERA, IDX_DAY];

pub(crate) fn dberr(e: impl std::fmt::Display) -> CatalogError {
    let msg = format!("{DB_FILE}: {e}");
    // redb reports a damaged file (`StorageError::Corrupted`, through every wrapping error type)
    // as "DB corrupted: …", a garbled header as "Not a redb database": that is damage the user restores from a backup, not an I/O failure
    if msg.contains("DB corrupted") || msg.contains("Not a redb database") {
        CatalogError::Corrupt(format!("{msg} (restore it from a backup)"))
    } else {
        CatalogError::Io(msg)
    }
}

fn corrupt(what: impl std::fmt::Display) -> CatalogError {
    CatalogError::Corrupt(format!("{DB_FILE}: {what}"))
}

/// The small, catalog-wide part (one record).
#[derive(serde::Serialize, serde::Deserialize, Default, PartialEq, Clone)]
struct Head {
    next_photo: u64,
    next_album: u64,
    next_stack: u64,
    #[serde(default)]
    label_names: BTreeMap<ColorLabel, String>,
    #[serde(default)]
    browsed: BTreeMap<String, String>,
    /// Keywords listed on their own or given attributes (format version 5).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    keyword_list: BTreeMap<String, crate::keywords::ListedKeyword>,
    /// What the library keeps about its folders (format version 5).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    folder_records: BTreeMap<String, crate::FolderRecord>,
    /// The Map module's saved locations (format version 6).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    saved_locations: BTreeMap<String, dac_geo::SavedLocation>,
}

impl Head {
    fn of(c: &Catalog) -> Head {
        Head {
            next_photo: c.next_photo,
            next_album: c.next_album,
            next_stack: c.next_stack,
            label_names: c.label_names.clone(),
            browsed: c.browsed.clone(),
            keyword_list: c.keyword_list.clone(),
            folder_records: c.folder_records.clone(),
            saved_locations: c.saved_locations.clone(),
        }
    }
}

/// A secondary index to look photos up by.
#[derive(Clone, Debug, PartialEq)]
pub enum Index<'a> {
    /// Photos directly in this folder (as [`crate::folder_of`] gives it).
    Folder(&'a str),
    /// A keyword, exactly (case-insensitive).
    Keyword(&'a str),
    /// Camera model, case-insensitive.
    Camera(&'a str),
    /// Capture date prefix (`2026`, `2026-04`, `2026-04-12`).
    Captured(&'a str),
}

/// Index entries of one photo: (table, key).
fn str_keys(p: &Photo) -> Vec<(usize, String)> {
    let mut v = Vec::with_capacity(4 + p.meta.keywords.len());
    if let Some(f) = crate::folder_of(p) {
        v.push((0, f));
    }
    for k in &p.meta.keywords {
        v.push((1, k.to_lowercase()));
    }
    if !p.meta.camera.is_empty() {
        v.push((2, p.meta.camera.to_lowercase()));
    }
    if let Some(day) = p.captured.as_deref().and_then(|c| c.get(..10)) {
        v.push((3, day.to_string()));
    }
    v
}

/// Run `f` over `items` on up to 16 worker threads, in order (on this thread below `min` items).
pub(crate) fn par_map<T: Sync, R: Send>(items: &[T], min: usize, f: impl Fn(&T) -> R + Sync) -> Result<Vec<R>> {
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    if items.len() < min.max(2) || threads == 1 {
        return Ok(items.iter().map(&f).collect());
    }
    let chunk = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = items.chunks(chunk).map(|c| s.spawn(move || c.iter().map(f).collect::<Vec<R>>())).collect();
        let mut out = Vec::with_capacity(items.len());
        for h in handles {
            out.extend(h.join().map_err(|_| CatalogError::Io("catalog worker thread panicked".into()))?);
        }
        Ok(out)
    })
}

/// What the store holds as of the last checkpoint (to find what changed since).
#[derive(Default)]
struct Baseline {
    photos: HashMap<PhotoId, Arc<Photo>>,
    albums: BTreeMap<AlbumId, Album>,
    stacks: BTreeMap<StackId, Stack>,
    previews: BTreeMap<PhotoId, PreviewEntry>,
    remote: RemoteTable,
    head: Head,
}

/// The v4 store of a library. The file is opened for each load, checkpoint or read and closed
/// right after (cheap for redb): a session never holds it between writes, so reopening a library
/// in the same process works as with the JSON snapshot, and one session per library stays the
/// job of [`crate::LibraryLock`]. Checkpoints check a generation number, so a store that another
/// writer changed since is rewritten whole rather than patched.
pub struct CatalogDb {
    path: PathBuf,
    /// The store's generation as of the last load or checkpoint.
    generation: u64,
    /// Settings hashes already in `develop_settings`.
    known: Arc<RwLock<HashSet<u128>>>,
    base: Baseline,
    last: CheckpointStats,
}

/// What a checkpoint wrote.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CheckpointStats {
    pub photos_written: usize,
    pub photos_removed: usize,
    pub settings_written: usize,
    pub encode_ms: f64,
    pub commit_ms: f64,
    /// Size of the store file afterwards.
    pub bytes: u64,
}

fn open_database(path: &Path, create: bool) -> Result<Database> {
    let mut b = Database::builder();
    // redb's own page cache (default 1 GiB): the catalog lives in RAM already
    b.set_cache_size(64 << 20);
    if create { b.create(path) } else { b.open(path) }.map_err(dberr)
}

impl CatalogDb {
    /// Create a new, empty store at `path` (replacing nothing: fails if it exists).
    pub fn create(path: &Path) -> Result<CatalogDb> {
        guarded("create", || CatalogDb::create_inner(path))
    }

    fn create_inner(path: &Path) -> Result<CatalogDb> {
        if path.exists() {
            return Err(CatalogError::Invalid(format!("{} exists", path.display())));
        }
        drop(open_database(path, true)?);
        let mut me = CatalogDb::at(path);
        me.write_tables(&Catalog::new(), 0, true)?;
        Ok(me)
    }

    fn at(path: &Path) -> CatalogDb {
        CatalogDb { path: path.to_path_buf(), generation: 0, known: Arc::default(), base: Baseline::default(), last: CheckpointStats::default() }
    }

    /// Open an existing store (without loading the catalog: see [`CatalogDb::load`]).
    fn open_inner(path: &Path) -> Result<CatalogDb> {
        let mut me = CatalogDb::at(path);
        me.generation = me.check_format()?.1;
        Ok(me)
    }

    /// The file, opened for one operation.
    fn handle(&self) -> Result<Database> {
        open_database(&self.path, false)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Check the store is a catalog this version reads; its `seq` and generation.
    fn check_format(&self) -> Result<(u64, u64)> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let t = r.open_table(META).map_err(dberr)?;
        let format = t.get("format").map_err(dberr)?.map(|v| v.value().to_vec()).unwrap_or_default();
        if format != FORMAT.as_bytes() {
            return Err(corrupt(format!("not a {} catalog", dac_brand::DISPLAY_NAME)));
        }
        let version: u32 =
            t.get("version").map_err(dberr)?.and_then(|v| std::str::from_utf8(v.value()).ok().and_then(|s| s.parse().ok())).unwrap_or(0);
        if version > crate::journal::VERSION {
            return Err(CatalogError::Newer(format!("{DB_FILE} is catalog format v{version}")));
        }
        let seq = t.get("seq").map_err(dberr)?.and_then(|v| std::str::from_utf8(v.value()).ok().and_then(|s| s.parse().ok())).unwrap_or(0);
        let generation =
            t.get("generation").map_err(dberr)?.and_then(|v| std::str::from_utf8(v.value()).ok().and_then(|s| s.parse().ok())).unwrap_or(0);
        Ok((seq, generation))
    }

    /// The `seq` of the last op the store holds.
    pub fn seq(&self) -> Result<u64> {
        self.check_format().map(|(seq, _)| seq)
    }

    /// Load the whole catalog (and remember it as the checkpoint baseline). Returns it, its `seq`
    /// and how many records could not be read (skipped, logged).
    fn load_inner(&mut self) -> Result<(Catalog, u64, usize)> {
        let (seq, generation) = self.check_format()?;
        self.generation = generation;
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let mut bad = 0usize;
        let mut cat = Catalog::new();

        let meta = r.open_table(META).map_err(dberr)?;
        if let Some(h) = meta.get("head").map_err(dberr)? {
            let head: Head = serde_json::from_slice(h.value()).map_err(|e| corrupt(format!("head: {e}")))?;
            cat.next_photo = head.next_photo;
            cat.next_album = head.next_album;
            cat.next_stack = head.next_stack;
            cat.label_names = head.label_names;
            cat.browsed = head.browsed;
            cat.keyword_list = head.keyword_list;
            cat.folder_records = head.folder_records;
            cat.saved_locations = head.saved_locations;
        }

        // settings first: photos refer to them
        let st = r.open_table(SETTINGS).map_err(dberr)?;
        let mut raw: Vec<(u128, Vec<u8>)> = Vec::with_capacity(st.len().map_err(dberr)? as usize);
        for e in st.iter().map_err(dberr)? {
            let (k, v) = e.map_err(dberr)?;
            raw.push((k.value(), v.value().to_vec()));
        }
        let decoded = par_map(&raw, 256, |(h, json)| (*h, serde_json::from_slice::<DevelopSettings>(json)))?;
        drop(raw);
        let mut settings: HashMap<u128, Arc<DevelopSettings>> = HashMap::with_capacity(decoded.len());
        for (h, s) in decoded {
            match s {
                Ok(s) => {
                    settings.insert(h, Arc::new(s));
                }
                Err(e) => {
                    log::warn!("catalog: unreadable develop settings {h:032x}: {e}");
                    bad += 1;
                }
            }
        }
        *self.known.write().unwrap_or_else(std::sync::PoisonError::into_inner) = settings.keys().copied().collect();
        let resolver: Resolver = Arc::new(settings);

        // photos, in batches decoded in parallel (bounded extra memory)
        let pt = r.open_table(PHOTOS).map_err(dberr)?;
        let mut batch: Vec<(u64, Vec<u8>)> = Vec::new();
        let flush = |batch: &mut Vec<(u64, Vec<u8>)>, cat: &mut Catalog, bad: &mut usize| -> Result<()> {
            let res = resolver.clone();
            let out = par_map(batch, 2048, |(id, json)| (*id, settings_ref::decode_scope(res.clone(), || serde_json::from_slice::<Photo>(json))))?;
            for (id, p) in out {
                match p {
                    Ok(p) if p.id.0 == id => {
                        cat.photos.insert(p.id, Arc::new(p));
                    }
                    Ok(_) => {
                        log::warn!("catalog: photo record {id} holds another id; skipped");
                        *bad += 1;
                    }
                    Err(e) => {
                        log::warn!("catalog: unreadable photo record {id}: {e}");
                        *bad += 1;
                    }
                }
            }
            batch.clear();
            Ok(())
        };
        for e in pt.iter().map_err(dberr)? {
            let (k, v) = e.map_err(dberr)?;
            batch.push((k.value(), v.value().to_vec()));
            if batch.len() >= 65_536 {
                flush(&mut batch, &mut cat, &mut bad)?;
            }
        }
        flush(&mut batch, &mut cat, &mut bad)?;

        fn each<V: serde::de::DeserializeOwned>(
            t: &impl ReadableTable<u64, &'static [u8]>,
            what: &str,
            bad: &mut usize,
            mut put: impl FnMut(V),
        ) -> Result<()> {
            for e in t.iter().map_err(dberr)? {
                let (k, v) = e.map_err(dberr)?;
                match serde_json::from_slice::<V>(v.value()) {
                    Ok(x) => put(x),
                    Err(err) => {
                        log::warn!("catalog: unreadable {what} record {}: {err}", k.value());
                        *bad += 1;
                    }
                }
            }
            Ok(())
        }
        each::<Album>(&r.open_table(COLLECTIONS).map_err(dberr)?, "collection", &mut bad, |a| {
            cat.albums.insert(a.id, a);
        })?;
        each::<Stack>(&r.open_table(STACKS).map_err(dberr)?, "stack", &mut bad, |s| {
            cat.stacks.insert(s.id, s);
        })?;
        let pv = r.open_table(PREVIEWS).map_err(dberr)?;
        for e in pv.iter().map_err(dberr)? {
            let (k, v) = e.map_err(dberr)?;
            match serde_json::from_slice::<PreviewEntry>(v.value()) {
                Ok(x) => {
                    cat.previews.insert(PhotoId(k.value()), x);
                }
                Err(_) => bad += 1,
            }
        }
        let rt = r.open_table(REMOTE).map_err(dberr)?;
        let mut links = Vec::new();
        for e in rt.iter().map_err(dberr)? {
            let (_, v) = e.map_err(dberr)?;
            match serde_json::from_slice::<RemoteIdentity>(v.value()) {
                Ok(x) => links.push(x),
                Err(_) => bad += 1,
            }
        }
        cat.remote = links.into_iter().collect();
        cat.next_photo = cat.next_photo.max(cat.photos.keys().next_back().map_or(1, |k| k.0 + 1));
        self.base = Baseline {
            photos: cat.photos.iter().map(|(k, v)| (*k, v.clone())).collect(),
            albums: cat.albums.clone(),
            stacks: cat.stacks.clone(),
            previews: cat.previews.clone(),
            remote: cat.remote.clone(),
            head: Head::of(&cat),
        };
        Ok((cat, seq, bad))
    }

    /// Write everything that changed since the last checkpoint (or load) in one transaction,
    /// as the state after op `seq`.
    fn checkpoint_inner(&mut self, cat: &Catalog, seq: u64) -> Result<CheckpointStats> {
        self.write_tables(cat, seq, false)
    }

    /// Encode photos with settings references; returns the records and the new settings.
    fn encode(&self, photos: &[&Arc<Photo>]) -> Result<(Vec<(u64, Vec<u8>)>, HashMap<u128, Vec<u8>>)> {
        let known = self.known.clone();
        let is_known: Arc<dyn Fn(u128) -> bool + Send + Sync> = Arc::new(move |h| known.read().map(|k| k.contains(&h)).unwrap_or(false));
        // each worker encodes a chunk in its own scope
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
        let chunk = photos.len().div_ceil(threads).max(1024);
        let chunks: Vec<&[&Arc<Photo>]> = photos.chunks(chunk).collect();
        let parts = par_map(&chunks, 2, |c| {
            settings_ref::encode_scope(is_known.clone(), || {
                c.iter().map(|p| serde_json::to_vec(p.as_ref()).map(|j| (p.id.0, j))).collect::<std::result::Result<Vec<_>, _>>()
            })
        })?;
        let mut records = Vec::with_capacity(photos.len());
        let mut settings = HashMap::new();
        for (recs, new) in parts {
            records.extend(recs.map_err(|e| CatalogError::Io(format!("encode photo: {e}")))?);
            settings.extend(new);
        }
        Ok((records, settings))
    }

    fn write_tables(&mut self, cat: &Catalog, seq: u64, mut full: bool) -> Result<CheckpointStats> {
        let t0 = web_time::Instant::now();
        let mut stats = CheckpointStats::default();
        let current = match self.check_format() {
            Ok((_, g)) => g,
            Err(e) if !full => return Err(e),
            Err(_) => 0,
        };
        if !full && current != self.generation {
            // written by someone else since we read it: our baseline says nothing about it
            log::warn!("catalog: {} changed since it was loaded; rewriting it whole", self.path.display());
            full = true;
        }
        if full {
            self.base = Baseline::default();
            if let Ok(mut k) = self.known.write() {
                k.clear();
            }
        }
        // photos that changed (new Arc) or are new, and the ones gone
        let changed: Vec<&Arc<Photo>> = if full {
            cat.photos.values().collect()
        } else {
            cat.photos.iter().filter(|(id, p)| self.base.photos.get(id).is_none_or(|b| !Arc::ptr_eq(b, p))).map(|(_, p)| p).collect()
        };
        let removed: Vec<Arc<Photo>> =
            if full { Vec::new() } else { self.base.photos.iter().filter(|(id, _)| !cat.photos.contains_key(id)).map(|(_, p)| p.clone()).collect() };
        let (records, new_settings) = self.encode(&changed)?;
        stats.encode_ms = t0.elapsed().as_secs_f64() * 1e3;
        let t1 = web_time::Instant::now();

        let db = self.handle()?;
        let txn = db.begin_write().map_err(dberr)?;
        if full {
            // start from empty tables (a rewrite of a store someone else changed)
            for t in [PHOTOS, COLLECTIONS, STACKS, PREVIEWS] {
                txn.delete_table(t).map_err(dberr)?;
            }
            txn.delete_table(SETTINGS).map_err(dberr)?;
            txn.delete_table(REMOTE).map_err(dberr)?;
            txn.delete_table(REMOTE_BY_REMOTE).map_err(dberr)?;
            for t in STR_INDEXES {
                txn.delete_multimap_table(t).map_err(dberr)?;
            }
        }
        let generation = current.wrapping_add(1);
        {
            let mut meta = txn.open_table(META).map_err(dberr)?;
            meta.insert("format", FORMAT.as_bytes()).map_err(dberr)?;
            meta.insert("version", crate::journal::VERSION.to_string().as_bytes()).map_err(dberr)?;
            meta.insert("seq", seq.to_string().as_bytes()).map_err(dberr)?;
            meta.insert("generation", generation.to_string().as_bytes()).map_err(dberr)?;
            let head = Head::of(cat);
            if full || head != self.base.head {
                let j = serde_json::to_vec(&head).map_err(|e| CatalogError::Io(e.to_string()))?;
                meta.insert("head", j.as_slice()).map_err(dberr)?;
            }

            let mut st = txn.open_table(SETTINGS).map_err(dberr)?;
            for (h, json) in &new_settings {
                st.insert(*h, json.as_slice()).map_err(dberr)?;
            }
            stats.settings_written = new_settings.len();

            let mut pt = txn.open_table(PHOTOS).map_err(dberr)?;
            let mut tables = Vec::with_capacity(4);
            for def in STR_INDEXES {
                tables.push(txn.open_multimap_table(def).map_err(dberr)?);
            }
            let unindex = |p: &Photo, tables: &mut Vec<redb::MultimapTable<&str, u64>>| -> Result<()> {
                for (def, k) in str_keys(p) {
                    if let Some(t) = tables.get_mut(def) {
                        t.remove(k.as_str(), p.id.0).map_err(dberr)?;
                    }
                }
                Ok(())
            };
            for p in &removed {
                pt.remove(p.id.0).map_err(dberr)?;
                unindex(p, &mut tables)?;
            }
            for p in &changed {
                if let Some(old) = self.base.photos.get(&p.id).cloned() {
                    unindex(&old, &mut tables)?;
                }
            }
            let report = crate::progress::active();
            let total = records.len() as u64;
            for (i, ((id, json), p)) in records.iter().zip(&changed).enumerate() {
                if report && i % 4096 == 0 {
                    crate::progress::phase(crate::progress::MigrationPhase::Writing, i as u64, total);
                }
                pt.insert(*id, json.as_slice()).map_err(dberr)?;
                for (def, k) in str_keys(p) {
                    if let Some(t) = tables.get_mut(def) {
                        t.insert(k.as_str(), p.id.0).map_err(dberr)?;
                    }
                }
            }
            stats.photos_written = records.len();
            stats.photos_removed = removed.len();

            sync_map(&mut txn.open_table(COLLECTIONS).map_err(dberr)?, &self.base.albums, &cat.albums, |k| k.0, full)?;
            sync_map(&mut txn.open_table(STACKS).map_err(dberr)?, &self.base.stacks, &cat.stacks, |k| k.0, full)?;
            sync_map(&mut txn.open_table(PREVIEWS).map_err(dberr)?, &self.base.previews, &cat.previews, |k| k.0, full)?;

            let mut rt = txn.open_table(REMOTE).map_err(dberr)?;
            let mut rr = txn.open_table(REMOTE_BY_REMOTE).map_err(dberr)?;
            if full || self.base.remote != cat.remote {
                for old in self.base.remote.iter() {
                    if cat.remote.get(&old.key()) != Some(old) {
                        rt.remove((old.photo_id.0, old.service.as_str(), old.account_id.as_str())).map_err(dberr)?;
                        rr.remove((old.service.as_str(), old.account_id.as_str(), old.remote_id.as_str())).map_err(dberr)?;
                    }
                }
                for new in cat.remote.iter() {
                    if full || self.base.remote.get(&new.key()) != Some(new) {
                        let j = serde_json::to_vec(new).map_err(|e| CatalogError::Io(e.to_string()))?;
                        rt.insert((new.photo_id.0, new.service.as_str(), new.account_id.as_str()), j.as_slice()).map_err(dberr)?;
                        rr.insert((new.service.as_str(), new.account_id.as_str(), new.remote_id.as_str()), new.photo_id.0).map_err(dberr)?;
                    }
                }
            }
        }
        txn.commit().map_err(dberr)?;
        drop(db);
        self.generation = generation;
        stats.bytes = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        stats.commit_ms = t1.elapsed().as_secs_f64() * 1e3;

        // the store now holds `cat`
        if let Ok(mut k) = self.known.write() {
            k.extend(new_settings.keys().copied());
        }
        for p in &removed {
            self.base.photos.remove(&p.id);
        }
        for p in &changed {
            self.base.photos.insert(p.id, (*p).clone());
        }
        self.base.albums.clone_from(&cat.albums);
        self.base.stacks.clone_from(&cat.stacks);
        self.base.previews.clone_from(&cat.previews);
        self.base.remote.clone_from(&cat.remote);
        self.base.head = Head::of(cat);
        self.last = stats;
        Ok(stats)
    }

    /// Replace the whole content with `cat` (a fresh file next to this one, renamed over it): the
    /// optimise step, which also drops develop settings nothing refers to any more and rebuilds
    /// every index. On an error the old file is left as it was.
    pub fn rewrite(&mut self, cat: &Catalog, seq: u64) -> Result<()> {
        let tmp = self.path.with_extension("redb.rewrite");
        let _ = std::fs::remove_file(&tmp);
        let written = CatalogDb::create(&tmp).and_then(|mut fresh| guarded("rewrite", || fresh.write_tables(cat, seq, true)).map(|_| fresh));
        let fresh = match written.and_then(|f| {
            std::fs::rename(&tmp, &self.path).map(|()| f).map_err(|e| CatalogError::Io(format!("replace {}: {e}", self.path.display())))
        }) {
            Ok(f) => f,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        let CatalogDb { generation, known, base, last, .. } = fresh;
        (self.generation, self.known, self.base, self.last) = (generation, known, base, last);
        Ok(())
    }

    /// redb's own consistency check (checksums of every page). `Ok(true)`: intact.
    fn check_integrity_inner(&mut self) -> Result<bool> {
        self.handle()?.check_integrity().map_err(dberr)
    }

    /// Give free pages back to the file system.
    fn compact_inner(&mut self) -> Result<bool> {
        self.handle()?.compact().map_err(dberr)
    }

    // ---- lazy reads straight from the file

    /// Number of photos stored.
    fn photo_count_inner(&self) -> Result<u64> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        r.open_table(PHOTOS).map_err(dberr)?.len().map_err(dberr)
    }

    /// One photo, read from the file (its develop settings resolved).
    fn read_photo_inner(&self, id: PhotoId) -> Result<Option<Photo>> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let Some(v) = r.open_table(PHOTOS).map_err(dberr)?.get(id.0).map_err(dberr)? else { return Ok(None) };
        let value: serde_json::Value = serde_json::from_slice(v.value()).map_err(|e| corrupt(format!("photo {}: {e}", id.0)))?;
        // the settings it refers to
        let mut refs = Vec::new();
        let mut take = |v: Option<&serde_json::Value>| {
            if let Some(s) = v.and_then(|v| v.as_str()).and_then(|s| u128::from_str_radix(s, 16).ok()) {
                refs.push(s);
            }
        };
        take(value.get("develop"));
        take(value.get("import_look"));
        for key in ["history", "versions"] {
            for step in value.get(key).and_then(|v| v.as_array()).into_iter().flatten() {
                take(step.get("settings"));
            }
        }
        let st = r.open_table(SETTINGS).map_err(dberr)?;
        let mut map = HashMap::new();
        for h in refs {
            if let Some(j) = st.get(h).map_err(dberr)? {
                let s: DevelopSettings = serde_json::from_slice(j.value()).map_err(|e| corrupt(format!("settings {h:032x}: {e}")))?;
                map.insert(h, Arc::new(s));
            }
        }
        let p = settings_ref::decode_scope(Arc::new(map), || serde_json::from_value::<Photo>(value))
            .map_err(|e| corrupt(format!("photo {}: {e}", id.0)))?;
        Ok(Some(p))
    }

    /// Photo ids in id order after `after` (`None`: from the start), at most `limit`: pages for a
    /// grid that loads photos as they scroll into view.
    fn page_ids_inner(&self, after: Option<PhotoId>, limit: usize) -> Result<Vec<PhotoId>> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let t = r.open_table(PHOTOS).map_err(dberr)?;
        let start = after.map_or(0, |a| a.0.saturating_add(1));
        let mut out = Vec::with_capacity(limit.min(1 << 16));
        for e in t.range(start..).map_err(dberr)?.take(limit) {
            out.push(PhotoId(e.map_err(dberr)?.0.value()));
        }
        Ok(out)
    }

    /// Photos by a secondary index, in id order.
    fn ids_where_inner(&self, index: Index<'_>) -> Result<Vec<PhotoId>> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let mut out = Vec::new();
        let mut collect = |vals: redb::MultimapValue<'static, u64>| -> Result<()> {
            for v in vals {
                out.push(PhotoId(v.map_err(dberr)?.value()));
            }
            Ok(())
        };
        match index {
            Index::Folder(f) => collect(r.open_multimap_table(FOLDERS).map_err(dberr)?.get(f).map_err(dberr)?)?,
            Index::Keyword(k) => collect(r.open_multimap_table(KEYWORDS).map_err(dberr)?.get(k.to_lowercase().as_str()).map_err(dberr)?)?,
            Index::Camera(c) => collect(r.open_multimap_table(IDX_CAMERA).map_err(dberr)?.get(c.to_lowercase().as_str()).map_err(dberr)?)?,
            Index::Captured(prefix) => {
                let t = r.open_multimap_table(IDX_DAY).map_err(dberr)?;
                for e in t.range(prefix..).map_err(dberr)? {
                    let (k, vals) = e.map_err(dberr)?;
                    if !k.value().starts_with(prefix) {
                        break;
                    }
                    collect(vals)?;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// The photo linked to a remote asset, read from the file.
    fn photo_of_remote_inner(&self, service: &str, account_id: &str, remote_id: &str) -> Result<Option<PhotoId>> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let t = r.open_table(REMOTE_BY_REMOTE).map_err(dberr)?;
        Ok(t.get((service, account_id, remote_id)).map_err(dberr)?.map(|v| PhotoId(v.value())))
    }

    /// A photo's remote links, read from the file.
    fn remote_of_photo_inner(&self, id: PhotoId) -> Result<Vec<RemoteIdentity>> {
        let db = self.handle()?;
        let r = db.begin_read().map_err(dberr)?;
        let t = r.open_table(REMOTE).map_err(dberr)?;
        let mut out = Vec::new();
        for e in t.range((id.0, "", "")..).map_err(dberr)? {
            let (k, v) = e.map_err(dberr)?;
            if k.value().0 != id.0 {
                break;
            }
            out.push(serde_json::from_slice(v.value()).map_err(|e| corrupt(format!("remote link: {e}")))?);
        }
        Ok(out)
    }
}

/// redb panics on some damaged files (an index out of range while reading its allocator state,
/// for one): every call into it goes through here, so a damaged catalog is an error the user can
/// act on (restore a backup), never a crash.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(CatalogError::Corrupt(format!("{DB_FILE}: {what} failed: the store is damaged (restore it from a backup)"))))
}

/// The public face of [`CatalogDb`]: each call guarded (see [`guarded`]).
impl CatalogDb {
    /// Open an existing store (without loading the catalog: see [`CatalogDb::load`]).
    pub fn open(path: &Path) -> Result<CatalogDb> {
        guarded("open", || CatalogDb::open_inner(path))
    }
    /// Load the whole catalog (and remember it as the checkpoint baseline). Returns it, its `seq`
    /// and how many records could not be read (skipped, logged).
    pub fn load(&mut self) -> Result<(Catalog, u64, usize)> {
        guarded("load", || self.load_inner())
    }
    /// Write everything that changed since the last checkpoint (or load) in one transaction,
    /// as the state after op `seq`.
    pub fn checkpoint(&mut self, cat: &Catalog, seq: u64) -> Result<CheckpointStats> {
        guarded("checkpoint", || self.checkpoint_inner(cat, seq))
    }
    /// redb's own consistency check (checksums of every page). `Ok(true)`: intact.
    pub fn check_integrity(&mut self) -> Result<bool> {
        guarded("integrity check", || self.check_integrity_inner())
    }
    /// Give free pages back to the file system.
    pub fn compact(&mut self) -> Result<bool> {
        guarded("compact", || self.compact_inner())
    }
    /// Number of photos stored.
    pub fn photo_count(&self) -> Result<u64> {
        guarded("count", || self.photo_count_inner())
    }
    /// One photo, read from the file (its develop settings resolved).
    pub fn read_photo(&self, id: PhotoId) -> Result<Option<Photo>> {
        guarded("read", || self.read_photo_inner(id))
    }
    /// Photo ids in id order after `after` (`None`: from the start), at most `limit`: pages for a
    /// grid that loads photos as they scroll into view.
    pub fn page_ids(&self, after: Option<PhotoId>, limit: usize) -> Result<Vec<PhotoId>> {
        guarded("page", || self.page_ids_inner(after, limit))
    }
    /// Photos by a secondary index, in id order.
    pub fn ids_where(&self, index: Index<'_>) -> Result<Vec<PhotoId>> {
        guarded("index lookup", || self.ids_where_inner(index))
    }
    /// The photo linked to a remote asset, read from the file.
    pub fn photo_of_remote(&self, service: &str, account_id: &str, remote_id: &str) -> Result<Option<PhotoId>> {
        guarded("remote lookup", || self.photo_of_remote_inner(service, account_id, remote_id))
    }
    /// A photo's remote links, read from the file.
    pub fn remote_of_photo(&self, id: PhotoId) -> Result<Vec<RemoteIdentity>> {
        guarded("remote lookup", || self.remote_of_photo_inner(id))
    }
    /// What the last checkpoint wrote.
    pub fn last_checkpoint(&self) -> CheckpointStats {
        self.last
    }
}

/// Bring a table keyed by id in line with `now` (only the entries that differ from `before`).
fn sync_map<K: Ord + Copy, V: PartialEq + serde::Serialize>(
    t: &mut redb::Table<u64, &[u8]>,
    before: &BTreeMap<K, V>,
    now: &BTreeMap<K, V>,
    key: impl Fn(&K) -> u64,
    full: bool,
) -> Result<()> {
    for k in before.keys() {
        if !now.contains_key(k) {
            t.remove(key(k)).map_err(dberr)?;
        }
    }
    for (k, v) in now {
        if full || before.get(k) != Some(v) {
            let j = serde_json::to_vec(v).map_err(|e| CatalogError::Io(e.to_string()))?;
            t.insert(key(k), j.as_slice()).map_err(dberr)?;
        }
    }
    Ok(())
}
