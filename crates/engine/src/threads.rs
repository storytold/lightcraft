//! Names for the threads of rayon's global pool (#3071). On Linux an unnamed thread takes the
//! name of the thread that started it, so without this a thread dump of a stuck run shows the
//! global pool's threads under the name of whichever render worker first used rayon.

/// The name of the global pool's thread `i` (Linux keeps the first 15 bytes of a name).
pub fn rayon_thread_name(i: usize) -> String {
    format!("lc-rayon-{i}")
}

/// How [`name_global_pool`] left rayon's global pool when it could use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalPool {
    /// Built here, its threads named [`rayon_thread_name`].
    Named,
    /// It already existed (something used rayon first): it keeps its threads and their names.
    AlreadyBuilt,
}

/// Rayon's global pool could not start its threads. rayon-core (1.13) marks the global pool as
/// set up even then, with no pool, so every later use of rayon's global pool panics ("The global
/// thread pool has not been initialized"): the app has to stop before it runs any rayon work.
#[derive(Debug, thiserror::Error)]
#[error("its worker threads could not start: {0}")]
pub struct PoolStartFailed(#[source] rayon::ThreadPoolBuildError);

/// Build rayon's global pool with its threads named [`rayon_thread_name`], at rayon's default
/// size. The app binaries call this once at start-up, before anything uses rayon, and stop on an
/// error (see [`PoolStartFailed`]); test binaries keep rayon's lazy, unnamed pool. A pool that
/// already exists is left as it is ([`GlobalPool::AlreadyBuilt`], logged at debug level).
pub fn name_global_pool() -> Result<GlobalPool, PoolStartFailed> {
    pool_outcome(rayon::ThreadPoolBuilder::new().thread_name(rayon_thread_name).build_global())
}

/// What a `build_global` result means. rayon's error kinds are private: a failed thread start is
/// the one that carries its I/O error as the `source`; the others (the global pool already
/// exists, or the current thread is in a pool, which only `use_current_thread` builds check)
/// carry none. A call after a failed build also reads as "already built", which is why the apps
/// stop on the first error.
fn pool_outcome(built: Result<(), rayon::ThreadPoolBuildError>) -> Result<GlobalPool, PoolStartFailed> {
    use std::error::Error as _;
    match built {
        Ok(()) => Ok(GlobalPool::Named),
        Err(e) if e.source().is_some() => Err(PoolStartFailed(e)),
        Err(e) => {
            log::debug!("rayon's global pool keeps its thread names: {e}");
            Ok(GlobalPool::AlreadyBuilt)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fit_linux_thread_names() {
        assert_eq!(rayon_thread_name(0), "lc-rayon-0");
        assert!(rayon_thread_name(99_999).len() <= 15, "Linux would cut the index off");
    }

    /// A pool whose threads can't start is an error (the apps stop on it), not "already built".
    #[test]
    fn a_pool_whose_threads_cannot_start_is_an_error() {
        let built = rayon::ThreadPoolBuilder::new().num_threads(1).spawn_handler(|_| Err(std::io::Error::other("no threads here"))).build().map(drop);
        let e = pool_outcome(built).unwrap_err();
        assert_eq!(e.to_string(), "its worker threads could not start: no threads here");
        assert_eq!(pool_outcome(Ok(())).unwrap(), GlobalPool::Named);
    }

    /// `name_global_pool` in a process of its own (the global pool is built once per process):
    /// its threads carry the name, and a second call leaves the pool as it is.
    #[test]
    fn the_global_pool_threads_are_named() {
        use std::io::Read;
        use std::process::Stdio;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "threads::tests::in_a_fresh_process", "--ignored", "--test-threads=1"])
            .env("LC_NAME_GLOBAL_POOL", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // Drain both pipes on threads so a chatty child can't block on a full pipe.
        let drain = |mut r: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut s = String::new();
                let _ = r.read_to_string(&mut s);
                s
            })
        };
        let (stdout, stderr) = (drain(Box::new(child.stdout.take().unwrap())), drain(Box::new(child.stderr.take().unwrap())));
        // A stalled child (pool start-up, `broadcast`) fails the test instead of hanging the suite.
        let limit = std::time::Duration::from_secs(60);
        let deadline = std::time::Instant::now() + limit;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if std::time::Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("the child process did not finish within {limit:?}");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let (stdout, stderr) = (stdout.join().unwrap(), stderr.join().unwrap());
        assert!(status.success() && stdout.contains("1 passed"), "{stdout}\n{stderr}");
    }

    #[test]
    #[ignore = "run in a process of its own by the_global_pool_threads_are_named"]
    fn in_a_fresh_process() {
        if std::env::var_os("LC_NAME_GLOBAL_POOL").is_none() {
            return;
        }
        assert_eq!(name_global_pool().unwrap(), GlobalPool::Named, "the first call builds the pool");
        let names: Vec<Option<String>> = rayon::broadcast(|_| std::thread::current().name().map(String::from));
        assert_eq!(names.len(), rayon::current_num_threads());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(name.as_deref(), Some(rayon_thread_name(i).as_str()));
        }
        assert_eq!(name_global_pool().unwrap(), GlobalPool::AlreadyBuilt, "a second call leaves the pool as it is");
    }
}
