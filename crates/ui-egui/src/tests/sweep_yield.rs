//! The availability sweep asks "is file work going on?" from its own worker thread every few hundred milliseconds.
//! That question must be answerable while the UI thread, a repaint request or the control server holds the egui
//! `Context` lock: a worker queued for the lock makes every other caller wait behind it (a frozen window on a large
//! library, seen with the sidebar open while the sweep ran).

use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use lightcraft_engine::activity::{Activity, Cancel};

use crate::clock::sweep_gives_way;

/// With the `Context` lock held by another thread, the yield check still answers at once (it used to ask the context
/// for the listing count on every call), and it follows the listing count and the task guards.
#[test]
fn the_sweeps_yield_check_never_waits_for_the_context_lock() {
    let ctx = egui::Context::default();
    let activity = Activity::default();
    let busy = sweep_gives_way(activity.clone(), &ctx);
    let listings = crate::panels::left::fs_running_counter(&ctx);

    let (asked, answers) = mpsc::channel();
    let (release, hold) = mpsc::channel::<()>();
    let held = std::thread::spawn({
        let ctx = ctx.clone();
        move || {
            // the lock is held until the check has answered (or the test gives up on it)
            ctx.data_mut(|_| {
                let _ = asked.send(());
                let _ = hold.recv_timeout(Duration::from_secs(10));
            });
        }
    });
    answers.recv_timeout(Duration::from_secs(10)).expect("the lock holder started");

    let (done, result) = mpsc::channel();
    let worker = std::thread::spawn({
        let busy = busy.clone();
        let (listings, activity) = (listings.clone(), activity.clone());
        move || {
            let idle = busy();
            listings.fetch_add(1, Ordering::AcqRel);
            let listing = busy();
            listings.fetch_sub(1, Ordering::AcqRel);
            let _guard = activity.start("scan", "Reading photos", Cancel::No);
            let scanning = busy();
            let _ = done.send((idle, listing, scanning));
        }
    });
    let outcome = result.recv_timeout(Duration::from_secs(3));
    let _ = release.send(());
    held.join().unwrap();
    worker.join().unwrap();
    let (idle, listing, scanning) = outcome.expect("the yield check waited for the egui Context lock");
    assert!(!idle, "nothing running: the sweep goes on");
    assert!(listing, "a Local listing running: the sweep gives way");
    assert!(scanning, "a scan running: the sweep gives way");
}
