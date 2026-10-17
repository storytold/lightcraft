//! Crash-safe catalog persistence: an append-only op log plus periodic snapshots.
//!
//! Files (in a [`Store`]):
//! - `catalog.snap` — `{"format":"lightcraft-catalog","version":V,"seq":N,"defaults":{…},"catalog":{…}}`, the
//!   state after op `N` in catalog format `V` ([`VERSION`]); replaced atomically (temp + fsync +
//!   rename).
//! - `catalog.log` — JSON lines, one per op applied after the snapshot:
//!   `{"seq":N,"crc":C,"op":{…}}` where `C` is the CRC-32 of the op's JSON text. Appends are
//!   fsynced before [`Journal::append`] returns.
//!
//! Loading = snapshot + replay of the records with `seq > snapshot seq`. Recovery rules:
//! - a **torn final record** (crash mid-append: truncated line or bad CRC at the end) is dropped
//!   and the file is cut back to the last good record, so later appends start on a clean line;
//! - a **failed append** (disk full, a dropped network share) may leave part of its batch in the
//!   file: [`Journal::append`] cuts the log back to the last whole record before returning the
//!   error (or, if that fails too, before the next append), so the retry — the same ops with the
//!   same `seq`s — starts on a clean line. Logs written by builds without that cut-back hold the
//!   fragment glued to the first retried record on one line; the loader recognises the whole,
//!   CRC-valid record at the end of such a line and skips only the fragment;
//! - records already covered by the snapshot (crash between writing the snapshot and resetting
//!   the log) are skipped;
//! - a bad record **followed by good ones** is real damage: replay stops there, the log is kept
//!   as `catalog.log.damaged-<seq>`, and a fresh snapshot is written so the library stays usable.
//!
//! **Background compaction** ([`Journal::snapshot_in_background`], when the store offers a
//! [`Store::background_writer`]): a worker thread writes the snapshot of a copy of the catalog at
//! op `N` while appends go on (durably, as always) to the same log. Once the snapshot is durable,
//! [`Journal::poll`] atomically replaces the log with the records appended since the snapshot
//! started (`seq > N`, kept in memory). Every crash point is covered by the rules above:
//! - before the rename of `catalog.snap`: the old snapshot + the whole log (nothing was removed);
//! - between the rename and the log rewrite: the new snapshot + the whole log, whose records
//!   `<= N` are skipped as stale;
//! - after the log rewrite: the new snapshot + exactly the records `> N`.
//!
//! **When to compact** ([`SnapshotPolicy`]): a snapshot rewrites the whole catalog, so it is due
//! when the log has grown to a share of the snapshot's size, not after a fixed number of changes.
//! Writing then stays proportional to what changed: an import of any size rewrites a few times
//! the final snapshot in all, where compacting every 2000 records rewrote it once per 2000 photos.
//!
//! Closing a library compacts its log too, unless the snapshot is large and the log still short
//! ([`Journal::wants_snapshot_on_close`]): quitting after a few changes doesn't rewrite a large
//! catalog, and a log is never left long from one session to the next.
//!
//! At most one snapshot is in flight; a synchronous [`Journal::snapshot`] and dropping the journal
//! first wait for it, so an older snapshot can never replace a newer one.
//!
//! # Format versions
//!
//! The snapshot's `version` is the catalog format, and it covers the log beside it: the log has
//! no header of its own, and is only ever appended to under a snapshot that states the defaults
//! its records leave out (this build's format, or one from 9 on with this build's defaults).
//!
//! Every format, what it adds and which build wrote it first is one row of [`FORMATS`]: the
//! newest row is [`VERSION`], what this build writes.
//!
//! Rules:
//! - **Add a row to [`FORMATS`]** in the change that adds an [`Op`] variant or a serialized field
//!   to the catalog, a photo, an album, a stack or develop settings that an older build would
//!   misread or silently drop. New fields must deserialize from older data (`#[serde(default)]`),
//!   so newer builds keep reading every older format. The row says whether it adds an op
//!   ([`NEW_OP`]) or only fields ([`FIELDS`]); a row's description is free to name an op either
//!   way (a field an op carries), since nothing reads its words.
//! - **A number means one format, and is final only on `main`.** Two changes written at the same
//!   time both take the next number; the one that lands second takes the number after it (and
//!   renames its fixture). A number that is spoken for but not in this build yet can have its
//!   row all the same ([`Format::here`] set to `false`), so nothing else takes it.
//! - **Every format from 6 on has a fixture**: a small library written by the build of that
//!   format (`fixtures/v<N>-catalog.*`; `tests_stored::write_this_formats_fixture` writes this
//!   build's), which every later build must open as it was (`tests_format_version`).
//! - **Opening an older format upgrades it**: right after loading, [`Journal::open`] writes a
//!   snapshot in this build's format (also for a new library), before anything is appended — so a
//!   library is never a mix, and older builds refuse it from then on instead of reading part of it.
//!   [`LoadReport::upgraded_from`] says when that happened.
//! - **An upgrade keeps what it replaces**: older builds can't read the upgraded library, so the
//!   files it was are first kept beside it as `catalog.snap.v<format>-<seq>` and
//!   `catalog.log.v<format>-<seq>` (`seq`: the last change they hold), byte for byte
//!   ([`LoadReport::kept`]; a damaged log is kept under its own name instead). Put back under
//!   their own names, they are the library as the older build left it. They stay unless the
//!   library on disk is known to be in its old format still: an upgrade that did not happen
//!   leaves no copy, and one that was written keeps its copy whatever fails after it. Nothing
//!   removes them afterwards: they are the user's to delete. (Not where the store says there is
//!   no going back, [`Store::keeps_replaced_formats`].)
//! - **A library that can't be upgraded is not opened**, where appending to it as it is would
//!   not be safe: one that states no defaults for develop settings (formats before 9) or other
//!   ones than this build's ([`crate::stored`]), since a record written by this build leaves
//!   out what equals this build's defaults. It is left as it was, and the error says why. (A
//!   library that states this build's defaults opens as it is, and the next open tries again.)
//! - **A newer format is refused, untouched**: a snapshot whose `version` is above [`VERSION`],
//!   or a log record whose CRC matches (so it is exactly what was written) but whose op doesn't
//!   parse, fails [`Journal::open`] with [`CatalogError::Newer`] before any file is modified — it
//!   is never mistaken for a torn tail or damage. (Builds up to v0.2.0 refuse a v2 snapshot with
//!   "unsupported format".)

use crate::store::Store;
use crate::{Catalog, CatalogError, Op, Result};

pub const SNAPSHOT: &str = "catalog.snap";
pub const LOG: &str = "catalog.log";
const FORMAT: &str = "lightcraft-catalog";
/// One catalog format: a number in `catalog.snap` that says how the library beside it is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub version: u32,
    /// What it came with.
    pub written_by: &'static str,
    /// What it adds to the format before it.
    pub adds: &'static str,
    /// It adds an [`Op`] variant ([`NEW_OP`]), not only fields ([`FIELDS`]): the newest of these is the newest op's
    /// format (the guard test `op_variants_are_versioned` holds that against the ops themselves).
    pub adds_op: bool,
    /// This build reads and writes what the format adds. `false`: the number is spoken for by a
    /// change that is not in this build yet (see the module docs → *Format versions*).
    pub here: bool,
}

/// A row of [`FORMATS`] that adds an [`Op`] variant ([`Format::adds_op`]).
pub const NEW_OP: bool = true;
/// A row of [`FORMATS`] that adds fields, or a way of storing records, but no op.
pub const FIELDS: bool = false;

const fn format(version: u32, written_by: &'static str, adds_op: bool, adds: &'static str) -> Format {
    Format { version, written_by, adds, adds_op, here: true }
}

/// The newest format in `formats` that this build has and that adds an op, by each row's flag.
pub fn newest_op_format(formats: &[Format]) -> Option<&Format> {
    formats.iter().rev().find(|f| f.here && f.adds_op)
}

/// Every catalog format, oldest first. (Before them: early builds wrote a log without a snapshot.)
pub const FORMATS: &[Format] = &[
    format(1, "up to v0.2.0", FIELDS, ""),
    format(2, "after v0.2.0", NEW_OP, "`Op::SetBrowsed`, `Catalog.browsed`, `Photo.local_baseline`"),
    format(3, "albums by hand", NEW_OP, "`Op::SetAlbumOrder`, `Album.order`"),
    format(4, "Reload reads lens data", NEW_OP, "`Op::SetEmbeddedLens`"),
    format(5, "the keyword list", NEW_OP, "`Op::SetKeyword`, `Catalog.keyword_list`"),
    format(6, "folder labels", NEW_OP, "`Op::SetFolderRecord`, `Catalog.folder_records`"),
    format(7, "Kelvin white balance (#730)", NEW_OP, "`Op::SetAsShotWb`, `WhiteBalance.scale`"),
    format(8, "the History cursor", NEW_OP, "`Op::SetHistoryCursor`, `Photo.history_cursor`"),
    format(9, "preview-only recheck (#974)", FIELDS, "`Photo.preview_checked`, `Op::SetContent`'s `preview_checked`"),
    format(10, "compact records", FIELDS, "the stored form (`crate::stored`): unset fields and default develop settings are left out"),
];

/// The catalog format this build writes (and the newest it reads): the newest of [`FORMATS`].
pub const VERSION: u32 = newest(FORMATS);

const fn newest(formats: &[Format]) -> u32 {
    match formats.last() {
        Some(f) => f.version,
        None => 0,
    }
}

/// When [`Journal::wants_snapshot`] says it's time to compact the log.
///
/// The log is worth compacting once it is long (`max_records` or `max_bytes`) **and** has grown
/// to `1 / snapshot_share` of the snapshot it would replace; or, whatever the snapshot's size,
/// once it reaches `log_cap_bytes` (what opening the library has to replay stays bounded).
#[derive(Clone, Copy, Debug)]
pub struct SnapshotPolicy {
    pub max_records: u64,
    pub max_bytes: u64,
    /// The log must also have reached this share of the snapshot's size: 4 = a quarter of it.
    /// 0: no such condition (a long log is compacted whatever the snapshot's size).
    pub snapshot_share: u64,
    /// A log this large is compacted in any case.
    pub log_cap_bytes: u64,
    /// Closing the library compacts whatever log there is while the snapshot is at most this
    /// large (rewriting it is quick, and the next open has nothing to replay). A larger snapshot
    /// is rewritten on close only once the log is long (`max_records` or `max_bytes`): a log
    /// never grows past that from one session to the next, so what a damaged record in it can
    /// cost stays what it was.
    pub close_bytes: u64,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        SnapshotPolicy { max_records: 2000, max_bytes: 16 << 20, snapshot_share: 4, log_cap_bytes: 256 << 20, close_bytes: 64 << 20 }
    }
}

impl SnapshotPolicy {
    /// Whether a log of `records` records and `bytes` bytes is due for compaction into a snapshot
    /// that is now `snapshot_bytes` long.
    pub fn due(&self, records: u64, bytes: u64, snapshot_bytes: u64) -> bool {
        if bytes >= self.log_cap_bytes {
            return true;
        }
        let share = match self.snapshot_share {
            0 => true,
            n => bytes >= snapshot_bytes / n,
        };
        self.long(records, bytes) && share
    }

    /// A log of this length is long (`max_records` or `max_bytes`), whatever the snapshot's size.
    pub fn long(&self, records: u64, bytes: u64) -> bool {
        records >= self.max_records || bytes >= self.max_bytes
    }
}

/// How much an upgrade to this build's format is needed (see [`Journal::upgrade`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Upgrade {
    /// The library may go on as it is if it can't be upgraded now.
    May,
    /// The library can't be used as it is.
    Must,
    /// As [`Upgrade::Must`], for a library whose log was damaged: the snapshot must also have
    /// replaced that log.
    Repair,
}

/// What happened while loading.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadReport {
    /// `seq` of the snapshot that was loaded (0 = none).
    pub snapshot_seq: u64,
    /// Log records applied on top of the snapshot.
    pub replayed: usize,
    /// Records skipped because the snapshot already contained them.
    pub stale: usize,
    /// Records whose op failed to apply (should never happen; reported, not fatal).
    pub failed: usize,
    /// Bytes of a torn final record that were dropped.
    pub torn_bytes: u64,
    /// Partial records left mid-log by a failed append that was retried (written by builds that
    /// didn't cut the log back after a failed write): skipped, the retried records were replayed.
    pub torn_fragments: usize,
    /// The damaged log was preserved under this name.
    pub damaged: Option<String>,
    /// No catalog files existed (a new library).
    pub created: bool,
    /// The library was in an older catalog format (`0` = before format versions: a log without a
    /// snapshot) and was rewritten in this version's format ([`VERSION`]).
    pub upgraded_from: Option<u32>,
    /// What [`Catalog::repair`] changed in a damaged snapshot (empty for a sound one).
    pub repaired: Vec<String>,
    /// The files that upgrade replaced, kept beside the library under these names (the snapshot,
    /// then the log; a file there was none of has no copy).
    pub kept: Vec<String>,
    /// The library stated other defaults for develop settings than this build has (it was written
    /// by a build with other ones): its records were read over its own defaults, and it was
    /// rewritten with this build's.
    pub other_defaults: bool,
}

/// Where persistence time goes (reported by `library.info` → `persistence`; printed to stderr
/// per write under `LIGHTCRAFT_PROFILE`). Times are wall-clock milliseconds on the calling
/// thread, i.e. how long the caller (the UI thread, for the app) was blocked.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersistStats {
    /// [`Journal::append`] calls that wrote something.
    pub appends: u64,
    /// Encode + write + `sync_data` of the last / slowest append.
    pub last_append_ms: f64,
    pub max_append_ms: f64,
    /// Snapshots written (compactions, plus the ones on close / repair).
    pub snapshots: u64,
    /// The last snapshot, by stage.
    pub last_snapshot: SnapshotTiming,
    /// Total time of the slowest snapshot.
    pub max_snapshot_ms: f64,
    /// Longest time a snapshot blocked the caller (for background snapshots: copying the catalog
    /// and starting the worker, plus rewriting the log when it is done).
    pub max_blocking_ms: f64,
    /// A background snapshot is being written.
    pub snapshot_running: bool,
    /// Background snapshots that failed (the log was kept whole; retried later).
    pub failed_snapshots: u64,
}

/// One snapshot (compaction), by stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTiming {
    /// Serialising the catalog to JSON, streamed into the temp file (buffered writes included).
    pub serialize_ms: f64,
    /// Flushing, `sync_all` and the atomic rename of `catalog.snap`.
    pub write_sync_ms: f64,
    /// Resetting `catalog.log` (an atomic rewrite to empty, or to the records appended while a
    /// background snapshot ran; fsynced).
    pub reset_ms: f64,
    /// Start to finish (for a background snapshot: until [`Journal::poll`] saw it done).
    pub total_ms: f64,
    /// How long the caller was blocked (equal to `total_ms` unless `background`).
    pub blocking_ms: f64,
    /// Written by a worker thread.
    pub background: bool,
    /// Size of `catalog.snap`.
    pub bytes: u64,
    /// Log records the snapshot compacted.
    pub records: u64,
}

/// `LIGHTCRAFT_PROFILE` is set: print persistence timings to stderr.
fn profiling() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LIGHTCRAFT_PROFILE").is_some())
}

fn ms_since(t: web_time::Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

pub struct Journal {
    store: Box<dyn Store>,
    /// `seq` of the last durable op.
    seq: u64,
    snapshot_seq: u64,
    /// Size of `catalog.snap` (0: none yet).
    snapshot_bytes: u64,
    log_records: u64,
    log_bytes: u64,
    pub policy: SnapshotPolicy,
    stats: PersistStats,
    /// The background snapshot in flight.
    pending: Option<Pending>,
    /// After a failed background snapshot: don't retry before the log has this many records.
    retry_at_records: u64,
    /// A failed append may have left part of its records after `log_bytes`, and cutting them off
    /// failed too: the next append cuts the log back to `log_bytes` first.
    tail_dirty: bool,
}

/// A background snapshot in flight.
struct Pending {
    /// The snapshot holds the state after this op.
    seq: u64,
    /// Log bytes / records appended since it started (all `> seq`): the log once it's done.
    tail: Vec<u8>,
    tail_records: u64,
    /// Log records it compacts.
    records: u64,
    started: web_time::Instant,
    /// Time the caller spent starting it.
    start_ms: f64,
    worker: std::thread::JoinHandle<std::io::Result<SnapshotTiming>>,
}

fn io(e: std::io::Error) -> CatalogError {
    CatalogError::Io(e.to_string())
}

/// One log line (without the newline) for op number `seq`.
pub fn encode_record(seq: u64, op: &Op) -> String {
    let body = serde_json::to_string(op).unwrap_or_default();
    let crc = crc32fast::hash(body.as_bytes());
    format!("{{\"seq\":{seq},\"crc\":{crc},\"op\":{body}}}")
}

/// Parse one log line (without the newline). `None` if it's malformed, the CRC doesn't match, or
/// the op is one this version doesn't know (see [`Journal::open`]).
pub fn decode_record(line: &str) -> Option<(u64, Op)> {
    match decode_line(line) {
        Line::Record(seq, op) => Some((seq, op)),
        Line::Newer(_) | Line::Bad => None,
    }
}

/// One log line, decoded.
enum Line {
    Record(u64, Op),
    /// The CRC matches — the line is exactly what was written — but the op doesn't parse: it
    /// was written by a newer LightCraft (an op or field this version doesn't know), not damaged.
    Newer(u64),
    /// Malformed or a CRC mismatch: torn or damaged.
    Bad,
}

fn decode_line(line: &str) -> Line {
    let parts = || {
        let rest = line.strip_prefix("{\"seq\":")?;
        let (seq, rest) = rest.split_once(",\"crc\":")?;
        let (crc, rest) = rest.split_once(",\"op\":")?;
        let body = rest.strip_suffix('}')?;
        Some((seq.parse::<u64>().ok()?, crc.parse::<u32>().ok()?, body))
    };
    let Some((seq, crc, body)) = parts() else { return Line::Bad };
    if crc32fast::hash(body.as_bytes()) != crc {
        return Line::Bad;
    }
    match serde_json::from_str(body) {
        Ok(op) => Line::Record(seq, op),
        Err(_) => Line::Newer(seq),
    }
}

/// The error for a library written by a newer LightCraft.
fn newer(what: String) -> CatalogError {
    CatalogError::Newer(format!("{what}; this version reads catalog format v{VERSION} and older"))
}

/// A line that doesn't decode but ends with a whole record: a fragment of a failed append followed
/// by the record that retried it (the retry re-encodes the same ops with the same `seq`s). Returns
/// the fragment's length and the record, if that record continues the log (`seq <= last + 1`).
/// The record's CRC must match, so damage is never mistaken for it.
fn resync(line: &[u8], last: u64) -> Option<(usize, (u64, Op))> {
    const START: &[u8] = b"{\"seq\":";
    (1..line.len().saturating_sub(START.len() - 1)).filter(|&i| line[i..].starts_with(START)).find_map(|i| {
        let rec = std::str::from_utf8(&line[i..]).ok().map(|l| l.trim_end_matches('\r')).and_then(decode_record)?;
        (rec.0 <= last + 1).then_some((i, rec))
    })
}

/// Write `catalog.snap` for the state after op `seq`, streaming the JSON into the store (no
/// whole-file string). The bytes are exactly
/// `{"format":"lightcraft-catalog","version":1,"seq":N,"catalog":<serde_json of the catalog>}\n`,
/// as before streaming. Fills in the serialise / write+sync times and the size.
fn write_snapshot(store: &mut dyn Store, seq: u64, catalog: &Catalog) -> std::io::Result<SnapshotTiming> {
    let t0 = web_time::Instant::now();
    let mut serialize_ms = 0.0;
    let bytes = store.write_atomic_with(SNAPSHOT, &mut |w| {
        use std::io::Write;
        let t = web_time::Instant::now();
        // serde_json emits many tiny writes: buffer them in a concrete writer (inlined), so only
        // 1 MiB chunks go through the store's `dyn Write`
        let mut b = std::io::BufWriter::with_capacity(1 << 20, w);
        write!(b, "{}", snapshot_head(seq))?;
        serde_json::to_writer(&mut b, catalog).map_err(std::io::Error::other)?;
        b.write_all(b"}\n")?;
        b.flush()?;
        serialize_ms = ms_since(t);
        Ok(())
    })?;
    Ok(SnapshotTiming { serialize_ms, write_sync_ms: (ms_since(t0) - serialize_ms).max(0.0), bytes, ..Default::default() })
}

/// How `catalog.snap` starts for the state after op `seq`, up to the catalog itself: what it is,
/// and what a record's missing develop settings read as ([`crate::stored`]).
fn snapshot_head(seq: u64) -> String {
    format!("{{\"format\":\"{FORMAT}\",\"version\":{VERSION},\"seq\":{seq},\"defaults\":{},\"catalog\":", crate::stored::base().0)
}

/// Just the identification of `catalog.snap` (read before the catalog itself).
#[derive(serde::Deserialize)]
struct SnapHeader {
    format: String,
    version: u32,
    /// What a record's missing develop settings read as (from format 10 on).
    #[serde(default)]
    defaults: Option<serde_json::Value>,
}

/// The whole `catalog.snap` (after [`SnapHeader`] was checked; `format` and `version` are ignored).
#[derive(serde::Deserialize)]
struct SnapFile {
    seq: u64,
    catalog: Catalog,
}

impl Journal {
    /// Open (or create) the catalog in `store`: load the snapshot, replay the log, repair a torn
    /// tail. Fails if the snapshot is unreadable or a newer LightCraft's (then nothing is
    /// modified), or if the library is in an older format and can't be brought to this build's
    /// where it must be (see the module docs → *Format versions*).
    pub fn open(mut store: Box<dyn Store>) -> Result<(Journal, Catalog, LoadReport)> {
        let mut report = LoadReport::default();
        let snap = store.read(SNAPSHOT).map_err(io)?;
        let log = store.read(LOG).map_err(io)?;
        report.created = snap.is_none() && log.is_none();
        let snapshot_bytes = snap.as_ref().map_or(0, |b| b.len() as u64);
        // (dropped once the snapshot and the log are read: their records are read over the
        // library's own defaults where those are not this build's)
        let mut reading = None;
        // (the snapshot says what its records' missing develop settings read as: from format 10)
        let mut states_defaults = false;
        let (mut catalog, snapshot_seq, snapshot_version) = match &snap {
            Some(bytes) => {
                // the header first: a newer snapshot may not parse as this version's catalog
                let h: SnapHeader = serde_json::from_slice(bytes).map_err(|e| CatalogError::Corrupt(format!("{SNAPSHOT}: {e}")))?;
                if h.format != FORMAT {
                    return Err(CatalogError::Corrupt(format!("{SNAPSHOT}: not a LightCraft catalog (format {:?})", h.format)));
                }
                if h.version > VERSION {
                    return Err(newer(format!("{SNAPSHOT} is catalog format v{}", h.version)));
                }
                states_defaults = h.defaults.is_some();
                if let Some(theirs) = h.defaults.filter(|theirs| !crate::stored::reads_the_same(theirs)) {
                    log::info!("catalog: the library was written with other develop defaults; reading it over its own");
                    report.other_defaults = true;
                    reading = Some(crate::stored::reading_over(Some(std::sync::Arc::new(theirs))));
                }
                let s: SnapFile = serde_json::from_slice(bytes).map_err(|e| CatalogError::Corrupt(format!("{SNAPSHOT}: {e}")))?;
                let mut catalog = s.catalog;
                report.repaired = catalog.repair();
                for r in &report.repaired {
                    log::warn!("{SNAPSHOT}: {r}");
                }
                (catalog, s.seq, Some(h.version))
            }
            None => (Catalog::new(), 0, None),
        };
        report.snapshot_seq = snapshot_seq;
        let mut j = Journal {
            store,
            seq: snapshot_seq,
            snapshot_seq,
            snapshot_bytes,
            log_records: 0,
            log_bytes: 0,
            policy: SnapshotPolicy::default(),
            stats: PersistStats::default(),
            pending: None,
            retry_at_records: 0,
            tail_dirty: false,
        };
        let log = log.unwrap_or_default();

        // Scan records; `good_end` is the byte offset just past the last good record.
        let mut pos = 0usize;
        let mut good_end = 0usize;
        let mut damaged_at: Option<usize> = None;
        let mut needs_newline = false;
        while pos < log.len() {
            let (line_end, next, has_nl) = match log[pos..].iter().position(|b| *b == b'\n') {
                Some(i) => (pos + i, pos + i + 1, true),
                None => (log.len(), log.len(), false),
            };
            let line = std::str::from_utf8(&log[pos..line_end]).ok().map(|l| l.trim_end_matches('\r'));
            if line.is_some_and(|l| l.trim().is_empty()) {
                pos = next;
                good_end = next;
                continue;
            }
            let mut record = line.and_then(decode_record);
            if record.is_none()
                && let Some((at, r)) = resync(&log[pos..line_end], j.seq)
            {
                // the torn start of a failed append, then the retried records (older builds
                // appended them right after the fragment)
                log::warn!("catalog log: skipped a partial record ({at} bytes) left by a failed write");
                report.torn_fragments += 1;
                record = Some(r);
            }
            if record.is_none()
                && let Some(Line::Newer(seq)) = line.map(decode_line)
            {
                return Err(newer(format!("{LOG} record {seq} holds a change this version doesn't know")));
            }
            match record {
                Some((seq, op)) if seq <= j.seq => {
                    // already in the snapshot
                    let _ = op;
                    report.stale += 1;
                }
                Some((seq, op)) if seq == j.seq + 1 => {
                    if catalog.apply(op).is_err() {
                        report.failed += 1;
                    } else {
                        report.replayed += 1;
                    }
                    j.seq = seq;
                }
                _ => {
                    // bad record (or a gap): torn tail if nothing good follows, else damage
                    let mut rest_has_good = false;
                    for l in log[next..].split(|b| *b == b'\n') {
                        match std::str::from_utf8(l).map(|l| decode_line(l.trim_end_matches('\r'))) {
                            Ok(Line::Record(..)) => rest_has_good = true,
                            // refused before anything is modified
                            Ok(Line::Newer(seq)) => return Err(newer(format!("{LOG} record {seq} holds a change this version doesn't know"))),
                            Ok(Line::Bad) | Err(_) => {}
                        }
                    }
                    if rest_has_good {
                        damaged_at = Some(pos);
                    }
                    break;
                }
            }
            j.log_records += 1;
            pos = next;
            good_end = next;
            needs_newline = !has_nl;
        }

        drop(reading);
        // An older (or snapshot-less) library is brought to this build's format before anything
        // is appended: a snapshot in this format makes older builds refuse the library instead
        // of misreading ops or dropping fields they don't know.
        let older = (!report.created && snapshot_version != Some(VERSION)).then(|| snapshot_version.unwrap_or(0));
        // (what this open has written the library anew with: this build's format and defaults)
        let mut rewritten = false;
        if let Some(at) = damaged_at {
            let name = format!("{LOG}.damaged-{}", j.seq);
            j.store.write_atomic(&name, &log).map_err(io)?;
            log::warn!("catalog log damaged at byte {at}; kept as {name}; state recovered up to op {}", j.seq);
            report.damaged = Some(name);
            match older {
                // (its log goes either way: the snapshot it was is kept like any other upgrade's)
                Some(from) => {
                    report.kept = j.upgrade(&catalog, from, snap.as_deref(), &[], Upgrade::Repair)?.unwrap_or_default();
                    report.upgraded_from = Some(from);
                }
                None => j.snapshot(&catalog)?,
            }
            rewritten = true;
        } else {
            if good_end < log.len() {
                report.torn_bytes = (log.len() - good_end) as u64;
                log::warn!("catalog log: dropped a torn final record ({} bytes)", report.torn_bytes);
                j.store.truncate(LOG, good_end as u64).map_err(io)?;
                j.log_bytes = good_end as u64;
            } else if needs_newline {
                j.store.append(LOG, b"\n").map_err(io)?;
                j.log_bytes = good_end as u64 + 1;
            } else {
                j.log_bytes = good_end as u64;
            }
        }
        if report.created {
            // tiny: written directly, not counted as a compaction
            let empty = format!("{}{}}}\n", snapshot_head(0), catalog.to_snapshot());
            // (A new library that can't be written is not one to work in: what would be added to
            // its log leaves out defaults that nothing states.)
            j.store
                .write_atomic(SNAPSHOT, empty.as_bytes())
                .map_err(|e| CatalogError::Io(format!("can't write the new library's {SNAPSHOT}: {e}")))?;
            j.snapshot_bytes = empty.len() as u64;
        } else if let (Some(from), false) = (older, rewritten) {
            // A library that states its defaults and has this build's may go on as it is when it
            // can't be upgraded now (the next open tries again): what is appended to it reads the
            // same in every build that reads it. One that states none (formats before 9), or
            // other ones, must not be appended to as it is: a record written from here on leaves
            // out what equals this build's defaults.
            let how = if report.other_defaults || !states_defaults { Upgrade::Must } else { Upgrade::May };
            if let Some(kept) = j.upgrade(&catalog, from, snap.as_deref(), &log, how)? {
                rewritten = true;
                report.upgraded_from = Some(from);
                report.kept = kept;
            }
        }
        if report.other_defaults && !rewritten {
            // The same format, other defaults: the library must state this build's before
            // anything is appended. Nothing is kept for that: any build reads a library over
            // the defaults it states.
            j.snapshot(&catalog)?;
        }

        catalog.revision = 0;
        Ok((j, catalog, report))
    }

    /// Bring a library in format `from` to this build's: keep the files it was (`snap` and `log`,
    /// see [`Journal::keep_replaced`]), then write the snapshot. `Some`: upgraded, with the names
    /// of the files kept. `None` (only with [`Upgrade::May`]): not upgraded, and as it was, with
    /// nothing kept. Otherwise that is an error: the library can't be used as it is.
    ///
    /// The kept files stay unless the library on disk is known to be in its old format still:
    /// also when the snapshot was committed and something after it failed (emptying the log),
    /// and when what is on disk can't be read to say.
    fn upgrade(&mut self, catalog: &Catalog, from: u32, snap: Option<&[u8]>, log: &[u8], how: Upgrade) -> Result<Option<Vec<String>>> {
        let refused = |what: &str, e: String| CatalogError::Io(format!("the library is in an older format (v{from}) and {what}: {e}"));
        let kept = match self.keep_replaced(from, snap, log) {
            Ok(kept) => kept,
            Err(e) if how == Upgrade::May => {
                // not upgraded without its way back: it opens as it is; retried on next open
                log::warn!("catalog: can't keep the format v{from} files, so the library is not upgraded: {e}");
                return Ok(None);
            }
            Err(e) => return Err(refused("its files can't be kept before it is upgraded", e.to_string())),
        };
        if let Err(e) = self.snapshot(catalog) {
            match self.on_disk_version() {
                // Committed all the same (the log could not be emptied): the library is upgraded,
                // and its way back stays. Its log's records are in the snapshot and are passed
                // over when it is read. Not so a damaged log, which would be appended to after
                // its damage: the next open has to deal with it first.
                Some(Some(VERSION)) if how != Upgrade::Repair => log::warn!("catalog: upgraded to format v{VERSION}, but: {e}"),
                Some(Some(VERSION)) => return Err(refused("was upgraded, but its damaged log is still in place", e.to_string())),
                // what is on disk can't be read to say: the way back stays
                None => return Err(refused("it can't be told whether its upgrade was written", e.to_string())),
                // Nothing lost: the old files are intact and still load. No copy of them stays:
                // there is nothing to go back from.
                Some(_) => {
                    self.forget_kept(&kept);
                    if how != Upgrade::May {
                        return Err(refused("can't be upgraded", e.to_string()));
                    }
                    log::warn!("catalog: can't write the format v{VERSION} snapshot: {e}");
                    return Ok(None);
                }
            }
        }
        log::info!("catalog: upgraded from format v{from} to v{VERSION}; the files it was are kept as {kept:?}");
        Ok(Some(kept))
    }

    /// The format of the snapshot in the store: `Some(None)` where there is none, `None` where
    /// it can't be read to say.
    fn on_disk_version(&mut self) -> Option<Option<u32>> {
        match self.store.read(SNAPSHOT) {
            Ok(None) => Some(None),
            Ok(Some(bytes)) => serde_json::from_slice::<SnapHeader>(&bytes).ok().map(|h| Some(h.version)),
            Err(_) => None,
        }
    }

    /// Keep the files of a library in format `from` before an upgrade replaces them: `snap` and
    /// `log` as they were read, under names that say their format and the last change they hold.
    /// Returns the names (none where the store keeps none). All or nothing: on an error, what was
    /// written is removed again.
    fn keep_replaced(&mut self, from: u32, snap: Option<&[u8]>, log: &[u8]) -> std::io::Result<Vec<String>> {
        if !self.store.keeps_replaced_formats() {
            return Ok(Vec::new());
        }
        let seq = self.seq;
        let files = [(SNAPSHOT, snap), (LOG, (!log.is_empty()).then_some(log))];
        let mut kept = Vec::new();
        for (name, bytes) in files {
            let Some(bytes) = bytes else { continue };
            let name = format!("{name}.v{from}-{seq}");
            if let Err(e) = self.store.write_atomic(&name, bytes) {
                self.forget_kept(&kept);
                return Err(e);
            }
            kept.push(name);
        }
        Ok(kept)
    }

    /// Remove kept files that keep nothing (the upgrade they were for did not happen).
    fn forget_kept(&mut self, kept: &[String]) {
        for name in kept {
            if let Err(e) = self.store.remove(name) {
                log::warn!("catalog: can't remove {name}: {e}");
            }
        }
    }

    /// Append ops (already applied to the live catalog) durably, in order.
    pub fn append(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let t0 = web_time::Instant::now();
        let mut buf = String::new();
        let mut seq = self.seq;
        for op in ops {
            seq += 1;
            buf.push_str(&encode_record(seq, op));
            buf.push('\n');
        }
        if self.tail_dirty {
            self.store.truncate(LOG, self.log_bytes).map_err(io)?;
            self.tail_dirty = false;
        }
        if let Err(e) = self.store.append(LOG, buf.as_bytes()) {
            // Part of the batch may be in the file. Cut it back to the last whole record, so the
            // retry (same seqs) starts on a clean line instead of after a fragment that would
            // read back as damage.
            if let Err(t) = self.store.truncate(LOG, self.log_bytes) {
                log::error!("catalog: can't cut the log back after a failed append: {t}");
                self.tail_dirty = true;
            }
            return Err(io(e));
        }
        if let Some(p) = self.pending.as_mut() {
            p.tail.extend_from_slice(buf.as_bytes());
            p.tail_records += ops.len() as u64;
        }
        self.seq = seq;
        self.log_records += ops.len() as u64;
        self.log_bytes += buf.len() as u64;
        let ms = ms_since(t0);
        self.stats.appends += 1;
        self.stats.last_append_ms = ms;
        self.stats.max_append_ms = self.stats.max_append_ms.max(ms);
        if profiling() {
            eprintln!("catalog: append {} op(s), {} B: {ms:.2} ms", ops.len(), buf.len());
        }
        Ok(())
    }

    /// Write a snapshot of `catalog` (which must reflect every appended op) and reset the log.
    pub fn snapshot(&mut self, catalog: &Catalog) -> Result<()> {
        self.snapshot_with_unlogged(catalog, 0)
    }

    /// [`Journal::snapshot`] of a catalog that also reflects `unlogged` ops applied after the
    /// last appended one but never written to the log (their append failed). The snapshot counts
    /// them (it holds the state after op `seq + unlogged`), so they are saved and must not be
    /// appended afterwards. Once the snapshot is durable [`Journal::seq`] includes them, even if
    /// resetting the log then fails (the error is still returned).
    pub fn snapshot_with_unlogged(&mut self, catalog: &Catalog, unlogged: u64) -> Result<()> {
        // never two snapshot writers; the one in flight is older, so it must land first
        if let Err(e) = self.wait() {
            log::warn!("catalog: background snapshot failed ({e}); writing one now");
        }
        let t0 = web_time::Instant::now();
        let seq = self.seq + unlogged;
        let mut timing = match write_snapshot(self.store.as_mut(), seq, catalog) {
            Ok(t) => t,
            Err(e) => {
                // nothing lost (the log is whole); don't retry on every append
                self.stats.failed_snapshots += 1;
                self.retry_at_records = self.log_records + self.policy.max_records;
                return Err(io(e));
            }
        };
        // the snapshot is durable: it holds every op up to `seq`
        self.seq = seq;
        self.snapshot_seq = seq;
        self.snapshot_bytes = timing.bytes;
        let t2 = web_time::Instant::now();
        // A crash here leaves old records in the log; they are skipped by seq on load.
        self.store.write_atomic(LOG, b"").map_err(io)?;
        timing.reset_ms = ms_since(t2);
        timing.total_ms = ms_since(t0);
        timing.blocking_ms = timing.total_ms;
        timing.records = self.log_records;
        self.record_snapshot(timing);
        self.log_records = 0;
        self.log_bytes = 0;
        self.retry_at_records = 0;
        self.tail_dirty = false;
        Ok(())
    }

    /// Compact like [`Journal::snapshot`], but write the snapshot on a worker thread from a copy
    /// of `catalog` (cheap: photos are shared) when the store supports it (else synchronously).
    /// `catalog` must reflect every appended op. Appends go on meanwhile; [`Journal::poll`]
    /// finishes the compaction. No-op while a snapshot is already in flight.
    pub fn snapshot_in_background(&mut self, catalog: &Catalog) -> Result<()> {
        if self.pending.is_some() {
            return Ok(());
        }
        let t0 = web_time::Instant::now();
        let Some(mut writer) = self.store.background_writer() else { return self.snapshot(catalog) };
        let copy = catalog.clone();
        let seq = self.seq;
        let spawned = std::thread::Builder::new().name("catalog-snapshot".into()).spawn(move || write_snapshot(writer.as_mut(), seq, &copy));
        let worker = match spawned {
            Ok(w) => w,
            Err(e) => {
                log::warn!("catalog: can't start a snapshot thread ({e}); writing it now");
                return self.snapshot(catalog);
            }
        };
        self.pending =
            Some(Pending { seq, tail: Vec::new(), tail_records: 0, records: self.log_records, started: t0, start_ms: ms_since(t0), worker });
        Ok(())
    }

    /// Finish a background snapshot if its worker is done (cheap otherwise). Returns whether one
    /// finished. An error means the snapshot failed: nothing was lost (the log is whole), and
    /// compaction is retried once the log has grown by another [`SnapshotPolicy::max_records`].
    pub fn poll(&mut self) -> Result<bool> {
        if !self.pending.as_ref().is_some_and(|p| p.worker.is_finished()) {
            return Ok(false);
        }
        self.finish().map(|()| true)
    }

    /// Wait for the background snapshot in flight, if any, and finish it.
    pub fn wait(&mut self) -> Result<()> {
        if self.pending.is_none() {
            return Ok(());
        }
        self.finish()
    }

    /// A background snapshot is in flight (until [`Journal::poll`] or [`Journal::wait`] finish it).
    pub fn snapshot_running(&self) -> bool {
        self.pending.is_some()
    }

    /// The background worker is done (its result not yet applied by [`Journal::poll`]).
    pub fn snapshot_written(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| p.worker.is_finished())
    }

    /// Join the worker; on success replace the log with the records appended meanwhile.
    fn finish(&mut self) -> Result<()> {
        let Some(p) = self.pending.take() else { return Ok(()) };
        let t0 = web_time::Instant::now();
        let written = p.worker.join().unwrap_or_else(|_| Err(std::io::Error::other("snapshot thread panicked")));
        let mut timing = match written {
            Ok(t) => t,
            Err(e) => {
                // the log still holds every record: nothing lost, retry later
                self.stats.failed_snapshots += 1;
                self.retry_at_records = self.log_records + self.policy.max_records;
                log::error!("catalog: background snapshot failed: {e}");
                return Err(io(e));
            }
        };
        // `catalog.snap` (op `p.seq`) is durable; the log's records `<= p.seq` are now stale.
        // Keep exactly the ones appended since (a crash before this rewrite skips the stale ones).
        let t1 = web_time::Instant::now();
        self.snapshot_seq = p.seq;
        self.snapshot_bytes = timing.bytes;
        if let Err(e) = self.store.write_atomic(LOG, &p.tail) {
            // the log stays whole (stale records are skipped on load); compact again later
            self.retry_at_records = self.log_records + self.policy.max_records;
            log::error!("catalog: can't trim the log after a snapshot: {e}");
            return Err(io(e));
        }
        timing.reset_ms = ms_since(t1);
        timing.total_ms = ms_since(p.started);
        timing.blocking_ms = p.start_ms + ms_since(t0);
        timing.background = true;
        timing.records = p.records;
        self.record_snapshot(timing);
        self.log_records = p.tail_records;
        self.log_bytes = p.tail.len() as u64;
        self.retry_at_records = 0;
        self.tail_dirty = false;
        Ok(())
    }

    fn record_snapshot(&mut self, t: SnapshotTiming) {
        self.snapshot_bytes = t.bytes;
        self.stats.snapshots += 1;
        self.stats.last_snapshot = t;
        self.stats.max_snapshot_ms = self.stats.max_snapshot_ms.max(t.total_ms);
        self.stats.max_blocking_ms = self.stats.max_blocking_ms.max(t.blocking_ms);
        if profiling() {
            eprintln!(
                "catalog: {}snapshot of {} records, {} B: serialize {:.1} ms, write+sync {:.1} ms, log reset {:.1} ms, total {:.1} ms, blocking {:.1} ms",
                if t.background { "background " } else { "" },
                t.records,
                t.bytes,
                t.serialize_ms,
                t.write_sync_ms,
                t.reset_ms,
                t.total_ms,
                t.blocking_ms
            );
        }
    }

    /// Where persistence time went so far.
    pub fn stats(&self) -> PersistStats {
        PersistStats { snapshot_running: self.pending.is_some(), ..self.stats }
    }

    /// The log is long enough to be worth compacting.
    pub fn wants_snapshot(&self) -> bool {
        self.pending.is_none() && self.log_records >= self.retry_at_records && self.policy.due(self.log_records, self.log_bytes, self.snapshot_bytes)
    }

    /// Whether closing the library should compact the log: there is one, and the snapshot is
    /// small enough to rewrite in passing ([`SnapshotPolicy::close_bytes`]) or the log is long
    /// ([`SnapshotPolicy::long`]; during a session it may grow further, to its share of the
    /// snapshot, but it isn't left that long between sessions). Nothing is lost either way: what
    /// isn't compacted stays in the log and is replayed on open.
    pub fn wants_snapshot_on_close(&self) -> bool {
        self.log_records > 0 && (self.snapshot_bytes <= self.policy.close_bytes || self.policy.long(self.log_records, self.log_bytes))
    }

    /// Size of `catalog.snap` as last written or loaded (0: none yet).
    pub fn snapshot_bytes(&self) -> u64 {
        self.snapshot_bytes
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }
    pub fn snapshot_seq(&self) -> u64 {
        self.snapshot_seq
    }
    pub fn log_records(&self) -> u64 {
        self.log_records
    }
    pub fn log_bytes(&self) -> u64 {
        self.log_bytes
    }
    pub fn describe(&self) -> String {
        self.store.describe()
    }
}

impl Drop for Journal {
    /// Wait for a background snapshot (never leave a writer behind that could replace a newer
    /// snapshot of a journal reopened on the same files). The log is left whole: the snapshot's
    /// records in it are skipped as stale on the next load.
    fn drop(&mut self) {
        if let Some(p) = self.pending.take()
            && let Ok(Err(e)) = p.worker.join()
        {
            log::error!("catalog: background snapshot failed: {e}");
        }
    }
}
