//! The detached readers on their own ([`super::reads`]): the pool is driven with fake reads that hang, make progress,
//! panic and answer late, with no import around it. What a stalled read looks like to the person is up to whoever uses
//! the pool; these tests are about what the pool does: ✕, naming the stalled read, Skip, the cap, a late answer.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::reads::{MAX_SKIPPED, MAX_STUCK_READERS, Outcome, Pool, ReadWatch, StuckReaders, abandon_flag, progressed};

/// How long a read may go without progress before the tests call it stalled. Long next to anything a test does between
/// two signs of progress (a few milliseconds), so that a busy machine delaying a thread does not name a healthy read.
const STALL: Duration = Duration::from_millis(500);

/// A read that does not answer until `open`.
#[derive(Clone, Default)]
struct Gate(Arc<(Mutex<bool>, Condvar)>);

impl Gate {
    fn wait(&self) {
        let (open, bell) = &*self.0;
        let mut open = open.lock().unwrap();
        while !*open {
            open = bell.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.0.0.lock().unwrap() = true;
        self.0.1.notify_all();
    }
}

fn stuck() -> Arc<StuckReaders> {
    Arc::new(StuckReaders::new(MAX_STUCK_READERS))
}

/// A watch that looks every 10 ms (not 250) and counts its abandoned readers in `stuck`.
fn watch(stuck: &Arc<StuckReaders>) -> Arc<ReadWatch> {
    let watch = ReadWatch::with_stuck(STALL, stuck.clone());
    watch.set_poll(Duration::from_millis(10));
    Arc::new(watch)
}

/// Run `work` over `jobs` on `threads` readers, job `i` named "job<i>"; results that arrive late go to `late`.
fn run<J: Send + 'static, R: Send + 'static>(
    watch: &ReadWatch,
    cancel: &AtomicBool,
    threads: usize,
    jobs: Vec<J>,
    work: impl Fn(J) -> R + Send + Sync + 'static,
    late: impl Fn(R) + Send + Sync + 'static,
) -> Vec<Outcome<R>> {
    let pool = Pool { watch, cancel, name: "test-read", threads };
    pool.run(jobs, &|i| format!("job{i}"), Arc::new(work), Arc::new(late), Arc::new(|| {}), &|_| {})
}

/// [`run`] on a thread of its own, so that the test can watch it and press ✕ or Skip meanwhile.
fn run_in_background<J: Send + 'static, R: Send + 'static>(
    watch: &Arc<ReadWatch>,
    cancel: &Arc<AtomicBool>,
    threads: usize,
    jobs: Vec<J>,
    work: impl Fn(J) -> R + Send + Sync + 'static,
    late: impl Fn(R) + Send + Sync + 'static,
) -> JoinHandle<Vec<Outcome<R>>> {
    let (watch, cancel) = (watch.clone(), cancel.clone());
    std::thread::spawn(move || run(&watch, &cancel, threads, jobs, work, late))
}

/// Wait for `condition` for up to 30 s (a hung test is worse than a slow one).
fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !condition() {
        assert!(Instant::now() < until, "gave up waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Wait for a run started with [`run_in_background`] to end, for up to 30 s: a run that never ends fails the test
/// instead of hanging it.
fn join_within<R>(handle: JoinHandle<R>) -> R {
    wait_until("the run to end", || handle.is_finished());
    handle.join().unwrap()
}

fn is_skipped<R>(outcome: &Outcome<R>) -> bool {
    matches!(outcome, Outcome::Failed(why) if why.starts_with("skipped: no answer after "))
}

#[test]
fn every_job_is_done_and_counted_in_its_own_slot() {
    let (stuck, cancel) = (stuck(), AtomicBool::new(false));
    let watch = watch(&stuck);
    let counted = AtomicUsize::new(0);
    let pool = Pool { watch: &watch, cancel: &cancel, name: "test-read", threads: 3 };
    let outcomes = pool.run((0..10u32).collect(), &|i| format!("job{i}"), Arc::new(|n: u32| n * 2), Arc::new(|_| {}), Arc::new(|| {}), &|r| {
        assert!(r.is_some());
        counted.fetch_add(1, Relaxed);
    });
    let got: Vec<u32> = outcomes.iter().map(|o| if let Outcome::Done(n) = o { *n } else { u32::MAX }).collect();
    assert_eq!(got, (0..10).map(|n| n * 2).collect::<Vec<_>>());
    assert_eq!(counted.load(Relaxed), 10);
    assert_eq!((watch.skipped(), watch.ended(), watch.stalled()), (0, None, None));
    assert_eq!(stuck.alive(), 0);
}

#[test]
fn a_panicking_job_fails_only_itself() {
    let (stuck, cancel) = (stuck(), AtomicBool::new(false));
    let watch = watch(&stuck);
    let outcomes = run(
        &watch,
        &cancel,
        2,
        vec![1u32, 2, 3, 4],
        |n| {
            assert_ne!(n, 3, "a read that panics");
            n
        },
        |_| {},
    );
    assert!(matches!(outcomes[0], Outcome::Done(1)) && matches!(outcomes[1], Outcome::Done(2)) && matches!(outcomes[3], Outcome::Done(4)));
    assert!(matches!(&outcomes[2], Outcome::Failed(_)), "{:?}", outcomes[2]);
    assert_eq!(watch.ended(), None, "one failed read does not end the run");
}

/// Given reads that hang,
/// When ✕ is pressed,
/// Then the run ends at once with what answered so far, the hung reads are left behind and the jobs still queued never
/// start.
#[test]
fn cancel_ends_the_run_while_reads_hang() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let (gate, started) = (Gate::default(), Arc::new(AtomicUsize::new(0)));
    let (g, s) = (gate.clone(), started.clone());
    let run = run_in_background(
        &watch,
        &cancel,
        2,
        (0..8u32).collect(),
        move |n| {
            s.fetch_add(1, Relaxed);
            if n < 2 {
                g.wait();
            }
            n
        },
        |_| {},
    );
    wait_until("both readers to be on a hung read", || started.load(Relaxed) == 2);
    let pressed = Instant::now();
    cancel.store(true, Relaxed);
    let outcomes = join_within(run);
    assert!(pressed.elapsed() < Duration::from_secs(3), "the run took {:?} to end", pressed.elapsed());
    assert!(outcomes.iter().all(|o| matches!(o, Outcome::Unreached)), "{outcomes:?}");
    assert_eq!(started.load(Relaxed), 2, "no queued job was started");
    assert_eq!(stuck.alive(), 2, "the two readers that were left behind are counted");
    gate.open();
    wait_until("the readers left behind to exit", || stuck.alive() == 0);
    assert_eq!(started.load(Relaxed), 2, "and they took no job on their way out");
}

/// Given a read that makes progress now and then for much longer than the stall time,
/// Then it is never named.
#[test]
fn a_slow_read_that_keeps_progressing_is_never_named() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let run = run_in_background(
        &watch,
        &cancel,
        1,
        vec![()],
        |()| {
            for _ in 0..120 {
                std::thread::sleep(Duration::from_millis(20));
                progressed();
            }
        },
        |_| {},
    );
    let mut named = None;
    wait_until("the run to end", || {
        named = named.take().or(watch.stalled());
        run.is_finished()
    });
    assert!(matches!(join_within(run)[0], Outcome::Done(())));
    assert_eq!(named, None, "a read that goes on is not stalled (it ran for {:?}, the stall time is {STALL:?})", Duration::from_millis(2400));
}

/// Given a read that does not progress,
/// Then it is named after the stall time, and nothing is named once it answers.
#[test]
fn a_read_without_progress_is_named_until_it_answers() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let gate = Gate::default();
    let g = gate.clone();
    let run = run_in_background(
        &watch,
        &cancel,
        1,
        vec![0u32],
        move |n| {
            g.wait();
            n
        },
        |_| {},
    );
    wait_until("the read to be named", || watch.stalled().is_some());
    let named = watch.stalled().unwrap();
    assert_eq!(named.what, "job0");
    assert!(named.waited >= STALL, "{named:?}");
    gate.open();
    assert!(matches!(join_within(run)[0], Outcome::Done(0)));
    assert_eq!(watch.stalled(), None);
}

/// Given two reads that both hang,
/// When Skip is asked for,
/// Then only the one that is named (the longest without progress) is given up, a fresh reader takes its place so that
/// the other jobs go on, and the other read is named next.
#[test]
fn skip_gives_up_only_the_read_that_is_named() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let gates = [Gate::default(), Gate::default()];
    let hung = gates.clone();
    let late = Arc::new(Mutex::new(Vec::new()));
    let l = late.clone();
    let reading = run_in_background(
        &watch,
        &cancel,
        2,
        vec![0usize, 1, 2, 3],
        move |n| {
            if let Some(gate) = hung.get(n) {
                gate.wait();
            }
            n
        },
        move |n| l.lock().unwrap().push(n),
    );
    // (which of the two is named first depends on which reader took its job first)
    wait_until("a read to be named", || watch.stalled().is_some());
    let first: usize = watch.stalled().unwrap().what.trim_start_matches("job").parse().unwrap();
    let second = 1 - first;
    watch.skip();
    wait_until("the other read to be named after it", || watch.stalled().is_some_and(|s| s.what == format!("job{second}")));
    assert_eq!(watch.skipped(), 1);
    gates[second].open();
    let outcomes = join_within(reading);
    assert!(is_skipped(&outcomes[first]), "{:?}", outcomes[first]);
    assert!(matches!(outcomes[second], Outcome::Done(n) if n == second), "the other hung read was not skipped: {:?}", outcomes[second]);
    assert!(matches!(outcomes[2], Outcome::Done(2)) && matches!(outcomes[3], Outcome::Done(3)), "a fresh reader goes on");
    assert_eq!((watch.skipped(), watch.ended()), (1, None));
    assert_eq!(stuck.alive(), 1, "the given-up read is still blocked, and counted");
    assert!(late.lock().unwrap().is_empty());
    gates[first].open();
    wait_until("the given-up read to exit", || stuck.alive() == 0);
}

/// Given a given-up read that answers after all,
/// Then its result goes to the late handler (which disposes of it: nothing is applied), and the job stays failed.
#[test]
fn a_late_answer_from_a_given_up_read_goes_to_the_late_handler() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let gate = Gate::default();
    let g = gate.clone();
    let late = Arc::new(Mutex::new(Vec::new()));
    let l = late.clone();
    let run = run_in_background(
        &watch,
        &cancel,
        1,
        vec![7u32, 8],
        move |n| {
            if n == 7 {
                g.wait();
            }
            n
        },
        move |n| l.lock().unwrap().push(n),
    );
    wait_until("job0 to be named", || watch.stalled().is_some());
    watch.skip();
    let outcomes = join_within(run);
    assert!(is_skipped(&outcomes[0]) && matches!(outcomes[1], Outcome::Done(8)), "{outcomes:?}");
    gate.open();
    wait_until("the late answer", || !late.lock().unwrap().is_empty());
    assert_eq!(*late.lock().unwrap(), vec![7]);
    assert!(is_skipped(&outcomes[0]), "the job stays failed");
    wait_until("the given-up read to exit", || stuck.alive() == 0);
}

/// A Skip asked for when nothing has stalled, or for a read that answered meanwhile, gives up nothing.
#[test]
fn a_skip_with_nothing_stalled_does_nothing() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let run = run_in_background(
        &watch,
        &cancel,
        1,
        vec![0u32],
        |n| {
            std::thread::sleep(Duration::from_millis(150));
            n
        },
        |_| {},
    );
    watch.skip();
    assert!(matches!(join_within(run)[0], Outcome::Done(0)));
    assert_eq!((watch.skipped(), watch.ended(), stuck.alive()), (0, None, 0));
    // and a request made before the next stall is not kept for it
    let gate = Gate::default();
    let g = gate.clone();
    let run = run_in_background(
        &watch,
        &cancel,
        1,
        vec![0u32],
        move |n| {
            g.wait();
            n
        },
        |_| {},
    );
    watch.skip();
    wait_until("the read to be named", || watch.stalled().is_some());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(watch.skipped(), 0, "a Skip made while nothing was named is not kept");
    gate.open();
    assert!(matches!(join_within(run)[0], Outcome::Done(0)));
}

/// Given a drive whose reads all hang,
/// When each stalled read is skipped as it is named,
/// Then the fourth skip ends the run with an error naming the last file, instead of leaving more threads blocked.
#[test]
fn the_fourth_skipped_read_ends_the_run() {
    let (stuck, cancel) = (stuck(), Arc::new(AtomicBool::new(false)));
    let watch = watch(&stuck);
    let gate = Gate::default();
    let g = gate.clone();
    let started = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let run = run_in_background(
        &watch,
        &cancel,
        2,
        (0..12u32).collect(),
        move |n| {
            s.fetch_add(1, Relaxed);
            g.wait();
            n
        },
        |_| {},
    );
    // (Skip is asked for only once every reader is on a read: the two first ones, then the replacement each Skip
    // started, so that how many readers are left behind at the end does not depend on how quickly a thread starts)
    wait_until("the run to end", || {
        if watch.stalled().is_some() && started.load(Relaxed) == watch.skipped() + 2 {
            watch.skip();
        }
        run.is_finished()
    });
    let outcomes = join_within(run);
    assert_eq!(MAX_SKIPPED, 4);
    assert_eq!(watch.skipped(), MAX_SKIPPED);
    let why = watch.ended().expect("the run ended with an error");
    assert!(why.contains("not answering") && why.contains("job"), "{why}");
    assert_eq!(outcomes.iter().filter(|o| is_skipped(o)).count(), MAX_SKIPPED);
    assert!(outcomes.iter().all(|o| is_skipped(o) || matches!(o, Outcome::Unreached)), "{outcomes:?}");
    // (the four given up, and the one reader still on a read when the run ended: it is left behind too, and no fifth
    // reader was started)
    assert_eq!(stuck.alive(), MAX_SKIPPED + 1);
    gate.open();
    wait_until("the given-up reads to exit", || stuck.alive() == 0);
}

/// Given too many given-up reads still blocked (of any run, on any drive),
/// Then a run does not start (and says why), and does once the reads let go.
#[test]
fn a_run_does_not_start_while_too_many_given_up_reads_are_blocked() {
    let (stuck, cancel) = (Arc::new(StuckReaders::new(1)), Arc::new(AtomicBool::new(false)));
    let first = watch(&stuck);
    let gate = Gate::default();
    let g = gate.clone();
    let reading = run_in_background(
        &first,
        &cancel,
        1,
        vec![0u32, 1],
        move |n| {
            g.wait();
            n
        },
        |_| {},
    );
    wait_until("the read to be named", || first.stalled().is_some());
    first.skip();
    let outcomes = join_within(reading);
    assert!(is_skipped(&outcomes[0]) && matches!(outcomes[1], Outcome::Unreached), "{outcomes:?}");
    let why = first.ended().expect("the cap ended the run");
    assert!(why.contains("still waiting"), "{why}");
    // another run, another watch, the same count
    let second = watch(&stuck);
    let started = Arc::new(AtomicUsize::new(0));
    let s = started.clone();
    let outcomes = run(
        &second,
        &cancel,
        2,
        vec![1u32, 2],
        move |n| {
            s.fetch_add(1, Relaxed);
            n
        },
        |_| {},
    );
    assert!(outcomes.iter().all(|o| matches!(o, Outcome::Unreached)), "{outcomes:?}");
    assert_eq!(started.load(Relaxed), 0, "nothing was started");
    assert!(second.ended().is_some_and(|why| why.contains("still waiting")));
    gate.open();
    wait_until("the given-up read to exit", || stuck.alive() == 0);
    let third = watch(&stuck);
    let outcomes = run(&third, &cancel, 2, vec![1u32, 2], |n| n, |_| {});
    assert!(outcomes.iter().all(|o| matches!(o, Outcome::Done(_))), "{outcomes:?}");
}

/// Given an `init` that panics on every reader,
/// Then the jobs are still read: a reader that could not be set up is not left waiting for a job it never takes.
#[test]
fn a_panicking_init_does_not_strand_the_jobs() {
    let stuck = stuck();
    let watch = watch(&stuck);
    let pool_watch = watch.clone();
    let reading = std::thread::spawn(move || {
        let cancel = AtomicBool::new(false);
        let pool = Pool { watch: &pool_watch, cancel: &cancel, name: "test-read", threads: 2 };
        pool.run(
            vec![1u32, 2, 3],
            &|i| format!("job{i}"),
            Arc::new(|n| n),
            Arc::new(|_| {}),
            Arc::new(|| panic!("a reader that cannot be set up")),
            &|_| {},
        )
    });
    let outcomes = join_within(reading);
    assert!(outcomes.iter().all(|o| matches!(o, Outcome::Done(_))), "{outcomes:?}");
}

/// `progressed` and the abandon flag belong to the pool's threads: elsewhere they do nothing.
#[test]
fn progress_and_the_abandon_flag_belong_to_the_pool_threads() {
    progressed();
    assert!(abandon_flag().is_none());
    let (stuck, cancel) = (stuck(), AtomicBool::new(false));
    let watch = watch(&stuck);
    let outcomes = run(&watch, &cancel, 1, vec![()], |()| abandon_flag().map(|f| f.load(Relaxed)), |_| {});
    assert!(matches!(outcomes[0], Outcome::Done(Some(false))), "{outcomes:?}");
}

#[test]
fn a_watch_calls_a_read_stalled_after_the_time_it_is_given() {
    let watch = ReadWatch::default();
    assert_eq!(watch.stall_after(), super::STALL_AFTER);
    watch.set_stall_after(Duration::from_secs(3));
    assert_eq!(watch.stall_after(), Duration::from_secs(3));
    assert_eq!((watch.stalled(), watch.skipped(), watch.ended()), (None, 0, None));
}
