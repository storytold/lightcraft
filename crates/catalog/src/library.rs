//! A catalog on disk (native): one folder with an entry-point file, the v4 store, the op log and
//! the catalog's own settings; plus the catalog-level operations around it: new / open / recent,
//! the v3 → v4 migration, backups, the integrity test and optimise.
//!
//! ```text
//! My Catalog/
//!   My Catalog.<catalog_ext>   entry point (what File → Libraries → Open Library… picks; JSON, see EntryFile)
//!   catalog.redb               the v4 store (crate::db)
//!   catalog.log                op log: undo journal, change feed, crash safety (crate::journal)
//!   catalog.snap               a stub that makes builds before v4 refuse the folder, untouched
//!   catalog-settings.json      backup schedule, backup folder, last backup
//!   backups/                   default backup folder; `before-v<N> <time>/` keeps the library as it was
//!                              before an upgrade to format N (crate::upgrade_backup)
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::db::{CatalogDb, DB_FILE};
use crate::journal::{LOG, SNAPSHOT, VERSION};
use crate::progress::{MigrationKind, MigrationPhase};
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
    /// [`lcc_brand::CATALOG_MAGIC`]: stable across product renames.
    pub magic: String,
    pub version: u32,
    /// The store file, relative to the folder.
    pub store: String,
}

impl Default for EntryFile {
    fn default() -> Self {
        EntryFile { magic: lcc_brand::CATALOG_MAGIC.to_string(), version: VERSION, store: DB_FILE.to_string() }
    }
}

/// The entry-point file of the catalog in `dir`: `<dir name>.<catalog_ext>`.
pub fn entry_path(dir: &Path) -> PathBuf {
    let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "catalog".into());
    dir.join(format!("{name}.{}", lcc_brand::CATALOG_EXT))
}

/// An entry-point file in `dir` (any name with a current or legacy catalog extension).
pub fn find_entry(dir: &Path) -> Option<PathBuf> {
    let preferred = entry_path(dir);
    if preferred.is_file() {
        return Some(preferred);
    }
    let rd = std::fs::read_dir(dir).ok()?;
    rd.filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_file() && p.extension().and_then(|x| x.to_str()).is_some_and(|x| lcc_brand::catalog_exts().any(|c| c.eq_ignore_ascii_case(x))))
}

/// The catalog folder for a path the user picked: the entry-point file or the folder itself.
/// A folder must hold an entry point or catalog files.
pub fn resolve(path: &Path) -> Result<PathBuf> {
    if path.is_file() {
        let ext_ok = path.extension().and_then(|x| x.to_str()).is_some_and(|x| lcc_brand::catalog_exts().any(|c| c.eq_ignore_ascii_case(x)));
        if !ext_ok {
            return Err(CatalogError::Invalid(format!("{} is not a catalog (expected a .{} file)", path.display(), lcc_brand::CATALOG_EXT)));
        }
        let bytes = std::fs::read(path).map_err(ioe)?;
        let e: EntryFile = serde_json::from_slice(&bytes).map_err(|e| CatalogError::Corrupt(format!("{}: {e}", path.display())))?;
        if e.magic != lcc_brand::CATALOG_MAGIC {
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
    let dir = create_folder(parent, name)?;
    let (j, _, _) = Journal::open(Box::new(FsStore::open(&dir).map_err(ioe)?))?;
    drop(j);
    Ok(entry_path(&dir))
}

/// The folder of a new catalog, `parent/name` (must not exist or be empty), created empty: opening
/// it as a library then creates the catalog (with the demo photos, when asked for). Returns it.
pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf> {
    let name = name.trim();
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(CatalogError::Invalid(format!("not a catalog name: {name:?}")));
    }
    let dir = parent.join(name);
    if dir.exists() && std::fs::read_dir(&dir).map_err(ioe)?.next().is_some() {
        return Err(CatalogError::Invalid(format!("{} exists and is not empty", dir.display())));
    }
    std::fs::create_dir_all(&dir).map_err(ioe)?;
    Ok(dir)
}

/// Whether opening the library folder `dir` will first migrate it from v3 (the JSON snapshot and
/// log) to the v4 store: a one-time step that takes tens of seconds for a large library, which
/// frontends run with a progress display ([`crate::progress::migration`]). Reads only the start
/// of the snapshot.
pub fn needs_migration(dir: &Path) -> bool {
    use std::io::Read;
    if dir.join(DB_FILE).exists() {
        return false;
    }
    let mut head = [0u8; 64];
    let snap = std::fs::File::open(dir.join(SNAPSHOT)).ok().and_then(|mut f| f.read(&mut head).ok()).map(|n| head.get(..n).unwrap_or(&[]).to_vec());
    match snap {
        // another app's catalog is never upgraded (in place) as if it were this app's
        Some(start) => !start.starts_with(STUB.trim_end().as_bytes()) && foreign_format(dir).is_none(),
        None => std::fs::metadata(dir.join(LOG)).is_ok_and(|m| m.len() > 0),
    }
}

/// The catalog format named by the library folder `dir`, when it is not one this app reads:
/// a catalog made by another app, e.g. the upstream app this one was forked from (`None` for
/// this app's own catalogs, an empty or missing folder, or a snapshot without a readable header).
/// Reads only the start of the snapshot; never modifies anything.
pub fn foreign_format(dir: &Path) -> Option<String> {
    use std::io::Read;
    let mut head = vec![0u8; 512];
    let n = std::fs::File::open(dir.join(SNAPSHOT)).ok()?.read(&mut head).ok()?;
    let head = String::from_utf8_lossy(head.get(..n)?).into_owned();
    // `{"format":"…"` starts every snapshot this app or its ancestors wrote
    let rest = head.split_once("\"format\"")?.1.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let format = rest.split_once('"')?.0;
    (format != crate::journal::FORMAT && format != crate::journal::V4_FORMAT).then(|| format.to_string())
}

/// Migrate the library folder `dir` from v3 to the v4 store if it needs it ([`needs_migration`]),
/// without loading it: `Ok(true)` when it was migrated. Progress shows in
/// [`crate::progress::migration`] meanwhile. Opening the folder afterwards is a plain v4 open.
pub fn migrate(dir: &Path) -> Result<bool> {
    if !needs_migration(dir) {
        return Ok(false);
    }
    let store = FsStore::open(dir).map_err(ioe)?;
    migrate_v3(Box::new(store), dir)?;
    Ok(true)
}

/// Bring the v4 store of the library in `dir` up to this build's format version without opening
/// the library — the step [`Journal::open_v4`] otherwise does while it opens one: a log a newer
/// build wrote to is refused first, then the store, its log and the stub are copied and verified
/// to `backups/before-v<N> <time>/` ([`crate::upgrade_backup`]) and only then is the store marked
/// with this version, so the build that made it can still open the copy. Split out so a frontend
/// can run it on a worker thread and show [`crate::progress::migration`] (the copy is nearly all
/// of the wait after an update, byte for byte over a store that can be gigabytes) while the
/// opening that follows — in the session, on the interface's thread — finds the current version
/// and is quick. `Ok(false)`: nothing to do (no store, or already this version). The library
/// must be locked by the caller ([`crate::lock`]).
pub fn prepare_upgrade(dir: &Path) -> Result<bool> {
    prepare_upgrade_inner(dir).map_err(|e| match e {
        CatalogError::Corrupt(m) => CatalogError::Corrupt(format!("{m}. {}", backup_hint(dir))),
        e => e,
    })
}

fn prepare_upgrade_inner(dir: &Path) -> Result<bool> {
    let db_path = dir.join(DB_FILE);
    if !db_path.is_file() {
        return Ok(false);
    }
    let mut db = CatalogDb::open(&db_path)?;
    let stored = db.version();
    if stored >= crate::journal::VERSION {
        return Ok(false);
    }
    // (as the open reads it: a missing log is an empty one)
    let log_path = dir.join(LOG);
    let log = if log_path.exists() { std::fs::read(&log_path).map_err(ioe)? } else { Vec::new() };
    crate::journal::refuse_newer_records(&log)?;
    let _running = crate::progress::Running::start(MigrationKind::FormatBump, dir.display().to_string());
    // copy first: the store, its log and stub as the older build left them, checked, before
    // anything of the library is written (the store is closed meanwhile, so the copy is the file
    // as it is on disk)
    drop(db);
    crate::upgrade_backup::keep_copy(dir, stored, crate::journal::VERSION, &[DB_FILE, LOG, SNAPSHOT])?;
    db = CatalogDb::open(&db_path)?;
    crate::progress::phase(MigrationPhase::Finishing, 0, 0);
    db.mark_version().map_err(|e| match e {
        CatalogError::Io(e) => CatalogError::Io(format!(
            "can't upgrade {} from catalog format v{stored} to v{} ({e}). The catalog was left unchanged: free some disk space (or make it writable), then try again",
            db_path.display(),
            crate::journal::VERSION
        )),
        // a damaged store keeps its kind (and gets the backup hint)
        e => e,
    })?;
    log::info!("catalog: upgraded {} from format v{stored} to v{}", db_path.display(), crate::journal::VERSION);
    Ok(true)
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
const STUB: &str = "{\"format\":\"lcc-catalog-db\",\"version\":4,\"store\":\"catalog.redb\"}\n";

impl Journal {
    /// Open the library in `dir` with the v4 store, creating it, or migrating a v3 library
    /// (JSON snapshot + log) first: its files are copied and verified to `backups/before-v<N> <time>/`
    /// ([`crate::upgrade_backup`]), the
    /// store is built in a temporary file and renamed into place only once complete, so a crash
    /// at any point leaves either the v3 library (migrated again next time) or the v4 one.
    ///
    /// A damaged store is a [`CatalogError::Corrupt`] that names where this catalog's backups are
    /// (the newest one, when there is one), so the user can restore it.
    pub fn open_v4(store: Box<dyn Store>, dir: &Path) -> Result<(Journal, Catalog, LoadReport)> {
        Self::open_v4_inner(store, dir).map_err(|e| match e {
            CatalogError::Corrupt(m) => CatalogError::Corrupt(format!("{m}. {}", backup_hint(dir))),
            e => e,
        })
    }

    fn open_v4_inner(mut store: Box<dyn Store>, dir: &Path) -> Result<(Journal, Catalog, LoadReport)> {
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
            let (s, db, from, kept) = migrate_v3(store, dir)?;
            store = s;
            report.upgraded_from = Some(from);
            report.upgrade_backup = Some(kept);
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
        let log = store.read(LOG).map_err(ioe)?;
        // An older build wrote the store: mark it with this build's format before anything is
        // appended to the log, as the JSON snapshot is upgraded, so the older build refuses the
        // library from then on instead of dropping what it doesn't know. Refused when that fails:
        // opened anyway, the log could gain records the older build then drops. A log a newer
        // build wrote to is refused first, untouched, as the replay would refuse it.
        let stored = db.version();
        if stored < crate::journal::VERSION {
            crate::journal::refuse_newer_records(log.as_deref().unwrap_or_default())?;
            // copy first: the store, its log and stub as the older build left them, checked, before
            // anything of the library is written (the store is closed meanwhile, so the copy is
            // the file as it is on disk)
            drop(db);
            let kept = crate::upgrade_backup::keep_copy(dir, stored, crate::journal::VERSION, &[DB_FILE, LOG, SNAPSHOT])?;
            report.upgrade_backup = Some(kept);
            db = CatalogDb::open(&db_path)?;
            db.mark_version().map_err(|e| match e {
                CatalogError::Io(e) => CatalogError::Io(format!(
                    "can't upgrade {} from catalog format v{stored} to v{} ({e}). The catalog was left unchanged: free some disk space (or make it writable), then try again",
                    db_path.display(),
                    crate::journal::VERSION
                )),
                // a damaged store keeps its kind (and gets the backup hint)
                e => e,
            })?;
            log::info!("catalog: upgraded {} from format v{stored} to v{}", db_path.display(), crate::journal::VERSION);
            report.upgraded_from.get_or_insert(stored);
        }
        if store.read(SNAPSHOT).map_err(ioe)?.as_deref() != Some(STUB.as_bytes()) {
            store.write_atomic(SNAPSHOT, STUB.as_bytes()).map_err(ioe)?;
        }
        write_entry_if_missing(dir);
        Self::open_from_db(store, log, catalog, seq, report, db)
    }
}

/// The newest backup of the library in `dir` (a folder in its backup root holding a store or a
/// v3 snapshot), by name: backup folders are named by their sortable UTC time.
pub fn latest_backup(dir: &Path) -> Option<PathBuf> {
    let root = CatalogSettings::load(dir).backup_root(dir);
    std::fs::read_dir(&root)
        .ok()?
        .take(100_000)
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(DB_FILE).is_file() || p.join(SNAPSHOT).is_file())
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

/// Where to restore the library in `dir` from, for an error message.
pub fn backup_hint(dir: &Path) -> String {
    match latest_backup(dir) {
        Some(b) => format!("The newest backup of this catalog is in {}", b.display()),
        None => format!("No backup of this catalog was found in {}", CatalogSettings::load(dir).backup_root(dir).display()),
    }
}

/// What was wrong with a settings file, to tell the user (the host words it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsProblem {
    /// It didn't parse and was moved aside to the file named `kept`; the defaults stand in.
    SetAside { kept: String },
    /// It couldn't be read, or didn't parse and couldn't be moved aside (`reason`): the defaults
    /// stand in for now, and the file is never overwritten while it is there.
    Unreadable { reason: String },
}

/// A JSON settings file: `Ok(None)` when there is none, `Err` with the reason when it is there but
/// can't be read or doesn't parse (nothing is changed on disk).
fn read_settings<T: serde::de::DeserializeOwned>(file: &Path) -> std::result::Result<Option<T>, (bool, String)> {
    let bytes = match std::fs::read(file) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err((false, e.to_string())),
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|e| (true, e.to_string()))
}

/// Loads a settings file, moving it aside when it doesn't parse ([`set_aside`]).
fn load_settings<T: serde::de::DeserializeOwned>(file: &Path) -> (Option<T>, Option<SettingsProblem>) {
    match read_settings(file) {
        Ok(v) => (v, None),
        Err((false, reason)) => (None, Some(SettingsProblem::Unreadable { reason })),
        Err((true, err)) => match set_aside(file) {
            Ok(kept) => {
                (None, Some(SettingsProblem::SetAside { kept: kept.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() }))
            }
            Err(w) => (None, Some(SettingsProblem::Unreadable { reason: format!("{err}; couldn't be moved aside: {w}") })),
        },
    }
}

/// Writes a settings file, unless one is there that can't be read or doesn't parse: that is the
/// user's, not to be replaced by defaults that only stood in for it.
fn save_settings(file: &Path, body: &[u8]) -> Result<()> {
    if let Err((_, why)) = read_settings::<serde_json::Value>(file) {
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Err(CatalogError::Io(format!("{name} couldn't be read ({why}), so it isn't overwritten; repair or remove it, then try again")));
    }
    crate::safe_file::write_atomic(file, body).map_err(ioe)
}

/// Renames the damaged `file` to `<name>.corrupt-<unix time>` (a free name: `-2`, `-3`, … when
/// that is taken) next to it; the new path.
fn set_aside(file: &Path) -> std::io::Result<PathBuf> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let base = format!("{name}.corrupt-{}", now_secs());
    for n in 1..100u32 {
        let candidate = file.with_file_name(if n == 1 { base.clone() } else { format!("{base}-{n}") });
        if !candidate.exists() {
            std::fs::rename(file, &candidate)?;
            return Ok(candidate);
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "too many damaged copies already kept"))
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `2026-10-10 153005` (UTC): sortable, valid in file names on every platform.
pub(crate) fn stamp(secs: i64) -> String {
    crate::dates::civil(secs).replace('T', " ").replace(':', "")
}

/// v3 → v4: back up, load the JSON library, write the store, put the stub in place.
fn migrate_v3(store: Box<dyn Store>, dir: &Path) -> Result<(Box<dyn Store>, CatalogDb, u32, PathBuf)> {
    // another app's catalog: refused before anything (the backup folder included) is written
    if let Some(f) = foreign_format(dir) {
        return Err(CatalogError::Foreign(format!("{SNAPSHOT}: format {f:?}")));
    }
    let _running = crate::progress::Running::start(MigrationKind::V3ToV4, dir.display().to_string());
    #[derive(Deserialize)]
    struct Header {
        #[serde(default)]
        version: u32,
    }
    // the header only (the catalog in it is skipped, not built)
    let from = std::fs::read(dir.join(SNAPSHOT)).ok().and_then(|b| serde_json::from_slice::<Header>(&b).ok()).map_or(0, |h| h.version);
    // copy first: the v3 files, checked, before anything of the library is written
    let backup = crate::upgrade_backup::keep_copy(dir, from, VERSION, &[SNAPSHOT, LOG])?;
    crate::progress::phase(MigrationPhase::Reading, 0, 0);
    let (mut j, catalog, rep) = Journal::open_json(store, false)?;
    crate::progress::phase(MigrationPhase::Encoding, 0, catalog.len() as u64);
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
    crate::progress::phase(MigrationPhase::Finishing, catalog.len() as u64, catalog.len() as u64);
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
    Ok((store, CatalogDb::open(&dir.join(DB_FILE))?, from, backup))
}

// ---- recent catalogs

/// The recent list of older versions (most recent first, in `recent-catalogs.json`): read once, to
/// carry its libraries over into the app's one recent list (issue #871).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecentCatalogs {
    pub paths: Vec<PathBuf>,
    /// The catalog older versions opened at startup instead of the last one (not carried over).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PathBuf>,
    /// Older versions showed their catalog chooser at startup (carried over as At launch: Ask me).
    #[serde(default)]
    pub prompt_at_startup: bool,
}

/// Entries kept in the list.
pub const RECENT_MAX: usize = 10;

impl RecentCatalogs {
    /// `recent-catalogs.json` in the settings folder `settings` (the host resolves it, e.g.
    /// `lcc_engine::config::recent_catalogs_file`, so `<PREFIX>_CONFIG_DIR` and portable mode apply).
    pub fn file_in(settings: &Path) -> PathBuf {
        settings.join("recent-catalogs.json")
    }
    /// The list in `file` (empty if missing or unusable: never an error; the problem is logged).
    pub fn load(file: &Path) -> RecentCatalogs {
        let (r, problem) = RecentCatalogs::load_checked(file);
        if let Some(p) = problem {
            log::warn!("recent catalogs: {p:?}");
        }
        r
    }
    /// The list in `file`, and what was wrong with it: one that doesn't parse is moved aside to
    /// `recent-catalogs.json.corrupt-<unix time>`; one that can't be read is left alone (and
    /// [`RecentCatalogs::save`] then refuses to overwrite it).
    pub fn load_checked(file: &Path) -> (RecentCatalogs, Option<SettingsProblem>) {
        let (r, problem) = load_settings(file);
        (r.unwrap_or_default(), problem)
    }
    /// Writes the list; refused while `file` is there but can't be read or doesn't parse.
    pub fn save(&self, file: &Path) -> Result<()> {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(ioe)?;
        }
        let body = serde_json::to_vec_pretty(self).map_err(|e| CatalogError::Io(e.to_string()))?;
        save_settings(file, &body)
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
    /// What the catalog was at the last backup ([`Journal::backup_mark`]): an exit with nothing
    /// changed since needs no backup, and isn't asked for one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_mark: Option<String>,
    /// Test integrity before a backup.
    pub test_integrity: bool,
    /// Optimise after a backup.
    pub optimize: bool,
    /// The preview store's settings (standard size, 1:1 discard, previews at import). Owned by
    /// the engine (`library.previewSettings`); kept opaque here so the catalog needn't know them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previews: Option<serde_json::Value>,
    /// A stable id of this library, made when first needed: what keeps data held outside it, by
    /// photo id (plug-in data), apart from other libraries' (photo ids are only unique within one).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub library_id: Option<String>,
}

impl CatalogSettings {
    /// The settings in `dir`; [`CatalogSettings::new_library`] when there are none or the file
    /// can't be used (logged). Changes nothing on disk: opening the library
    /// ([`CatalogSettings::load_checked`]) is what moves a damaged file aside and tells the user,
    /// and [`CatalogSettings::save`] never overwrites a file it couldn't read.
    pub fn load(dir: &Path) -> CatalogSettings {
        match read_settings(&dir.join(SETTINGS_FILE)) {
            Ok(s) => s.unwrap_or_else(CatalogSettings::new_library),
            Err((_, why)) => {
                log::warn!("catalog settings: {SETTINGS_FILE} can't be used ({why}); the defaults stand in");
                CatalogSettings::new_library()
            }
        }
    }

    /// The settings a new library starts with: backups kept (10) and tested.
    pub fn new_library() -> CatalogSettings {
        CatalogSettings { keep_backups: 10, test_integrity: true, ..CatalogSettings::default() }
    }

    /// The settings in `dir`, and what was wrong with its file, to tell the user. A missing file
    /// gives [`CatalogSettings::new_library`] and no problem. A file that doesn't parse is moved
    /// aside to `catalog-settings.json.corrupt-<unix time>` (so the next load doesn't report it
    /// again) and the new-library defaults are used; so are they when the file can't be read: it
    /// is left where it is, and [`CatalogSettings::save`] refuses to overwrite it.
    pub fn load_checked(dir: &Path) -> (CatalogSettings, Option<SettingsProblem>) {
        let (s, problem) = load_settings(&dir.join(SETTINGS_FILE));
        (s.unwrap_or_else(CatalogSettings::new_library), problem)
    }
    /// Writes the settings; refused while the file is there but can't be read or doesn't parse
    /// (the defaults that stood in for it never replace the user's settings).
    pub fn save(&self, dir: &Path) -> Result<()> {
        let body = serde_json::to_vec_pretty(self).map_err(|e| CatalogError::Io(e.to_string()))?;
        save_settings(&dir.join(SETTINGS_FILE), &body)
    }
    /// A backup is due on exit at `now`.
    pub fn backup_due(&self, now: i64) -> bool {
        match (self.backup.interval_secs(), self.last_backup) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(every), Some(last)) => now.saturating_sub(last) >= every,
        }
    }
    /// A backup is due on exit at `now` and the catalog changed since the last one: `mark` is what
    /// it is now ([`Journal::backup_mark`]; `None`, unknown: changed).
    pub fn backup_wanted(&self, now: i64, mark: Option<&str>) -> bool {
        self.backup_due(now) && (mark.is_none() || self.backup_mark.as_deref() != mark)
    }
    pub fn backup_root(&self, dir: &Path) -> PathBuf {
        self.backup_dir.clone().unwrap_or_else(|| dir.join(BACKUPS_DIR))
    }
}

/// prefs.json larger than this counts by its size in [`Journal::backup_mark`] (ours are far smaller).
const MAX_MARKED_FILE: u64 = 256 << 20;

/// FNV-1a, 64-bit: a hash that stays the same across builds and platforms (std's `DefaultHasher`
/// may change between Rust releases), for [`Journal::backup_mark`].
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
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

    /// What the catalog is now, for telling whether anything changed since its last backup: the
    /// position of its last op and the contents of the library's prefs.json (its presets and
    /// habits, which a backup copies too). Equal marks: nothing to back up.
    pub fn backup_mark(&self) -> Result<String> {
        let dir = self.db_dir()?;
        let mut h = Fnv::default();
        h.write(&self.seq().to_le_bytes());
        // (a file too large to be one of ours, or unreadable, counts by its size)
        let prefs = dir.join("prefs.json");
        match std::fs::metadata(&prefs) {
            Ok(m) if m.len() <= MAX_MARKED_FILE => h.write(&std::fs::read(&prefs).unwrap_or_default()),
            Ok(m) => h.write(&m.len().to_le_bytes()),
            Err(_) => h.write(b"-"),
        }
        Ok(format!("{}:{:016x}", self.seq(), h.0))
    }

    /// Back up the catalog into `root/<time>/` (a checkpoint first, so the copy holds every op;
    /// the copy is then opened and integrity-tested). Keeps the newest `keep` backups (0 = all).
    /// Returns the backup folder.
    pub fn backup(&mut self, catalog: &Catalog, root: &Path, keep: usize) -> Result<PathBuf> {
        self.snapshot(catalog)?;
        let dir = self.db_dir()?;
        let mark = self.backup_mark().ok();
        let dest = root.join(stamp(now_secs()));
        std::fs::create_dir_all(&dest).map_err(ioe)?;
        // (the library's prefs.json too, where the app keeps its presets: gathered back from a backup)
        let mut files = vec![DB_FILE.to_string(), LOG.to_string(), SNAPSHOT.to_string(), SETTINGS_FILE.to_string(), "prefs.json".to_string()];
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
        settings.backup_mark = mark;
        settings.save(&dir)?;
        if keep > 0 {
            prune_backups(root, keep);
        }
        Ok(dest)
    }

    /// The exit-time step: back up if the schedule says so and the catalog changed since the last
    /// backup (with the integrity test and optimise when set). `Ok(None)`: not due, or nothing
    /// changed.
    pub fn backup_if_due(&mut self, catalog: &Catalog) -> Result<Option<PathBuf>> {
        let dir = self.db_dir()?;
        let s = CatalogSettings::load(&dir);
        if !s.backup_wanted(now_secs(), self.backup_mark().ok().as_deref()) {
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

/// Delete all but the newest `keep` timestamped backups in `root` (never a copy kept before an
/// upgrade, `before-v<N> …`: it is what an older version opens).
fn prune_backups(root: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && p.join(DB_FILE).exists() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("before-v")))
        .collect();
    dirs.sort();
    let excess = dirs.len().saturating_sub(keep);
    for d in dirs.into_iter().take(excess) {
        if let Err(e) = std::fs::remove_dir_all(&d) {
            log::warn!("catalog: can't remove old backup {}: {e}", d.display());
        }
    }
}
