//! Reads that stop answering (a network drive that dropped its connection): the import or scan names the file, can
//! skip it, and ✕ does not wait for it ([`super::reads`]).

use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::import::MAX_SKIPPED;
use crate::import::scan::{ScanInput, scan_with};
use lightcraft_catalog::MediaKind;

use super::*;

/// A read that does not answer until `open`.
#[derive(Clone, Default)]
pub(crate) struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    pub(crate) fn wait(&self) {
        let (open, bell) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = bell.wait(open).unwrap();
        }
    }

    pub(crate) fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

/// How long a read may go without progress before the tests call it stalled. Long next to anything a test does between
/// two signs of progress (a few milliseconds), so that a busy machine delaying a thread by tens or hundreds of
/// milliseconds cannot make a healthy read look stalled; the tests that wait for a stall pay for it in time, and wait
/// for it with a deadline of seconds ([`until`]).
pub(crate) const STALL: Duration = Duration::from_millis(400);

/// A count of abandoned readers of its own, so that tests running in parallel in one process cannot fill each other's
/// ([`Session::stuck_readers`]).
pub(crate) fn own_stuck() -> Arc<reads::StuckReaders> {
    Arc::new(reads::StuckReaders::new(reads::MAX_STUCK_READERS))
}

/// Wait (at most 10 s) until `ok`.
pub(crate) fn until(what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(started.elapsed() < Duration::from_secs(10), "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// An import job whose reads count as stalled after [`STALL`] without progress, looked at every 5 ms, and whose probe is
/// `probe`.
fn job(probe: impl Fn(&str) -> Result<ProbeInfo, String> + Send + Sync + 'static) -> ImportJob {
    let mut s = Session::new();
    s.stuck_readers = own_stuck();
    let mut job = ImportJob::new(&mut s, ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
    job.reads.set_stall_after(STALL);
    job.reads.set_poll(Duration::from_millis(5));
    job.probe = Some(Arc::new(probe));
    job
}

fn jpeg() -> Result<ProbeInfo, String> {
    Ok(ProbeInfo { format: "JPEG".into(), ..ProbeInfo::default() })
}

fn files(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| std::env::temp_dir().join(format!("lc-reads-{n}")).to_string_lossy().into()).collect()
}

/// A probe that hangs on every file called `hang…`, and notes the ones it read.
fn hangs_on(gate: &Gate, read: &Arc<Mutex<Vec<String>>>) -> impl Fn(&str) -> Result<ProbeInfo, String> + Send + Sync + 'static {
    let (gate, read) = (gate.clone(), read.clone());
    move |path: &str| {
        let name = shown(path);
        if name.starts_with("lc-reads-hang") {
            gate.wait();
        }
        read.lock().unwrap().push(name);
        jpeg()
    }
}

fn failures(p: &Prepared) -> Vec<(String, String)> {
    p.items
        .iter()
        .filter_map(|i| match i {
            PreparedItem::Failed(path, why) => Some((shown(path), why.clone())),
            _ => None,
        })
        .collect()
}

fn ready(p: &Prepared) -> usize {
    p.items.iter().filter(|i| matches!(i, PreparedItem::Ready(_))).count()
}

/// Given an import whose second file never answers,
/// When ✕ is pressed,
/// Then the import ends within about a second, without waiting for that read.
#[test]
fn cancel_ends_the_import_while_a_read_hangs() {
    let gate = Gate::default();
    let mut job = job(hangs_on(&gate, &Default::default()));
    let paths = files(&["a.jpg", "hang.jpg", "b.jpg", "c.jpg"]);
    let cancel = Arc::new(AtomicBool::new(false));
    let (ended, ends) = std::sync::mpsc::channel();
    let in_job = cancel.clone();
    std::thread::spawn(move || {
        let prepared = job.prepare(&paths, &in_job);
        let _ = ended.send(prepared.len());
    });
    std::thread::sleep(Duration::from_millis(100));
    assert!(ends.try_recv().is_err(), "the hung read holds the import");
    let pressed = Instant::now();
    cancel.store(true, Relaxed);
    let r = ends.recv_timeout(Duration::from_secs(5));
    gate.open();
    assert!(r.is_ok(), "the import still waits for the read that hangs");
    assert!(pressed.elapsed() < Duration::from_secs(4), "took {:?}", pressed.elapsed());
}

/// Given a file that has not answered for longer than the stall time,
/// When the import is asked to skip it,
/// Then that file is named while it waits, then fails with the skip reason, and the others are read and readied.
#[test]
fn a_stalled_read_is_named_and_skipping_it_fails_that_file_and_goes_on() {
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut job = job(hangs_on(&gate, &read));
    let reads = job.reads.clone();
    let paths = files(&["a.jpg", "hang.jpg", "b.jpg", "c.jpg"]);
    assert!(reads.stalled().is_none());
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    until("the stall to be reported", || reads.stalled().is_some());
    let stalled = reads.stalled().expect("stalled");
    assert_eq!(stalled.what, "lc-reads-hang.jpg");
    assert!(stalled.waited >= STALL, "{:?}", stalled.waited);
    assert!(!worker.is_finished(), "the import waits for it");
    reads.skip();
    let prepared = worker.join().unwrap();
    gate.open();
    assert_eq!(prepared.len(), 4);
    assert_eq!(ready(&prepared), 3, "the others are readied");
    let failed = failures(&prepared);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0].0, "lc-reads-hang.jpg");
    assert!(failed[0].1.starts_with("skipped: no answer after "), "{}", failed[0].1);
    assert!(failed[0].1.ends_with(" s"), "{}", failed[0].1);
    assert_eq!(reads.skipped(), 1);
    assert!(reads.stalled().is_none(), "nothing is waited for any more");
    assert!(prepared.stopped().is_none());
}

/// Given a read that is slow but keeps making progress (a big file on a slow drive, or queueing for memory),
/// Then it is never reported as waiting, however long it takes in all.
#[test]
fn a_slow_read_that_keeps_progressing_is_never_reported() {
    let seen = Arc::new(AtomicBool::new(false));
    let mut job = job(|path: &str| {
        if shown(path).starts_with("lc-reads-slow") {
            // 60 steps of 20 ms: 1.2 s in all, longer than the stall time (1 s), but never more than a fiftieth of it
            // without progress (a step would have to be delayed fifty-fold, a whole second, to trip it)
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(20));
                reads::progressed();
            }
        }
        jpeg()
    });
    job.reads.set_stall_after(Duration::from_secs(1));
    let reads = job.reads.clone();
    let paths = files(&["slow.jpg", "a.jpg"]);
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    while !worker.is_finished() {
        if reads.stalled().is_some() {
            seen.store(true, Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let prepared = worker.join().unwrap();
    assert!(!seen.load(Relaxed), "a read that goes on was called stalled");
    assert_eq!((ready(&prepared), failures(&prepared).len()), (2, 0));
}

/// Given two reads without progress,
/// When Skip is asked for,
/// Then only the one the row names (the longer without progress) is given up; the other is named next.
#[test]
fn skip_gives_up_only_the_read_that_is_named() {
    let watch = Arc::new(ReadWatch::with_stuck(STALL, own_stuck()));
    watch.set_poll(Duration::from_millis(5));
    let gate = Gate::default();
    let cancel = Arc::new(AtomicBool::new(false));
    // job 1 starts hanging only once job 0 has: whatever the scheduling, job 0 has gone longer without progress
    let first_hangs = Arc::new(AtomicBool::new(false));
    let work: Arc<dyn Fn(u32) -> u32 + Send + Sync> = {
        let (gate, first_hangs) = (gate.clone(), first_hangs.clone());
        Arc::new(move |job| {
            if job == 0 {
                first_hangs.store(true, Relaxed);
            } else {
                while !first_hangs.load(Relaxed) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::thread::sleep(Duration::from_millis(20));
                reads::progressed();
            }
            gate.wait();
            job
        })
    };
    let (w, c) = (watch.clone(), cancel.clone());
    let run = std::thread::spawn(move || {
        let pool = reads::Pool { watch: &w, cancel: &c, name: "test-read", threads: 2 };
        pool.run(vec![0u32, 1], &|i| format!("job-{i}"), work, Arc::new(|_| {}), Arc::new(|| {}), &|_| {})
    });
    until("job 0 to be named", || watch.stalled().is_some_and(|s| s.what == "job-0"));
    // (long enough for job 1 to stall as well; job 0 is named however long this takes)
    std::thread::sleep(STALL * 2);
    assert_eq!(watch.stalled().map(|s| s.what).as_deref(), Some("job-0"), "the longest is named");
    watch.skip();
    until("job 1 to be named next", || watch.skipped() == 1 && watch.stalled().is_some_and(|s| s.what == "job-1"));
    assert!(!run.is_finished(), "job 1 was not given up with it");
    watch.skip();
    let outcomes = run.join().unwrap();
    gate.open();
    assert_eq!(watch.skipped(), 2);
    assert!(outcomes.iter().all(|o| matches!(o, reads::Outcome::Failed(why) if why.starts_with("skipped: no answer after "))), "{outcomes:?}");
}

/// Given an import where file after file stops answering,
/// When each is skipped,
/// Then at the fourth skipped read the import ends with an error naming the drive's last file, instead of hanging
/// on, and a later batch is not started.
#[test]
fn the_fourth_skipped_read_ends_the_import_with_an_error() {
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut job = job(hangs_on(&gate, &read));
    let reads = job.reads.clone();
    let mut list: Vec<String> = (0..4).map(|i| format!("hang-{i}.jpg")).collect();
    list.extend((0..8).map(|i| format!("ok-{i}.jpg")));
    let paths = files(&list.iter().map(String::as_str).collect::<Vec<_>>());
    let worker = std::thread::spawn(move || {
        let prepared = job.prepare(&paths, &AtomicBool::new(false));
        (job, prepared)
    });
    until("the import to end", || {
        if reads.stalled().is_some() {
            reads.skip();
        }
        worker.is_finished()
    });
    let (mut job, prepared) = worker.join().unwrap();
    gate.open();
    let why = prepared.stopped().expect("the import ended with an error").to_string();
    assert!(why.contains("not answering") && why.contains("lc-reads-hang-"), "{why}");
    assert_eq!(reads.ended().as_deref(), Some(why.as_str()));
    assert_eq!(reads.skipped(), MAX_SKIPPED);
    // it stays ended: the next batch is not started
    let next = job.prepare_files(files(&["later.jpg"]), &AtomicBool::new(false));
    assert!(next.is_empty() && next.stopped() == Some(why.as_str()));
    assert!(!read.lock().unwrap().iter().any(|n| n == "lc-reads-later.jpg"));
}

/// Given an import that ends at the fourth skipped read, after four files were read and four more stopped answering,
/// When the batch comes back,
/// Then the files read before the stop are readied in it and each skipped file is in it as a failure, while the files the
/// ended run never reached are in neither.
#[test]
fn an_import_that_ended_keeps_the_files_read_before_it_ended() {
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut job = job(hangs_on(&gate, &read));
    let reads = job.reads.clone();
    let mut list: Vec<String> = (0..4).map(|i| format!("ok-{i}.jpg")).collect();
    list.extend((0..4).map(|i| format!("hang-{i}.jpg")));
    list.extend((4..12).map(|i| format!("ok-{i}.jpg")));
    let paths = files(&list.iter().map(String::as_str).collect::<Vec<_>>());
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    until("the import to end", || {
        if reads.stalled().is_some() {
            reads.skip();
        }
        worker.is_finished()
    });
    let prepared = worker.join().unwrap();
    gate.open();
    assert!(prepared.stopped().is_some(), "the import ended with an error");
    let mut failed: Vec<String> = failures(&prepared).into_iter().map(|(name, _)| name).collect();
    failed.sort();
    assert_eq!(failed, ["lc-reads-hang-0.jpg", "lc-reads-hang-1.jpg", "lc-reads-hang-2.jpg", "lc-reads-hang-3.jpg"], "each skipped file is named");
    assert!(ready(&prepared) >= 4, "the four files read before the stop are in the batch: {}", ready(&prepared));
    assert_eq!(prepared.len(), ready(&prepared) + 4, "a file the ended run never reached is no failure");
}

/// Given a Move import that ends at the fourth skipped read, with a symbolic link read before the stop,
/// When the batch comes back,
/// Then the files that were to be moved are neither moved nor reported: the link is not said to have been copied, and the
/// drive that stopped answering is not asked about the files again.
#[cfg(unix)]
#[test]
fn a_move_import_that_ended_reports_nothing_for_files_it_did_not_move() {
    let t = Transfers::new("ended-move");
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut job = t.job(ImportMode::Move, false);
    job.probe = Some(Arc::new(hangs_on(&gate, &read)));
    let reads = job.reads.clone();
    let target = t.dir.join("target.jpg");
    std::fs::write(&target, b"not really a jpeg").unwrap();
    let link = t.dir.join("lc-reads-ok-link.jpg");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let mut paths = vec![link.to_string_lossy().to_string()];
    paths.extend((0..4).map(|i| t.dir.join(format!("lc-reads-hang-{i}.jpg")).to_string_lossy().to_string()));
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    until("the import to end", || {
        if reads.stalled().is_some() {
            reads.skip();
        }
        worker.is_finished()
    });
    let prepared = worker.join().unwrap();
    gate.open();
    assert!(prepared.stopped().is_some(), "the import ended with an error");
    assert_eq!(failures(&prepared).len(), 4, "each skipped file is a failure");
    assert_eq!(prepared.len(), 4, "nothing else: no file is readied, and no link is said to have been copied");
    assert!(link.exists() && target.exists(), "nothing was moved");
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Given a Move import in which three reads were skipped and then a transfer is skipped, which ends the run while the
/// files after it are being planned, the next of them a symbolic link into the same folder,
/// When the batch comes back,
/// Then the link is neither said to have been copied nor looked at on the drive that stopped answering.
#[cfg(unix)]
#[test]
fn a_move_import_that_ends_at_a_skipped_transfer_reports_nothing_for_the_files_after_it() {
    let t = Transfers::new("ended-at-transfer");
    let _hang = crate::import_move::HANG_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::import_move::HANG_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut job = t.job(ImportMode::Move, false);
    job.probe = Some(Arc::new(hangs_on(&gate, &read)));
    let reads = job.reads.clone();
    let (first, second) = (t.dir.join("lc-reads-ok.cr2"), t.dir.join("lc-reads-ok-1.cr2"));
    std::fs::write(&first, b"not really a raw").unwrap();
    // (the same name family into the same folder: the first one's transfer is run before the second is planned)
    std::os::unix::fs::symlink(&t.source, &second).unwrap();
    let mut paths: Vec<String> = (0..3).map(|i| t.dir.join(format!("lc-reads-hang-{i}.jpg")).to_string_lossy().to_string()).collect();
    paths.extend([first, second].map(|p| p.to_string_lossy().to_string()));
    let worker = std::thread::spawn(move || {
        crate::import_move::inject(crate::import_move::Fault::Hang);
        job.prepare(&paths, &AtomicBool::new(false))
    });
    until("the import to end", || {
        if reads.stalled().is_some() {
            reads.skip();
        }
        worker.is_finished()
    });
    let prepared = worker.join().unwrap();
    gate.open();
    assert!(prepared.stopped().is_some(), "the fourth skip ended the import");
    assert_eq!(failures(&prepared).len(), 4, "the three reads and the transfer that were skipped");
    assert_eq!(prepared.len(), 4, "nothing else: the link after the end is not said to have been copied");
    crate::import_move::HANG_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Given a Copy import batch that was readied and then not committed (the import stopped: `import_with` returns the
/// error, and nothing will ever catalog what the batch copied),
/// When the batch is discarded,
/// Then the copies it made are removed too, and the originals are untouched: a retry would otherwise name its own copies `-1`.
#[test]
fn a_discarded_copy_batch_takes_back_its_copies() {
    let t = Transfers::new("discard");
    let job = &mut t.job(ImportMode::Copy, false);
    let paths: Vec<String> = ["ok-0.cr2", "ok-1.cr2"]
        .iter()
        .map(|n| {
            let p = t.dir.join(n);
            std::fs::write(&p, b"not really a raw").unwrap();
            p.to_string_lossy().to_string()
        })
        .collect();
    let prepared = job.prepare(&paths, &AtomicBool::new(false));
    assert_eq!(ready(&prepared), 2);
    assert_eq!(t.left_at_destination().len(), 2, "the copies are at the destination");
    prepared.rollback();
    assert_eq!(t.left_at_destination().len(), 2, "a rollback keeps copies, as for a failed commit");
    prepared.discard();
    assert!(t.left_at_destination().is_empty(), "{:?}", t.left_at_destination());
    assert!(paths.iter().all(|p| Path::new(p).exists()), "the originals are untouched");
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Given a read that was given up and answers later, while the run goes on,
/// Then its answer is handed to the late handler and nothing else: the file stays failed.
#[test]
fn a_late_answer_from_a_given_up_read_is_ignored_while_the_run_goes_on() {
    let watch = ReadWatch::with_stuck(STALL, own_stuck());
    watch.set_poll(Duration::from_millis(5));
    let cancel = AtomicBool::new(false);
    let (gate, late_flag) = (Gate::default(), Gate::default());
    let late_seen = Arc::new(Mutex::new(Vec::<u32>::new()));
    // one reader: the second job only starts once the first was given up, and ends once its late answer arrived
    let pool = reads::Pool { watch: &watch, cancel: &cancel, name: "test-read", threads: 1 };
    let work: Arc<dyn Fn(u32) -> u32 + Send + Sync> = {
        let (gate, late_flag) = (gate.clone(), late_flag.clone());
        Arc::new(move |job| {
            if job == 0 {
                gate.wait();
                100
            } else {
                late_flag.wait();
                job
            }
        })
    };
    let late: Arc<dyn Fn(u32) + Send + Sync> = {
        let (late_seen, late_flag) = (late_seen.clone(), late_flag.clone());
        Arc::new(move |r| {
            late_seen.lock().unwrap().push(r);
            late_flag.open();
        })
    };
    let outcomes = std::thread::scope(|sc| {
        sc.spawn(|| {
            until("the stall to be reported", || watch.stalled().is_some());
            assert_eq!(watch.stalled().map(|s| s.what).as_deref(), Some("job-0"));
            watch.skip();
            until("the read to be given up", || watch.skipped() == 1);
            gate.open();
        });
        pool.run(vec![0u32, 1], &|i| format!("job-{i}"), work, late, Arc::new(|| {}), &|_| {})
    });
    assert!(matches!(outcomes.first(), Some(reads::Outcome::Failed(why)) if why.starts_with("skipped: no answer after ")), "{outcomes:?}");
    assert!(matches!(outcomes.get(1), Some(reads::Outcome::Done(1))), "{outcomes:?}");
    assert_eq!(*late_seen.lock().unwrap(), vec![100]);
}

/// A scan reads the files through the same pool: a probe that stops answering is named, and skipping it hands the
/// file back as a candidate that could not be read, with the skip as its reason.
#[test]
fn a_review_scan_names_a_stalled_file_and_lists_it_as_unreadable_once_skipped() {
    let dir = std::env::temp_dir().join(format!("lc-reads-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["a.jpg", "hang.jpg", "b.jpg"] {
        std::fs::write(dir.join(name), name).unwrap();
    }
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut s = Session::new();
    let probe = hangs_on(&gate, &read);
    // (the probe's hang is keyed on the `lc-reads-` prefix the other tests give their files)
    s.media.file_probe = Some(Arc::new(move |p: &str| probe(&format!("lc-reads-{}", shown(p)))));
    let (input, paths) = ScanInput::new(&mut s, &[dir.to_string_lossy().to_string()]);
    let progress = Arc::new(ScanProgress::new(STALL, own_stuck()));
    progress.reads.set_poll(Duration::from_millis(5));
    let p = progress.clone();
    let worker = std::thread::spawn(move || scan_with(input, &paths, &p));
    until("the stall to be reported", || progress.reads.stalled().is_some());
    assert_eq!(progress.reads.stalled().map(|s| s.what).as_deref(), Some("hang.jpg"));
    progress.reads.skip();
    let out = worker.join().unwrap();
    gate.open();
    let by_name = |n: &str| out.candidates.iter().find(|c| c.name == n).unwrap_or_else(|| panic!("no candidate {n}"));
    assert!(by_name("hang.jpg").error.as_deref().is_some_and(|e| e.starts_with("skipped: no answer after ")), "{:?}", by_name("hang.jpg"));
    assert!(by_name("a.jpg").error.is_none() && by_name("b.jpg").error.is_none());
    assert!(!out.probes.contains_key(&dir.join("hang.jpg").to_string_lossy().to_string()), "nothing is kept of a file never read");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a read abandoned on the process-wide count (a session that is not a test's own),
/// When a transfer test builds its job,
/// Then that job counts abandoned readers on a count of its own: another test's hung reads cannot fill it and end its run.
#[test]
fn a_transfer_job_does_not_share_the_process_wide_stuck_count() {
    let (gate, read) = (Gate::default(), Arc::<Mutex<Vec<String>>>::default());
    let mut other = ImportJob::new(&mut Session::new(), ImportOptions { mode: ImportMode::Add, ..ImportOptions::default() }).unwrap();
    other.reads.set_stall_after(STALL);
    other.reads.set_poll(Duration::from_millis(5));
    other.probe = Some(Arc::new(hangs_on(&gate, &read)));
    let reads = other.reads.clone();
    let paths = files(&["hang-shared.jpg"]);
    let worker = std::thread::spawn(move || other.prepare(&paths, &AtomicBool::new(false)));
    until("the read to be given up", || {
        if reads.stalled().is_some() {
            reads.skip();
        }
        worker.is_finished()
    });
    worker.join().unwrap();
    let t = Transfers::new("own-stuck");
    assert_eq!(t.job(ImportMode::Copy, false).reads.stuck_readers().alive(), 0, "the count is the transfer job's own");
    gate.open();
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// A files folder, a destination folder, and a probe that says every file is a raw.
struct Transfers {
    dir: PathBuf,
    source: PathBuf,
    dest: PathBuf,
}

impl Transfers {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-reads-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        let source = dir.join("IMG_1.cr2");
        std::fs::write(&source, b"not really a raw").unwrap();
        Transfers { source, dest: dir.join("dest"), dir }
    }

    fn job(&self, mode: ImportMode, convert_dng: bool) -> ImportJob {
        let opts = ImportOptions { mode, convert_dng, destination: Some(self.dest.to_string_lossy().to_string()), ..ImportOptions::default() };
        let mut s = Session::new();
        s.stuck_readers = own_stuck();
        let mut job = ImportJob::new(&mut s, opts).unwrap();
        job.reads.set_stall_after(STALL);
        job.reads.set_poll(Duration::from_millis(5));
        job.probe = Some(Arc::new(|_: &str| Ok(ProbeInfo { format: "CR2".into(), kind: MediaKind::Raw, ..ProbeInfo::default() })));
        job
    }

    /// What is at the destination, as file names.
    fn left_at_destination(&self) -> Vec<String> {
        let mut found = Vec::new();
        let mut folders = vec![self.dest.clone()];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(folder).unwrap().flatten() {
                match entry.file_type() {
                    Ok(t) if t.is_dir() => folders.push(entry.path()),
                    _ => found.push(entry.file_name().to_string_lossy().to_string()),
                }
            }
        }
        found
    }

    fn assert_source_intact(&self) {
        assert_eq!(std::fs::read(&self.source).unwrap(), b"not really a raw", "the original is as it was");
    }
}

/// Given a Copy as DNG whose conversion never answers (the raw is read back from the drive for it),
/// When Skip is asked for and the drive answers later,
/// Then the file fails as skipped, the original is untouched, and the copy the late thread leaves is taken back.
#[test]
fn a_skipped_copy_as_dng_leaves_the_source_alone_and_nothing_at_the_destination() {
    let t = Transfers::new("dng");
    let gate = Gate::default();
    let mut job = t.job(ImportMode::Copy, true);
    let in_reader = gate.clone();
    job.file_bytes = Some(Arc::new(move |_: &str| {
        in_reader.wait();
        Err("the drive answered too late".into())
    }));
    let reads = job.reads.clone();
    let paths = vec![t.source.to_string_lossy().to_string()];
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    until("the stall to be reported", || reads.stalled().is_some());
    reads.skip();
    let prepared = worker.join().unwrap();
    let failed = failures(&prepared);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].1.starts_with("skipped: no answer after "), "{}", failed[0].1);
    assert_eq!(ready(&prepared), 0);
    t.assert_source_intact();
    gate.open();
    until("the late copy to be taken back", || t.left_at_destination().is_empty());
    t.assert_source_intact();
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Given a Move whose copy to the destination never answers half-way,
/// When Skip is asked for and the drive answers later,
/// Then the file fails as skipped, the original stays where it was, and what the late thread placed is taken back.
#[test]
fn a_skipped_move_keeps_the_source_and_leaves_nothing_at_the_destination() {
    let t = Transfers::new("move");
    let _hang = crate::import_move::HANG_TESTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    crate::import_move::HANG_OPEN.store(false, std::sync::atomic::Ordering::SeqCst);
    let mut job = t.job(ImportMode::Move, false);
    let reads = job.reads.clone();
    let paths = vec![t.source.to_string_lossy().to_string()];
    let worker = std::thread::spawn(move || {
        // (the readers inherit the fault of the thread that starts them)
        crate::import_move::inject(crate::import_move::Fault::Hang);
        job.prepare(&paths, &AtomicBool::new(false))
    });
    until("the stall to be reported", || reads.stalled().is_some());
    assert!(!t.left_at_destination().is_empty(), "half a copy is there while it hangs");
    reads.skip();
    let prepared = worker.join().unwrap();
    let failed = failures(&prepared);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].1.starts_with("skipped: no answer after "), "{}", failed[0].1);
    t.assert_source_intact();
    crate::import_move::HANG_OPEN.store(true, std::sync::atomic::Ordering::SeqCst);
    until("the late copy to be taken back", || t.left_at_destination().is_empty());
    t.assert_source_intact();
    let _ = std::fs::remove_dir_all(&t.dir);
}

/// Given a read that waits for memory (a gate of the test's own, held by other work) for longer than the stall time,
/// Then it is never reported as stalled: waiting for memory is not a drive that stopped answering.
#[test]
fn a_read_queued_at_the_memory_gate_is_never_reported() {
    let gate = Arc::new(crate::memory::WorkGate::new(100).with_heartbeat(Duration::from_millis(20)));
    // (the gate says every 20 ms that a wait goes on: fifty times in the stall time)
    let watch = ReadWatch::with_stuck(Duration::from_secs(1), own_stuck());
    watch.set_poll(Duration::from_millis(5));
    let cancel = AtomicBool::new(false);
    // held by work that is not a pool's (an interactive render, say)
    let held = gate.acquire(100);
    let work: Arc<dyn Fn(u32) -> u32 + Send + Sync> = {
        let gate = gate.clone();
        Arc::new(move |job| {
            let _permit = gate.acquire(1);
            job
        })
    };
    let outcomes = std::thread::scope(|sc| {
        sc.spawn(|| {
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(2200) {
                assert!(watch.stalled().is_none(), "waiting at the gate was called a stall after {:?}", started.elapsed());
                std::thread::sleep(Duration::from_millis(5));
            }
            drop(held);
        });
        let pool = reads::Pool { watch: &watch, cancel: &cancel, name: "test-read", threads: 1 };
        pool.run(vec![7u32], &|i| format!("job-{i}"), work, Arc::new(|_| {}), Arc::new(|| {}), &|_| {})
    });
    assert!(matches!(outcomes.first(), Some(reads::Outcome::Done(7))), "{outcomes:?}");
}

/// Given a reader that holds the whole memory gate and was given up (it keeps the memory until the drive answers),
/// When other reads need the gate,
/// Then each is named as stalled (the wait is blocked by abandoned work only, which is not progress), Skip works on
/// it, and the fourth skipped read ends the run: the import cannot sit there forever.
#[test]
fn reads_blocked_only_by_an_abandoned_holder_are_named_and_the_cap_ends_the_run() {
    let gate = Arc::new(crate::memory::WorkGate::new(100).with_heartbeat(Duration::from_millis(20)));
    let (drive, release) = (Gate::default(), Gate::default());
    let watch = ReadWatch::with_stuck(Duration::from_millis(600), own_stuck());
    // (thirty times the gate's heartbeat: were a wait for abandoned memory counted as progress, the clock would be
    // reset before it could run out)
    watch.set_poll(Duration::from_millis(5));
    let cancel = AtomicBool::new(false);
    let work: Arc<dyn Fn(u32) -> u32 + Send + Sync> = {
        let (gate, drive, release) = (gate.clone(), drive.clone(), release.clone());
        Arc::new(move |job| {
            let _permit = gate.acquire(100);
            if job == 0 {
                // the drive stops answering while this read holds all the memory
                drive.wait();
            }
            release.wait();
            job
        })
    };
    let outcomes = std::thread::scope(|sc| {
        sc.spawn(|| {
            // (a failed wait below must end the run too, or the scope would never end)
            struct Stop<'a>(&'a AtomicBool);
            impl Drop for Stop<'_> {
                fn drop(&mut self) {
                    self.0.store(true, Relaxed);
                }
            }
            let _stop = Stop(&cancel);
            until("the first read to be named", || watch.stalled().is_some_and(|s| s.what == "job-0"));
            watch.skip();
            until("the first read to be given up", || watch.skipped() == 1);
            assert_eq!(gate.abandoned_bytes(), 100, "its memory stays counted");
            // the next read waits for that memory, which is not progress: it is named in its turn
            for n in 2..=4 {
                until("a read waiting at the gate to be named", || watch.stalled().is_some_and(|s| s.what == format!("job-{}", n - 1)));
                watch.skip();
                until("it to be given up", || watch.skipped() == n);
            }
            release.open();
            drive.open();
        });
        let pool = reads::Pool { watch: &watch, cancel: &cancel, name: "test-read", threads: 1 };
        pool.run((0..6u32).collect(), &|i| format!("job-{i}"), work, Arc::new(|_| {}), Arc::new(|| {}), &|_| {})
    });
    assert_eq!(watch.skipped(), MAX_SKIPPED);
    assert!(watch.ended().is_some_and(|why| why.contains("not answering")), "{:?}", watch.ended());
    assert!(outcomes.iter().take(4).all(|o| matches!(o, reads::Outcome::Failed(_))), "{outcomes:?}");
    assert!(outcomes.iter().skip(4).all(|o| matches!(o, reads::Outcome::Unreached)), "{outcomes:?}");
}

/// Given reads that block in the operating system and stay blocked after their run was cancelled,
/// When the run is cancelled and started again and again,
/// Then the readers still blocked from earlier runs stay bounded across runs: a run that finds the cap reached starts
/// no reader and ends with the "drive is not answering" error, and once the drive lets go the next run completes.
#[test]
fn cancel_and_restart_cannot_pile_up_blocked_readers() {
    const CAP: usize = 6;
    const THREADS: usize = 3;
    let stuck = Arc::new(reads::StuckReaders::new(CAP));
    let (gate, entered, inside) = (Gate::default(), Arc::new(Mutex::new(0usize)), Arc::new(Mutex::new((0usize, 0usize))));
    let work: Arc<dyn Fn(u32) -> u32 + Send + Sync> = {
        let (gate, entered, inside) = (gate.clone(), entered.clone(), inside.clone());
        Arc::new(move |job| {
            *entered.lock().unwrap() += 1;
            {
                let mut n = inside.lock().unwrap();
                n.0 += 1;
                n.1 = n.1.max(n.0);
            }
            gate.wait();
            inside.lock().unwrap().0 -= 1;
            job
        })
    };
    // one run of `jobs` reads; with `cancel_after_starts`, ✕ is pressed once that many readers are blocked inside
    // `work` (no timing guess: it waits for them)
    let attempt = |jobs: u32, cancel_after_starts: Option<usize>| {
        let watch = ReadWatch::with_stuck(STALL, stuck.clone());
        watch.set_poll(Duration::from_millis(5));
        let cancel = AtomicBool::new(false);
        let over = AtomicBool::new(false);
        let before = *entered.lock().unwrap();
        let outcomes = std::thread::scope(|sc| {
            let (cancel, entered, over) = (&cancel, &entered, &over);
            sc.spawn(move || {
                match cancel_after_starts {
                    Some(n) => until("the readers to start", || *entered.lock().unwrap() >= before + n),
                    // nothing is to start, or the run is meant to finish: ✕ only as a way out if it does not
                    None => {
                        let waited = Instant::now();
                        while !over.load(Relaxed) && waited.elapsed() < Duration::from_secs(10) {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                }
                cancel.store(true, Relaxed);
            });
            let pool = reads::Pool { watch: &watch, cancel, name: "test-read", threads: THREADS };
            let outcomes = pool.run((0..jobs).collect(), &|i| format!("job-{i}"), work.clone(), Arc::new(|_| {}), Arc::new(|| {}), &|_| {});
            over.store(true, Relaxed);
            outcomes
        });
        (watch, outcomes, *entered.lock().unwrap() - before)
    };
    for run in 0..6 {
        let capped = run >= CAP / THREADS;
        let (watch, outcomes, started) = attempt(5, (!capped).then_some(THREADS));
        if capped {
            assert_eq!(started, 0, "run {run} finds the cap reached and starts nothing");
            assert!(watch.ended().is_some_and(|why| why.contains("not answering")), "{:?}", watch.ended());
            assert!(outcomes.iter().all(|o| matches!(o, reads::Outcome::Unreached)));
            assert_eq!(stuck.alive(), CAP, "and adds nothing to what is blocked");
        } else {
            assert_eq!(started, THREADS, "run {run} starts its readers");
            assert!(watch.ended().is_none());
            assert_eq!(stuck.alive(), THREADS * (run + 1), "every reader the cancelled run left blocked is counted");
        }
    }
    assert!(inside.lock().unwrap().1 <= CAP, "never more readers blocked at once than the cap");
    gate.open();
    until("the blocked readers to exit", || stuck.alive() == 0);
    let (watch, outcomes, _) = attempt(2, None);
    assert!(watch.ended().is_none());
    assert!(outcomes.iter().all(|o| matches!(o, reads::Outcome::Done(_))), "{outcomes:?}");
}

/// Given a share that is slow but answers (32 KiB per 10 ms sleep, so a whole 2 MiB file takes 0.64 s at the least and
/// about 1.3 s measured) and a stall time of 0.4 s,
/// When the file is read in steps ([`crate::files::fs::read_steps`]),
/// Then it is never reported as waiting: a step is 64 KiB (20 ms at the least, about 40 ms measured), and the clock
/// starts again after each.
#[test]
fn a_file_on_a_slow_share_is_read_in_steps_that_keep_the_stall_clock_going() {
    struct Slow(usize);
    impl std::io::Read for Slow {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0 == 0 {
                return Ok(0);
            }
            std::thread::sleep(Duration::from_millis(10));
            let n = buf.len().min(32 << 10).min(self.0);
            buf[..n].fill(1);
            self.0 -= n;
            Ok(n)
        }
    }
    let seen = Arc::new(AtomicBool::new(false));
    let mut job = job(|path: &str| {
        if shown(path).starts_with("lc-reads-slow") {
            let mut bytes = Vec::new();
            crate::files::fs::read_steps(&mut Slow(2 << 20), &mut bytes).unwrap();
            assert_eq!(bytes.len(), 2 << 20);
        }
        jpeg()
    });
    job.reads.set_stall_after(Duration::from_millis(400));
    let reads = job.reads.clone();
    let paths = files(&["slow.jpg", "a.jpg"]);
    let worker = std::thread::spawn(move || job.prepare(&paths, &AtomicBool::new(false)));
    while !worker.is_finished() {
        if reads.stalled().is_some() {
            seen.store(true, Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let prepared = worker.join().unwrap();
    assert!(!seen.load(Relaxed), "a slow read that goes on was called stalled");
    assert_eq!((ready(&prepared), failures(&prepared).len()), (2, 0));
}

/// Given a Copy as DNG import of a raw file on a share that is slow but answers (a named pipe fed 32 KiB every 10 ms,
/// so the whole file takes longer than the stall time) and a stall time of 0.4 s,
/// When `write_dng_with` reads the file,
/// Then it is never reported as waiting: the read goes in steps, each saying that it goes on.
#[cfg(unix)]
#[test]
fn the_raw_read_of_a_dng_conversion_on_a_slow_share_keeps_the_stall_clock_going() {
    use std::io::Write;
    // (the process id in the name: two test runs at once must not share one pipe)
    let paths = files(&[&format!("dng-pipe-{}.jpg", std::process::id()), "a.jpg"]);
    let pipe = std::path::PathBuf::from(&paths[0]);
    let _ = std::fs::remove_file(&pipe);
    assert!(std::process::Command::new("mkfifo").arg(&pipe).status().unwrap().success());
    let feed = {
        let pipe = pipe.clone();
        std::thread::spawn(move || {
            let mut w = std::fs::OpenOptions::new().write(true).open(pipe).unwrap();
            for _ in 0..64 {
                std::thread::sleep(Duration::from_millis(10));
                if w.write_all(&[1u8; 32 << 10]).is_err() {
                    return;
                }
            }
        })
    };
    let mut job = job(|path: &str| {
        if shown(path).starts_with("lc-reads-dng-pipe") {
            // (the bytes are no raw file: the conversion fails after its read, which is all this is about)
            assert!(crate::cmd::convert::write_dng_with(None, path, String::new()).is_err());
        }
        jpeg()
    });
    job.reads.set_stall_after(Duration::from_millis(400));
    let reads = job.reads.clone();
    let list = paths.clone();
    let worker = std::thread::spawn(move || job.prepare(&list, &AtomicBool::new(false)));
    let seen = AtomicBool::new(false);
    while !worker.is_finished() {
        if reads.stalled().is_some() {
            seen.store(true, Relaxed);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    worker.join().unwrap();
    feed.join().unwrap();
    let _ = std::fs::remove_file(&pipe);
    assert!(!seen.load(Relaxed), "a slow raw read that goes on was called stalled");
}
