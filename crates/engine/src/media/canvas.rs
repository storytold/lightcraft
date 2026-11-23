//! The size of the picture a photo is developed from when "Show Full Image" shows a raw past its default crop.

use super::quick::content_key;
use super::{HashMap, PhotoId, Source};

/// Whole-picture sizes of "Show Full Image" photos, by photo, with the [`content_key`] each was read for.
#[derive(Default)]
pub(crate) struct CanvasSizes {
    known: HashMap<PhotoId, (String, (u32, u32))>,
    /// Background reads in flight ([`crate::Session::canvas_size_soon`]), so a file is read once at a time.
    reading: std::collections::HashSet<(PhotoId, String)>,
    /// Background reads that failed, with when: the same content isn't read again for [`SIZE_RETRY`], so a missing
    /// file doesn't start a read (and a warning) on every frame that asks.
    pub(crate) failed: HashMap<(PhotoId, String), web_time::Instant>,
}

/// How long [`crate::Session::canvas_size_soon`] waits before reading a file whose size read failed again.
pub(crate) const SIZE_RETRY: std::time::Duration = std::time::Duration::from_secs(10);

pub(crate) fn lock_sizes(m: &std::sync::Mutex<CanvasSizes>) -> std::sync::MutexGuard<'_, CanvasSizes> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The whole picture's size of the raw at `path`, read with `read` (the host's reader) or from the file system.
fn read_full_frame_size(read: Option<&crate::merge::ByteReader>, path: &str) -> Result<(u32, u32), String> {
    let bytes = match read {
        Some(read) => read(path),
        None => crate::read_file::read_regular(path, crate::read_file::MAX_MEDIA_BYTES).map_err(|e| format!("{path}: {e}")),
    }?;
    crate::files::full_frame_size(&bytes)
}

/// What [`crate::Session::canvas_size`] needs for a photo.
enum CanvasPlan {
    /// The size is known (the catalog's, or the whole picture's read before).
    Known((u32, u32)),
    /// The whole picture's size is to be read from the file at `path`, whose content is `key`; `listed` is the
    /// catalog's size meanwhile.
    Read { listed: (u32, u32), path: String, key: String },
}

impl crate::Session {
    /// The size (oriented) of the picture photo `id` is developed from, before its crop: the catalog's size, except
    /// with "Show Full Image" on, where it is the raw's whole picture past its default crop (read from the file's
    /// headers once per file content). Export plans its native size from this. A file that can't be read gives the
    /// catalog's size (the render then says what is wrong). `None`: no such photo. A size not known yet is read here,
    /// on the calling thread: the UI uses [`Self::canvas_size_soon`].
    pub fn canvas_size(&mut self, id: PhotoId) -> Option<(u32, u32)> {
        let (listed, path, key) = match self.canvas_plan(id)? {
            CanvasPlan::Known(size) => return Some(size),
            CanvasPlan::Read { listed, path, key } => (listed, path, key),
        };
        match read_full_frame_size(self.media.file_bytes.as_ref(), &path) {
            Ok(size) => {
                lock_sizes(&self.media.canvas_sizes).known.insert(id, (key, size));
                Some(size)
            }
            Err(e) => {
                log::warn!("photo {}: whole picture size not read: {e}", id.0);
                Some(listed)
            }
        }
    }

    /// [`Self::canvas_size`] for the UI thread, which must not wait for a file: a size not known yet is read from the
    /// file's headers in the background (a row in the activity stack while it runs), and until it is known this
    /// gives the catalog's size. A read that fails is tried again on an ask [`SIZE_RETRY`] later (the file may come
    /// back), not on every ask.
    pub fn canvas_size_soon(&mut self, id: PhotoId) -> Option<(u32, u32)> {
        let (listed, path, key) = match self.canvas_plan(id)? {
            CanvasPlan::Known(size) => return Some(size),
            CanvasPlan::Read { listed, path, key } => (listed, path, key),
        };
        // the web build has no threads (and reads its files from memory)
        if cfg!(target_arch = "wasm32") {
            return self.canvas_size(id);
        }
        let sizes = self.media.canvas_sizes.clone();
        {
            let mut guard = lock_sizes(&sizes);
            let probe = (id, key.clone());
            if guard.failed.get(&probe).is_some_and(|at| at.elapsed() < SIZE_RETRY) || !guard.reading.insert(probe.clone()) {
                return Some(listed);
            }
            guard.failed.remove(&probe);
        }
        let read = self.media.file_bytes.clone();
        let task = self.activity.start("probe", "Reading picture size", crate::activity::Cancel::No);
        std::thread::spawn(move || {
            let _task = task;
            let size = crate::guard::catch("reading the picture size", || read_full_frame_size(read.as_ref(), &path)).and_then(|r| r);
            let mut sizes = lock_sizes(&sizes);
            sizes.reading.remove(&(id, key.clone()));
            match size {
                Ok(size) => {
                    sizes.known.insert(id, (key, size));
                }
                Err(e) => {
                    log::warn!("photo {}: whole picture size not read: {e}", id.0);
                    sizes.failed.insert((id, key), web_time::Instant::now());
                }
            }
        });
        Some(listed)
    }

    /// What [`Self::canvas_size`] needs for photo `id`. `None`: no such photo.
    fn canvas_plan(&self, id: PhotoId) -> Option<CanvasPlan> {
        let p = self.catalog.photo(id)?;
        let listed = (p.width, p.height);
        let Source::File { path } = &p.source else { return Some(CanvasPlan::Known(listed)) };
        if !self.media.shows_full_frame(p) {
            return Some(CanvasPlan::Known(listed));
        }
        let key = content_key(p);
        if let Some((k, size)) = lock_sizes(&self.media.canvas_sizes).known.get(&id)
            && *k == key
        {
            return Some(CanvasPlan::Known(*size));
        }
        Some(CanvasPlan::Read { listed, path: path.clone(), key })
    }
}
