//! Crash-safe catalog persistence: an append-only op log plus periodic snapshots.
//!
//! Files (in a [`Store`]):
//! - `catalog.snap` — `{"format":"lightcraft-catalog","version":V,"seq":N,"defaults":{…},
//!   "catalog":{…},"segments":[…]}`, the state after op `N` in catalog format `V` ([`VERSION`]); replaced
//!   atomically (temp + fsync + rename). The catalog's photos are in the segment files it lists
//!   (`segments/….json`, see [`crate::segments`]), or in `catalog.snap` itself where the store has
//!   no segments;
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
//! At most one snapshot is in flight; a synchronous [`Journal::snapshot`] and dropping the journal
//! first wait for it, so an older snapshot can never replace a newer one.
//!
//! # Format versions
//!
//! The snapshot's `version` is the catalog format, and it covers the log beside it: the log has
//! no header of its own, and is only ever appended to under a snapshot that states the defaults
//! its records leave out (this build's format, or one from 10 on with this build's defaults).
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
//!   (Except where the upgrade can't be written or its files can't be kept: see the next rule.)
//!   [`LoadReport::upgraded_from`] says when the upgrade happened at open.
//! - **An upgrade keeps what it replaces**: older builds can't read the upgraded library, so the
//!   files it was are first kept beside it as `catalog.snap.v<format>-<seq>` and
//!   `catalog.log.v<format>-<seq>` (`seq`: the last change they hold), byte for byte
//!   ([`LoadReport::kept`]; a damaged log is kept under its own name instead). Put back under
//!   their own names, they are the library as the older build left it (what a later snapshot
//!   keeps is the library as it was at open: a torn tail already cut, and what a session of this
//!   build appended while it stayed in its old format, which an older build may refuse). They stay unless the
//!   library on disk is known to be in its old format still: an upgrade at open that did not
//!   happen leaves no copy, and one that was written keeps its copy whatever fails after it.
//!   Nothing removes them afterwards: they are the user's to delete. (Not where the store says
//!   there is no going back, [`Store::keeps_replaced_formats`].)
//! - **A library that can't be upgraded is not opened**, where appending to it as it is would
//!   not be safe: one in a format before 10, or one that states other defaults than this build's
//!   ([`crate::stored`]). A record written by this build leaves out what equals this build's
//!   defaults, which a build of a format before 10 reads as its own. It is left as it was, and the
//!   error says why. (A library in format 10 or later, which writes records so and states its
//!   defaults, may go on as it is when it can't be upgraded now: a later snapshot, compaction or
//!   closing, upgrades it, first keeps the files as they were at open (the snapshot, and the log
//!   up to where it ended then) and is not written when it can't be (a change that could not be
//!   appended to the log is then saved in the format on disk, in one file, so none is lost);
//!   those files stay even if that snapshot then fails and the library is still in its old
//!   format. The next open tries again.)
//! - **A newer format is refused, untouched**: a snapshot whose `version` is above [`VERSION`],
//!   or a log record whose CRC matches (so it is exactly what was written) but whose op doesn't
//!   parse, fails [`Journal::open`] with [`CatalogError::Newer`] before any file is modified — it
//!   is never mistaken for a torn tail or damage. (Builds up to v0.2.0 refuse a v2 snapshot with
//!   "unsupported format".)

use crate::segments::{self, Photos, Segment};
use crate::store::Store;
use crate::{Catalog, CatalogError, Op, PhotoId, Result};

pub const SNAPSHOT: &str = "catalog.snap";
pub const LOG: &str = "catalog.log";
pub use crate::segments::{SEGMENT_DIR, SEGMENT_PREFIX, SEGMENT_SUFFIX, segment_file};
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
    format(11, "the snapshot in segments", FIELDS, "`catalog.snap` lists segment files that hold the photos (`crate::segments`)"),
];

/// The catalog format this build writes (and the newest it reads): the newest of [`FORMATS`]
/// that is [`Format::here`].
pub const VERSION: u32 = newest(FORMATS);

/// The first format whose records leave out what equals this build's defaults ([`crate::stored`]).
const COMPACT_RECORDS: u32 = 10;

const fn newest(formats: &[Format]) -> u32 {
    match formats.split_last() {
        Some((f, _)) if f.here => f.version,
        Some((_, rest)) => newest(rest),
        None => 0,
    }
}

/// When [`Journal::wants_snapshot`] says it's time to compact the log.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotPolicy {
    pub max_records: u64,
    pub max_bytes: u64,
    /// Photos per segment (see [`crate::segments`]): a segment that changed is written in pieces
    /// of this many once it holds more than twice as many.
    pub segment_photos: usize,
    /// And about this many bytes: where records are large (edited photos with their history), a
    /// segment holds fewer photos, down to 64. 0: any size.
    pub segment_bytes: u64,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        SnapshotPolicy {
            max_records: 2000,
            max_bytes: 16 << 20,
            // (measured: docs/catalog-persistence.md, "Segment size")
            segment_photos: 4096,
            segment_bytes: 2 << 20,
        }
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
    /// The library's stated defaults lack a setting this build has (it was written by a build
    /// that didn't know it): they are restated at open, before any compact record is appended;
    /// where that can't be written, records state develop settings whole until a snapshot is.
    pub defaults_added: bool,
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
    /// Segment files the snapshot lists, and the size of the snapshot with them.
    pub segments: u64,
    pub snapshot_bytes: u64,
}

/// One snapshot (compaction), by stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotTiming {
    /// Writing the segments that changed (serialised, fsynced and, when a one-file library is
    /// split, read back).
    pub serialize_ms: f64,
    /// Writing `catalog.snap` (serialised, `sync_all`, the atomic rename) and removing the
    /// segment files it no longer lists.
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
    /// Bytes written: `catalog.snap` and the segments that changed.
    pub bytes: u64,
    /// Segments written.
    pub segments: u64,
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
    /// Size of `catalog.snap` and its segments together (0: none yet), and of `catalog.snap`.
    snapshot_bytes: u64,
    /// The snapshot in the store is still in an older format (`Some(format)`): the library was
    /// not upgraded at open, because its files could not be kept or the upgrade could not be
    /// written. A later snapshot (a compaction, or closing) writes the upgrade then,
    /// and keeps the files first ([`Journal::keep_before_upgrade`]).
    behind: Option<u32>,
    /// What the library was at open while `behind` is set: the `seq` its files held and the
    /// length of its log. Appends only add to the log after that, so the first `log_len` bytes
    /// and the snapshot are the library as this session opened it, whatever is appended
    /// meanwhile (an earlier session of this build may have appended already, while the library
    /// stayed in its old format: that is in them, and an older build may refuse it). After
    /// changes were saved in the old format ([`Journal::save_in_old_format`]) it is the `seq`
    /// that snapshot holds and an empty log: its log was reset.
    behind_at: (u64, u64),
    /// The files [`Journal::keep_before_upgrade`] kept for a snapshot that is not written yet,
    /// and, once that snapshot is, the names for [`Journal::take_kept`].
    kept_pending: Option<Vec<String>>,
    kept_written: Vec<String>,
    head_bytes: u64,
    /// The segments `catalog.snap` lists, and the photos they hold (see [`crate::segments`]).
    segments: Vec<Segment>,
    baseline: Photos,
    /// `catalog.snap` itself holds photos: a one-file library (formats up to 10, or written
    /// where there are no segments).
    inline_photos: bool,
    /// The snapshot read when the library was opened listed segments (see
    /// [`Journal::remove_unlisted_segments`]).
    opened_with_segments: bool,
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
    /// The snapshot's header doesn't state every setting this build has (it was written by a
    /// build that didn't know one, and restating it failed at open): develop settings are
    /// written whole in every record ([`crate::stored::writing_full`]) until a snapshot, which
    /// states them all, is written. Never set where compact records would be unsafe for another
    /// reason (other defaults, an older format that can't be upgraded refuse the library).
    header_incomplete: bool,
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
    /// Returns how it went, with the photos on disk then: the ones it wrote, or after a failure
    /// the ones that were there before.
    worker: std::thread::JoinHandle<(std::io::Result<Saved>, Photos)>,
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

/// The most `resync` spends re-validating the suffixes of one line whose whole bytes are not valid
/// UTF-8 (a real fragment ends at any byte): one line of a modest malformed log that holds a start
/// of a record in every few bytes used to re-validate the whole remaining suffix at every attempt
/// (CWE-407). Past the budget the rest of the line stays a torn tail, which the damaged path keeps
/// and reports, same state as before, minus the CPU.
const RESYNC_BUDGET_BYTES: usize = 64 << 20; // 64 MiB

/// A line that doesn't decode but ends with a whole record: a fragment of a failed append followed
/// by the record that retried it (the retry re-encodes the same ops with the same `seq`s). Returns
/// the fragment's length and the record, if that record continues the log (`seq <= last + 1`).
/// The record's CRC must match, so damage is never mistaken for it.
fn resync(line: &[u8], last: u64) -> Option<(usize, (u64, Op))> {
    const START: &[u8] = b"{\"seq\":";
    // One record per line, so a line of valid UTF-8 holds a valid suffix at every record start and
    // one validation covers every candidate; only a line that is not wholly valid re-checks, and
    // then up to the budget rather than the suffix length times the candidate count.
    let whole = std::str::from_utf8(line).ok();
    let mut spent = 0usize;
    (1..line.len().saturating_sub(START.len() - 1)).filter(|&i| line[i..].starts_with(START)).find_map(|i| {
        let text = match &whole {
            Some(t) => &t[i..],
            None => {
                if spent >= RESYNC_BUDGET_BYTES {
                    return None;
                }
                spent = spent.saturating_add(line.len() - i);
                std::str::from_utf8(&line[i..]).ok()?
            }
        };
        let rec = decode_record(text.trim_end_matches('\r'))?;
        (rec.0 <= last.saturating_add(1)).then_some((i, rec))
    })
}

/// What a snapshot is written of, and against.
struct Job<'a> {
    /// The snapshot is of the state after this op.
    seq: u64,
    catalog: &'a Catalog,
    /// The segments on disk and the photos they hold.
    segments: &'a [Segment],
    baseline: &'a Photos,
    /// How large segments are to be.
    size: segments::Size,
    /// The library is in one file: read the segments it is split into back before committing.
    verify: bool,
}

/// A snapshot that was written.
struct Saved {
    timing: SnapshotTiming,
    /// The segments it lists.
    segments: Vec<Segment>,
    /// Size of `catalog.snap`.
    head_bytes: u64,
    /// `catalog.snap` holds the photos itself (the store has no segments).
    inline_photos: bool,
    /// The stretches of ids whose photos it wrote (see [`segments::refresh`]).
    changed: Vec<(std::ops::Bound<PhotoId>, std::ops::Bound<PhotoId>)>,
}

/// Write the snapshot of `job`: the segments whose photos changed, then `catalog.snap`, which
/// lists the segments and is the commit (replaced atomically); then the segment files it no
/// longer lists are removed. Where the store has no segments, `catalog.snap` holds the photos
/// itself. An error leaves the snapshot before this one as it was.
fn write_snapshot(store: &mut dyn Store, job: &Job<'_>) -> std::io::Result<Saved> {
    let t0 = web_time::Instant::now();
    let segmented = store.segmented();
    let written = if segmented {
        let on_disk =
            segments::OnDisk { version: VERSION, seq: job.seq, segments: job.segments, baseline: job.baseline, size: job.size, verify: job.verify };
        Some(segments::write(store, job.catalog.photo_map(), &on_disk)?)
    } else {
        None
    };
    // Every segment the snapshot will list is still there, the ones kept as well as the ones
    // written: a program that opened the library without its lock may have removed files it took
    // for unused, and committing a snapshot that lists a file that is gone would leave a library
    // that doesn't open. Only the files written for this snapshot are removed again.
    if let Some(w) = &written {
        let listed: Vec<String> = w.segments.iter().map(|s| s.name.clone()).collect();
        if !segments::present(store, &listed) {
            segments::discard(store, &w.new);
            return Err(std::io::Error::other(
                "a segment was removed while the snapshot was being written (is the library open in another program?)",
            ));
        }
    }
    let serialize_ms = ms_since(t0);
    let head;
    let (body, listed): (&Catalog, &[Segment]) = match &written {
        Some(w) => {
            head = job.catalog.without_photos();
            (&head, &w.segments)
        }
        None => (job.catalog, &[]),
    };
    let seq = job.seq;
    let head_bytes = store.write_atomic_with(SNAPSHOT, &mut |w| {
        use std::io::Write;
        // serde_json emits many tiny writes: buffer them in a concrete writer (inlined), so only
        // 1 MiB chunks go through the store's `dyn Write`
        let mut b = std::io::BufWriter::with_capacity(1 << 20, w);
        write!(b, "{}", snapshot_head(seq))?;
        serde_json::to_writer(&mut b, body).map_err(std::io::Error::other)?;
        b.write_all(b",\"segments\":")?;
        serde_json::to_writer(&mut b, listed).map_err(std::io::Error::other)?;
        writeln!(b, ",\"photos\":{}}}", job.catalog.len())?;
        b.flush()
    });
    let head_bytes = match head_bytes {
        Ok(n) => n,
        Err(e) => {
            // Not committed: the segments written for it are no snapshot's. (Unless the
            // `catalog.snap` in the store lists them after all, or can't be read to say: a
            // replace that failed after it took effect. Then they stay.)
            if let Some(w) = &written
                && !may_list(store, &w.new)
            {
                segments::discard(store, &w.new);
            }
            return Err(e);
        }
    };
    // committed: what the snapshot before listed and this one doesn't is no longer needed
    match &written {
        Some(w) => segments::discard(store, &w.obsolete),
        None => segments::discard(store, &job.segments.iter().map(|s| s.name.clone()).collect::<Vec<_>>()),
    }
    let (segments, count, bytes, changed) = written.map_or((Vec::new(), 0, 0, Vec::new()), |w| (w.segments, w.written, w.bytes, w.changed));
    let timing = SnapshotTiming {
        serialize_ms,
        write_sync_ms: (ms_since(t0) - serialize_ms).max(0.0),
        bytes: head_bytes + bytes,
        segments: count,
        ..Default::default()
    };
    Ok(Saved { timing, segments, head_bytes, inline_photos: !segmented && !job.catalog.is_empty(), changed })
}

/// The highest `seq` a snapshot may hold; beyond it the counter is treated as corrupt, far from overflow.
const MAX_SEQ: u64 = u64::MAX / 2;

/// How `catalog.snap` starts for the state after op `seq`, up to the catalog itself: what it is,
/// and what a record's missing develop settings read as ([`crate::stored`]).
fn snapshot_head(seq: u64) -> String {
    snapshot_head_in(VERSION, seq)
}

/// [`snapshot_head`] of a snapshot in format `version`.
fn snapshot_head_in(version: u32, seq: u64) -> String {
    format!("{{\"format\":\"{FORMAT}\",\"version\":{version},\"seq\":{seq},\"defaults\":{},\"catalog\":", crate::stored::base().0)
}

/// Write the snapshot of the state after op `seq` as one file in the older format `version`
/// (10: the photos in `catalog.snap`, no segments), as a build of that format wrote it. Used
/// where the upgrade can't be done (the files of that format can't be kept, or the new snapshot
/// can't be written), so that the library is not upgraded, and what is only in memory is still
/// saved. An error leaves the snapshot before this one as it was.
fn write_one_file_snapshot(store: &mut dyn Store, seq: u64, version: u32, catalog: &Catalog) -> std::io::Result<Saved> {
    let t0 = web_time::Instant::now();
    let mut serialize_ms = 0.0;
    let head_bytes = store.write_atomic_with(SNAPSHOT, &mut |w| {
        use std::io::Write;
        let t = web_time::Instant::now();
        let mut b = std::io::BufWriter::with_capacity(1 << 20, w);
        write!(b, "{}", snapshot_head_in(version, seq))?;
        serde_json::to_writer(&mut b, catalog).map_err(std::io::Error::other)?;
        b.write_all(b"}\n")?;
        b.flush()?;
        serialize_ms = ms_since(t);
        Ok(())
    })?;
    let timing = SnapshotTiming { serialize_ms, write_sync_ms: (ms_since(t0) - serialize_ms).max(0.0), bytes: head_bytes, ..Default::default() };
    Ok(Saved { timing, segments: Vec::new(), head_bytes, inline_photos: !catalog.is_empty(), changed: Vec::new() })
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
    /// The segments that hold the catalog's photos (none: `catalog` holds them itself).
    #[serde(default)]
    segments: Vec<Segment>,
    /// How many photos the catalog has (format 11 on): a list of segments that doesn't add up to
    /// this is not the one that was written.
    #[serde(default)]
    photos: Option<u64>,
}

/// Just the segments `catalog.snap` lists.
#[derive(serde::Deserialize)]
struct SnapSegments {
    #[serde(default)]
    segments: Vec<Segment>,
}

/// The segments the `catalog.snap` in the store lists (none when there is no such file); `None`
/// when it can't be read to say.
fn listed_on_disk(store: &mut dyn Store) -> Option<Vec<Segment>> {
    match store.read(SNAPSHOT) {
        Ok(Some(bytes)) => serde_json::from_slice::<SnapSegments>(&bytes).ok().map(|snap| snap.segments),
        Ok(None) => Some(Vec::new()),
        Err(_) => None,
    }
}

/// Whether the `catalog.snap` in the store lists any of `names`, or can't be read to say.
fn may_list(store: &mut dyn Store, names: &[String]) -> bool {
    listed_on_disk(store).is_none_or(|listed| listed.iter().any(|s| names.contains(&s.name)))
}

/// Whether `log` starts with the catalog's first change: then it holds the whole catalog.
fn log_from_the_start(log: Option<&[u8]>) -> bool {
    let first = log.unwrap_or_default().split(|b| *b == b'\n').find(|l| !l.iter().all(u8::is_ascii_whitespace));
    first.and_then(|l| std::str::from_utf8(l).ok()).and_then(|l| decode_record(l.trim_end_matches('\r'))).is_some_and(|(seq, _)| seq == 1)
}

impl Journal {
    /// Open (or create) the catalog in `store`: load the snapshot, replay the log, repair a torn
    /// tail. Fails if the snapshot is unreadable or a newer LightCraft's (then nothing is
    /// modified), or if the library is in an older format and can't be brought to this build's
    /// where it must be (its damaged log can't be replaced, its snapshot can't be kept, or it can't
    /// be told whether its upgrade was written: see the module docs → *Format versions*), or if
    /// the library states other develop defaults than this build's and its rewrite with this
    /// build's can't be written, or its log can't then be emptied, or if a new library's first
    /// snapshot can't be written.
    pub fn open(mut store: Box<dyn Store>) -> Result<(Journal, Catalog, LoadReport)> {
        let mut report = LoadReport::default();
        let snap = store.read(SNAPSHOT).map_err(io)?;
        let log = store.read(LOG).map_err(io)?;
        report.created = snap.is_none() && log.is_none();
        // Segment files without the snapshot that lists them (a folder copied or synced in part,
        // a `catalog.snap` that was removed): they hold the photos, and opening this as a new
        // library would abandon them. Not so when the log holds every change from the first: a
        // library that never had a snapshot, whose first one got as far as writing segments.
        if snap.is_none()
            && store.segmented()
            && !log_from_the_start(log.as_deref())
            && let n @ 1.. = segments::files(store.as_mut()).map_err(io)?
        {
            return Err(CatalogError::Corrupt(format!(
                "{SNAPSHOT} is missing, but {n} segment file(s) of a catalog are here ({SEGMENT_PREFIX}…{SEGMENT_SUFFIX})"
            )));
        }
        let head_bytes = snap.as_ref().map_or(0, |b| b.len() as u64);
        let mut listed = Vec::new();
        let mut stated = None;
        // (dropped once the snapshot, its segments and the log are read: their records are read
        // over the library's own defaults where those are not this build's)
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
                if h.defaults.as_ref().is_some_and(|theirs| !crate::stored::states_every_setting(theirs)) {
                    report.defaults_added = true;
                }
                if let Some(theirs) = h.defaults.filter(|theirs| !crate::stored::reads_the_same(theirs)) {
                    log::info!("catalog: the library was written with other develop defaults; reading it over its own");
                    report.other_defaults = true;
                    reading = Some(crate::stored::reading_over(Some(std::sync::Arc::new(theirs))));
                }
                let s: SnapFile = serde_json::from_slice(bytes).map_err(|e| CatalogError::Corrupt(format!("{SNAPSHOT}: {e}")))?;
                // a seq this high can't come from real use; the next record's seq would wrap
                if s.seq > MAX_SEQ {
                    return Err(CatalogError::Corrupt(format!("{SNAPSHOT}: seq {} is out of range", s.seq)));
                }
                listed = s.segments;
                stated = s.photos;
                (s.catalog, s.seq, Some(h.version))
            }
            None => (Catalog::new(), 0, None),
        };
        report.snapshot_seq = snapshot_seq;
        // the photos: in `catalog.snap` itself (a one-file library), or in the segments it lists
        let inline_photos = !catalog.is_empty();
        let mut baseline = Photos::new();
        if !listed.is_empty() {
            if inline_photos {
                return Err(CatalogError::Corrupt(format!("{SNAPSHOT}: holds photos and lists segments")));
            }
            let readers = segments::readers(store.as_ref(), listed.len());
            let photos = segments::load(store.as_mut(), readers, &listed).map_err(|e| match e.kind() {
                std::io::ErrorKind::InvalidData => CatalogError::Corrupt(e.to_string()),
                _ => io(e),
            })?;
            if store.segmented() && !report.other_defaults {
                // (shared with the catalog: pointers, not copies. Not where the segments were
                // written with other defaults: none of them is what this build would write, so
                // none is what the next snapshot may keep.)
                baseline = photos.clone();
            }
            catalog.set_photos(photos);
        }
        if stated.is_some_and(|n| n != catalog.len() as u64) {
            return Err(CatalogError::Corrupt(format!(
                "{SNAPSHOT}: its segments hold {} photos where it states {}",
                catalog.len(),
                stated.unwrap_or(0)
            )));
        }
        // (once the photos are in: on a library in segments `catalog.snap` holds none, and its
        // stacks would read as stacks of missing photos)
        report.repaired = catalog.repair();
        for r in &report.repaired {
            log::warn!("{SNAPSHOT}: {r}");
        }
        let snapshot_bytes = head_bytes + listed.iter().map(|s| s.bytes).sum::<u64>();
        let mut j = Journal {
            store,
            seq: snapshot_seq,
            snapshot_seq,
            snapshot_bytes,
            behind: None,
            behind_at: (0, 0),
            kept_pending: None,
            kept_written: Vec::new(),
            header_incomplete: false,
            head_bytes,
            opened_with_segments: !listed.is_empty(),
            segments: listed,
            baseline,
            inline_photos,
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
                Some((seq, op)) if j.seq.checked_add(1) == Some(seq) => {
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
            j.trim_log_tail(&log, good_end, needs_newline, &mut report)?;
        }
        if report.created {
            // tiny: written directly, not counted as a compaction
            let empty = format!("{}{}}}\n", snapshot_head(0), catalog.to_snapshot());
            // (A new library that can't be written is not one to work in: what would be added to
            // its log leaves out defaults that nothing states.)
            j.store
                .write_atomic(SNAPSHOT, empty.as_bytes())
                .map_err(|e| CatalogError::Io(format!("can't write the new library's {SNAPSHOT}: {e}")))?;
            j.head_bytes = empty.len() as u64;
            j.snapshot_bytes = j.head_bytes;
        } else if let (Some(from), false) = (older, rewritten) {
            // A library in a format that writes compact records and states its defaults may go on
            // as it is when it can't be upgraded now (the next open tries again): what is appended
            // to it reads the same in every build that reads it. One before that (a build of those
            // formats reads a missing value as its own default, and a library of format 9 that
            // states defaults is still read so by builds that don't know the statement), or one
            // that states other defaults, must not be appended to as it is: a record written from
            // here on leaves out what equals this build's defaults. (One that only lacks a setting
            // this build has may: its records are written whole until it is restated,
            // `header_incomplete`.) What is appended is this build's ops; the files kept when a
            // later snapshot upgrades it are the library as it is here (`behind_at`).
            let how = if report.other_defaults || !states_defaults || from < COMPACT_RECORDS { Upgrade::Must } else { Upgrade::May };
            if let Some(kept) = j.upgrade(&catalog, from, snap.as_deref(), &log, how)? {
                rewritten = true;
                report.upgraded_from = Some(from);
                report.kept = kept;
            } else {
                j.behind = Some(from);
                j.behind_at = (j.seq, j.log_bytes);
            }
        }
        if !rewritten {
            j.restate_defaults(&catalog, &report)?;
        }

        catalog.revision = 0;
        Ok((j, catalog, report))
    }

    /// Cut a torn final record off the log, or end an unterminated last record with a newline, and
    /// note where the log now ends (`good_end`: just past the last good record).
    fn trim_log_tail(&mut self, log: &[u8], good_end: usize, needs_newline: bool, report: &mut LoadReport) -> Result<()> {
        if good_end < log.len() {
            report.torn_bytes = (log.len() - good_end) as u64;
            log::warn!("catalog log: dropped a torn final record ({} bytes)", report.torn_bytes);
            self.store.truncate(LOG, good_end as u64).map_err(io)?;
            self.log_bytes = good_end as u64;
        } else if needs_newline {
            self.store.append(LOG, b"\n").map_err(io)?;
            self.log_bytes = good_end as u64 + 1;
        } else {
            self.log_bytes = good_end as u64;
        }
        Ok(())
    }

    /// A library that an open did not write anew is made to state this build's develop defaults
    /// when its header's do not match: see the cases below.
    fn restate_defaults(&mut self, catalog: &Catalog, report: &LoadReport) -> Result<()> {
        if report.other_defaults {
            // The same format, other defaults: the library must state this build's before
            // anything is appended, since a compact record leaves out what equals the default the
            // header states for it. Nothing is kept for that: any build reads a library over the
            // defaults it states. If it can't be rewritten the library is refused.
            self.snapshot(catalog)?;
        } else if report.defaults_added {
            // A setting this build has that the header doesn't state: restate it before a
            // compact record is appended. If that can't be done now (a full disk, a read-only
            // volume; or the upgrade just above failed, which keeps no copies behind, so the
            // snapshot is not tried again at once), the library still opens, and every record is
            // written whole until a snapshot succeeds (`header_incomplete`). A crash at any point
            // finds the old files whole, or a header that states every setting before any compact
            // record was written.
            self.header_incomplete = true;
            if self.behind.is_none() {
                match self.snapshot(catalog) {
                    Ok(()) => {}
                    Err(e) if self.header_incomplete => {
                        log::warn!("catalog: can't restate the library's defaults ({e}); writing whole records until it can")
                    }
                    Err(e) => log::warn!("catalog: the library's defaults were restated, but its log could not be reset: {e}"),
                }
            }
        }
        Ok(())
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
        // (A library in one file: what it was is its snapshot and its log. One in segments would
        // need its segments kept as well; no format before this build's has any.)
        if !self.store.keeps_replaced_formats() || self.opened_with_segments {
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

    /// A library that was left in an older format at open (see `behind`) keeps its files, as
    /// they were then, before a snapshot upgrades it: the snapshot as it is (nothing writes it
    /// until the upgrade, but for [`Journal::save_in_old_format`], which moves `behind_at` to
    /// what it wrote) and the log up to its length at open (what was appended since is this
    /// build's, which an older build may refuse). The names are those the open would have kept
    /// them under, so files kept for an upgrade that did not land are never left behind under
    /// other names. Kept once (until changes are saved in the old format, which replaces them): when a snapshot failed and is tried again, they stay as they are
    /// (the failed one may have landed all the same, and the snapshot is then no longer what
    /// could be kept). A snapshot that is already in this build's format (that landed after its
    /// files were kept, or before them, reporting a failure) is the upgrade done, with nothing
    /// to keep. An error means they could not be kept (or read): the snapshot is not written, so
    /// the library stays as it was, with its way back.
    fn keep_before_upgrade(&mut self) -> Result<()> {
        let Some(from) = self.behind else { return Ok(()) };
        if self.kept_pending.is_some() {
            return Ok(());
        }
        let snap = self.store.read(SNAPSHOT).map_err(io)?;
        if let Some(bytes) = snap.as_deref() {
            match serde_json::from_slice::<SnapHeader>(bytes).ok().map(|h| h.version) {
                Some(v) if v == from => {}
                // the upgrade is written already: the snapshot after it is a plain one
                Some(VERSION) => {
                    self.upgraded_later();
                    return Ok(());
                }
                _ => return Err(CatalogError::Io(format!("the snapshot is no longer in format v{from}, and can't be kept as it was"))),
            }
        }
        let mut log = self.store.read(LOG).map_err(io)?.unwrap_or_default();
        let (seq, log_len) = self.behind_at;
        log.truncate(usize::try_from(log_len).unwrap_or(usize::MAX));
        let at = std::mem::replace(&mut self.seq, seq);
        let kept = self.keep_replaced(from, snap.as_deref(), &log);
        self.seq = at;
        let kept = kept.map_err(io)?;
        log::info!("catalog: the files of format v{from} are kept as {kept:?} before the upgrade");
        self.kept_pending = Some(kept);
        Ok(())
    }

    /// The snapshot that upgrades a library left behind is durable: the files kept for it are
    /// now the library's way back, for [`Journal::take_kept`] to tell.
    fn upgraded_later(&mut self) {
        self.behind = None;
        if let Some(kept) = self.kept_pending.take() {
            self.kept_written.extend(kept);
        }
    }

    /// The files a snapshot kept when it upgraded a library that was left in an older format at
    /// open (see `behind`), once, after it was written: [`LoadReport::kept`] has only what the
    /// open kept. Empty when there are none.
    pub fn take_kept(&mut self) -> Vec<String> {
        std::mem::take(&mut self.kept_written)
    }

    /// Append ops (already applied to the live catalog) durably, in order.
    pub fn append(&mut self, ops: &[Op]) -> Result<()> {
        if ops.is_empty() {
            return Ok(());
        }
        let t0 = web_time::Instant::now();
        let mut buf = String::new();
        let mut seq = self.seq;
        let _whole = self.header_incomplete.then(crate::stored::writing_full);
        for op in ops {
            seq = seq.checked_add(1).ok_or_else(|| CatalogError::Corrupt(format!("{LOG}: op counter overflow")))?;
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
        // (a library left behind at open has its files kept first: `keep_before_upgrade`)
        let t0 = web_time::Instant::now();
        let seq = self.seq.checked_add(unlogged).ok_or_else(|| CatalogError::Corrupt(format!("{SNAPSHOT}: op counter overflow")))?;
        if let Err(e) = self.keep_before_upgrade() {
            // not upgraded without its way back; try again later like any failed snapshot
            self.stats.failed_snapshots += 1;
            self.retry_at_records = self.log_records + self.policy.max_records;
            return match self.behind {
                // ... but what is only in memory is saved all the same, in the format on disk
                Some(from) if unlogged > 0 && self.segments.is_empty() => self.save_in_old_format(catalog, seq, from, e),
                _ => Err(e),
            };
        }
        let job = Job {
            seq,
            catalog,
            segments: &self.segments,
            baseline: &self.baseline,
            size: self.segment_size(catalog),
            verify: self.inline_photos || self.segments.is_empty(),
        };
        let saved = match write_snapshot(self.store.as_mut(), &job) {
            Ok(saved) => saved,
            Err(e) => {
                // nothing lost (the log is whole); don't retry on every append
                self.stats.failed_snapshots += 1;
                // The new snapshot may have been committed all the same (the failure came after
                // its rename): then the upgrade is done and the files kept before are its way
                // back, told when asked for. A disk that can't be read, or still reads the old
                // format, stays `behind`: what was kept stays for the next try, which does not
                // keep again (unless the changes are saved in the old format just below, which
                // makes it obsolete: `save_in_old_format` removes it).
                // (committed all the same, but its folder was not synced: `seq` is not advanced, so the
                // ops stay queued and are numbered again after the old seq; skipped as stale if
                // the snapshot survived a crash, replayed if not)
                if self.on_disk_version() == Some(Some(VERSION)) {
                    self.upgraded_later();
                }
                self.retry_at_records = self.log_records.saturating_add(self.policy.max_records);
                return match self.behind {
                    // Still in the old format: what is only in memory is saved all the same, in
                    // the format on disk.
                    Some(from) if unlogged > 0 && self.segments.is_empty() => self.save_in_old_format(catalog, seq, from, io(e)),
                    _ => Err(io(e)),
                };
            }
        };
        // the snapshot is durable: it holds every op up to `seq`, and the files it replaced
        // (if the library was behind) are kept
        self.behind = None;
        let mut timing = saved.timing;
        // what is on disk now: as before, but for the stretches this snapshot wrote
        let mut on_disk = std::mem::take(&mut self.baseline);
        if self.store.segmented() {
            segments::refresh(&mut on_disk, catalog.photo_map(), &saved.changed);
        } else {
            on_disk.clear();
        }
        self.adopt(saved, on_disk);
        self.seq = seq;
        self.snapshot_seq = seq;
        self.upgraded_later();
        self.header_incomplete = false;
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

    /// [`Journal::snapshot_with_unlogged`] where the upgrade can't be done (`why`: the files of
    /// the format on disk, `from`, can't be kept, or the new snapshot can't be written): the snapshot is written in that format,
    /// one file, with the `unlogged` ops in it, so that they are saved, and the library is still
    /// `behind` (the upgrade is tried again by the next snapshot, which keeps this one: the
    /// files kept before, the library as it was at open, go, as they are not the way back from
    /// what is on disk now). If that can't be written either, it is `why` that is returned and
    /// nothing has changed.
    fn save_in_old_format(&mut self, catalog: &Catalog, seq: u64, from: u32, why: CatalogError) -> Result<()> {
        let t0 = web_time::Instant::now();
        // only over what is known to be the format `from`: not over an upgrade that may have taken effect
        if self.on_disk_version() != Some(Some(from)) {
            return Err(why);
        }
        let saved = match write_one_file_snapshot(self.store.as_mut(), seq, from, catalog) {
            Ok(saved) => saved,
            Err(e) => {
                log::warn!("catalog: can't upgrade from format v{from} ({why}), and can't save in that format either: {e}");
                return Err(why);
            }
        };
        log::warn!("catalog: can't upgrade from format v{from} ({why}): saved in that format, not upgraded");
        let mut timing = saved.timing;
        self.adopt(saved, Photos::new());
        self.seq = seq;
        self.snapshot_seq = seq;
        // (a header that states this build's defaults: records are compact again)
        self.header_incomplete = false;
        if let Some(kept) = self.kept_pending.take() {
            self.forget_kept(&kept);
        }
        self.behind_at = (seq, 0);
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
        // (the upgrade is tried again once the log has grown by a compaction's worth, or on close)
        self.retry_at_records = self.policy.max_records;
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
        if let Err(e) = self.keep_before_upgrade() {
            self.stats.failed_snapshots += 1;
            self.retry_at_records = self.log_records + self.policy.max_records;
            return Err(e);
        }
        let Some(mut writer) = self.store.background_writer() else { return self.snapshot(catalog) };
        let copy = catalog.clone();
        let seq = self.seq;
        // The worker takes what is on disk with it (the segment list and the photos they hold)
        // and gives back what is on disk when it is done. Nothing else writes snapshots meanwhile.
        let listed = self.segments.clone();
        let baseline = std::mem::take(&mut self.baseline);
        let (size, verify) = (self.segment_size(catalog), self.inline_photos || self.segments.is_empty());
        let spawned = std::thread::Builder::new().name("catalog-snapshot".into()).spawn(move || {
            let job = Job { seq, catalog: &copy, segments: &listed, baseline: &baseline, size, verify };
            match write_snapshot(writer.as_mut(), &job) {
                Ok(saved) => {
                    let on_disk = if writer.segmented() { copy.into_photo_map() } else { Photos::new() };
                    (Ok(saved), on_disk)
                }
                Err(e) => (Err(e), baseline),
            }
        });
        let worker = match spawned {
            Ok(w) => w,
            Err(e) => {
                // (what is on disk went with the thread that didn't start: the snapshot written
                // now takes nothing on disk for granted and writes every segment)
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
        // (a worker that panicked took what is on disk with it: the next snapshot takes nothing
        // on disk for granted and writes every segment)
        let (written, on_disk) = p.worker.join().unwrap_or_else(|_| (Err(std::io::Error::other("snapshot thread panicked")), Photos::new()));
        let saved = match written {
            Ok(saved) => saved,
            Err(e) => {
                // the log still holds every record: nothing lost, retry later
                self.baseline = on_disk;
                self.stats.failed_snapshots += 1;
                self.retry_at_records = self.log_records.saturating_add(self.policy.max_records);
                log::error!("catalog: background snapshot failed: {e}");
                // (it may have been committed all the same, its folder not synced: then the library
                // is upgraded and what was kept before stays)
                if self.on_disk_version() == Some(Some(VERSION)) {
                    self.upgraded_later();
                }
                return Err(io(e));
            }
        };
        // `catalog.snap` (op `p.seq`) is durable; the log's records `<= p.seq` are now stale.
        // Keep exactly the ones appended since (a crash before this rewrite skips the stale ones).
        let t1 = web_time::Instant::now();
        let mut timing = saved.timing;
        self.adopt(saved, on_disk);
        self.snapshot_seq = p.seq;
        self.upgraded_later();
        self.header_incomplete = false;
        if let Err(e) = self.store.write_atomic(LOG, &p.tail) {
            // the log stays whole (stale records are skipped on load); compact again later
            self.retry_at_records = self.log_records.saturating_add(self.policy.max_records);
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

    /// How large the segments of a snapshot of `catalog` are to be.
    fn segment_size(&self, catalog: &Catalog) -> segments::Size {
        // (for photos in no segment yet: what a record takes on average in the snapshot there is)
        let photos = self.snapshot_bytes.saturating_sub(if self.inline_photos { 0 } else { self.head_bytes });
        let record_bytes = photos.checked_div(catalog.len() as u64).unwrap_or(0);
        segments::Size { per: self.policy.segment_photos, bytes: self.policy.segment_bytes, record_bytes }
    }

    /// A snapshot was committed: what is on disk now.
    fn adopt(&mut self, saved: Saved, on_disk: Photos) {
        self.head_bytes = saved.head_bytes;
        self.snapshot_bytes = saved.head_bytes + saved.segments.iter().map(|s| s.bytes).sum::<u64>();
        self.segments = saved.segments;
        self.baseline = on_disk;
        self.inline_photos = saved.inline_photos;
    }

    fn record_snapshot(&mut self, t: SnapshotTiming) {
        self.stats.snapshots += 1;
        self.stats.last_snapshot = t;
        self.stats.max_snapshot_ms = self.stats.max_snapshot_ms.max(t.total_ms);
        self.stats.max_blocking_ms = self.stats.max_blocking_ms.max(t.blocking_ms);
        if profiling() {
            eprintln!(
                "catalog: {}snapshot of {} records, {} segment(s), {} B written: segments {:.1} ms, catalog.snap {:.1} ms, log reset {:.1} ms, total {:.1} ms, blocking {:.1} ms",
                if t.background { "background " } else { "" },
                t.records,
                t.segments,
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
        PersistStats {
            snapshot_running: self.pending.is_some(),
            segments: self.segments.len() as u64,
            snapshot_bytes: self.snapshot_bytes,
            ..self.stats
        }
    }

    /// The log is long enough to be worth compacting.
    pub fn wants_snapshot(&self) -> bool {
        self.pending.is_none()
            && self.log_records >= self.retry_at_records
            && (self.log_records >= self.policy.max_records || self.log_bytes >= self.policy.max_bytes)
    }

    /// Size of the snapshot as last written or loaded: `catalog.snap` and its segments (0: none
    /// yet).
    pub fn snapshot_bytes(&self) -> u64 {
        self.snapshot_bytes
    }

    /// The library is still in an older format than this build's: its upgrade was put off at
    /// open, and a snapshot (a compaction, or closing) makes it.
    pub fn upgrade_put_off(&self) -> bool {
        self.behind.is_some()
    }

    /// The segments `catalog.snap` lists.
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Remove the segment files the snapshot doesn't list: what a crash left between writing
    /// segments and committing the snapshot, or between the commit and removing the old ones.
    ///
    /// Only for a caller that has the library to itself (it holds the library's lock): to anyone
    /// else, the segments another program is writing for its next snapshot look the same. And
    /// only when the library was opened from a snapshot that lists segments: files beside a
    /// one-file `catalog.snap` (an older one put back over a newer folder) may be all there is of
    /// the newer catalog, and are left alone for as long as this journal is open.
    ///
    /// What "the snapshot lists" is read from `catalog.snap` as it is on disk, together with what
    /// this journal takes it to list (the two differ after a snapshot whose outcome it never
    /// learned); if the file can't be read, nothing is removed.
    pub fn remove_unlisted_segments(&mut self) {
        if self.pending.is_some() || !self.store.segmented() || !self.opened_with_segments || self.segments.is_empty() {
            return;
        }
        let Some(mut listed) = listed_on_disk(self.store.as_mut()) else { return };
        listed.extend(self.segments.iter().cloned());
        segments::discard_unlisted(self.store.as_mut(), &listed);
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
        if let Some(p) = self.pending.take() {
            let (written, _) = p.worker.join().unwrap_or_else(|_| (Err(std::io::Error::other("snapshot thread panicked")), Photos::new()));
            if let Err(e) = written {
                log::error!("catalog: background snapshot failed: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests_resync {
    use super::*;

    /// A torn first line may end at any byte and a failed append's retried records may follow its
    /// own start `{"seq":` more than once: `resync` finds the CRC-valid record after a fragment of
    /// many record starts. UTF-8 is validated once for a valid line, and for a line that isn't
    /// wholly valid only up to the budget instead of the suffix length per candidate (CWE-407).
    #[test]
    fn resync_finds_the_retried_record_after_a_fragment_of_many_starts() {
        let op = Op::Batch { ops: vec![] };
        let record = encode_record(5, &op);
        let mut line = Vec::from(b"\xf2" as &[u8]); // an invalid byte, so the whole line is not UTF-8
        for i in 0..1024 {
            line.extend_from_slice(format!("{{\"seq\":{},\"crc\":{{", i).as_bytes());
        }
        let fragment = line.len();
        line.extend_from_slice(record.as_bytes());
        assert_eq!(resync(&line, 4).map(|(at, (seq, _))| (at, seq)), Some((fragment, 5)));
        // a retry is still recognised past `last`: the fragment logs nothing valid, but the CRC
        // matches, so the double `Some` is only reached with `last >= 4`
        assert_eq!(resync(&line, 100).map(|(at, (seq, _))| (at, seq)), Some((fragment, 5)));
    }
}

#[cfg(test)]
#[path = "tests_upgrade_attempts.rs"]
mod tests_upgrade_attempts;
