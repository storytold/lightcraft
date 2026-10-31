//! A once-per-session alert when the drive holding the library's preview cache runs low.
//!
//! The automatic preview cache budget grows with the library and has no ceiling
//! ([`crate::media::default_disk_cache_bytes`]): a large library can fill its drive. Rather than cap
//! it, the session looks at the drive's free space when the library opens and, at most every
//! [`CHECK_EVERY`] while the app runs (thumbnails and previews fill the cache); when it is under [`LOW_FREE_PERCENT`] of the drive or
//! under [`LOW_FREE_BYTES`], it hands out one [`LowDiskAlert`] ([`crate::Session::take_disk_space_alert`])
//! that the UI shows with a way to Settings ▸ Performance, where the cache can be given a limit. It is
//! not repeated until the next library open or app run. Reading the free space can fail (a network
//! share that doesn't say, the web build): then there is no alert, only a log line.
//! Natively the read runs on a worker thread and its answer is taken on a later call, so a slow
//! network share never holds up the UI thread that asks.

use std::path::Path;

/// The drive is low when less than this share of it (percent) is free…
pub const LOW_FREE_PERCENT: u64 = 10;
/// …or when less than this is free (20 GB), whichever comes first.
pub const LOW_FREE_BYTES: u64 = 20 << 30;
/// While previews are built, the free space is read at most this often (statvfs on the cache's
/// drive: cheap, but not something for every frame or every write).
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Space on one drive, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpace {
    /// Free for this user (what can still be written).
    pub available: u64,
    /// The drive's size.
    pub total: u64,
}

impl DiskSpace {
    /// Under [`LOW_FREE_PERCENT`] of the drive free, or under [`LOW_FREE_BYTES`]. A drive that
    /// reports no size isn't judged by its share.
    pub fn is_low(&self) -> bool {
        let under_share = self.total > 0 && u128::from(self.available) * 100 < u128::from(self.total) * u128::from(LOW_FREE_PERCENT);
        under_share || self.available < LOW_FREE_BYTES
    }
}

/// What the UI tells the user once: the drive's free space and size, and how big the preview
/// cache is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowDiskAlert {
    pub space: DiskSpace,
    /// Bytes the preview cache takes on that drive.
    pub cache_bytes: u64,
}

/// The free space of the drive holding `dir` (or its parent when `dir` isn't there yet). `None`
/// (logged) when it can't be read.
pub fn read(dir: &Path) -> Option<DiskSpace> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let probe = if dir.exists() { dir } else { dir.parent().unwrap_or(dir) };
        match fs4::statvfs(probe) {
            Ok(s) => Some(DiskSpace { available: s.available_space(), total: s.total_space() }),
            Err(e) => {
                log::info!("disk space: can't read the free space of {}: {e}", probe.display());
                None
            }
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = dir;
        None
    }
}

/// Reads the free space of a drive ([`read`]; a test can put a slow one in its place).
pub type Reader = std::sync::Arc<dyn Fn(&Path) -> Option<DiskSpace> + Send + Sync>;

/// The session's state of the alert (reset when a library opens).
#[derive(Default)]
pub(crate) struct DiskAlertState {
    /// The alert was handed out (or is waiting to be) this session: never again until reset.
    pub(crate) raised: bool,
    /// The alert waiting for the UI.
    pub(crate) pending: Option<LowDiskAlert>,
    /// When the free space was last read.
    pub(crate) checked_at: Option<web_time::Instant>,
    /// The read running on a worker (natively): a network share can take its time to answer, and
    /// the check runs on the UI thread. It answers with the drive's space and, when that is low,
    /// the preview cache's size (counting the cache can mean reading every file's size: that too
    /// stays off the UI thread).
    probe: Option<std::sync::mpsc::Receiver<Option<(DiskSpace, u64)>>>,
    /// The running read was started for the library open before this one: its answer is ignored,
    /// and the drive is read again for the current library once it has come. Until then no second
    /// read starts, so switching libraries while a drive hangs never piles up workers.
    stale: bool,
    /// In place of [`read`] (tests).
    pub(crate) reader: Option<Reader>,
}

impl DiskAlertState {
    /// A fresh state for a newly opened library (the reader stays; a read still running is kept,
    /// marked stale).
    pub(crate) fn reset(&mut self) {
        let probe = self.probe.take();
        let stale = probe.is_some();
        *self = DiskAlertState { reader: self.reader.take(), probe, stale, ..Default::default() };
    }
}

impl crate::Session {
    /// Read the free space of the drive holding the preview cache, at most every [`CHECK_EVERY`]
    /// (`now`: whatever the last read, as when the library opens), and raise the alert if it is low
    /// ([`Session::note_disk_space`](crate::Session::note_disk_space)). Nothing once the alert was
    /// raised this session, without an on-disk library, or when the space can't be read.
    /// The read itself runs on a worker (natively) and its answer is taken by a later call or
    /// [`Session::poll_disk_space`]: a slow drive never holds up the caller.
    pub fn check_disk_space(&mut self, now: bool) {
        self.poll_disk_space();
        // (a read still running for an earlier library open starts this library's read when it answers)
        if self.disk_alert.raised || self.disk_alert.probe.is_some() {
            return;
        }
        let Some(dir) = self.library.as_ref().filter(|l| l.on_disk).map(crate::library::Library::thumbs_dir) else { return };
        let at = web_time::Instant::now();
        if !now && self.disk_alert.checked_at.is_some_and(|t| at.saturating_duration_since(t) < CHECK_EVERY) {
            return;
        }
        self.disk_alert.checked_at = Some(at);
        let reader = self.disk_alert.reader.clone();
        let rendered = self.media.rendered.clone();
        let read_now = move || {
            let space = match reader {
                Some(r) => r(&dir),
                None => read(&dir),
            }?;
            // the cache is counted only when the alert will tell its size
            let cache_bytes = if space.is_low() { rendered.disk().map_or(0, |d| d.size()) } else { 0 };
            Some((space, cache_bytes))
        };
        #[cfg(not(target_arch = "wasm32"))]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new().name("disk-space".into()).spawn(move || {
                let _ = tx.send(read_now());
            });
            match started {
                Ok(_) => self.disk_alert.probe = Some(rx),
                Err(e) => log::info!("disk space: can't start the free-space read: {e}"),
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some((space, cache_bytes)) = read_now() {
            self.raise_disk_alert(space, cache_bytes);
        }
    }

    /// Take the answer of the free-space read running on a worker, if it has come, and raise the
    /// alert when the drive is low. An answer for an earlier library open is dropped and the drive
    /// of the current one is read instead. Called by [`Session::check_disk_space`],
    /// [`Session::take_disk_space_alert`] and `library.info`.
    pub fn poll_disk_space(&mut self) {
        let Some(rx) = self.disk_alert.probe.as_ref() else { return };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        };
        self.disk_alert.probe = None;
        if std::mem::take(&mut self.disk_alert.stale) {
            self.check_disk_space(true);
        } else if let Some((space, cache_bytes)) = answer {
            self.raise_disk_alert(space, cache_bytes);
        }
    }

    /// Raise the low-disk alert for `space` when it is low and the alert wasn't raised this session
    /// yet (the numbers come from [`read`], or from a test). Returns whether it was raised now.
    /// Counts the preview cache here, on the caller's thread: the session's own reads count it on
    /// their worker.
    pub fn note_disk_space(&mut self, space: DiskSpace) -> bool {
        if self.disk_alert.raised || !space.is_low() {
            return false;
        }
        let cache_bytes = self.media.rendered.disk().map_or(0, |d| d.size());
        self.raise_disk_alert(space, cache_bytes)
    }

    /// [`Session::note_disk_space`] with the cache's size already counted.
    fn raise_disk_alert(&mut self, space: DiskSpace, cache_bytes: u64) -> bool {
        if self.disk_alert.raised || !space.is_low() {
            return false;
        }
        log::warn!("disk space: {} of {} bytes free; the preview cache takes {cache_bytes} bytes", space.available, space.total);
        self.disk_alert.raised = true;
        self.disk_alert.pending = Some(LowDiskAlert { space, cache_bytes });
        true
    }

    /// The low-disk alert not shown yet, if any (handed out once per session).
    pub fn take_disk_space_alert(&mut self) -> Option<LowDiskAlert> {
        self.poll_disk_space();
        self.disk_alert.pending.take()
    }

    /// Read the drive's space with `reader` instead of asking the system (tests of the app).
    #[doc(hidden)]
    pub fn set_disk_space_reader(&mut self, reader: Option<Reader>) {
        self.disk_alert.reader = reader;
    }

    /// Forget when the space was last read, as if [`CHECK_EVERY`] had passed (tests of the app).
    #[doc(hidden)]
    pub fn forget_disk_space_check(&mut self) {
        self.disk_alert.checked_at = None;
    }

    /// Whether the low-disk alert was raised since the library opened (`library.info` →
    /// `lowDiskSpace`).
    pub fn disk_space_alert_raised(&self) -> bool {
        self.disk_alert.raised
    }
}

#[cfg(test)]
mod tests {
    use super::{DiskSpace, LOW_FREE_BYTES};
    use crate::Session;

    const GB: u64 = 1 << 30;

    #[test]
    fn plenty_of_room_on_both_counts_raises_no_alert() {
        let mut s = Session::new();
        assert!(!s.note_disk_space(DiskSpace { available: 200 * GB, total: 1000 * GB }), "20 % and 200 GB free");
        assert!(!s.note_disk_space(DiskSpace { available: 25 * GB, total: 100 * GB }), "25 % and 25 GB free");
        assert!(s.take_disk_space_alert().is_none());
        assert!(!s.disk_space_alert_raised());
    }

    #[test]
    fn under_ten_percent_free_raises_the_alert() {
        let mut s = Session::new();
        // 300 GB is far above 20 GB, but only 7.5 % of a 4 TB drive
        let space = DiskSpace { available: 300 * GB, total: 4000 * GB };
        assert!(s.note_disk_space(space));
        let alert = s.take_disk_space_alert().expect("the alert is handed out");
        assert_eq!(alert.space, space);
        assert!(s.disk_space_alert_raised());
    }

    #[test]
    fn under_twenty_gb_free_raises_the_alert() {
        let mut s = Session::new();
        // 15 % of a 100 GB drive is free, but that is under 20 GB
        let space = DiskSpace { available: 15 * GB, total: 100 * GB };
        assert!(space.available < LOW_FREE_BYTES);
        assert!(s.note_disk_space(space));
        assert_eq!(s.take_disk_space_alert().map(|a| a.space), Some(space));
    }

    #[test]
    fn the_alert_is_raised_once_per_session() {
        let mut s = Session::new();
        let low = DiskSpace { available: GB, total: 500 * GB };
        assert!(s.note_disk_space(low));
        assert!(s.take_disk_space_alert().is_some());
        assert!(s.take_disk_space_alert().is_none(), "handed out once");
        assert!(!s.note_disk_space(DiskSpace { available: GB / 2, ..low }), "lower still: not again this session");
        assert!(s.take_disk_space_alert().is_none());
        s.check_disk_space(true);
        assert!(s.take_disk_space_alert().is_none(), "a check after the alert reads nothing");
    }

    #[test]
    fn opening_a_library_starts_a_new_session_for_the_alert() {
        let dir = std::env::temp_dir().join(format!("lc-disk-alert-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Session::new().with_fs();
        s.open_library(&dir, false).unwrap();
        // whatever this machine's drive holds, raise it by hand
        let _ = s.take_disk_space_alert();
        let low = DiskSpace { available: GB, total: 500 * GB };
        if !s.disk_space_alert_raised() {
            assert!(s.note_disk_space(low));
        }
        assert!(!s.note_disk_space(low), "once per session");
        s.close_library().unwrap();
        s.open_library(&dir, false).unwrap();
        let _ = s.take_disk_space_alert();
        let raised_by_open = s.disk_space_alert_raised();
        assert!(raised_by_open || s.note_disk_space(low), "a new library open may warn again");
        s.close_library().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A drive that takes its time to answer (a network share) doesn't hold up the library open
    /// or the checks on the UI thread: the read runs on a worker and its answer is taken later.
    #[test]
    fn a_slow_drive_does_not_hold_up_the_check() {
        use std::sync::{Arc, Mutex, mpsc};
        let dir = std::env::temp_dir().join(format!("lc-disk-alert-slow-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let mut s = Session::new().with_fs();
        s.disk_alert.reader = Some(Arc::new(move |_: &std::path::Path| {
            // answers once the test lets it (or after 10 s, so a broken test can't hang)
            let _ = wait.lock().unwrap().recv_timeout(std::time::Duration::from_secs(10));
            Some(DiskSpace { available: GB, total: 500 * GB })
        }));
        s.open_library(&dir, false).unwrap();
        assert!(s.take_disk_space_alert().is_none(), "the open didn't wait for the drive's answer");
        s.check_disk_space(true);
        assert!(s.take_disk_space_alert().is_none(), "nor does a check while the read runs");
        release.send(()).unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let alert = loop {
            if let Some(a) = s.take_disk_space_alert() {
                break a;
            }
            assert!(std::time::Instant::now() < until, "the drive's answer was never taken");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert_eq!(alert.space.available, GB);
        assert!(s.disk_space_alert_raised());
        s.close_library().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A drive that hangs while the user switches libraries: the read still running is kept, no
    /// second worker starts beside it, its answer (for the old library) is dropped, and the current
    /// library's drive is read once it has come.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn switching_libraries_while_a_drive_hangs_starts_no_second_read() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        let root = std::env::temp_dir().join(format!("lc-disk-alert-switch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (release, wait) = mpsc::channel::<()>();
        let (started_tx, started) = mpsc::channel::<std::path::PathBuf>();
        let wait = Mutex::new(wait);
        let started_tx = Mutex::new(started_tx);
        let mut s = Session::new().with_fs();
        s.disk_alert.reader = Some(Arc::new(move |dir: &std::path::Path| {
            let _ = started_tx.lock().unwrap().send(dir.to_path_buf());
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(10));
            Some(DiskSpace { available: GB, total: 500 * GB })
        }));
        s.open_library(&a, false).unwrap();
        let first = started.recv_timeout(Duration::from_secs(10)).expect("the open reads the drive");
        assert!(first.starts_with(&a), "{first:?}");
        s.close_library().unwrap();
        s.open_library(&b, false).unwrap();
        s.check_disk_space(true);
        assert!(started.recv_timeout(Duration::from_millis(300)).is_err(), "no second read beside the hanging one");
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        let second = loop {
            assert!(s.take_disk_space_alert().is_none(), "the old library's answer is dropped");
            if let Ok(dir) = started.try_recv() {
                break dir;
            }
            assert!(Instant::now() < until, "the current library's drive was never read");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(second.starts_with(&b), "{second:?}");
        assert!(!s.disk_space_alert_raised());
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(10);
        while s.take_disk_space_alert().is_none() {
            assert!(Instant::now() < until, "the current library's answer was never taken");
            std::thread::sleep(Duration::from_millis(5));
        }
        s.close_library().unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// When the drive is low, the preview cache is counted on the read's worker too: counting it can
    /// mean reading the size of every file (a big cache on a slow share), never on the UI thread
    /// that takes the answer.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_preview_cache_is_counted_off_the_ui_thread() {
        use std::sync::{Arc, Mutex, mpsc};
        use std::time::{Duration, Instant};
        let dir = std::env::temp_dir().join(format!("lc-disk-alert-count-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (release, wait) = mpsc::channel::<()>();
        let wait = Mutex::new(wait);
        let mut s = Session::new().with_fs();
        s.disk_alert.reader = Some(Arc::new(move |_: &std::path::Path| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(10));
            Some(DiskSpace { available: GB, total: 500 * GB })
        }));
        s.open_library(&dir, false).unwrap();
        let disk = || s.media.rendered.disk().map(lightcraft_preview::DiskCache::known_size);
        assert_eq!(disk(), Some(None), "an on-disk library has a preview cache, not counted yet");
        release.send(()).unwrap();
        // no poll here: only the worker can count the cache
        let until = Instant::now() + Duration::from_secs(10);
        let counted = loop {
            if let Some(Some(bytes)) = disk() {
                break bytes;
            }
            assert!(Instant::now() < until, "the worker never counted the cache");
            std::thread::sleep(Duration::from_millis(5));
        };
        let until = Instant::now() + Duration::from_secs(10);
        let alert = loop {
            if let Some(a) = s.take_disk_space_alert() {
                break a;
            }
            assert!(Instant::now() < until, "the drive's answer was never taken");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(alert.cache_bytes, counted);
        s.close_library().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn the_free_space_of_a_real_drive_is_read() {
        let space = super::read(&std::env::temp_dir().join("lc-disk-alert-not-there")).expect("the temp folder's drive says its size");
        assert!(space.total > 0 && space.available <= space.total, "{space:?}");
    }

    #[test]
    fn a_drive_that_reports_no_size_is_judged_by_its_free_bytes() {
        assert!(!DiskSpace { available: 50 * GB, total: 0 }.is_low());
        assert!(DiskSpace { available: 5 * GB, total: 0 }.is_low());
    }
}
