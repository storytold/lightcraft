//! Journal (op log + snapshot) tests: replay(log) == state, torn tails, stale logs, damage.

use std::sync::Arc;

use lcc_develop::DevelopSettings;
use proptest::prelude::*;

use crate::journal::{LOG, SNAPSHOT, decode_record, encode_record};
use crate::*;

fn open(m: &MemStore) -> (Journal, Catalog, LoadReport) {
    Journal::open(Box::new(m.clone())).unwrap()
}

/// Interpret a random action against the live catalog as an op (may be invalid: then the live
/// apply fails and, like in the engine, nothing is logged).
fn op_for(c: &mut Catalog, kind: u8, a: u8, b: u8) -> Op {
    let photos: Vec<PhotoId> = c.photos().map(|p| p.id).collect();
    let albums: Vec<AlbumId> = c.albums().map(|a| a.id).collect();
    let pid = photos.get(a as usize % photos.len().max(1)).copied().unwrap_or(PhotoId(99));
    let aid = albums.get(a as usize % albums.len().max(1)).copied().unwrap_or(AlbumId(99));
    let rules = |b: u8| Filter {
        rating: b % 6,
        flag: [None, Some(Flag::Pick), Some(Flag::Reject)][b as usize % 3],
        date_from: (b % 4 == 1).then(|| "2026-01".to_string()),
        text: if b.is_multiple_of(5) { format!("{}", b % 10) } else { String::new() },
        ..Default::default()
    };
    match kind % 16 {
        0 | 1 => {
            let id = c.alloc_photo_id();
            let mut p = Photo::new(id, Source::File { path: format!("/p/{}.jpg", id.0) }, &format!("{}.jpg", id.0), "JPEG", 60, 40, "2026-01-01");
            p.content_hash = Some(format!("{:032x}", id.0 * 7919));
            Op::AddPhoto { photo: Box::new(p) }
        }
        2 => Op::SetRating { id: pid, rating: b % 7 },
        3 => Op::SetFlag { id: pid, flag: [Flag::None, Flag::Pick, Flag::Reject][b as usize % 3] },
        4 => {
            let mut d = DevelopSettings::default();
            d.light.exposure = b as f64 / 37.0 - 2.0;
            d.light.contrast = -(a as f64) * 0.37;
            let settings = Arc::new(d);
            Op::Batch {
                ops: vec![
                    Op::SetDevelop { id: pid, settings: settings.clone(), label: "Exposure".into(), edited: Some(format!("t{b}")) },
                    Op::PushHistory { id: pid, step: HistoryStep { label: "Exposure".into(), settings, batch: None } },
                ],
            }
        }
        5 => {
            let id = c.alloc_album_id();
            Op::AddAlbum {
                album: Album {
                    id,
                    name: format!("A{b}"),
                    parent: None,
                    folder: b.is_multiple_of(4),
                    photos: vec![],
                    cover: None,
                    smart: None,
                    quick: false,
                    order: None,
                    creation: None,
                    photo_order: Vec::new(),
                },
            }
        }
        6 => Op::SetAlbumPhotos { id: aid, photos: photos.iter().copied().filter(|p| (p.0 + b as u64).is_multiple_of(3)).collect() },
        7 => Op::RenameAlbum { id: aid, name: format!("Renamed {b} ✓") },
        8 => Op::SetDeleted { id: pid, deleted: b.is_multiple_of(2) },
        10 => {
            let id = c.alloc_album_id();
            Op::AddAlbum { album: Album { smart: Some(Box::new(rules(b))), ..Album::new(id, format!("Smart {b}")) } }
        }
        11 => Op::SetAlbumRules { id: aid, rules: Box::new(rules(b)) },
        12 => {
            let members: Vec<PhotoId> = photos.iter().copied().filter(|p| (p.0 + b as u64).is_multiple_of(4)).collect();
            c.group_ops(pid, &members, b.is_multiple_of(2)).unwrap_or(Op::Batch { ops: vec![] })
        }
        13 => Op::Batch { ops: c.remove_from_stacks_ops(&[pid]) },
        14 => c.set_top_ops(pid).unwrap_or(Op::Batch { ops: vec![] }),
        9 if b.is_multiple_of(5) => Op::SetLabelName { label: ColorLabel::ALL[a as usize % 5], name: (a % 3 != 1).then(|| format!("Label {b}")) },
        9 if b % 7 == 5 => Op::SetKeyword {
            path: format!("travel|place {}", b % 4),
            info: (!b.is_multiple_of(3)).then(|| crate::keywords::KeywordInfo {
                person: a.is_multiple_of(2),
                synonyms: vec![format!("trip {a}")],
                ..Default::default()
            }),
        },
        9 if b % 11 == 4 => c.folder_label_op(&format!("/p/f{}", a % 4), ColorLabel::ALL.get(usize::from(b % 6)).copied()),
        9 if b % 13 == 6 => match c.album(aid) {
            Some(al) => Op::SetAlbumPhotoOrder { id: aid, order: al.photos.iter().rev().copied().chain([pid]).collect() },
            None => Op::SetAlbumOrder { id: aid, order: None },
        },
        9 if b % 7 == 3 => Op::SetAlbumOrder { id: aid, order: (!b.is_multiple_of(3)).then_some(u32::from(b % 5)) },
        9 if b.is_multiple_of(2) => {
            Op::SetFile { id: pid, file_name: format!("Renamed_{b}.jpg"), source: Source::File { path: format!("/p/Renamed_{b}.jpg") } }
        }
        9 => Op::SetCaptured { id: pid, captured: (!b.is_multiple_of(3)).then(|| format!("2026-0{}-1{}T10:{:02}:00", 1 + b % 9, b % 10, a % 60)) },
        15 => match c.photo(pid).cloned() {
            // a virtual copy: same source, own id, copy_of/copy_name set
            Some(src) => {
                let id = c.alloc_photo_id();
                let mut v = (*src).clone();
                v.id = id;
                v.copy_of = Some(pid);
                v.copy_name = Some(format!("Copy {b}"));
                Op::AddPhoto { photo: Box::new(v) }
            }
            None => Op::Batch { ops: vec![] },
        },
        _ => c.delete_permanently_ops(pid),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// replay(snapshot + log) == live state, with snapshots at random points and reopening
    /// (resuming appends) in between.
    #[test]
    fn replay_equals_state(actions in proptest::collection::vec((0u8..20, 0u8..255, 0u8..255, 0u8..12), 1..80)) {
        let m = MemStore::new();
        let (mut j, mut live, r) = open(&m);
        prop_assert!(r.created);
        for (kind, a, b, ctl) in actions {
            let op = op_for(&mut live, kind, a, b);
            if live.apply(op.clone()).is_ok() {
                j.append(&[op]).unwrap();
            }
            match ctl {
                0 => j.snapshot(&live).unwrap(),
                1 => {
                    let (j2, c2, _) = open(&m);
                    prop_assert_eq!(c2.to_snapshot(), live.to_snapshot());
                    j = j2;
                }
                _ => {}
            }
        }
        let (_, loaded, r) = open(&m);
        prop_assert_eq!(r.failed, 0);
        prop_assert_eq!(loaded.to_snapshot(), live.to_snapshot());
        // allocation continues where it left off
        prop_assert_eq!(loaded.clone().alloc_photo_id(), live.clone().alloc_photo_id());
    }
}

fn sample_catalog_with_log() -> (MemStore, Catalog, Catalog) {
    let m = MemStore::new();
    let (mut j, mut c, _) = open(&m);
    for k in 0..6u8 {
        let op = op_for(&mut c, k, k, k * 3);
        if c.apply(op.clone()).is_ok() {
            j.append(&[op]).unwrap();
        }
    }
    let before_last = c.clone();
    let op = Op::SetRating { id: c.photos().next().unwrap().id, rating: 5 };
    c.apply(op.clone()).unwrap();
    j.append(&[op]).unwrap();
    (m, before_last, c)
}

/// A crash mid-append: every possible truncation of the last record still loads (to the state
/// before that op), the torn bytes are cut off, and appending afterwards works.
#[test]
fn torn_last_record_is_tolerated() {
    let (m, before_last, full) = sample_catalog_with_log();
    let log = m.get(LOG).unwrap();
    let last_start = log[..log.len() - 1].iter().rposition(|b| *b == b'\n').unwrap() + 1;
    for cut in last_start..log.len() {
        let t = MemStore::new();
        t.set(LOG, log[..cut].to_vec());
        let (mut j, mut c, r) = open(&t);
        // the full record minus only its newline is complete and kept
        let expect = if cut == log.len() - 1 { &full } else { &before_last };
        assert_eq!(c.to_snapshot(), expect.to_snapshot(), "cut at {cut}");
        assert!(r.damaged.is_none());
        if cut < log.len() - 1 {
            assert_eq!(r.torn_bytes as usize, cut - last_start, "cut at {cut}");
        }
        // keeps working after recovery
        let id = c.photos().next().unwrap().id;
        let op = Op::SetFlag { id, flag: Flag::Reject };
        c.apply(op.clone()).unwrap();
        j.append(&[op]).unwrap();
        let (_, c2, r2) = open(&t);
        assert_eq!(c2.to_snapshot(), c.to_snapshot(), "cut at {cut}");
        assert_eq!(r2.torn_bytes, 0);
    }
    // garbage bytes (a partially flushed page) at the end are a torn tail too
    let t = MemStore::new();
    let mut g = log.clone();
    g.extend_from_slice(&[0, 0, 0, b'{', 0xff]);
    t.set(LOG, g);
    let (_, c, r) = open(&t);
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    assert_eq!(r.torn_bytes, 5);
}

/// Crash after the snapshot was written but before the log was reset: the stale records are
/// skipped by sequence number.
#[test]
fn stale_log_after_snapshot_is_skipped() {
    let (m, _, full) = sample_catalog_with_log();
    let old_log = m.get(LOG).unwrap();
    let (mut j, c, _) = open(&m);
    j.snapshot(&c).unwrap();
    assert!(m.get(LOG).unwrap().is_empty());
    m.set(LOG, old_log);
    let (_, c2, r) = open(&m);
    assert_eq!(c2.to_snapshot(), full.to_snapshot());
    assert!(r.stale > 0 && r.replayed == 0);
}

/// Damage in the middle of the log (not a torn tail): recover what precedes it, keep the
/// damaged file, and write a consistent snapshot.
#[test]
fn mid_log_damage_is_preserved_and_recovered() {
    let (m, _, _) = sample_catalog_with_log();
    let log = m.get(LOG).unwrap();
    let text = String::from_utf8(log).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(lines.len() > 4);
    lines[2] = lines[2].replacen("\"op\":{\"op\":\"", "\"op\":{\"op\":\"x", 1);
    let bad = format!("{}\n", lines.join("\n")).into_bytes();
    m.set(LOG, bad.clone());
    let (_, c, r) = open(&m);
    let name = r.damaged.clone().expect("damage reported");
    assert_eq!(m.get(&name).unwrap(), bad);
    assert_eq!(r.replayed, 2);
    // the recovered state was snapshotted: reopening is clean and identical
    let (_, c2, r2) = open(&m);
    assert_eq!(c2.to_snapshot(), c.to_snapshot());
    assert!(r2.damaged.is_none());
    // damaged again at the same op: a second copy, the first one kept (issue #2140)
    let again = [&bad[..], b"{\"seq\":1,\"broken\n"].concat();
    m.set(LOG, again.clone());
    let (_, _, r3) = open(&m);
    let second = r3.damaged.expect("damage reported");
    assert_ne!(second, name);
    assert_eq!((m.get(&name).unwrap(), m.get(&second).unwrap()), (bad, again));
}

/// `log` with its third record damaged, the damage spelled `mark` (so two such logs differ).
fn damaged_at_record_three(log: &[u8], mark: &str) -> Vec<u8> {
    let text = String::from_utf8(log.to_vec()).unwrap();
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    assert!(lines.len() > 4);
    lines[2] = lines[2].replacen("\"op\":{\"op\":\"", &format!("\"op\":{{\"op\":\"{mark}"), 1);
    format!("{}\n", lines.join("\n")).into_bytes()
}

/// A library that reaches the same change number twice with a damaged log (restored from an older
/// backup, say) keeps both damaged logs: the second never replaces the first.
#[test]
fn a_second_damaged_log_at_the_same_change_is_kept_beside_the_first() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let first = damaged_at_record_three(&clean, "x");
    m.set(LOG, first.clone());
    let (_, _, r1) = open(&m);
    let name1 = r1.damaged.expect("damage reported");
    assert_eq!(m.get(&name1).unwrap(), first);

    let second = damaged_at_record_three(&clean, "y");
    m.set(LOG, second.clone());
    let (_, _, r2) = open(&m);
    let name2 = r2.damaged.expect("damage reported again");
    assert_eq!(name2, format!("{name1}-2"), "the second copy takes the next name (the web start-up reads names of this shape)");
    assert_eq!(m.get(&name1).unwrap(), first, "the first copy is untouched");
    assert_eq!(m.get(&name2).unwrap(), second);

    let third = damaged_at_record_three(&clean, "z");
    m.set(LOG, third.clone());
    let (_, _, r3) = open(&m);
    let name3 = r3.damaged.expect("damage reported a third time");
    assert_eq!(name3, format!("{name1}-3"));
    assert_eq!(m.get(&name1).unwrap(), first);
    assert_eq!(m.get(&name2).unwrap(), second);
    assert_eq!(m.get(&name3).unwrap(), third);
}

/// The same damaged log twice (an open whose snapshot failed, then a retry) is the copy already
/// kept: no second file, and the report names the first.
#[test]
fn the_same_damaged_log_twice_is_kept_once() {
    let (m, _, _) = sample_catalog_with_log();
    let bad = damaged_at_record_three(&m.get(LOG).unwrap(), "x");
    m.set(LOG, bad.clone());
    let (_, _, r1) = open(&m);
    let name1 = r1.damaged.expect("damage reported");
    m.set(LOG, bad.clone());
    let (_, _, r2) = open(&m);
    assert_eq!(r2.damaged, Some(name1.clone()));
    assert_eq!(m.get(&name1).unwrap(), bad);
    assert_eq!(m.get(&format!("{name1}-2")), None, "no second copy of the same bytes");
}

/// A taken name `catalog.log.damaged-<seq>-<n>` (another log's copy) for each `n` in `2..=last`.
fn take_damaged_names(m: &MemStore, first: &str, last: u32) -> Vec<(String, Vec<u8>)> {
    (2..=last)
        .map(|n| {
            let (name, bytes) = (format!("{first}-{n}"), format!("another copy {n}").into_bytes());
            m.set(&name, bytes.clone());
            (name, bytes)
        })
        .collect()
}

/// With every free name taken the library is refused, its damaged log still in place (a copy is
/// never replaced to make room).
#[test]
fn a_damaged_log_with_every_name_taken_is_refused_untouched() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let kept = damaged_at_record_three(&clean, "x");
    m.set(LOG, kept.clone());
    let (_, _, r) = open(&m);
    let first = r.damaged.expect("damage reported");
    let others = take_damaged_names(&m, &first, 100);
    let bad = damaged_at_record_three(&clean, "w");
    m.set(LOG, bad.clone());
    let Err(e) = Journal::open(Box::new(m.clone())) else { panic!("the library opened with every name taken") };
    assert!(e.to_string().contains("100 damaged logs") && e.to_string().contains(&first), "{e}");
    assert_eq!(m.get(LOG).unwrap(), bad, "the damaged log stays where it is");
    assert_eq!(m.get(&first).unwrap(), kept, "the first copy is untouched");
    for (name, bytes) in others {
        assert_eq!(m.get(&name).unwrap(), bytes, "{name} is untouched");
    }
}

/// The last name, `-100`, is tried: with 2 to 99 taken the copy goes there.
#[test]
fn the_last_free_damaged_log_name_is_used() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    m.set(LOG, damaged_at_record_three(&clean, "x"));
    let (_, _, r) = open(&m);
    let first = r.damaged.expect("damage reported");
    take_damaged_names(&m, &first, 99);
    let bad = damaged_at_record_three(&clean, "w");
    m.set(LOG, bad.clone());
    let (_, _, r) = open(&m);
    assert_eq!(r.damaged, Some(format!("{first}-100")));
    assert_eq!(m.get(&format!("{first}-100")).unwrap(), bad);
}

/// A store that misbehaves around the damaged-log copies: `rival` appears under its name right
/// after a read found nothing there (another open of the same folder, between a read and a
/// write), `damaged_unreadable` fails any read of a copy, `damaged_unwritable` any write of one.
struct Copies {
    files: MemStore,
    rival: Option<(String, Vec<u8>)>,
    damaged_unreadable: bool,
    damaged_unwritable: bool,
}

impl Copies {
    fn new(files: &MemStore) -> Copies {
        Copies { files: files.clone(), rival: None, damaged_unreadable: false, damaged_unwritable: false }
    }
}

impl Store for Copies {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        if self.damaged_unreadable && name.starts_with("catalog.log.damaged-") {
            return Err(std::io::Error::other("network drive went away"));
        }
        let read = self.files.read(name)?;
        if read.is_none()
            && let Some((rival, bytes)) = self.rival.take_if(|r| r.0 == name)
        {
            self.files.set(&rival, bytes);
        }
        Ok(read)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.damaged_unwritable && name.starts_with("catalog.log.damaged-") {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.write_atomic(name, data)
    }
    fn write_new(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.damaged_unwritable && name.starts_with("catalog.log.damaged-") {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.write_new(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        self.files.describe()
    }
}

/// The name the first damaged copy of a log marked `mark` gets in a library like `m` (found by
/// keeping it in a scratch copy of its files).
fn first_copy_name(m: &MemStore, mark: &str) -> String {
    let scratch = MemStore::new();
    *scratch.files.lock().unwrap() = m.files.lock().unwrap().clone();
    scratch.set(LOG, damaged_at_record_three(&m.get(LOG).unwrap(), mark));
    open(&scratch).2.damaged.expect("damage reported")
}

/// Another open of the same folder keeps a different damaged log under the name this one just
/// found free: the copy that appeared is not replaced, this one goes to the next name.
#[test]
fn a_damaged_log_kept_by_another_open_meanwhile_is_not_replaced() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let name1 = first_copy_name(&m, "x");
    let mine = damaged_at_record_three(&clean, "y");
    m.set(LOG, mine.clone());
    let mut store = Copies::new(&m);
    store.rival = Some((name1.clone(), b"the other open's copy".to_vec()));
    Journal::open(Box::new(store)).unwrap();
    assert_eq!(m.get(&name1).unwrap(), b"the other open's copy", "the copy that appeared meanwhile is untouched");
    assert_eq!(m.get(&format!("{name1}-2")).unwrap(), mine);
}

/// … and when what appeared is this very log (two opens of the same damaged log), it is the copy.
#[test]
fn the_same_damaged_log_kept_by_another_open_meanwhile_is_the_copy() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let name1 = first_copy_name(&m, "x");
    let mine = damaged_at_record_three(&clean, "x");
    m.set(LOG, mine.clone());
    let mut store = Copies::new(&m);
    store.rival = Some((name1.clone(), mine.clone()));
    let (_, _, report) = Journal::open(Box::new(store)).unwrap();
    assert_eq!(report.damaged, Some(name1.clone()));
    assert_eq!(m.get(&name1).unwrap(), mine);
    assert_eq!(m.get(&format!("{name1}-2")), None, "no second copy of the same bytes");
}

/// The identical copy is found under a later name too, not only under the first.
#[test]
fn the_same_damaged_log_is_found_under_a_later_name() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let name1 = first_copy_name(&m, "x");
    let bad = damaged_at_record_three(&clean, "y");
    m.set(&name1, b"another log's copy".to_vec());
    m.set(&format!("{name1}-2"), bad.clone());
    m.set(LOG, bad.clone());
    let (_, _, r) = open(&m);
    assert_eq!(r.damaged, Some(format!("{name1}-2")));
    assert_eq!(m.get(&format!("{name1}-3")), None, "no new file");
    assert_eq!(m.get(&name1).unwrap(), b"another log's copy");
}

/// … even when every name is taken: the copy that is this log is what the open needs.
#[test]
fn the_same_damaged_log_is_found_when_every_name_is_taken() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let name1 = first_copy_name(&m, "x");
    let bad = damaged_at_record_three(&clean, "y");
    m.set(&name1, b"another log's copy".to_vec());
    take_damaged_names(&m, &name1, 100);
    m.set(&format!("{name1}-100"), bad.clone());
    m.set(LOG, bad);
    let (_, _, r) = open(&m);
    assert_eq!(r.damaged, Some(format!("{name1}-100")));
}

/// A copy that can't be read, or written, fails the open and leaves the log and the copies kept
/// before as they are (the next open tries again).
#[test]
fn a_damaged_log_that_cannot_be_kept_fails_the_open_untouched() {
    let (m, _, _) = sample_catalog_with_log();
    let clean = m.get(LOG).unwrap();
    let name1 = first_copy_name(&m, "x");
    m.set(&name1, b"a copy kept before".to_vec());
    let bad = damaged_at_record_three(&clean, "y");
    m.set(LOG, bad.clone());
    let files_before = m.files.lock().unwrap().clone();
    for (unreadable, unwritable) in [(true, false), (false, true)] {
        let mut store = Copies::new(&m);
        store.damaged_unreadable = unreadable;
        store.damaged_unwritable = unwritable;
        let Err(e) = Journal::open(Box::new(store)) else { panic!("the library opened without keeping the damaged log") };
        let why = e.to_string();
        assert!(why.contains("network drive went away") || why.contains("disk full"), "{why}");
        assert_eq!(*m.files.lock().unwrap(), files_before, "no file changed (unreadable {unreadable}, unwritable {unwritable})");
    }
}

#[test]
fn record_codec() {
    let op = Op::RenameAlbum { id: AlbumId(3), name: "Line\nbreak \"quoted\" }".into() };
    let line = encode_record(42, &op);
    assert!(!line.contains('\n'));
    assert_eq!(decode_record(&line), Some((42, op)));
    assert_eq!(decode_record(&line.replace("Line", "Lime")), None, "crc mismatch");
    assert_eq!(decode_record(&line[..line.len() - 1]), None);
    // log lines stay plain JSON
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["op"]["op"], "renameAlbum");
}

/// A retried record whose smart-album rules hold an object with a `seq` key of its own, and strings with
/// braces, quotes and backslashes, is still found after the fragment before it (from upstream PR #2153).
#[test]
fn fragmented_record_keeps_nested_seq_values_and_escaped_strings() {
    let rules = serde_json::from_value(serde_json::json!({
        "rules": [{"field": "title", "op": "contains", "value": {"seq": {"text": "braces { } and quotes \" \\"}}}]
    }))
    .unwrap();
    let album =
        Album { smart: Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() })), ..Album::new(AlbumId(1), "Größe ✓ { \" \\") };
    let op = Op::AddAlbum { album: album.clone() };
    let record = encode_record(1, &op);
    assert_eq!(record.matches("{\"seq\":").count(), 2);
    for fragment in [b"{\"seq\":".as_slice(), b"\xff\"{\\", b"garbage}"] {
        let m = MemStore::new();
        let mut log = fragment.to_vec();
        log.extend_from_slice(record.as_bytes());
        log.extend_from_slice(b"\r\n");
        m.set(LOG, log);
        let (_, loaded, report) = open(&m);
        assert_eq!(loaded.album(album.id), Some(&album));
        assert_eq!((report.replayed, report.torn_fragments, report.torn_bytes), (1, 1, 0));
        assert!(report.damaged.is_none());
    }
}

/// Many nested `seq` objects in one record: more than any fixed number of record starts tried from the end.
#[test]
fn a_record_with_many_nested_seq_values_is_found() {
    let rules: Vec<serde_json::Value> = (0..20).map(|i| serde_json::json!({"field": "title", "op": "contains", "value": {"seq": i}})).collect();
    let rules = serde_json::from_value(serde_json::json!({ "rules": rules })).unwrap();
    let album = Album { smart: Some(Box::new(Filter { rule_set: Some(rules), ..Default::default() })), ..Album::new(AlbumId(1), "Many") };
    let record = encode_record(1, &Op::AddAlbum { album: album.clone() });
    assert!(record.matches("{\"seq\":").count() > 8);
    let m = MemStore::new();
    let mut log = b"{\"seq\":".to_vec();
    log.extend_from_slice(record.as_bytes());
    log.push(b'\n');
    m.set(LOG, log);
    let (_, loaded, report) = open(&m);
    assert_eq!(loaded.album(album.id), Some(&album));
    assert_eq!((report.replayed, report.torn_fragments), (1, 1));
}

#[test]
fn push_history_is_bounded_and_invertible() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    c.apply(Op::AddPhoto { photo: Box::new(Photo::new(id, Source::Demo { scene: 1 }, "a", "JPEG", 1, 1, "t")) }).unwrap();
    let step = |i: usize| HistoryStep { label: format!("s{i}"), settings: Arc::new(DevelopSettings::default()), batch: None };
    for i in 0..HISTORY_LIMIT + 5 {
        c.apply(Op::PushHistory { id, step: step(i) }).unwrap();
    }
    let h = &c.photo(id).unwrap().history;
    assert_eq!(h.len(), HISTORY_LIMIT);
    assert_eq!(h[0].label, "s5");
    let before = c.to_snapshot();
    let inv = c.apply(Op::PushHistory { id, step: step(999) }).unwrap();
    c.apply(inv).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

/// A photo added with, or given, a whole History (op log replay, undo of a removal) kept every
/// step: only `PushHistory` applied the limit (#2579).
#[test]
fn add_photo_and_set_history_are_bounded_and_invertible() {
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let step = |i: usize| HistoryStep { label: format!("s{i}"), settings: Arc::new(DevelopSettings::default()), batch: None };
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a", "JPEG", 1, 1, "t");
    p.history = (0..HISTORY_LIMIT + 5).map(step).collect();
    p.history_cursor = Some(10);
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let p = c.photo(id).unwrap();
    assert_eq!((p.history.len(), p.history[0].label.as_str()), (HISTORY_LIMIT, "s5"));
    assert_eq!(p.history_cursor, Some(5), "the cursor stays on step s10");

    let before = c.to_snapshot();
    let inv = c.apply(Op::SetHistory { id, history: (0..HISTORY_LIMIT * 2).map(step).collect() }).unwrap();
    let p = c.photo(id).unwrap();
    assert_eq!(p.history.len(), HISTORY_LIMIT);
    assert_eq!(p.history[0].label, format!("s{HISTORY_LIMIT}"));
    assert_eq!(p.history_cursor, Some(0), "its step was dropped: the oldest one kept");
    c.apply(inv).unwrap();
    assert_eq!(c.to_snapshot(), before);
}

#[test]
fn fs_store_roundtrip() {
    let dir = std::env::temp_dir().join(format!("lc-journal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (mut j, mut c, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert!(r.created);
    let id = c.alloc_photo_id();
    let add = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: "/a.jpg".into() }, "a.jpg", "JPEG", 1, 1, "t")) };
    c.apply(add.clone()).unwrap();
    c.apply(Op::SetRating { id, rating: 4 }).unwrap();
    j.append(&[add, Op::SetRating { id, rating: 4 }]).unwrap();
    drop(j);
    let (mut j, c2, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert_eq!((r.replayed, r.created), (2, false));
    assert_eq!(c2.to_snapshot(), c.to_snapshot());
    j.snapshot(&c2).unwrap();
    assert_eq!(std::fs::metadata(dir.join(LOG)).unwrap().len(), 0);
    assert!(!dir.join(format!("{SNAPSHOT}.tmp")).exists());
    drop(j);
    let (_, c3, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert_eq!((r.snapshot_seq, r.replayed), (2, 0));
    assert_eq!(c3.to_snapshot(), c.to_snapshot());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A library on an SMB share (upstream issue 1407): macOS can't make the full flush there (`F_FULLFSYNC`
/// fails with ENOTSUP), so the files are flushed with a plain `fsync` and the new library's
/// snapshot, its log and `prefs.json` are still written.
#[test]
fn a_volume_without_a_full_flush_still_gets_its_files() {
    let dir = std::env::temp_dir().join(format!("lcc-journal-nofullsync-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let fsyncs = crate::safe_file::plain_fsyncs_on_this_thread();
    let c = {
        let _smb = crate::safe_file::full_sync_unsupported();
        let (mut j, mut c, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
        assert!(r.created);
        assert!(dir.join(SNAPSHOT).exists(), "the new library's snapshot is written");
        let id = c.alloc_photo_id();
        let add = Op::AddPhoto { photo: Box::new(Photo::new(id, Source::File { path: "/a.jpg".into() }, "a.jpg", "JPEG", 1, 1, "t")) };
        c.apply(add.clone()).unwrap();
        j.append(&[add]).unwrap();
        j.snapshot(&c).unwrap();
        let mut s = FsStore::open(&dir).unwrap();
        s.write_atomic("prefs.json", b"{}").unwrap();
        s.truncate(LOG, 0).unwrap();
        // at least the new library's snapshot, the snapshot, prefs.json and the truncate fell back to
        // fsync on this thread (the log's own appends and reset are synced by its writer thread;
        // the exact count depends on the write path)
        assert!(crate::safe_file::plain_fsyncs_on_this_thread() - fsyncs >= 4, "each sync falls back to fsync");
        c
    };
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
    for want in [LOG, SNAPSHOT, "prefs.json"] {
        assert!(names.iter().any(|n| n == want), "{want} written: {names:?}");
    }
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "no temp file left behind: {names:?}");
    let (_, c2, r) = Journal::open(Box::new(FsStore::open(&dir).unwrap())).unwrap();
    assert_eq!((r.snapshot_seq, c2.to_snapshot()), (1, c.to_snapshot()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// The streamed snapshot is byte-for-byte the format written before streaming (issue #37), so
/// existing libraries load and older builds can read new snapshots; FsStore and MemStore agree.
#[test]
fn streamed_snapshot_keeps_the_on_disk_format() {
    let (m, _, full) = sample_catalog_with_log();
    let (mut j, c, _) = open(&m);
    assert_eq!(c.to_snapshot(), full.to_snapshot());
    j.snapshot(&c).unwrap();
    let legacy =
        format!("{{\"format\":\"lcc-catalog\",\"version\":{},\"seq\":{},\"catalog\":{}}}\n", crate::journal::VERSION, j.seq(), c.to_snapshot());
    assert_eq!(String::from_utf8(m.get(SNAPSHOT).unwrap()).unwrap(), legacy);
    assert_eq!(j.stats().last_snapshot.bytes, legacy.len() as u64);

    let dir = std::env::temp_dir().join(format!("lc-journal-stream-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // an old-style snapshot (written by a build before streaming) loads, and is rewritten the same
    std::fs::write(dir.join(SNAPSHOT), legacy.as_bytes()).unwrap();
    let (mut fj, fc, r) = Journal::open(Box::new(FsStore::open_json(&dir).unwrap())).unwrap();
    assert_eq!((fc.to_snapshot(), r.snapshot_seq), (c.to_snapshot(), j.seq()));
    fj.snapshot(&fc).unwrap();
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), legacy.as_bytes());
    drop(fj);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A snapshot that fails while streaming (serialisation or I/O error) leaves the old file in
/// place and removes its temp file.
#[test]
fn failed_streamed_write_keeps_the_old_file() {
    let dir = std::env::temp_dir().join(format!("lc-journal-streamfail-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut s = FsStore::open(&dir).unwrap();
    s.write_atomic(SNAPSHOT, b"old").unwrap();
    let r = s.write_atomic_with(SNAPSHOT, &mut |w| {
        w.write_all(b"partial new content")?;
        Err(std::io::Error::other("serialisation failed"))
    });
    assert!(r.is_err());
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"old");
    assert!(!dir.join(format!("{SNAPSHOT}.tmp")).exists());
    assert_eq!(s.write_atomic_with(SNAPSHOT, &mut |w| w.write_all(b"new")).unwrap(), 3);
    assert_eq!(std::fs::read(dir.join(SNAPSHOT)).unwrap(), b"new");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A damaged rendering process in a saved photo (snapshot or log) reads as V1 and costs nothing
/// else: the photo and its edits load (`docs/process-versions.md`).
#[test]
fn a_damaged_process_value_loads_as_v1_with_the_edits() {
    use lcc_develop::ProcessVersion;
    let mut c = Catalog::new();
    let id = c.alloc_photo_id();
    let mut p = Photo::new(id, Source::Demo { scene: 1 }, "a.jpg", "JPEG", 10, 10, "2026-10-09T00:00:00");
    p.develop = Arc::new(DevelopSettings {
        process: ProcessVersion(7),
        light: lcc_develop::Light { exposure: 0.5, ..Default::default() },
        ..Default::default()
    });
    c.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let snapshot = c.to_snapshot();
    assert!(snapshot.contains(r#""process":7"#));
    let snapshot = format!(
        "{{\"format\":\"lcc-catalog\",\"version\":{},\"seq\":0,\"catalog\":{}}}\n",
        crate::journal::VERSION,
        snapshot.replace(r#""process":7"#, r#""process":-1"#)
    );
    let m = MemStore::new();
    m.set(SNAPSHOT, snapshot.into_bytes());
    let (_, loaded, _) = open(&m);
    let d = &loaded.photo(id).unwrap().develop;
    assert_eq!((d.process, d.light.exposure), (ProcessVersion::V1, 0.5));
    // and in a (CRC-valid) log record
    let edit =
        DevelopSettings { process: ProcessVersion(7), light: lcc_develop::Light { exposure: 0.8, ..Default::default() }, ..Default::default() };
    let op = Op::SetDevelop { id, settings: Arc::new(edit), label: "Edit".into(), edited: None };
    let body = serde_json::to_string(&op).unwrap().replace(r#""process":7"#, r#""process":"seven""#);
    assert!(body.contains("seven"));
    m.set(LOG, format!("{{\"seq\":1,\"crc\":{},\"op\":{body}}}\n", crc32fast::hash(body.as_bytes())).into_bytes());
    let (_, loaded, _) = open(&m);
    let d = &loaded.photo(id).unwrap().develop;
    assert_eq!((d.process, d.light.exposure), (ProcessVersion::V1, 0.8));
}

/// An op holding a NaN (written as `null`, so it would pass its CRC and fail to parse) is refused by
/// `append` instead of being logged: the library still opens, with everything before it (issue #2140).
#[test]
fn an_op_that_wont_read_back_is_never_logged() {
    let (m, _, c) = sample_catalog_with_log();
    let log = m.get(LOG).unwrap();
    let (mut j, _, _) = open(&m);
    let mut nan = DevelopSettings::default();
    nan.light.exposure = f64::NAN;
    let id = c.photos().next().unwrap().id;
    let op = Op::SetDevelop { id, settings: Arc::new(nan), label: "Exposure".into(), edited: None };
    let e = j.append(&[op]).unwrap_err();
    assert!(e.to_string().contains("can't be saved"), "{e}");
    assert_eq!(m.get(LOG).unwrap(), log, "nothing was appended");
    let (_, c2, r) = open(&m);
    assert_eq!((c2.to_snapshot(), r.damaged), (c.to_snapshot(), None));
}
