//! One decode of a file at a time, shared by the jobs that ask for the same pixels.
//!
//! A view's jobs are built on the UI thread with the source the session has decoded so far. On a
//! photo just landed on, nothing is decoded yet: its first full render decodes the raw, and a
//! slider draft built a moment later (note 8 of 2026-10-31) would decode it a second time, beside
//! the first (1–2 s of a 45 MP raw each), or after it, before the session has taken the first
//! decode in. Here the second job waits for the decode already running and takes its result; one
//! that comes after it takes the result while any job or the session still holds the pixels (only
//! a weak reference is kept here, so this never holds a decoded raw in memory by itself).

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

use lightcraft_raster::Rgb32f;

use crate::media::{DecodedSource, Twin};

/// How long a job waits for another job's decode of the same pixels before it decodes them itself
/// (a decode that hangs on a slow share must not hold every view of the photo with it).
const WAIT_AT_MOST: Duration = Duration::from_secs(60);

/// Finished decodes kept (weakly) at most; dead ones are dropped first.
const KEPT: usize = 64;

/// A finished decode: everything of a [`DecodedSource`], its pixels held weakly.
struct Kept {
    image: Weak<Rgb32f>,
    denoised: Option<Weak<Twin>>,
    /// The source with its pixels taken out (`image` replaced by an empty picture).
    rest: DecodedSource,
    /// Weak references to the loaders the key names by address: they keep those addresses from
    /// being reused by another loader while this entry is here.
    _anchors: Anchors,
}

/// See [`Kept::_anchors`].
pub(crate) type Anchors = Vec<Box<dyn std::any::Any + Send + Sync>>;

impl Kept {
    fn of(src: &DecodedSource, anchors: Anchors) -> Kept {
        let empty = Arc::new(Rgb32f { width: 0, height: 0, data: Vec::new() });
        Kept {
            image: Arc::downgrade(&src.image),
            denoised: src.denoised.as_ref().map(Arc::downgrade),
            rest: DecodedSource { image: empty, denoised: None, ..src.clone() },
            _anchors: anchors,
        }
    }

    /// The source again, while its pixels are still held somewhere.
    fn upgrade(&self) -> Option<DecodedSource> {
        let image = self.image.upgrade()?;
        let denoised = match &self.denoised {
            Some(w) => Some(w.upgrade()?),
            None => None,
        };
        Some(DecodedSource { image, denoised, ..self.rest.clone() })
    }

    fn alive(&self) -> bool {
        self.image.strong_count() > 0
    }
}

enum State {
    Running,
    Done(Box<Kept>),
}

#[derive(Default)]
struct Share {
    map: Mutex<HashMap<String, State>>,
    cv: Condvar,
}

fn share() -> &'static Share {
    static SHARE: OnceLock<Share> = OnceLock::new();
    SHARE.get_or_init(Share::default)
}

/// Releases a claimed decode: on success the result is kept for the jobs waiting and coming, else
/// the claim is dropped and a waiting job decodes for itself.
struct Claim<'a> {
    key: &'a str,
    result: Option<Kept>,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        let s = share();
        {
            let mut map = s.map.lock().unwrap_or_else(PoisonError::into_inner);
            match self.result.take() {
                Some(kept) => {
                    if map.len() >= KEPT {
                        map.retain(|_, st| match st {
                            State::Running => true,
                            State::Done(k) => k.alive(),
                        });
                    }
                    map.insert(self.key.to_string(), State::Done(Box::new(kept)));
                }
                None => {
                    map.remove(self.key);
                }
            }
        }
        s.cv.notify_all();
    }
}

/// The source `key` names: taken from a decode of it that is running (waited for) or still held,
/// else decoded with `decode`. `key` must name everything the pixels depend on.
pub(crate) fn decode_once(key: &str, anchors: Anchors, decode: impl FnOnce() -> Result<DecodedSource, String>) -> Result<DecodedSource, String> {
    let s = share();
    let deadline = Instant::now().checked_add(WAIT_AT_MOST);
    {
        let mut map = s.map.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            match map.get(key) {
                Some(State::Done(k)) => {
                    if let Some(src) = k.upgrade() {
                        return Ok(src);
                    }
                    break; // let go of: decode again
                }
                Some(State::Running) => {
                    let left = deadline.map_or(Duration::ZERO, |d| d.saturating_duration_since(Instant::now()));
                    if left.is_zero() {
                        // (decoded here as well; the claim stays the other job's)
                        drop(map);
                        return decode();
                    }
                    map = s.cv.wait_timeout(map, left).unwrap_or_else(PoisonError::into_inner).0;
                }
                None => break,
            }
        }
        map.insert(key.to_string(), State::Running);
    }
    let mut claim = Claim { key, result: None };
    let r = decode();
    if let Ok(src) = &r {
        claim.result = Some(Kept::of(src, anchors));
    }
    drop(claim);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn src(n: f32) -> DecodedSource {
        DecodedSource::new(Arc::new(Rgb32f { width: 1, height: 1, data: vec![[n, n, n]] }), None)
    }

    #[test]
    fn a_second_job_waits_for_the_running_decode_and_takes_it() {
        let decodes = Arc::new(AtomicUsize::new(0));
        let (started, has_started) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let d = decodes.clone();
        let first = std::thread::spawn(move || {
            decode_once("test:shared", Vec::new(), || {
                d.fetch_add(1, Ordering::SeqCst);
                let _ = started.send(());
                let _ = released.recv_timeout(Duration::from_secs(30));
                Ok(src(1.0))
            })
        });
        has_started.recv().unwrap();
        let d = decodes.clone();
        let second = std::thread::spawn(move || {
            decode_once("test:shared", Vec::new(), || {
                d.fetch_add(1, Ordering::SeqCst);
                Ok(src(2.0))
            })
        });
        std::thread::sleep(Duration::from_millis(50));
        drop(release);
        let a = first.join().unwrap().unwrap();
        let b = second.join().unwrap().unwrap();
        assert_eq!(decodes.load(Ordering::SeqCst), 1, "the file was decoded once");
        assert!(Arc::ptr_eq(&a.image, &b.image));
        // while the pixels are held, a later job takes them too; once let go, it decodes again
        let c = decode_once("test:shared", Vec::new(), || Ok(src(3.0))).unwrap();
        assert!(Arc::ptr_eq(&a.image, &c.image));
        drop((a, b, c));
        let e = decode_once("test:shared", Vec::new(), || Ok(src(4.0))).unwrap();
        assert_eq!(e.image.data[0][0], 4.0);
    }

    #[test]
    fn a_failed_or_panicked_decode_leaves_the_next_job_to_decode() {
        assert!(decode_once("test:failing", Vec::new(), || Err("unreadable".into())).is_err());
        assert_eq!(decode_once("test:failing", Vec::new(), || Ok(src(5.0))).unwrap().image.data[0][0], 5.0);
        let r = std::panic::catch_unwind(|| decode_once("test:panic", Vec::new(), || panic!("simulated decoder panic")));
        assert!(r.is_err());
        assert_eq!(decode_once("test:panic", Vec::new(), || Ok(src(6.0))).unwrap().image.data[0][0], 6.0);
    }
}
