//! The processor threads of the renders the user looks at (the loupe, the zoom window, Before,
//! compare panes): a rayon pool of their own, so the photos of an export rendering side by side
//! on the shared pool never hold them back.
//!
//! A rayon worker takes work from its own queue and from the other workers' before it takes a job
//! sent in from outside the pool. A view render runs its stages from a job-pool thread, outside
//! the pool, so each stage was such a job. While an export's lanes kept the shared pool's queues
//! full, every stage waited behind the export's work: opening a 61 MP photo during a 61 MP export
//! took 0.8–11.4 s with twelve lanes (insideTheFlux on PR #3530; 2.3–4.1 s on main, four lanes).
//! In a pool of their own a view's stages start at once, and the system shares the cores between
//! the two pools while both are busy. Idle, the pool's threads sleep.

/// Run `f` (a view render) on the view pool: its parallel work stays there. On the pool's own
/// threads, and where there are no threads (wasm), `f` runs where it is.
pub fn run<R: Send>(f: impl FnOnce() -> R + Send) -> R {
    match pool() {
        Some(p) => p.install(f),
        None => f(),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn pool() -> Option<&'static rayon::ThreadPool> {
    static POOL: std::sync::OnceLock<Option<rayon::ThreadPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| rayon::ThreadPoolBuilder::new().thread_name(|i| format!("lc-view-{i}")).build().ok()).as_ref()
}

#[cfg(target_arch = "wasm32")]
fn pool() -> Option<&'static rayon::ThreadPool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rayon::prelude::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn spin(d: Duration) {
        let t = Instant::now();
        while t.elapsed() < d {
            std::hint::spin_loop();
        }
    }

    /// A view render's stages, one after another from a thread outside the pools.
    fn stages() -> Duration {
        let t = Instant::now();
        for _ in 0..8 {
            (0..8).into_par_iter().for_each(|_| spin(Duration::from_micros(200)));
        }
        t.elapsed()
    }

    #[test]
    fn a_view_render_is_not_held_back_by_an_export_filling_the_shared_pool() {
        // export lanes: threads outside the pool running long stages on the shared pool, whose
        // queues then always hold work to steal (each piece checks `stop`, so the lanes end at once)
        let stop = Arc::new(AtomicBool::new(false));
        let lanes: Vec<_> = (0..rayon::current_num_threads().max(2) * 2)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        (0..2048).into_par_iter().for_each(|_| {
                            if !stop.load(Ordering::Relaxed) {
                                spin(Duration::from_millis(2));
                            }
                        });
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(200));
        let own = std::thread::spawn(|| run(stages)).join().unwrap();
        // the same stages on the shared pool: still running three times as long later (then the
        // lanes stop, and the stages finish)
        let limit = (own * 3).max(Duration::from_millis(300));
        let start = Instant::now();
        let shared = std::thread::spawn(stages);
        while start.elapsed() < limit && !shared.is_finished() {
            std::thread::sleep(Duration::from_millis(5));
        }
        let held_back = !shared.is_finished();
        stop.store(true, Ordering::Relaxed);
        let shared = shared.join().unwrap();
        for l in lanes {
            l.join().unwrap();
        }
        assert!(held_back, "the shared pool held the stages back too little to tell: own pool {own:?}, shared {shared:?}");
    }
}
