//! Cached, worker-checked proxy availability for UI status. Existence alone cannot establish
//! that an older ORF proxy contains sensor pixels, or that a JPEG can actually be decoded.

use std::path::Path;
use std::sync::Arc;

use lightcraft_catalog::Photo;

use crate::availability::{Availability as FileAvailability, Waker};

/// Cached validation is separate for ordinary, preview-derived and sensor-derived proxies.
#[derive(Clone, Debug)]
pub struct Availability {
    any: FileAvailability,
    preview: FileAvailability,
    sensor: FileAvailability,
}

impl Default for Availability {
    fn default() -> Self {
        let cache = |expected| {
            let mut cache = FileAvailability::default();
            cache.set_probe(Arc::new(move |path| super::load_compatible(Path::new(path), expected).is_ok()));
            cache
        };
        Self { any: cache(None), preview: cache(Some(false)), sensor: cache(Some(true)) }
    }
}

impl Availability {
    /// UI callers only queue requests and read cached answers; workers perform validation.
    pub fn run_in_background(&mut self, wake: Waker) {
        for cache in [&mut self.any, &mut self.preview, &mut self.sensor] {
            cache.run_in_background(wake.clone());
        }
    }

    pub fn is_background(&self) -> bool {
        self.any.is_background() && self.preview.is_background() && self.sensor.is_background()
    }

    /// Writes and explicit discards invalidate every interpretation, including in-flight probes.
    pub(crate) fn invalidate(&self, paths: &[std::path::PathBuf]) {
        for path in paths {
            let path = path.to_string_lossy();
            for cache in [&self.any, &self.preview, &self.sensor] {
                cache.invalidate(&path);
            }
        }
    }

    pub fn exists(&self, dir: &Path, photo: &Photo) -> bool {
        let preview = super::PreviewPath::new(dir, photo);
        let cache = match preview.expected_raw() {
            None => &self.any,
            Some(false) => &self.preview,
            Some(true) => &self.sensor,
        };
        preview.candidates().iter().any(|path| cache.exists(&path.to_string_lossy()) == Some(true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    /// Every interpretation is validated off the caller's thread.
    #[test]
    fn every_proxy_interpretation_is_checked_on_a_worker() {
        let mut availability = Availability::default();
        let caller = std::thread::current().id();
        let calls = Arc::new(Mutex::new(Vec::new()));
        for (cache, expected) in [(&mut availability.any, None), (&mut availability.preview, Some(false)), (&mut availability.sensor, Some(true))] {
            let calls = calls.clone();
            cache.set_probe(Arc::new(move |_| {
                calls.lock().unwrap().push((expected, std::thread::current().id()));
                true
            }));
        }
        availability.run_in_background(Arc::new(|| {}));
        let mut photos = Vec::new();
        for (format, preview_only) in [("JPEG", false), ("DNG", true), ("DNG", false)] {
            let mut p =
                Photo::new(lightcraft_catalog::PhotoId(1), lightcraft_catalog::Source::File { path: "offline".into() }, "offline", format, 8, 8, "");
            p.kind = if format == "JPEG" { lightcraft_catalog::MediaKind::Image } else { lightcraft_catalog::MediaKind::Raw };
            p.preview_only = preview_only.then(|| "old decoder".into());
            photos.push(p);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !photos.iter().all(|p| availability.exists(Path::new("previews"), p)) {
            assert!(Instant::now() < deadline, "the background checks answer");
            std::thread::sleep(Duration::from_millis(1));
        }
        let calls = calls.lock().unwrap();
        for expected in [None, Some(false), Some(true)] {
            assert!(calls.iter().any(|(kind, _)| *kind == expected));
        }
        assert!(calls.iter().all(|(_, thread)| *thread != caller));
    }
}
