//! A catalog on disk (native): one folder with an entry-point file, the v4 store, the op log and
//! the catalog's own settings; plus the catalog-level operations around it: new / open / recent,
//! the v3 → v4 migration, backups, the integrity test and optimise.
//!
//! ```text
//! My Catalog/
//!   My Catalog.<catalog_ext>   entry point (what File → Open Catalog picks; JSON, see EntryFile)
//!   catalog.redb               the v4 store (crate::db)
//!   catalog.log                op log: undo journal, change feed, crash safety (crate::journal)
//!   catalog.snap               a stub that makes builds before v4 refuse the folder, untouched
//!   catalog-settings.json      backup schedule, backup folder, last backup
//!   backups/                   default backup folder; `before-v4-…/` keeps the migrated v3 files
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::{CatalogDb, DB_FILE};
use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::store::FsStore;
use crate::{Catalog, CatalogError, Journal, LoadReport, Result, Store};

/// The catalog's own settings file, beside the entry point.
pub const SETTINGS_FILE: &str = "catalog-settings.json";
/// The default backup folder (inside the catalog folder).
pub const BACKUPS_DIR: &str = "backups";

fn ioe(e: std::io::Error) -> CatalogError {
    CatalogError::Io(e.to_string())
}

/// The entry-point file's content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryFile {
    /// [`dac_brand::CATALOG_MAGIC`]: stable across product renames.
    pub magic: String,
    pub version: u32,
    /// The store file, relative to the folder.
    pub store: String,
}

impl Default for EntryFile {
    fn default() -> Self {
        EntryFile { magic: dac_brand::CATALOG_MAGIC.to_string(), version: VERSION, store: DB_FILE.to_string() }
    }
}

/// The entry-point file of the catalog in `dir`: `<dir name>.<catalog_ext>`.
pub fn entry_path(dir: &Path) -> PathBuf {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "catalog".into());
    dir.join(format!("{name}.{}", dac_brand::CATALOG_EXT))
}

/// An entry-point file in `dir` (any name with a current or legacy catalog extension).
pub fn find_entry(dir: &Path) -> Option<PathBuf> {
    let preferred = entry_path(dir);
    if preferred.is_file() {
        return Some(preferred);
    }
    let rd = std::fs::read_dir(dir).ok()?;
    rd.filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| dac_brand::catalog_exts().any(|c| c.eq_ignore_ascii_case(x))))
}

/// The catalog folder for a path the user picked: the entry-point file or the folder itself.
/// A folder must hold an entry point or catalog files.
pub fn resolve(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        let ext_ok = path.extension().and_then(|x| x.to_str()).is_some_and(|x| dac_brand::catalog_exts().any(|c| c.eq_ignore_ascii_case(x)));
        if !ext_ok {
            return Err(CatalogError::Invalid(format!("{} is not a catalog (expected a .{} file)", path.display(), dac_brand::CATALOG_EXT)));
        }
        let bytes = std::fs::read(path).map_err(ioe)?;
        let e: EntryFile = serde_json::from_slice(&bytes).map_err(|e| CatalogError::Corrupt(format!("{}: {e}", path.display())))?;
        if e.magic != dac_brand::CATALOG_MAGIC {
            return Err(CatalogError::Corrupt(format!("{}: not a catalog entry file", path.display())));
        }
        if e.version > VERSION {
            return Err(CatalogError::Newer(format!("{} is catalog format v{}", path.display(), e.version)));
        }
        return path.parent().map(Path::to_path_buf).ok_or_else(|| CatalogError::Invalid("catalog file without a folder".into()));
    }
    if path.is_dir() {
        let known = [DB_FILE, SNAPSHOT, LOG];
        if find_entry(path).is_some() || known.iter().any(|f| path.join(f).exists()) {
            return Ok(path.to_path_buf());
        }
        return Err(CatalogError::Invalid(format!("{} holds no catalog", path.display())));
    }
    Err(CatalogError::Io(format!("{} not found", path.display())))
}

/// Create a new, empty catalog: the folder `parent/name` (must not exist or be empty) with its
/// entry point and store. Returns the entry-point path.
pub fn create(parent: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(CatalogError::Invalid(format!("not a catalog name: {name:?}")));
    }
    let dir = parent.join(name);
    if dir.exists() && std::fs::read_dir(&dir).map_err(ioe)?.next().is_some() {
        return Err(CatalogError::Invalid(format!("{} exists and is not empty", dir.display())));
    }
    std::fs::create_dir_all(&dir).map_err(ioe)?;
    let (j, _, _) = Journal::open(Box::new(FsStore::open(&dir).map_err(ioe)?))?;
    drop(j);
    Ok(entry_path(&dir))
}

/// Open the catalog at `path` (entry point or folder): migrating a v3 library on the way.
pub fn open(path: &Path) -> Result<(Journal, Catalog, LoadReport)> {
    let dir = resolve(path)?;
    Journal::open(Box::new(FsStore::open(&dir).map_err(ioe)?))
}

fn write_entry_if_missing(dir: &Path) {
    if find_entry(dir).is_some() {
        return;
    }
    let body = serde_json::to_vec_pretty(&EntryFile::default()).unwrap_or_default();
    if let Err(e) = crate::safe_file::write_atomic(&entry_path(dir), &body) {
        log::warn!("catalog: can't write the entry point in {}: {e}", dir.display());
    }
}

/// The stub `catalog.snap` of a v4 folder: builds before v4 read its header, see a format they
/// don't know and refuse the folder without touching it.
const STUB: &str = "{\"format\":\"dac-catalog-db\",\"version\":4,\"store\":\"catalog.redb\"}\n";

impl Journal {
    /// Open the library in `dir` with the v4 store, creating it, or migrating a v3 library
    /// (JSON snapshot + log) first: its files are copied to `backups/before-v4-<time>/`, the
    /// store is built in a temporary file and renamed into place only once complete, so a crash
    /// at any point leaves either the v3 library (migrated again next time) or the v4 one.
    pub fn open_v4(mut store: Box<dyn Store>, dir: &Path) -> Result<(Journal, Catalog, LoadReport)> {
        let db_path = dir.join(DB_FILE);
        let mut report = LoadReport::default();
        // (a v3 snapshot can be gigabytes: only its start is looked at, and not kept)
        let snap_state = if db_path.exists() { None } else { store.read(SNAPSHOT).map_err(ioe)?.map(|s| s.starts_with(STUB.trim_end().as_bytes())) };
        let mut db = if db_path.exists() {
            CatalogDb::open(&db_path)?
        } else if snap_state == Some(true) {
            // the stub without its store: the store was lost
            return Err(CatalogError::Corrupt(format!("{} is missing (restore it from a backup)", db_path.display())));
        } else if snap_state.is_some() || store.read(LOG).map_err(ioe)?.is_some_and(|l| !l.is_empty()) {
            let (s, db, from) = migrate_v3(store, dir)?;
            store = s;
            report.upgraded_from = Some(from);
            db
        } else {
            report.created = true;
            let tmp = dir.join(format!("{DB_FILE}.new"));
            let _ = std::fs::remove_file(&tmp);
            drop(CatalogDb::create(&tmp)?);
            std::fs::rename(&tmp, &db_path).map_err(ioe)?;
            CatalogDb::open(&db_path)?
        };
        let t = web_time::Instant::now();
        let (catalog, seq, bad) = db.load()?;
        log::info!("catalog: loaded {} photos from {} in {:.0} ms", catalog.len(), db_path.display(), t.elapsed().as_secs_f64() * 1e3);
        report.failed += bad;
        if store.read(SNAPSHOT).map_err(ioe)?.as_deref() != Some(STUB.as_bytes()) {
            store.write_atomic(SNAPSHOT, STUB.as_bytes()).map_err(ioe)?;
        }
        write_entry_if_missing(dir);
        let log = store.read(LOG).map_err(ioe)?;
        Self::open_from_db(store, log, catalog, seq, report, db)
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `2026-10-10 153005` (UTC): sortable, valid in file names on every platform.
fn stamp(secs: i64) -> String {
    crate::dates::civil(secs).replace('T', " ").replace(':', "")
}

/// v3 → v4: back up, load the JSON library, write the store, put the stub in place.
fn migrate_v3(store: Box<dyn Store>, dir: &Path) -> Result<(Box<dyn Store>, CatalogDb, u32)> {
    let backup = dir.join(BACKUPS_DIR).join(format!("before-v4 {}", stamp(now_secs())));
    std::fs::create_dir_all(&backup).map_err(ioe)?;
    for f in [SNAPSHOT, LOG] {
        let src = dir.join(f);
        if src.exists() {
            std::fs::copy(&src, backup.join(f)).map_err(|e| CatalogError::Io(format!("backup of {f} before the upgrade: {e}")))?;
        }
    }
    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        version: u32,
    }
    // the header only (the catalog in it is skipped, not built)
    let from = std::fs::read(dir.join(SNAPSHOT)).ok().and_then(|b| serde_json::from_slice::<Header>(&b).ok()).map_or(0, |h| h.version);
    let (mut j, catalog, rep) = Journal::open_json(store, false)?;
    if rep.damaged.is_some() {
        log::warn!("catalog: the v3 log was damaged; migrating the state recovered up to op {}", j.seq());
    }
    let seq = j.seq();
    let mut store = j.take_store();
    drop(j);
    let tmp = dir.join(format!("{DB_FILE}.migrating"));
    let _ = std::fs::remove_file(&tmp);
    let t = web_time::Instant::now();
    let mut db = CatalogDb::create(&tmp)?;
    db.checkpoint(&catalog, seq)?;
    drop(db);
    std::fs::rename(&tmp, dir.join(DB_FILE)).map_err(ioe)?;
    log::info!(
        "catalog: migrated {} photos to the v4 store in {:.0} ms (v3 files kept in {})",
        catalog.len(),
        t.elapsed().as_secs_f64() * 1e3,
        backup.display()
    );
    // the store holds every op up to `seq`: the log's records are stale now
    store.write_atomic(SNAPSHOT, STUB.as_bytes()).map_err(ioe)?;
    store.write_atomic(LOG, b"").map_err(ioe)?;
    Ok((store, CatalogDb::open(&dir.join(DB_FILE))?, from))
}

// ---- recent catalogs

/// The list behind File → Open Recent Catalog (most recent first), kept in a JSON file.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentCatalogs {
    pub paths: Vec<PathBuf>,
    /// Open this catalog at startup instead of the last one; `None`: the last one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PathBuf>,
    /// Show the catalog chooser at startup (also when Alt is held).
    #[serde(default)]
    pub prompt_at_startup: bool,
}

/// Entries kept in the list.
pub const RECENT_MAX: usize = 10;

impl RecentCatalogs {
    /// `recent-catalogs.json` in the app's settings folder.
    pub fn default_file() -> Option<PathBuf> {
        dac_brand::settings_dir().map(|d| d.join("recent-catalogs.json"))
    }
    /// The list in `file` (empty if missing or unreadable: never an error).
    pub fn load(file: &Path) -> RecentCatalogs {
        std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(ioe)?;
        }
        let body = serde_json::to_vec_pretty(self).map_err(|e| CatalogError::Io(e.to_string()))?;
        crate::safe_file::write_atomic(file, &body).map_err(ioe)
    }
    /// Move `path` (an entry point) to the top.
    pub fn touch(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.paths.insert(0, path.to_path_buf());
        self.paths.truncate(RECENT_MAX);
    }
    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
    }
    /// The entries that still exist.
    pub fn existing(&self) -> Vec<PathBuf> {
        self.paths.iter().filter(|p| p.exists()).cloned().collect()
    }
    /// What to open at startup: `None` = show the chooser (asked for, Alt held, or nothing to open).
    pub fn startup(&self, alt_held: bool) -> Option<PathBuf> {
        if alt_held || self.prompt_at_startup {
            return None;
        }
        self.default.clone().filter(|p| p.exists()).or_else(|| self.existing().into_iter().next())
    }
}

// ---- backup, integrity, optimise

/// When to back up (checked on exit, as Lightroom does).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BackupSchedule {
    Never,
    EveryExit,
    Daily,
    #[default]
    Weekly,
    Monthly,
}

impl BackupSchedule {
    fn interval_secs(self) -> Option<i64> {
        match self {
            BackupSchedule::Never => None,
            BackupSchedule::EveryExit => Some(0),
            BackupSchedule::Daily => Some(86_400),
            BackupSchedule::Weekly => Some(7 * 86_400),
            BackupSchedule::Monthly => Some(30 * 86_400),
        }
    }
}

/// The catalog's own settings (`catalog-settings.json`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatalogSettings {
    pub backup: BackupSchedule,
    /// Where backups go; `None`: `backups/` in the catalog folder.
    pub backup_dir: Option<PathBuf>,
    /// Backups kept (older ones are deleted); 0 = keep all.
    pub keep_backups: usize,
    /// When the last backup finished (seconds since the Unix epoch).
    pub last_backup: Option<i64>,
    /// Test integrity before a backup.
    pub test_integrity: bool,
    /// Optimise after a backup.
    pub optimize: bool,
    /// The preview store's settings (standard size, 1:1 discard, previews at import). Owned by
    /// the engine (`library.previewSettings`); kept opaque here so the catalog needn't know them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previews: Option<serde_json::Value>,
}

impl CatalogSettings {
    pub fn load(dir: &Path) -> CatalogSettings {
        let mut s: CatalogSettings = std::fs::read(dir.join(SETTINGS_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if s.keep_backups == 0 && !dir.join(SETTINGS_FILE).exists() {
            s.keep_backups = 10;
            s.test_integrity = true;
        }
        s
    }
    pub fn save(&self, dir: &Path) -> Result<()> {
        let body = serde_json::to_vec_pretty(self).map_err(|e| CatalogError::Io(e.to_string()))?;
        crate::safe_file::write_atomic(&dir.join(SETTINGS_FILE), &body).map_err(ioe)
    }
    /// A backup is due on exit at `now`.
    pub fn backup_due(&self, now: i64) -> bool {
        match (self.backup.interval_secs(), self.last_backup) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(every), Some(last)) => now.saturating_sub(last) >= every,
        }
    }
    pub fn backup_root(&self, dir: &Path) -> PathBuf {
        self.backup_dir.clone().unwrap_or_else(|| dir.join(BACKUPS_DIR))
    }
}

/// What [`Journal::check_integrity`] found.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrityReport {
    /// The store's pages all check out.
    pub store_ok: bool,
    /// Photos in the store vs in memory (after a checkpoint they must match).
    pub stored_photos: u64,
    pub photos: u64,
    /// Album / stack references to photos that don't exist.
    pub dangling_refs: usize,
}

impl IntegrityReport {
    pub fn ok(&self) -> bool {
        self.store_ok && self.stored_photos == self.photos && self.dangling_refs == 0
    }
}

/// What optimise did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizeReport {
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub ms: f64,
}

fn no_db() -> CatalogError {
    CatalogError::Invalid("this library has no v4 store (memory or web library)".into())
}

fn file_len(p: &Path) -> u64 {
    std::fs::metadata(p).map_or(0, |m| m.len())
}

impl Journal {
    fn db_dir(&self) -> Result<PathBuf> {
        let db = self.db.as_ref().ok_or_else(no_db)?;
        db.path().parent().map(Path::to_path_buf).ok_or_else(no_db)
    }

    /// Checkpoint, then test the store's integrity and the catalog's references.
    pub fn check_integrity(&mut self, catalog: &Catalog) -> Result<IntegrityReport> {
        self.snapshot(catalog)?;
        let db = self.db.as_mut().ok_or_else(no_db)?;
        let store_ok = db.check_integrity()?;
        let stored_photos = db.photo_count()?;
        let dangling_refs = catalog
            .albums()
            .flat_map(|a| a.photos.iter())
            .chain(catalog.stacks().flat_map(|s| s.photos.iter()))
            .filter(|id| catalog.photo(**id).is_none())
            .count();
        Ok(IntegrityReport { store_ok, stored_photos, photos: catalog.len() as u64, dangling_refs })
    }

    /// Optimise: rewrite the store from `catalog` (drops develop settings nothing uses, rebuilds
    /// every index) and compact it.
    pub fn optimize(&mut self, catalog: &Catalog) -> Result<OptimizeReport> {
        let t = web_time::Instant::now();
        self.snapshot(catalog)?;
        let seq = self.seq();
        let db = self.db.as_mut().ok_or_else(no_db)?;
        let path = db.path().to_path_buf();
        let before = file_len(&path);
        // on an error the old store is intact and still matches the baseline
        db.rewrite(catalog, seq)?;
        let _ = db.compact();
        Ok(OptimizeReport { bytes_before: before, bytes_after: file_len(&path), ms: t.elapsed().as_secs_f64() * 1e3 })
    }

    /// Back up the catalog into `root/<time>/` (a checkpoint first, so the copy holds every op;
    /// the copy is then opened and integrity-tested). Keeps the newest `keep` backups (0 = all).
    /// Returns the backup folder.
    pub fn backup(&mut self, catalog: &Catalog, root: &Path, keep: usize) -> Result<PathBuf> {
        self.snapshot(catalog)?;
        let dir = self.db_dir()?;
        let dest = root.join(stamp(now_secs()));
        std::fs::create_dir_all(&dest).map_err(ioe)?;
        let mut files = vec![DB_FILE.to_string(), LOG.to_string(), SNAPSHOT.to_string(), SETTINGS_FILE.to_string()];
        if let Some(e) = find_entry(&dir).and_then(|e| e.file_name().map(|n| n.to_string_lossy().into_owned())) {
            files.push(e);
        }
        for f in files {
            let src = dir.join(&f);
            if src.exists() {
                std::fs::copy(&src, dest.join(&f)).map_err(|e| CatalogError::Io(format!("backup of {f}: {e}")))?;
            }
        }
        let mut copy = CatalogDb::open(&dest.join(DB_FILE))?;
        if !copy.check_integrity()? {
            return Err(CatalogError::Corrupt(format!("the backup in {} failed its integrity test", dest.display())));
        }
        drop(copy);
        let mut settings = CatalogSettings::load(&dir);
        settings.last_backup = Some(now_secs());
        settings.save(&dir)?;
        if keep > 0 {
            prune_backups(root, keep);
        }
        Ok(dest)
    }

    /// The exit-time step: back up if the schedule says so (with the integrity test and
    /// optimise when set). `Ok(None)`: not due.
    pub fn backup_if_due(&mut self, catalog: &Catalog) -> Result<Option<PathBuf>> {
        let dir = self.db_dir()?;
        let s = CatalogSettings::load(&dir);
        if !s.backup_due(now_secs()) {
            return Ok(None);
        }
        if s.test_integrity {
            let r = self.check_integrity(catalog)?;
            if !r.ok() {
                return Err(CatalogError::Corrupt(format!("integrity test failed before the backup: {r:?}")));
            }
        }
        let out = self.backup(catalog, &s.backup_root(&dir), s.keep_backups)?;
        if s.optimize {
            self.optimize(catalog)?;
        }
        Ok(Some(out))
    }
}

/// Delete all but the newest `keep` timestamped backups in `root` (never the pre-v4 one).
fn prune_backups(root: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.join(DB_FILE).exists() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("before-v4")))
        .collect();
    dirs.sort();
    let excess = dirs.len().saturating_sub(keep);
    for d in dirs.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_dir_all(&d) {
            log::warn!("catalog: can't remove old backup {}: {e}", d.display());
        }
    }
}
