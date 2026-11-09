//! Cached, worker-checked proxy availability for UI status. Existence alone cannot establish
//! that an older ORF proxy contains sensor pixels, or that a JPEG can actually be decoded.

use std::path::Path;
use std::sync::Arc;

use lightcraft_catalog::Photo;

use crate::availability::{Availability as FileAvailability, Waker};

/// Separate cached answers for ordinary proxies and legacy proxies that must prove sensor facts.
/// Both reuse the file-availability worker, expiration and repaint behavior.
#[derive(Clone, Debug)]
pub struct Availability {
    current: FileAvailability,
    sensor: FileAvailability,
}

impl Default for Availability {
    /// Create separate validation caches for ordinary proxies and sensor-only legacy candidates.
    fn default() -> Self {
        let cache = |require_sensor| {
            let mut cache = FileAvailability::default();
            cache.set_probe(Arc::new(move |path| super::load_compatible(Path::new(path), require_sensor).is_ok()));
            cache
        };
        Self { current: cache(false), sensor: cache(true) }
    }
}

impl Availability {
    /// Use background validation from now on; the UI reads cached answers and queues unknowns.
    pub fn run_in_background(&mut self, wake: Waker) {
        self.current.run_in_background(wake.clone());
        self.sensor.run_in_background(wake);
    }

    /// Whether both classes of proxy are checked on worker threads.
    pub fn is_background(&self) -> bool {
        self.current.is_background() && self.sensor.is_background()
    }

    /// Whether a compatible, decodable proxy is known to be available in either folder layout.
    /// Unknown candidates count as absent until their worker answers.
    pub fn exists(&self, dir: &Path, photo: &Photo) -> bool {
        let preview = super::PreviewPath::new(dir, photo);
        preview.candidates(true).iter().any(|path| {
            let cache = if preview.is_previous(path) { &self.sensor } else { &self.current };
            cache.exists(&path.to_string_lossy()) == Some(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Current and legacy validation probes both execute on availability workers.
    #[test]
    fn both_proxy_checks_run_off_the_callers_thread() {
        let mut availability = Availability::default();
        let caller = std::thread::current().id();
        let calls = Arc::new(Mutex::new(Vec::new()));
        for (cache, sensor) in [(&mut availability.current, false), (&mut availability.sensor, true)] {
            let calls = calls.clone();
            cache.set_probe(Arc::new(move |_| {
                calls.lock().unwrap().push((sensor, std::thread::current().id()));
                sensor
            }));
        }
        availability.run_in_background(Arc::new(|| {}));
        let photo = Photo::new(
            lightcraft_catalog::PhotoId(1),
            lightcraft_catalog::Source::File { path: "offline.orf".into() },
            "offline.orf",
            "ORF",
            8,
            8,
            "2026-01-01T00:00:00",
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !availability.exists(Path::new("previews"), &photo) || !calls.lock().unwrap().iter().any(|(sensor, _)| !sensor) {
            assert!(Instant::now() < deadline, "the background sensor check answers");
            std::thread::sleep(Duration::from_millis(1));
        }
        let calls = calls.lock().unwrap();
        assert!(calls.iter().any(|(sensor, _)| *sensor), "a sensor candidate was checked");
        assert!(calls.iter().all(|(_, thread)| *thread != caller));
    }
}
