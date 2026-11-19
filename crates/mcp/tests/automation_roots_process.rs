//! Automation roots across a whole process. This is its own test binary because a process keeps
//! its roots for good.

use lightcraft_engine::Session;
use lightcraft_engine::file_access::{AutomationRoots, check_read};
use lightcraft_mcp::Headless;

/// A worker thread started by anything in the process obeys the backend's roots, and the roots can't be swapped.
#[test]
fn roots_reach_worker_threads_and_stay_for_good() {
    let base = std::env::temp_dir().join(format!("lc-process-roots-{}", std::process::id()));
    let (inside, other, outside) = (base.join("inside"), base.join("other"), base.join("outside"));
    for d in [&inside, &other, &outside] {
        std::fs::create_dir_all(d).unwrap();
    }
    let roots = AutomationRoots::new(Some(&inside), Some(&inside)).unwrap().unwrap();
    let backend = Headless::new(Session::new().with_fs()).with_automation_roots(roots.clone()).unwrap();

    let (out, into) = (outside.join("secret.png"), inside.join("photo.png"));
    let worker = std::thread::spawn(move || (check_read(&out), check_read(&into))).join().unwrap();
    assert!(worker.0.is_err(), "{worker:?}");
    assert!(worker.1.is_ok(), "{worker:?}");

    assert!(backend.with_automation_roots(roots).is_ok(), "the same roots again");
    let different = AutomationRoots::new(Some(&other), Some(&other)).unwrap().unwrap();
    assert!(Headless::new(Session::new().with_fs()).with_automation_roots(different).is_err());
    let _ = std::fs::remove_dir_all(&base);
}
