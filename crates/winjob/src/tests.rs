use super::*;

#[cfg(not(windows))]
#[test]
fn job_objects_are_unsupported_off_windows() {
    assert!(matches!(Job::new(), Err(Error::Unsupported)));
}

#[cfg(windows)]
mod windows {
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// A process that runs for about a minute unless something ends it.
    fn long_running() -> Child {
        Command::new("ping").args(["-n", "60", "127.0.0.1"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()
    }

    /// Whether `child` ends within `limit`.
    fn ends_within(child: &mut Child, limit: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < limit {
            if child.try_wait().unwrap().is_some() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn a_child_in_the_job_ends_when_the_job_is_dropped() {
        let job = Job::new().unwrap();
        let mut child = long_running();
        job.add(&child).unwrap();
        assert!(child.try_wait().unwrap().is_none(), "the child should still run while the job is open");
        drop(job);
        let ended = ends_within(&mut child, Duration::from_secs(10));
        if !ended {
            let _ = child.kill();
        }
        assert!(ended, "closing the job should end the child");
    }

    /// The other test's control: the same child, left out of the job, outlives it.
    #[test]
    fn a_child_left_out_of_the_job_outlives_it() {
        let job = Job::new().unwrap();
        let mut child = long_running();
        drop(job);
        let ended = ends_within(&mut child, Duration::from_secs(2));
        let _ = child.kill();
        let _ = child.wait();
        assert!(!ended, "a child not in the job should go on running");
    }
}
