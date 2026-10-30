//! Keep running while a long job does (macOS).
//!
//! While an export, import or other job the person started is in flight, macOS must neither put the Mac to idle
//! sleep nor App-Nap the window: a minimized or hidden app is throttled, and a long export of a library on a share
//! stalls. The UI says when that changes (`Services::keep_awake`, from `Activity::wake_reason`); this holds the
//! matching `NSProcessInfo` activity (user initiated, idle system sleep disabled) whose reason names the job, which
//! `pmset -g assertions` then shows.
//!
//! The activity is held with `performActivityWithOptions:reason:usingBlock:`, which is safe to call: a helper thread
//! sits inside the block until its [`Hold`] is dropped, and AppKit ends the activity when the block returns.
//! `beginActivityWithOptions:reason:` would need `endActivity:`, an `unsafe` method, and this crate forbids `unsafe`.
//! Activities belong to the process, not to the thread that began them.

use std::sync::{Arc, Mutex, PoisonError, mpsc};

use block2::RcBlock;
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};

/// A running activity: dropping it ends it.
pub struct Hold {
    /// The helper thread waits for this to disconnect.
    _release: mpsc::Sender<()>,
}

/// Begin an activity named `reason`. `previous` ends only once this one has begun, so the Mac is never left
/// unprotected when the job named in the reason changes. If the helper thread can't start, `previous` is returned
/// still held (the Mac stays protected, under the old reason), or `None` when there was none.
pub fn begin(reason: &str, previous: Option<Hold>) -> Option<Hold> {
    let (release, wait) = mpsc::channel::<()>();
    let reason = reason.to_owned();
    // shared with the thread so that a thread that never starts leaves it here
    let previous = Arc::new(Mutex::new(previous));
    let handed = previous.clone();
    let spawned = std::thread::Builder::new().name("keep-awake".into()).spawn(move || {
        // (an `NSString` is not `Send`: made here)
        let reason = NSString::from_str(&reason);
        let held = RcBlock::new(move || {
            // begun: the one before it can go
            drop(handed.lock().unwrap_or_else(PoisonError::into_inner).take());
            // (Err: the hold was dropped, which is the end)
            let _ = wait.recv();
        });
        NSProcessInfo::processInfo().performActivityWithOptions_reason_usingBlock(
            NSActivityOptions::UserInitiated | NSActivityOptions::IdleSystemSleepDisabled,
            &reason,
            &held,
        );
    });
    match spawned {
        Ok(_) => Some(Hold { _release: release }),
        Err(e) => {
            log::warn!("could not keep the Mac awake for a running job: {e}");
            previous.lock().unwrap_or_else(PoisonError::into_inner).take()
        }
    }
}

/// The host service: holds one activity while the UI says a job needs it.
pub fn service() -> lightcraft_ui_egui::KeepAwakeFn {
    let mut hold: Option<Hold> = None;
    Box::new(move |reason| {
        hold = match reason {
            Some(reason) => begin(reason, hold.take()),
            None => None,
        };
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn assertions() -> String {
        let out = std::process::Command::new("pmset").args(["-g", "assertions"]).output().expect("pmset runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn wait_for(reason: &str, present: bool) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if assertions().contains(reason) == present {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    #[test]
    fn a_hold_shows_in_the_power_assertions_until_it_is_dropped() {
        let reason = format!("LightCraft test hold {}", std::process::id());
        let hold = begin(&reason, None).expect("the helper thread starts");
        assert!(wait_for(&reason, true), "pmset lists the activity while the hold lives");
        drop(hold);
        assert!(wait_for(&reason, false), "pmset stops listing it once the hold is dropped");
    }

    #[test]
    fn a_new_reason_replaces_the_old_one() {
        let first = format!("LightCraft first hold {}", std::process::id());
        let second = format!("LightCraft second hold {}", std::process::id());
        let a = begin(&first, None);
        assert!(wait_for(&first, true));
        let b = begin(&second, a);
        assert!(wait_for(&second, true));
        assert!(wait_for(&first, false), "the earlier one ends once the later one has begun");
        drop(b);
        assert!(wait_for(&second, false));
    }
}
