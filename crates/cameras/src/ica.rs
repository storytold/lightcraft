//! macOS: ImageCaptureCore, the framework Image Capture and Photos read cameras and phones through.
//! The system's camera daemon (`ptpcamerad`) owns the USB connection and serves it to every app, so
//! LightCraft never claims the device itself. Every `unsafe` block is a call into that framework;
//! the module's functions are safe.
//!
//! Threading: ImageCaptureCore calls its delegates on the main queue only, whichever thread asked
//! (completion blocks come on a queue of its own). So every call into it runs on the main thread ([`on_main`]): the
//! app's event loop serves the main queue, and a caller that *is* the main thread (the CLI, the
//! probe) runs the work inline and spins the run loop while it waits ([`recv`]). The framework's
//! objects live only on the main thread ([`MAIN`]); other threads see plain data ([`SHARED`]) and
//! wait on channels, with time limits, so a main thread that never answers is an error and not a
//! hang.
//!
//! Read-only: sessions are opened without options, files are read with `requestReadDataAtOffset`;
//! nothing is deleted, uploaded or changed on the camera.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_foundation::{NSArray, NSData, NSDate, NSError, NSRunLoop, NSString, NSThread, NSTimeZone};
use objc2_image_capture_core::{
    ICCameraDevice, ICCameraDeviceDelegate, ICCameraFile, ICCameraItem, ICDevice, ICDeviceBrowser, ICDeviceBrowserDelegate, ICDeviceDelegate,
    ICDeviceLocationTypeMask, ICDeviceTypeMask, ICMediaPresentation,
};

use crate::layout::pick;
use crate::{Camera, CameraFile, Error, MAX_DEPTH};

/// How long the first [`cameras`] call waits for the browser's first look at the USB bus, and how
/// long at least: the browser says it has enumerated the local devices *before* it reports them
/// (an M11-P arrived 13 ms after that).
const FIRST_LOOK: Duration = Duration::from_millis(1500);
const FIRST_LOOK_MIN: Duration = Duration::from_millis(300);
/// How long a call waits for the main thread to run work handed to it.
const MAIN_WAIT: Duration = Duration::from_secs(5);
/// How long opening a session may take (a phone asks its user to trust the computer first).
const OPEN_WAIT: Duration = Duration::from_secs(60);
/// How long an iPhone or iPad that refused the session may take to lift its access restriction
/// (it does a moment later when unlocked, or once its user unlocks it and trusts this Mac).
const UNLOCK_WAIT: Duration = Duration::from_secs(30);
/// The catalog of a full card can take a while; it fails only when it stops growing this long.
const CATALOG_STALL: Duration = Duration::from_secs(60);
/// Bytes asked for per read, and how many reads are queued at once (the next is on its way while
/// the last is written).
const CHUNK: i64 = 4 * 1024 * 1024;
const IN_FLIGHT: usize = 2;
/// How long one read may take before the camera counts as gone.
const READ_WAIT: Duration = Duration::from_secs(60);
/// What ImageCaptureCore calls a memory card in a card reader: listed by the volume scan already.
const MASS_STORAGE: &str = "ICTransportTypeMassStorage";
/// At most this many files are listed per camera (a card holds tens of thousands at most).
const MAX_ITEMS: usize = 1_000_000;

/// Where a read's bytes (or the framework's error) arrive.
type Chunk = Receiver<Result<Vec<u8>, String>>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ---------------------------------------------------------------------------------------------
// Plain data any thread may read

#[derive(Default)]
struct Shared {
    /// The browser was asked to start.
    started: bool,
    /// It has reported what was connected when it started.
    enumerated: bool,
    cameras: Vec<Camera>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { started: false, enumerated: false, cameras: Vec::new() });

/// What a camera's delegate has seen, for the thread waiting on it.
#[derive(Default)]
struct CatalogState {
    /// Items reported so far (the running count for `found`).
    items: usize,
    ready: bool,
    removed: bool,
    /// An iPhone or iPad restricting access (`Some(true)`), or no longer (`Some(false)`); `None`
    /// until it says either.
    restricted: Option<bool>,
}

// ---------------------------------------------------------------------------------------------
// The main thread's objects

struct Open {
    camera: Retained<ICCameraDevice>,
    /// Kept alive here: a device holds its delegate weakly.
    delegate: Retained<CameraDelegate>,
    state: Arc<Mutex<CatalogState>>,
    /// Sessions using it; the camera's session is closed when the last one ends.
    users: usize,
}

#[derive(Default)]
struct Main {
    browser: Option<Retained<ICDeviceBrowser>>,
    browser_delegate: Option<Retained<BrowserDelegate>>,
    /// Connected cameras by id.
    devices: Vec<(String, Retained<ICDevice>)>,
    /// Cameras with a session, by id.
    open: HashMap<String, Open>,
    /// Each session's files, in the order [`Session::files`] listed them (a file's `object` is its
    /// index here).
    files: HashMap<u64, (String, Vec<Retained<ICCameraFile>>)>,
    next_session: u64,
}

thread_local! {
    /// Only ever touched on the main thread (from [`on_main`] work and the delegates).
    static MAIN: RefCell<Main> = RefCell::new(Main::default());
}

/// Run `f` with the main thread's state. `None` when it is already borrowed (a delegate call that
/// arrived while main-thread work held it; nothing here calls the framework while borrowed).
fn with_main<R>(f: impl FnOnce(&mut Main) -> R) -> Option<R> {
    MAIN.with(|m| m.try_borrow_mut().ok().map(|mut m| f(&mut m)))
}

fn string(s: Option<Retained<NSString>>) -> String {
    s.map(|s| s.to_string()).unwrap_or_default()
}

/// The text of an error the framework handed back (null: "unknown error").
fn describe(e: *mut NSError) -> String {
    // SAFETY: `e` is null or an `NSError` the framework passed to a completion block, valid for the
    // duration of the block.
    let Some(e) = (unsafe { e.as_ref() }) else { return "unknown error".into() };
    // SAFETY: `localizedDescription` takes no arguments and returns an `NSString` (nil is handled).
    let d: Option<Retained<NSString>> = unsafe { msg_send![e, localizedDescription] };
    let d = string(d);
    if d.is_empty() { "unknown error".into() } else { d }
}

/// A device's id: its UUID (stable while it stays plugged in), else its persistent id.
fn device_id(d: &ICDevice) -> String {
    // SAFETY: property getters without arguments; nil is handled.
    let id = string(unsafe { d.UUIDString() });
    if !id.is_empty() {
        return id;
    }
    // SAFETY: as above.
    string(unsafe { d.persistentIDString() })
}

/// A camera worth listing: not a scanner, and not a card in a reader (that is a mounted volume,
/// which the volume scan lists already).
fn is_listed(d: &ICDevice) -> bool {
    // SAFETY: property getters without arguments; nil is handled.
    let (kind, transport) = unsafe { (d.r#type(), d.transportType()) };
    kind.0 & ICDeviceTypeMask::Camera.0 != 0 && transport.is_none_or(|t| t.to_string() != MASS_STORAGE)
}

fn publish(m: &Main) {
    let cams = m
        .devices
        .iter()
        .map(|(id, d)| {
            // SAFETY: property getter without arguments; nil is handled.
            let name = string(unsafe { d.name() });
            Camera { id: id.clone(), name: if name.is_empty() { "Camera".into() } else { name }, manufacturer: String::new() }
        })
        .collect();
    lock(&SHARED).cameras = cams;
}

// ---------------------------------------------------------------------------------------------
// Delegates (main thread only)

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LightCraftCameraBrowserDelegate"]
    struct BrowserDelegate;

    unsafe impl NSObjectProtocol for BrowserDelegate {}

    unsafe impl ICDeviceBrowserDelegate for BrowserDelegate {
        #[unsafe(method(deviceBrowser:didAddDevice:moreComing:))]
        fn did_add(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            if !is_listed(device) {
                return;
            }
            let id = device_id(device);
            if id.is_empty() {
                return;
            }
            let device: Retained<ICDevice> = device.retain();
            with_main(|m| {
                if !m.devices.iter().any(|(d, _)| *d == id) {
                    m.devices.push((id, device));
                    publish(m);
                }
            });
        }

        #[unsafe(method(deviceBrowser:didRemoveDevice:moreGoing:))]
        fn did_remove(&self, _browser: &ICDeviceBrowser, device: &ICDevice, _more: bool) {
            let id = device_id(device);
            with_main(|m| {
                m.devices.retain(|(d, _)| *d != id);
                if let Some(o) = m.open.get(&id) {
                    lock(&o.state).removed = true;
                }
                publish(m);
            });
        }

        #[unsafe(method(deviceBrowserDidEnumerateLocalDevices:))]
        fn did_enumerate(&self, _browser: &ICDeviceBrowser) {
            lock(&SHARED).enumerated = true;
        }
    }
);

impl BrowserDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: `NSObject`'s designated initializer on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LightCraftCameraDelegate"]
    #[ivars = Arc<Mutex<CatalogState>>]
    struct CameraDelegate;

    unsafe impl NSObjectProtocol for CameraDelegate {}

    unsafe impl ICDeviceDelegate for CameraDelegate {
        #[unsafe(method(device:didCloseSessionWithError:))]
        fn did_close(&self, _device: &ICDevice, _error: Option<&NSError>) {}

        #[unsafe(method(didRemoveDevice:))]
        fn did_remove(&self, _device: &ICDevice) {
            lock(self.ivars()).removed = true;
        }

        // the session's result arrives through the completion block of `requestOpenSessionWithOptions`
        #[unsafe(method(device:didOpenSessionWithError:))]
        fn did_open(&self, _device: &ICDevice, _error: Option<&NSError>) {}
    }

    unsafe impl ICCameraDeviceDelegate for CameraDelegate {
        #[unsafe(method(cameraDevice:didAddItems:))]
        fn did_add_items(&self, _camera: &ICCameraDevice, items: &NSArray<ICCameraItem>) {
            let mut s = lock(self.ivars());
            s.items = s.items.saturating_add(items.count());
        }

        #[unsafe(method(cameraDevice:didRemoveItems:))]
        fn did_remove_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}

        #[unsafe(method(cameraDevice:didRenameItems:))]
        fn did_rename_items(&self, _camera: &ICCameraDevice, _items: &NSArray<ICCameraItem>) {}

        #[unsafe(method(cameraDeviceDidChangeCapability:))]
        fn did_change_capability(&self, _camera: &ICCameraDevice) {}

        #[unsafe(method(cameraDevice:didReceivePTPEvent:))]
        fn did_receive_ptp_event(&self, _camera: &ICCameraDevice, _event: &NSData) {}

        #[unsafe(method(deviceDidBecomeReadyWithCompleteContentCatalog:))]
        fn did_become_ready(&self, _camera: &ICCameraDevice) {
            lock(self.ivars()).ready = true;
        }

        #[unsafe(method(cameraDeviceDidRemoveAccessRestriction:))]
        fn did_remove_access_restriction(&self, _device: &ICDevice) {
            lock(self.ivars()).restricted = Some(false);
        }

        #[unsafe(method(cameraDeviceDidEnableAccessRestriction:))]
        fn did_enable_access_restriction(&self, _device: &ICDevice) {
            lock(self.ivars()).restricted = Some(true);
        }
    }

    // Required by the protocol but never asked for (no thumbnails or metadata are requested);
    // declared outside the protocol so their `CGImage`/`NSDictionary` arguments need no bindings.
    impl CameraDelegate {
        #[unsafe(method(cameraDevice:didReceiveThumbnail:forItem:error:))]
        fn did_receive_thumbnail(&self, _camera: &ICCameraDevice, _thumbnail: *const std::ffi::c_void, _item: &ICCameraItem, _error: Option<&NSError>) {}

        #[unsafe(method(cameraDevice:didReceiveMetadata:forItem:error:))]
        fn did_receive_metadata(&self, _camera: &ICCameraDevice, _metadata: *const std::ffi::c_void, _item: &ICCameraItem, _error: Option<&NSError>) {}
    }
);

impl CameraDelegate {
    fn new(mtm: MainThreadMarker, state: Arc<Mutex<CatalogState>>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(state);
        // SAFETY: `NSObject`'s designated initializer on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

// ---------------------------------------------------------------------------------------------
// Getting work onto the main thread and answers back

/// Run `f` on the main thread and return its result: inline when this is the main thread, else
/// through the main queue (an error when the main thread doesn't get to it within `MAIN_WAIT`).
fn on_main<R: Send + 'static>(f: impl FnOnce(MainThreadMarker) -> R + Send + 'static) -> Result<R, Error> {
    if let Some(mtm) = MainThreadMarker::new() {
        return Ok(f(mtm));
    }
    let (tx, rx) = mpsc::channel();
    DispatchQueue::main().exec_async(move || {
        // the main queue runs on the main thread, so the marker is always there
        if let Some(mtm) = MainThreadMarker::new() {
            let _ = tx.send(f(mtm));
        }
    });
    recv(&rx, MAIN_WAIT, None).map_err(|e| match e {
        Error::Device(_) => Error::Device("the main thread isn't answering (cameras are read through the app's event loop)".into()),
        e => e,
    })
}

/// Run `f` on the main thread without waiting for it: now when this is the main thread, else when
/// the main queue gets to it.
fn post_main(f: impl FnOnce(MainThreadMarker) + Send + 'static) {
    if let Some(mtm) = MainThreadMarker::new() {
        return f(mtm);
    }
    DispatchQueue::main().exec_async(move || {
        if let Some(mtm) = MainThreadMarker::new() {
            f(mtm);
        }
    });
}

/// Wait up to `timeout` for `rx`, stopping early on `cancel`. On the main thread the run loop is
/// spun meanwhile, so the framework's answers (which arrive there) can come in.
fn recv<R>(rx: &Receiver<R>, timeout: Duration, cancel: Option<&AtomicBool>) -> Result<R, Error> {
    let start = Instant::now();
    let main = NSThread::isMainThread_class();
    if main && !may_spin() {
        return Err(busy_main());
    }
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(Error::Cancelled);
        }
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return Err(Error::Device("the camera didn't answer in time".into()));
        }
        let slice = left.min(Duration::from_millis(50));
        if main {
            match rx.try_recv() {
                Ok(r) => return Ok(r),
                Err(TryRecvError::Disconnected) => return Err(Error::Device("the camera request was dropped".into())),
                Err(TryRecvError::Empty) => spin(slice.min(Duration::from_millis(10))),
            }
        } else {
            match rx.recv_timeout(slice) {
                Ok(r) => return Ok(r),
                Err(RecvTimeoutError::Disconnected) => return Err(Error::Device("the camera request was dropped".into())),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// Wait until `done` says so (or an error), checking every 50 ms; the run loop is spun on the main
/// thread meanwhile.
fn wait_for<T>(mut done: impl FnMut() -> Option<Result<T, Error>>) -> Result<T, Error> {
    let main = NSThread::isMainThread_class();
    if main && !may_spin() {
        return done().unwrap_or_else(|| Err(busy_main()));
    }
    loop {
        if let Some(r) = done() {
            return r;
        }
        if main {
            spin(Duration::from_millis(10));
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The main thread may spin its run loop to wait: it isn't inside one already. Inside one (an app's
/// event loop calling us) a nested run would deliver the app's own events re-entrantly, so the main
/// thread of an app doesn't wait for cameras at all; its workers do.
fn may_spin() -> bool {
    NSRunLoop::currentRunLoop().currentMode().is_none()
}

fn busy_main() -> Error {
    Error::Device("cameras are read off the app's main thread (it can't wait for them)".into())
}

/// Let the main run loop deliver what's pending for up to `d` (main thread only).
fn spin(d: Duration) {
    let until = NSDate::dateWithTimeIntervalSinceNow(d.as_secs_f64());
    NSRunLoop::currentRunLoop().runUntilDate(&until);
    // `runUntilDate` returns at once when the run loop has no sources yet
    std::thread::sleep(Duration::from_millis(1));
}

// ---------------------------------------------------------------------------------------------
// The API

fn start_browser(mtm: MainThreadMarker) {
    let delegate = BrowserDelegate::new(mtm);
    // SAFETY: a plain `alloc`/`init` of the framework's browser class.
    let browser = unsafe { ICDeviceBrowser::new() };
    // SAFETY: the delegate is retained in `MAIN` for as long as the browser (the browser holds it
    // weakly); the mask is a valid OR of a device type and a location type.
    unsafe {
        browser.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        browser.setBrowsedDeviceTypeMask(ICDeviceTypeMask(ICDeviceTypeMask::Camera.0 | ICDeviceLocationTypeMask::Local.0));
        browser.start();
    }
    with_main(|m| {
        m.browser = Some(browser);
        m.browser_delegate = Some(delegate);
    });
}

pub fn cameras() -> Result<Vec<Camera>, Error> {
    let first = {
        let mut s = lock(&SHARED);
        !std::mem::replace(&mut s.started, true)
    };
    if first {
        // not waited for: where nothing serves the main queue (a test harness) the list stays empty
        post_main(start_browser);
        // the browser reports what is plugged in already right after it starts (an app's main
        // thread doesn't wait: its next look, a moment later, has them)
        let start = Instant::now();
        let _ = wait_for(|| {
            let t = start.elapsed();
            ((lock(&SHARED).enumerated && t >= FIRST_LOOK_MIN) || t >= FIRST_LOOK).then_some(Ok(()))
        });
    }
    Ok(lock(&SHARED).cameras.clone())
}

pub struct Session {
    key: u64,
    id: String,
    state: Arc<Mutex<CatalogState>>,
}

impl Session {
    pub fn open(id: &str) -> Result<Session, Error> {
        // the browser must be running for the camera to be known
        cameras()?;
        let owned = id.to_string();
        let opened = on_main(move |mtm| open_on_main(mtm, owned))??;
        let (key, state, rx) = opened;
        let session = Session { key, id: id.to_string(), state };
        // dropping `session` on failure closes the camera again
        let Some(rx) = rx else { return Ok(session) };
        let Err(refused) = recv(&rx, OPEN_WAIT, None)? else { return Ok(session) };
        // An iPhone or iPad refuses the first session while it restricts access ("Please unlock
        // …"), even when it is unlocked, and lifts the restriction a moment later: wait for that
        // (or for its user to unlock it), then ask again.
        let reported = lock(&session.state).restricted.is_some();
        let owned = id.to_string();
        if !reported && !on_main(move |_| restricted_on_main(&owned))? {
            return Err(Error::Device(refused));
        }
        let start = Instant::now();
        let lifted = wait_for(|| {
            let s = lock(&session.state);
            if s.removed || start.elapsed() >= UNLOCK_WAIT {
                return Some(Ok(false));
            }
            (s.restricted == Some(false)).then_some(Ok(true))
        })?;
        if !lifted {
            return Err(Error::Device(refused));
        }
        let owned = id.to_string();
        let rx = on_main(move |_| reopen_on_main(&owned))??;
        recv(&rx, OPEN_WAIT, None)?.map_err(Error::Device)?;
        Ok(session)
    }

    pub fn files(&self, cancel: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error> {
        let mut last = (0usize, Instant::now());
        wait_for(|| {
            if cancel.load(Ordering::Relaxed) {
                return Some(Err(Error::Cancelled));
            }
            let s = lock(&self.state);
            if s.removed {
                return Some(Err(Error::Device("the camera was disconnected".into())));
            }
            if s.ready {
                return Some(Ok(()));
            }
            if s.items != last.0 {
                last = (s.items, Instant::now());
                found(s.items);
            } else if last.1.elapsed() >= CATALOG_STALL {
                return Some(Err(Error::Device("the camera stopped listing its photos".into())));
            }
            None
        })?;
        let (key, id) = (self.key, self.id.clone());
        let out = on_main(move |_| list_on_main(key, &id))??;
        found(out.len());
        Ok(out)
    }

    pub fn copy_to(&self, file: &CameraFile, out: &mut dyn std::io::Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
        let index: usize = file.object.parse().map_err(|_| Error::Device(format!("no file {} on the camera", file.object)))?;
        let size = file.size.and_then(|s| i64::try_from(s).ok());
        let mut pending: VecDeque<(i64, Chunk)> = VecDeque::new();
        let mut next: i64 = 0;
        let mut total: u64 = 0;
        loop {
            // keep reads queued: all of a known size, one at a time (until a short read) otherwise
            while pending.len() < IN_FLIGHT && size.is_none_or(|s| next < s) && (size.is_some() || pending.is_empty()) {
                let len = size.map_or(CHUNK, |s| CHUNK.min(s.saturating_sub(next)));
                let key = self.key;
                let rx = on_main(move |_| read_on_main(key, index, next, len))??;
                pending.push_back((len, rx));
                next = next.saturating_add(len);
            }
            let Some((asked, rx)) = pending.pop_front() else { return Ok(total) };
            let data = recv(&rx, READ_WAIT, Some(cancel))?.map_err(Error::Device)?;
            if data.len() as u64 > asked as u64 {
                return Err(Error::Device("the camera sent more than was asked for".into()));
            }
            if !data.is_empty() {
                out.write_all(&data).map_err(|e| Error::Write(e.to_string()))?;
                total = total.saturating_add(data.len() as u64);
                progress(total);
            }
            if (data.len() as i64) < asked {
                return match size {
                    Some(s) => Err(Error::Device(format!("the camera sent {total} of {s} bytes"))),
                    None => Ok(total),
                };
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // nobody waits for the camera to close
        let (key, id) = (self.key, std::mem::take(&mut self.id));
        post_main(move |_| close_on_main(key, &id));
    }
}

type Opened = (u64, Arc<Mutex<CatalogState>>, Option<Receiver<Result<(), String>>>);

/// Start a session with camera `id` (or join the one already open). The receiver, when there is
/// one, gets the session's result.
fn open_on_main(mtm: MainThreadMarker, id: String) -> Result<Opened, Error> {
    let found = with_main(|m| {
        m.next_session = m.next_session.wrapping_add(1);
        let key = m.next_session;
        if let Some(o) = m.open.get_mut(&id) {
            o.users = o.users.saturating_add(1);
            return Ok((key, o.state.clone(), None));
        }
        let device = m.devices.iter().find(|(d, _)| *d == id).map(|(_, d)| d.clone());
        let device = device.ok_or_else(|| Error::Device("the camera isn't connected any more".into()))?;
        let camera = device.downcast::<ICCameraDevice>().map_err(|_| Error::Device("the device isn't a camera".into()))?;
        let state = Arc::new(Mutex::new(CatalogState::default()));
        let delegate = CameraDelegate::new(mtm, state.clone());
        m.open.insert(id.clone(), Open { camera: camera.clone(), delegate: delegate.clone(), state: state.clone(), users: 1 });
        Ok((key, state, Some((camera, delegate))))
    })
    .ok_or_else(|| Error::Device("busy".into()))??;
    let (key, state, start) = found;
    let Some((camera, delegate)) = start else { return Ok((key, state, None)) };
    // SAFETY: the delegate is retained in `MAIN.open` while the session is (the camera holds it
    // weakly); the presentation is one of the framework's constants.
    unsafe {
        camera.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // the files as they are on the device (a phone would otherwise offer JPEG copies of HEICs)
        camera.setMediaPresentation(ICMediaPresentation::OriginalAssets);
    }
    Ok((key, state, Some(request_open(&camera))))
}

/// Ask `camera` for a session; the receiver gets the answer.
fn request_open(camera: &ICCameraDevice) -> Receiver<Result<(), String>> {
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |e: *mut NSError| {
        let _ = tx.send(if e.is_null() { Ok(()) } else { Err(describe(e)) });
    });
    // SAFETY: no options; the framework copies the block and calls it once.
    unsafe { camera.requestOpenSessionWithOptions_completion(None, &done) };
    rx
}

/// Camera `id` (already in `MAIN.open`) is an iPhone or iPad restricting access.
fn restricted_on_main(id: &str) -> bool {
    let camera = with_main(|m| m.open.get(id).map(|o| o.camera.clone())).flatten();
    // SAFETY: property getter without arguments.
    camera.is_some_and(|c| unsafe { c.isAccessRestrictedAppleDevice() })
}

/// Ask camera `id` (already in `MAIN.open`) for its session again.
fn reopen_on_main(id: &str) -> Result<Receiver<Result<(), String>>, Error> {
    let camera = with_main(|m| m.open.get(id).map(|o| o.camera.clone())).flatten();
    let camera = camera.ok_or_else(|| Error::Device("the camera's session has ended".into()))?;
    Ok(request_open(&camera))
}

fn close_on_main(key: u64, id: &str) {
    let camera = with_main(|m| {
        m.files.remove(&key);
        let o = m.open.get_mut(id)?;
        o.users = o.users.saturating_sub(1);
        if o.users > 0 {
            return None;
        }
        m.open.remove(id).map(|o| (o.camera, o.delegate))
    })
    .flatten();
    if let Some((camera, _delegate)) = camera {
        // SAFETY: plain requests on a camera this module opened; the delegate is dropped after.
        unsafe {
            camera.requestCloseSession();
            camera.setDelegate(None);
        }
    }
}

/// A file's folders, from the top down (at most `MAX_DEPTH + 2` of them are looked at: deeper ones
/// aren't imported anyway, and a broken parent chain can't loop forever).
fn folders_of(file: &ICCameraFile) -> Vec<String> {
    let mut out = Vec::new();
    // SAFETY: property getter without arguments; nil ends the chain.
    let mut parent = unsafe { file.parentFolder() };
    while let Some(folder) = parent {
        if out.len() > MAX_DEPTH + 1 {
            break;
        }
        // SAFETY: as above.
        out.push(string(unsafe { folder.name() }));
        // SAFETY: as above.
        parent = unsafe { folder.parentFolder() };
    }
    out.reverse();
    out
}

/// List camera `id`'s files for session `key` and keep them (by index) for reading. The framework
/// hands them over as one flat list (`contents`, the folder tree, stays empty on current macOS).
fn list_on_main(key: u64, id: &str) -> Result<Vec<CameraFile>, Error> {
    let camera = with_main(|m| m.open.get(id).map(|o| o.camera.clone())).flatten();
    let camera = camera.ok_or_else(|| Error::Device("the camera's session has ended".into()))?;
    // SAFETY: property getter without arguments; nil is an empty camera.
    let items = unsafe { camera.mediaFiles() }.map(|a| a.to_vec()).unwrap_or_default();
    if items.len() > MAX_ITEMS {
        return Err(Error::Device(format!("more than {MAX_ITEMS} files")));
    }
    let files: Vec<_> = items
        .into_iter()
        .filter_map(|item| item.downcast::<ICCameraFile>().ok())
        .map(|file| {
            let (name, folders) = (item_name(&file), folders_of(&file));
            (file, name, folders)
        })
        .collect();
    let mut kept = Vec::new();
    let mut out = Vec::new();
    for (file, name, folder) in pick(files) {
        // SAFETY: property getters without arguments; nil is handled.
        let (size, exif, created) = unsafe { (file.fileSize(), file.exifCreationDate(), file.creationDate()) };
        out.push(CameraFile {
            object: kept.len().to_string(),
            name,
            folder,
            size: u64::try_from(size).ok(),
            created: exif.or(created).and_then(|d| local_iso(&d)),
        });
        kept.push(file);
    }
    with_main(|m| {
        m.files.insert(key, (id.to_string(), kept));
    });
    Ok(out)
}

fn item_name(file: &ICCameraFile) -> String {
    // SAFETY: property getter without arguments; nil is handled.
    string(unsafe { file.name() })
}

/// Ask for `len` bytes at `offset` of the session's file `index`; the receiver gets them.
fn read_on_main(key: u64, index: usize, offset: i64, len: i64) -> Result<Chunk, Error> {
    let file = with_main(|m| m.files.get(&key).and_then(|(_, f)| f.get(index).cloned())).flatten();
    let file = file.ok_or_else(|| Error::Device(format!("no file {index} on the camera")))?;
    let (tx, rx) = mpsc::channel();
    let done = RcBlock::new(move |data: *mut NSData, e: *mut NSError| {
        // SAFETY: `data` is null or an `NSData` valid for the duration of the block.
        let r = match unsafe { data.as_ref() } {
            _ if !e.is_null() => Err(describe(e)),
            Some(d) => Ok(d.to_vec()),
            None => Ok(Vec::new()),
        };
        let _ = tx.send(r);
    });
    // SAFETY: a read request with a non-negative offset and length; the framework copies the block.
    unsafe { file.requestReadDataAtOffset_length_completion(offset.max(0), len.max(0), &done) };
    Ok(rx)
}

/// A date as the local `YYYY-MM-DDTHH:MM:SS` the camera's clock showed (the framework reads the
/// camera's zone-less times as local time; this turns them back).
fn local_iso(d: &NSDate) -> Option<String> {
    let t = d.timeIntervalSince1970();
    if !t.is_finite() {
        return None;
    }
    let offset = NSTimeZone::localTimeZone().secondsFromGMTForDate(d) as i64;
    crate::unix_to_iso((t.floor() as i64).saturating_add(offset))
}
