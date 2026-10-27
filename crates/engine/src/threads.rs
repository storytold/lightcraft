//! Names for the threads of rayon's global pool (#3071). On Linux an unnamed thread takes the
//! name of the thread that started it, so without this a thread dump of a stuck run shows the
//! global pool's threads under the name of whichever render worker first used rayon.

/// The name of the global pool's thread `i` (Linux keeps the first 15 bytes of a name).
pub fn rayon_thread_name(i: usize) -> String {
    format!("lc-rayon-{i}")
}

/// Build rayon's global pool with its threads named [`rayon_thread_name`], at rayon's default
/// size. The app binaries call this once at start-up, before anything uses rayon; test binaries
/// keep rayon's lazy, unnamed pool. Returns whether the pool was built here: when it already
/// exists, or its threads can't start, it is left as it is (logged at debug level), never a
/// panic.
pub fn name_global_pool() -> bool {
    match rayon::ThreadPoolBuilder::new().thread_name(rayon_thread_name).build_global() {
        Ok(()) => true,
        Err(e) => {
            log::debug!("rayon's global pool keeps its thread names: {e}");
            false
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

    /// `name_global_pool` in a process of its own (the global pool is built once per process):
    /// its threads carry the name, and a second call leaves the pool as it is.
    #[test]
    fn the_global_pool_threads_are_named() {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "threads::tests::in_a_fresh_process", "--ignored", "--test-threads=1"])
            .env("LC_NAME_GLOBAL_POOL", "1")
            .output()
            .unwrap();
        let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        assert!(out.status.success() && stdout.contains("1 passed"), "{stdout}\n{stderr}");
    }

    #[test]
    #[ignore = "run in a process of its own by the_global_pool_threads_are_named"]
    fn in_a_fresh_process() {
        if std::env::var_os("LC_NAME_GLOBAL_POOL").is_none() {
            return;
        }
        assert!(name_global_pool(), "the first call builds the pool");
        let names: Vec<Option<String>> = rayon::broadcast(|_| std::thread::current().name().map(String::from));
        assert_eq!(names.len(), rayon::current_num_threads());
        for (i, name) in names.iter().enumerate() {
            assert_eq!(name.as_deref(), Some(rayon_thread_name(i).as_str()));
        }
        assert!(!name_global_pool(), "a second call leaves the pool as it is");
    }
}
