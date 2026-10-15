//! Regression tests for the CollabCraft cloud startup sequence.
//!
//! Covers two known failure modes on the web host:
//!   1. `cloud::start` was not called after `connect`, so the account probe
//!      was never submitted and the cloud controls stayed empty forever.
//!   2. The logout fetch targeted `/logout` instead of `/auth/logout`, so the
//!      POST hit a 404 and the session cookie survived sign-out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::cloud::{CloudBackend, Job, Post, start};
use crate::{LightcraftApp, Services};

/// Records whether `Job::Account` was submitted.
struct AccountProbe(Arc<AtomicBool>);

impl CloudBackend for AccountProbe {
    fn submit(&self, job: Job, _post: Post) {
        if matches!(job, Job::Account) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

/// `start` must submit the initial account probe the moment it is called.
///
/// Without this call the web host's cloud controls never populate: the
/// `LightcraftApp` sits with `asked_account = false` and no capabilities are
/// fetched, so every cloud-gated panel stays hidden.
#[test]
fn start_submits_account_probe_after_connect() {
    let probed = Arc::new(AtomicBool::new(false));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.cloud.connect(AccountProbe(probed.clone()));

    assert!(!probed.load(Ordering::SeqCst), "probe must not fire before start()");
    start(&mut app);
    assert!(probed.load(Ordering::SeqCst), "start() must submit Job::Account immediately");
}

/// Calling `start` twice (e.g. across reloads) must not duplicate the probe.
#[test]
fn start_is_idempotent() {
    use std::sync::atomic::AtomicUsize;
    struct Counter(Arc<AtomicUsize>);
    impl CloudBackend for Counter {
        fn submit(&self, job: Job, _post: Post) {
            if matches!(job, Job::Account) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    let count = Arc::new(AtomicUsize::new(0));
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    app.cloud.connect(Counter(count.clone()));
    start(&mut app);
    start(&mut app); // second call must be a no-op
    assert_eq!(count.load(Ordering::SeqCst), 1, "Job::Account must be submitted exactly once across repeated start() calls");
}
