//! Issue #101: an append that fails part-way (disk full, a dropped network share) and is then
//! retried must not read back as log damage, and must lose nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::journal::{LOG, RESYNC_CANDIDATES};
use crate::*;

/// A store whose next append writes only a prefix of its data, then fails.
#[derive(Clone, Default)]
struct Torn {
    files: MemStore,
    /// The next append writes this many bytes, then returns an error.
    torn_at: Arc<Mutex<Option<usize>>>,
    /// `truncate` fails (the cut-back after a failed append can't be done right away).
    truncate_fails: Arc<AtomicBool>,
}

impl Torn {
    fn tear_next_append(&self, bytes: usize) {
        *self.torn_at.lock().unwrap() = Some(bytes);
    }
}

impl Store for Torn {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if let Some(n) = self.torn_at.lock().unwrap().take() {
            self.files.append(name, &data[..n.min(data.len())])?;
            return Err(std::io::Error::other("disk full"));
        }
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        if self.truncate_fails.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("share unavailable"));
        }
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        "torn".into()
    }
}

fn add_photo(c: &mut Catalog) -> Op {
    let id = c.alloc_photo_id();
    Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, "p.jpg", "JPEG", 4, 3, "2026-01-01")) }
}

/// Apply `ops` to the live catalog (like a command) and return them for the journal.
fn apply(c: &mut Catalog, ops: Vec<Op>) -> Vec<Op> {
    for op in &ops {
        c.apply(op.clone()).unwrap();
    }
    ops
}

/// Every tear position of a batch, then the retry (same ops, same seqs, like the engine's queue)
/// and more appends: reloading replays everything, reports no damage and no torn bytes.
#[test]
fn partial_append_then_retry_loads_cleanly() {
    let probe = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(probe.clone())).unwrap();
    let add = add_photo(&mut c);
    let first = apply(&mut c, vec![add]);
    j.append(&first).unwrap();
    let id = c.photos().next().unwrap().id;
    let batch = apply(&mut c, vec![Op::SetRating { id, rating: 3 }, Op::SetFlag { id, flag: Flag::Pick }, Op::SetRating { id, rating: 4 }]);
    let before = probe.get(LOG).unwrap().len();
    j.append(&batch).unwrap();
    let batch_len = probe.get(LOG).unwrap().len() - before;

    for tear in 1..batch_len {
        let t = Torn::default();
        let (mut j, mut c, _) = Journal::open(Box::new(t.clone())).unwrap();
        let add = add_photo(&mut c);
        let first = apply(&mut c, vec![add]);
        j.append(&first).unwrap();
        let good_len = t.files.get(LOG).unwrap().len();
        let batch = apply(&mut c, vec![Op::SetRating { id, rating: 3 }, Op::SetFlag { id, flag: Flag::Pick }, Op::SetRating { id, rating: 4 }]);
        t.tear_next_append(tear);
        assert!(j.append(&batch).is_err(), "tear at {tear}");
        // cut back to the last whole record
        assert_eq!(t.files.get(LOG).unwrap().len(), good_len, "tear at {tear}");
        j.append(&batch).unwrap();
        let more = apply(&mut c, vec![Op::SetLabel { id, label: Some(ColorLabel::ALL[0]) }]);
        j.append(&more).unwrap();
        drop(j);

        let (_, loaded, r) = Journal::open(Box::new(t.clone())).unwrap();
        assert!(r.damaged.is_none(), "tear at {tear}: {r:?}");
        assert_eq!((r.torn_bytes, r.torn_fragments, r.replayed), (0, 0, 5), "tear at {tear}");
        assert_eq!(loaded.to_snapshot(), c.to_snapshot(), "tear at {tear}");
    }
}

/// When cutting back fails too (the share is gone), the next append cuts back first; until that
/// works nothing more is appended after the fragment.
#[test]
fn failed_cut_back_is_done_before_the_next_append() {
    let t = Torn::default();
    let (mut j, mut c, _) = Journal::open(Box::new(t.clone())).unwrap();
    let add = add_photo(&mut c);
    let first = apply(&mut c, vec![add]);
    j.append(&first).unwrap();
    let good_len = t.files.get(LOG).unwrap().len();
    let id = c.photos().next().unwrap().id;
    let batch = apply(&mut c, vec![Op::SetRating { id, rating: 2 }, Op::SetRating { id, rating: 5 }]);
    t.truncate_fails.store(true, Ordering::SeqCst);
    t.tear_next_append(30);
    assert!(j.append(&batch).is_err());
    assert_eq!(t.files.get(LOG).unwrap().len(), good_len + 30, "the fragment is still there");
    // still unreachable: the retry fails without writing anything after the fragment
    assert!(j.append(&batch).is_err());
    assert_eq!(t.files.get(LOG).unwrap().len(), good_len + 30);
    t.truncate_fails.store(false, Ordering::SeqCst);
    j.append(&batch).unwrap();
    drop(j);
    let (_, loaded, r) = Journal::open(Box::new(t.clone())).unwrap();
    assert!(r.damaged.is_none(), "{r:?}");
    assert_eq!(r.replayed, 3);
    assert_eq!(loaded.to_snapshot(), c.to_snapshot());
}

/// Logs written before the fix: the fragment of the failed append is glued to the first retried
/// record on one line, followed by the rest of the retry. They load in full (the fragment is
/// skipped), instead of stopping at the fragment and setting the later records aside as damage.
#[test]
fn legacy_fragment_glued_to_the_retry_is_skipped() {
    let m = MemStore::new();
    let (mut j, mut c, _) = Journal::open(Box::new(m.clone())).unwrap();
    let add = add_photo(&mut c);
    let first = apply(&mut c, vec![add]);
    j.append(&first).unwrap();
    let good = m.get(LOG).unwrap();
    let id = c.photos().next().unwrap().id;
    let batch = apply(
        &mut c,
        vec![
            Op::SetRating { id, rating: 3 },
            Op::SetFlag { id, flag: Flag::Pick },
            Op::SetLabelName { label: ColorLabel::ALL[1], name: Some("Größe ✓".into()) },
        ],
    );
    j.append(&batch).unwrap();
    let full = m.get(LOG).unwrap();
    let batch_bytes = &full[good.len()..];
    let later = apply(&mut c, vec![Op::SetRating { id, rating: 1 }]);
    j.append(&later).unwrap();
    let later_bytes = m.get(LOG).unwrap()[full.len()..].to_vec();
    drop(j);

    // every tear (inside the first record, after it, inside a multi-byte character…)
    for tear in 1..batch_bytes.len() {
        if batch_bytes[tear - 1] == b'\n' {
            continue; // a tear on a record boundary leaves no fragment
        }
        let mut log = good.clone();
        log.extend_from_slice(&batch_bytes[..tear]);
        log.extend_from_slice(batch_bytes);
        log.extend_from_slice(&later_bytes);
        let t = MemStore::new();
        t.set(LOG, log);
        let (_, loaded, r) = Journal::open(Box::new(t)).unwrap();
        assert!(r.damaged.is_none(), "tear at {tear}: {r:?}");
        assert_eq!(r.torn_fragments, 1, "tear at {tear}");
        assert_eq!(loaded.to_snapshot(), c.to_snapshot(), "tear at {tear}");
    }

    // a damaged record whose tail happens to be a whole record from the future is still damage
    let mut log = good.clone();
    log.extend_from_slice(b"garbage");
    log.extend_from_slice(&later_bytes);
    log.extend_from_slice(&later_bytes);
    let t = MemStore::new();
    t.set(LOG, log);
    let (_, _, r) = Journal::open(Box::new(t)).unwrap();
    assert!(r.damaged.is_some(), "{r:?}");
}

/// A snapshot can absorb ops whose append failed: it records them as saved (`seq` moves past
/// them), and appending after it continues from there.
#[test]
fn snapshot_with_unlogged_ops_saves_them() {
    let t = Torn::default();
    let (mut j, mut c, _) = Journal::open(Box::new(t.clone())).unwrap();
    let add = add_photo(&mut c);
    let first = apply(&mut c, vec![add]);
    j.append(&first).unwrap();
    let id = c.photos().next().unwrap().id;
    let queued = apply(&mut c, vec![Op::SetRating { id, rating: 2 }, Op::SetFlag { id, flag: Flag::Reject }]);
    t.tear_next_append(10);
    assert!(j.append(&queued).is_err());
    j.snapshot_with_unlogged(&c, queued.len() as u64).unwrap();
    assert_eq!(j.seq(), 3);
    let after = apply(&mut c, vec![Op::SetRating { id, rating: 5 }]);
    j.append(&after).unwrap();
    drop(j);
    let (j, loaded, r) = Journal::open(Box::new(t.clone())).unwrap();
    assert_eq!((r.snapshot_seq, r.replayed), (3, 1));
    assert_eq!(j.seq(), 4);
    assert_eq!(loaded.to_snapshot(), c.to_snapshot());
}

/// `FsStore::truncate` on a file that was never created (an append that failed first) is a no-op.
#[test]
fn fs_store_truncates_a_missing_file_quietly() {
    let dir = std::env::temp_dir().join(format!("lc-torn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = FsStore::open(&dir).unwrap();
    s.truncate(LOG, 0).unwrap();
    assert!(!dir.join(LOG).exists());
    s.append(LOG, b"abc\n").unwrap();
    s.truncate(LOG, 0).unwrap();
    assert_eq!(std::fs::read(dir.join(LOG)).unwrap(), b"");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Issue #2124: `resync` must be linear in the line length, not quadratic. A line that is nothing
/// but the start marker repeated 300k times must open fast, and a valid retry record placed after
/// it must still be recovered.
#[test]
fn resync_is_not_quadratic_in_line_length() {
    let probe = MemStore::new();

    // Build a valid snapshot + an initial record so `j.seq()` is non-zero and the `seq <= last + 1`
    // gate in resync can be exercised.
    let (mut j, mut c, _) = Journal::open(Box::new(probe.clone())).unwrap();
    let add = add_photo(&mut c);
    let first = apply(&mut c, vec![add]);
    j.append(&first).unwrap();

    let log = probe.get(LOG).unwrap();
    let base_len = log.len();

    // A hostile 300k-repetition marker line (about 2.3 MB), then one valid retried record that
    // continues the log (seq == j.seq() + 1, so resync's gate accepts it).
    let marker = b"{\"seq\":";
    let mut hostile = Vec::with_capacity(marker.len() * 300_000 + log.len() + 256);
    hostile.extend_from_slice(&log[..base_len]);
    for _ in 0..300_000 {
        hostile.extend_from_slice(marker);
    }

    // Encode the retry record the same way the journal writes records, so decode_record accepts it.
    let next_seq = j.seq() + 1;
    let retry_op = Op::SetRating { id: c.photos().next().unwrap().id, rating: 5 };
    let body = serde_json::to_string(&retry_op).unwrap();
    let crc = crc32fast::hash(body.as_bytes());
    let retry = format!("{{\"seq\":{next_seq},\"crc\":{crc},\"op\":{body}}}\n");
    hostile.extend_from_slice(retry.as_bytes());

    probe.set(LOG, hostile);

    // Count the start markers resync decodes instead of timing it: a slow worker must not fail this.
    RESYNC_CANDIDATES.with(|c| c.set(0));
    let (j2, _c2, report) = Journal::open(Box::new(probe.clone())).unwrap();
    let candidates = RESYNC_CANDIDATES.with(|c| c.get());
    assert!(candidates <= 1, "resync decoded {candidates} start markers of a hostile 2 MB line; only the last one may be decoded");
    assert_eq!(j2.seq(), next_seq, "resync should have recovered the retried record after the fragment");
    assert_eq!(report.torn_fragments, 1, "resync should report exactly one torn fragment");
}

/// A retry record whose op body holds a nested `{"seq":` (a smart-album rule value such as
/// `{"seq":1}` on a `title isEmpty` rule) shadows the record's own start marker: `rposition` in
/// `resync` picks the nested occurrence, `decode_record` rejects the suffix, and `Journal::open`
/// truncates the final line instead of replaying the retry — the valid operation is lost.
#[test]
fn resync_preserves_retry_record_with_nested_start_marker() {
    let probe = MemStore::new();

    let (mut j, mut c, _) = Journal::open(Box::new(probe.clone())).unwrap();
    let add = add_photo(&mut c);
    j.append(&apply(&mut c, vec![add])).unwrap();

    let album_id = c.alloc_album_id();
    let add_album = Op::AddAlbum { album: Album { smart: Some(Box::new(Filter::default())), ..Album::new(album_id, "Smart") } };
    j.append(&apply(&mut c, vec![add_album])).unwrap();

    let retry_op = Op::SetAlbumRules {
        id: album_id,
        rules: Box::new(Filter {
            rule_set: Some(RuleSet {
                mode: Match::All,
                rules: vec![Rule::Field { field: "title".into(), op: "isEmpty".into(), value: serde_json::json!({ "seq": 1 }) }],
            }),
            ..Default::default()
        }),
    };
    apply(&mut c, vec![retry_op.clone()]);

    // Encode the retry exactly as the journal writes records, so `decode_record` accepts it once
    // resync finds its real start: `seq` continues the log and the CRC matches the body.
    let next_seq = j.seq() + 1;
    let body = serde_json::to_string(&retry_op).unwrap();
    let crc = crc32fast::hash(body.as_bytes());
    let retry = format!("{{\"seq\":{next_seq},\"crc\":{crc},\"op\":{body}}}\n");

    // A torn fragment glued to the retry on the final line. The outer record start sits right
    // after the fragment, but the last `{"seq":` byte occurrence is the nested rule value.
    let mut log = probe.get(LOG).unwrap();
    log.extend_from_slice(b"torn fragment without a newline");
    log.extend_from_slice(retry.as_bytes());
    probe.set(LOG, log);

    let (j2, loaded, report) = Journal::open(Box::new(probe.clone())).unwrap();
    assert_eq!(j2.seq(), next_seq, "resync should have recovered the retried record after the fragment");
    assert_eq!(report.torn_fragments, 1, "resync should report exactly one torn fragment");
    assert_eq!(report.torn_bytes, 0, "the valid retry record must not be truncated as torn");
    assert_eq!(loaded.to_snapshot(), c.to_snapshot(), "the recovered smart-album rule should be preserved");
}

/// The maintainer's regression for issue #2124, with the damaged line as the file's LAST one: a
/// torn fragment glued to a CRC-valid `SetAlbumRules` retry whose rules hold several nested
/// `{"seq":1}` values. Picking the last raw `{"seq":` occurrence as the record start (as the
/// original `rposition` scan did) lands in a nested value, rejects the whole record, and cuts the
/// final line away — losing the valid operation. `resync` must recover the retry (its real start
/// is followed by the `,"crc":` header no nested rule value has) and leave the log file whole.
#[test]
fn resync_recovers_rules_retry_with_many_nested_values_on_the_final_line() {
    let probe = MemStore::new();

    let (mut j, mut c, _) = Journal::open(Box::new(probe.clone())).unwrap();
    let add = add_photo(&mut c);
    j.append(&apply(&mut c, vec![add])).unwrap();

    let album_id = c.alloc_album_id();
    let add_album = Op::AddAlbum { album: Album { smart: Some(Box::new(Filter::default())), ..Album::new(album_id, "Smart") } };
    j.append(&apply(&mut c, vec![add_album])).unwrap();

    let rules =
        (0..12).map(|_| Rule::Field { field: "title".into(), op: "isEmpty".into(), value: serde_json::json!({ "seq": 1 }) }).collect::<Vec<_>>();
    let retry_op =
        Op::SetAlbumRules { id: album_id, rules: Box::new(Filter { rule_set: Some(RuleSet { mode: Match::All, rules }), ..Default::default() }) };
    apply(&mut c, vec![retry_op.clone()]);

    // Encode the retry exactly as the journal writes records, so `decode_record` accepts it once
    // resync finds its real start: `seq` continues the log and the CRC matches the body.
    let next_seq = j.seq() + 1;
    let body = serde_json::to_string(&retry_op).unwrap();
    let crc = crc32fast::hash(body.as_bytes());
    let retry = format!("{{\"seq\":{next_seq},\"crc\":{crc},\"op\":{body}}}");

    // The failed append's fragment glued to the retry, and nothing after: the damaged line is the
    // file's last, without a closing newline.
    let mut log = probe.get(LOG).unwrap();
    log.extend_from_slice(b"torn fragment without a newline");
    log.extend_from_slice(retry.as_bytes());
    let damaged = log.clone();
    probe.set(LOG, log);

    let (j2, loaded, report) = Journal::open(Box::new(probe.clone())).unwrap();
    assert_eq!(report.torn_fragments, 1, "the fragment must be reported and the retry recovered");
    assert_eq!(report.torn_bytes, 0, "the retry record must not be cut away as a torn tail");
    assert_eq!(j2.seq(), next_seq, "the retried SetAlbumRules must be recovered");
    assert_eq!(report.replayed, 3);
    assert_eq!(loaded.to_snapshot(), c.to_snapshot(), "the smart-album rules must survive the reload");

    // The log file is preserved: its bytes stay (plus only the newline that closes the final
    // line), instead of the damaged line being truncated away.
    let after = probe.get(LOG).unwrap();
    assert_eq!(after.len(), damaged.len() + 1, "the log must keep the fragment and the recovered record");
    assert_eq!(&after[..damaged.len()], &damaged[..]);
}

/// A crafted final line of many header-shaped markers (`{"seq":1,"crc":2,"op":`) passes the
/// record-header filter, so every marker becomes a resync candidate, and each attempt decodes the
/// whole rest of the line: trying all of them is quadratic in the line length, a stall of
/// `Journal::open` on a hostile log (CWE-407). The per-line scan budget must bound the candidate
/// decodes (counted, not timed) and open must still finish safely. A CRC-valid retry placed after
/// the junk must stay recoverable: it is the last candidate, so the scan reaches it first.
#[test]
fn resync_scan_budget_bounds_a_line_of_header_shaped_markers() {
    let probe = MemStore::new();

    let (mut j, mut c, _) = Journal::open(Box::new(probe.clone())).unwrap();
    let add = add_photo(&mut c);
    j.append(&apply(&mut c, vec![add])).unwrap();
    let good = probe.get(LOG).unwrap();

    const MARKERS: usize = 5_000;
    let marker: &[u8] = b"{\"seq\":1,\"crc\":2,\"op\":";
    let mut hostile = good.clone();
    hostile.extend_from_slice(b"torn fragment"); // so no marker sits at line position 0
    for _ in 0..MARKERS {
        hostile.extend_from_slice(marker);
    }
    let hostile_len = (hostile.len() - good.len()) as u64;
    probe.set(LOG, hostile);

    RESYNC_CANDIDATES.with(|count| count.set(0));
    let (j2, _loaded, report) = Journal::open(Box::new(probe.clone())).unwrap();
    let candidates = RESYNC_CANDIDATES.with(|count| count.get());
    assert!(
        candidates < MARKERS,
        "resync tried all {candidates} candidates of a hostile {hostile_len}-byte line; the scan budget must bound the work"
    );
    assert!(report.damaged.is_none(), "{report:?}");
    assert_eq!(report.torn_fragments, 0);
    assert_eq!(report.torn_bytes, hostile_len, "the hostile line must be dropped as a torn tail");
    assert_eq!(j2.seq(), 1, "the records before the hostile line must survive");
    assert_eq!(probe.get(LOG).unwrap().len(), good.len(), "the log must be cut back to the last good record");

    let retry_op = Op::SetRating { id: c.photos().next().unwrap().id, rating: 5 };
    let next_seq = j2.seq() + 1;
    let body = serde_json::to_string(&retry_op).unwrap();
    let crc = crc32fast::hash(body.as_bytes());
    let mut log = good.clone();
    for _ in 0..MARKERS {
        log.extend_from_slice(marker);
    }
    log.extend_from_slice(format!("{{\"seq\":{next_seq},\"crc\":{crc},\"op\":{body}}}\n").as_bytes());
    probe.set(LOG, log);

    RESYNC_CANDIDATES.with(|count| count.set(0));
    let (j3, _loaded, report) = Journal::open(Box::new(probe.clone())).unwrap();
    let candidates = RESYNC_CANDIDATES.with(|count| count.get());
    assert_eq!(candidates, 1, "the retry is the last candidate and must decode first");
    assert_eq!(j3.seq(), next_seq, "the retried record after the junk must be recovered");
    assert_eq!(report.torn_fragments, 1, "the junk before the retry is one skipped fragment");
    assert_eq!(report.torn_bytes, 0, "the valid retry record must not be cut away");
}
