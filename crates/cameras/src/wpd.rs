//! Windows Portable Devices: the COM API Windows' own MTP/PTP class driver (`WUDFWpdMtp`) serves
//! cameras and phones through. Every `unsafe` block is a call into that API (or freeing the memory
//! it hands back); the module's functions are safe.
//!
//! Threading: each thread that calls in initializes COM for itself ([`Com`]); [`Session`] keeps
//! its guard so the apartment outlives the objects.
#![allow(unsafe_code)]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Devices::PortableDevices::{
    IPortableDevice, IPortableDeviceContent, IPortableDeviceKeyCollection, IPortableDeviceManager, IPortableDeviceProperties, IPortableDeviceValues,
    PortableDeviceFTM, PortableDeviceKeyCollection, PortableDeviceManager, PortableDeviceValues, WPD_CLIENT_DESIRED_ACCESS, WPD_CLIENT_MAJOR_VERSION,
    WPD_CLIENT_MINOR_VERSION, WPD_CLIENT_NAME, WPD_CLIENT_REVISION, WPD_CONTENT_TYPE_FOLDER, WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT,
    WPD_DEVICE_OBJECT_ID, WPD_DEVICE_TYPE, WPD_DEVICE_TYPE_CAMERA, WPD_DEVICE_TYPE_PHONE, WPD_FUNCTIONAL_CATEGORY_STORAGE,
    WPD_FUNCTIONAL_OBJECT_CATEGORY, WPD_OBJECT_CONTENT_TYPE, WPD_OBJECT_DATE_CREATED, WPD_OBJECT_DATE_MODIFIED, WPD_OBJECT_NAME,
    WPD_OBJECT_ORIGINAL_FILE_NAME, WPD_OBJECT_SIZE, WPD_RESOURCE_DEFAULT,
};
use windows::Win32::Foundation::{GENERIC_READ, PROPERTYKEY, RPC_E_CHANGED_MODE, S_FALSE};
use windows::Win32::System::Com::StructuredStorage::{PROPVARIANT, PropVariantClear};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IStream, STGM_READ,
};
use windows::Win32::System::Variant::VT_DATE;
use windows::core::{PCWSTR, PWSTR};

use crate::{Camera, CameraFile, Error, MAX_DEPTH};

/// Objects asked for per `IEnumPortableDeviceObjectIDs::Next` call.
const BATCH: usize = 64;
/// Copy buffer bounds; WPD suggests a size per stream (the "optimal buffer size").
const MIN_BUFFER: usize = 64 * 1024;
const MAX_BUFFER: usize = 4 * 1024 * 1024;
/// At most this many objects are listed per camera (a card holds tens of thousands at most); a
/// device that keeps answering past it is treated as broken rather than read forever.
const MAX_OBJECTS: usize = 1_000_000;

fn dev(e: windows::core::Error) -> Error {
    Error::Device(e.message().trim().to_string())
}

/// COM initialized on this thread (multithreaded apartment) for as long as it lives.
struct Com {
    /// `CoUninitialize` is owed: only when this guard's `CoInitializeEx` succeeded.
    owned: bool,
}

impl Com {
    fn init() -> Result<Com, Error> {
        // SAFETY: plain COM initialization of the calling thread; balanced by `Drop` when it succeeds.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Com { owned: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // the thread is already in a single-threaded apartment (a UI thread): COM works, and the
            // owner of that initialization uninitializes it
            Ok(Com { owned: false })
        } else {
            Err(dev(hr.into()))
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: balances the successful `CoInitializeEx` of `Com::init` on this same thread
            // (`Com` is neither `Send` nor `Sync`, so it is dropped where it was made).
            unsafe { CoUninitialize() };
        }
    }
}

/// A string WPD allocated for us (`CoTaskMemAlloc`): copied out, then freed. Null is "".
fn take(p: PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is a non-null, NUL-terminated UTF-16 string returned by a WPD call.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: WPD hands out strings allocated with CoTaskMemAlloc and the caller frees them; `p` is
    // not used again.
    unsafe { CoTaskMemFree(Some(p.0 as *const core::ffi::c_void)) };
    s
}

/// `s` as a NUL-terminated UTF-16 string.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// One of the manager's per-device strings (friendly name, manufacturer), asked for twice: size, then
/// contents.
fn device_string(
    manager: &IPortableDeviceManager,
    id: &[u16],
    get: impl Fn(&IPortableDeviceManager, PCWSTR, PWSTR, *mut u32) -> windows::core::Result<()>,
) -> String {
    let mut len = 0u32;
    if get(manager, PCWSTR(id.as_ptr()), PWSTR::null(), &mut len).is_err() || len == 0 || len > 4096 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize];
    if get(manager, PCWSTR(id.as_ptr()), PWSTR(buf.as_mut_ptr()), &mut len).is_err() {
        return String::new();
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or_default()).trim().to_string()
}

pub fn cameras() -> Result<Vec<Camera>, Error> {
    let _com = Com::init()?;
    // SAFETY: creating the system's in-process device manager; the result is a counted reference.
    let manager: IPortableDeviceManager = unsafe { CoCreateInstance(&PortableDeviceManager, None, CLSCTX_INPROC_SERVER) }.map_err(dev)?;
    let mut count = 0u32;
    // SAFETY: a null array asks for the count only.
    unsafe { manager.GetDevices(std::ptr::null_mut(), &mut count) }.map_err(dev)?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut ids = vec![PWSTR::null(); count.min(1024) as usize];
    let mut got = ids.len() as u32;
    // SAFETY: `ids` holds `got` writable slots; each filled one is a CoTaskMemAlloc string, freed by `take`.
    unsafe { manager.GetDevices(ids.as_mut_ptr(), &mut got) }.map_err(dev)?;
    let ids: Vec<String> = ids.into_iter().take(got as usize).map(take).filter(|s| !s.is_empty()).collect();
    let mut out = Vec::new();
    for id in ids {
        let w = wide(&id);
        // only cameras and phones: WPD also lists media players, and USB drives through its file
        // system driver (those are already found as mounted volumes)
        if !matches!(device_type(&id), Some(t) if t == WPD_DEVICE_TYPE_CAMERA.0 as u32 || t == WPD_DEVICE_TYPE_PHONE.0 as u32) {
            continue;
        }
        // SAFETY (both closures): `id` is a valid NUL-terminated device id and `name`/`len` describe a
        // buffer the manager may fill (or a null one when it reports the length).
        let name = device_string(&manager, &w, |m, id, name, len| unsafe { m.GetDeviceFriendlyName(id, name, len) });
        let manufacturer = device_string(&manager, &w, |m, id, name, len| unsafe { m.GetDeviceManufacturer(id, name, len) });
        out.push(Camera { name: if name.is_empty() { "Camera".into() } else { name }, manufacturer, id });
    }
    Ok(out)
}

/// Device types read so far, by device id. The device list is rescanned every few seconds while
/// the app runs; a camera is opened to read its type only the first time it is seen. Only answers
/// are kept: a device that was busy is asked again next time.
static TYPES: std::sync::Mutex<Vec<(String, u32)>> = std::sync::Mutex::new(Vec::new());

/// The device's WPD_DEVICE_TYPE (camera, phone, media player …), opening it once.
fn device_type(id: &str) -> Option<u32> {
    let known = TYPES.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|(k, _)| k == id).map(|(_, t)| *t);
    if known.is_some() {
        return known;
    }
    let t = read_device_type(id)?;
    let mut types = TYPES.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if types.len() >= 256 {
        types.clear();
    }
    types.push((id.to_string(), t));
    Some(t)
}

fn read_device_type(id: &str) -> Option<u32> {
    let s = Session::open(id).ok()?;
    let v = s.values(WPD_DEVICE_OBJECT_ID, &[WPD_DEVICE_TYPE]).ok()?;
    // SAFETY: reading a property from the values object WPD just returned.
    unsafe { v.GetUnsignedIntegerValue(&WPD_DEVICE_TYPE) }.ok()
}

pub struct Session {
    device: IPortableDevice,
    content: IPortableDeviceContent,
    properties: IPortableDeviceProperties,
    /// Dropped last: the COM objects above need the apartment.
    _com: Com,
}

/// What an object is, as far as listing goes.
struct Object {
    id: String,
    name: String,
    folder: bool,
    storage: bool,
    size: Option<u64>,
    created: Option<String>,
}

impl Session {
    pub fn open(id: &str) -> Result<Session, Error> {
        let com = Com::init()?;
        // SAFETY: creating in-process WPD objects; each is a counted reference released on drop.
        let device: IPortableDevice = unsafe { CoCreateInstance(&PortableDeviceFTM, None, CLSCTX_INPROC_SERVER) }.map_err(dev)?;
        let client: IPortableDeviceValues = unsafe { CoCreateInstance(&PortableDeviceValues, None, CLSCTX_INPROC_SERVER) }.map_err(dev)?;
        // SAFETY: setting plain values on the client-information object we own.
        unsafe {
            client.SetStringValue(&WPD_CLIENT_NAME, windows::core::w!("LightCraft")).map_err(dev)?;
            client.SetUnsignedIntegerValue(&WPD_CLIENT_MAJOR_VERSION, 1).map_err(dev)?;
            client.SetUnsignedIntegerValue(&WPD_CLIENT_MINOR_VERSION, 0).map_err(dev)?;
            client.SetUnsignedIntegerValue(&WPD_CLIENT_REVISION, 0).map_err(dev)?;
            // read-only: LightCraft never writes to or deletes from a camera
            client.SetUnsignedIntegerValue(&WPD_CLIENT_DESIRED_ACCESS, GENERIC_READ.0).map_err(dev)?;
        }
        let w = wide(id);
        // SAFETY: `w` is a NUL-terminated device id that outlives the call.
        unsafe { device.Open(PCWSTR(w.as_ptr()), &client) }.map_err(dev)?;
        // SAFETY: the device is open; these return counted references to its services.
        let content = unsafe { device.Content() }.map_err(dev)?;
        let properties = unsafe { content.Properties() }.map_err(dev)?;
        Ok(Session { device, content, properties, _com: com })
    }

    /// The `keys` of object `id`.
    fn values(&self, id: PCWSTR, keys: &[PROPERTYKEY]) -> Result<IPortableDeviceValues, Error> {
        // SAFETY: an in-process key collection we fill with constant keys.
        let k: IPortableDeviceKeyCollection = unsafe { CoCreateInstance(&PortableDeviceKeyCollection, None, CLSCTX_INPROC_SERVER) }.map_err(dev)?;
        for key in keys {
            // SAFETY: `key` points at a valid PROPERTYKEY for the duration of the call.
            unsafe { k.Add(key) }.map_err(dev)?;
        }
        // SAFETY: `id` is a NUL-terminated object id that outlives the call.
        unsafe { self.properties.GetValues(id, &k) }.map_err(dev)
    }

    /// The ids of `parent`'s children.
    fn children(&self, parent: &str, cancel: &AtomicBool) -> Result<Vec<String>, Error> {
        let w = wide(parent);
        // SAFETY: `w` is a NUL-terminated object id; no filter.
        let e = unsafe { self.content.EnumObjects(0, PCWSTR(w.as_ptr()), None) }.map_err(dev)?;
        let mut out = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let mut ids = [PWSTR::null(); BATCH];
            let mut fetched = 0u32;
            // SAFETY: `ids` has BATCH writable slots; each filled one is a CoTaskMemAlloc string, freed by `take`.
            let hr = unsafe { e.Next(&mut ids, &mut fetched) };
            let n = (fetched as usize).min(BATCH);
            out.extend(ids.into_iter().take(n).map(take).filter(|s| !s.is_empty()));
            if hr.is_err() {
                return Err(dev(hr.into()));
            }
            if hr == S_FALSE || n == 0 || out.len() >= MAX_OBJECTS {
                return Ok(out);
            }
        }
    }

    fn object(&self, id: String) -> Result<Object, Error> {
        let w = wide(&id);
        let v = self.values(
            PCWSTR(w.as_ptr()),
            &[
                WPD_OBJECT_ORIGINAL_FILE_NAME,
                WPD_OBJECT_NAME,
                WPD_OBJECT_CONTENT_TYPE,
                WPD_FUNCTIONAL_OBJECT_CATEGORY,
                WPD_OBJECT_SIZE,
                WPD_OBJECT_DATE_CREATED,
                WPD_OBJECT_DATE_MODIFIED,
            ],
        )?;
        // SAFETY (this block): reading properties from the values object WPD just returned;
        // strings it hands out are freed by `take`.
        let (file_name, name, kind, category, size) = unsafe {
            (
                v.GetStringValue(&WPD_OBJECT_ORIGINAL_FILE_NAME).map(take).unwrap_or_default(),
                v.GetStringValue(&WPD_OBJECT_NAME).map(take).unwrap_or_default(),
                v.GetGuidValue(&WPD_OBJECT_CONTENT_TYPE).ok(),
                v.GetGuidValue(&WPD_FUNCTIONAL_OBJECT_CATEGORY).ok(),
                v.GetUnsignedLargeIntegerValue(&WPD_OBJECT_SIZE).ok(),
            )
        };
        let created = date(&v, &WPD_OBJECT_DATE_CREATED).or_else(|| date(&v, &WPD_OBJECT_DATE_MODIFIED));
        Ok(Object {
            id,
            name: if file_name.is_empty() { name } else { file_name },
            folder: kind == Some(WPD_CONTENT_TYPE_FOLDER),
            storage: kind == Some(WPD_CONTENT_TYPE_FUNCTIONAL_OBJECT) && category == Some(WPD_FUNCTIONAL_CATEGORY_STORAGE),
            size,
            created,
        })
    }

    pub fn files(&self, cancel: &AtomicBool, found: &mut dyn FnMut(usize)) -> Result<Vec<CameraFile>, Error> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let device = take_static(WPD_DEVICE_OBJECT_ID);
        for id in self.children(&device, cancel)? {
            let o = self.object(id)?;
            if !o.storage {
                continue;
            }
            let top = self.listing(&o.id, cancel, &mut seen)?;
            let dcim: Vec<&Object> = top.iter().filter(|c| c.folder && c.name.eq_ignore_ascii_case("DCIM")).collect();
            if dcim.is_empty() {
                // no DCF layout (some phones): the whole storage
                self.walk(top, "", 1, cancel, &mut seen, &mut out, found)?;
            } else {
                for d in dcim {
                    let inner = self.listing(&d.id, cancel, &mut seen)?;
                    self.walk(inner, &d.name, 2, cancel, &mut seen, &mut out, found)?;
                }
            }
        }
        Ok(out)
    }

    /// `parent`'s children, each read once (`seen` breaks cycles a broken device might report).
    fn listing(&self, parent: &str, cancel: &AtomicBool, seen: &mut HashSet<String>) -> Result<Vec<Object>, Error> {
        let mut v = Vec::new();
        for id in self.children(parent, cancel)? {
            if seen.len() >= MAX_OBJECTS {
                return Err(Error::Device(format!("more than {MAX_OBJECTS} objects")));
            }
            if seen.insert(id.clone()) {
                v.push(self.object(id)?);
            }
        }
        Ok(v)
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &self,
        objects: Vec<Object>,
        folder: &str,
        depth: usize,
        cancel: &AtomicBool,
        seen: &mut HashSet<String>,
        out: &mut Vec<CameraFile>,
        found: &mut dyn FnMut(usize),
    ) -> Result<(), Error> {
        for o in objects {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if o.folder || o.storage {
                if depth < MAX_DEPTH {
                    let path = if folder.is_empty() { o.name.clone() } else { format!("{folder}/{}", o.name) };
                    let inner = self.listing(&o.id, cancel, seen)?;
                    self.walk(inner, &path, depth + 1, cancel, seen, out, found)?;
                }
            } else if !o.name.is_empty() {
                out.push(CameraFile { object: o.id, name: o.name, folder: folder.to_string(), size: o.size, created: o.created });
            }
        }
        found(out.len());
        Ok(())
    }

    pub fn copy_to(&self, file: &CameraFile, out: &mut dyn std::io::Write, cancel: &AtomicBool, progress: &mut dyn FnMut(u64)) -> Result<u64, Error> {
        // SAFETY: the content object is valid while the session is; the result is a counted reference.
        let resources = unsafe { self.content.Transfer() }.map_err(dev)?;
        let w = wide(&file.object);
        let mut optimal = 0u32;
        let mut stream: Option<IStream> = None;
        // SAFETY: `w` is a NUL-terminated object id; `optimal` and `stream` are valid out-pointers.
        unsafe { resources.GetStream(PCWSTR(w.as_ptr()), &WPD_RESOURCE_DEFAULT, STGM_READ.0, &mut optimal, &mut stream) }.map_err(dev)?;
        let stream = stream.ok_or_else(|| Error::Device("no data stream for the file".into()))?;
        let mut buf = vec![0u8; (optimal as usize).clamp(MIN_BUFFER, MAX_BUFFER)];
        let mut total = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                // stop the transfer on the device side too (best effort)
                // SAFETY: cancels this session's pending operation; harmless when none is.
                let _ = unsafe { self.device.Cancel() };
                return Err(Error::Cancelled);
            }
            let mut read = 0u32;
            let len = u32::try_from(buf.len()).unwrap_or(u32::MAX);
            // SAFETY: `buf` has `len` writable bytes; `read` receives how many were filled.
            let hr = unsafe { stream.Read(buf.as_mut_ptr().cast(), len, Some(&mut read)) };
            if hr.is_err() {
                return Err(dev(hr.into()));
            }
            let n = (read as usize).min(buf.len());
            if n == 0 {
                return Ok(total);
            }
            out.write_all(buf.get(..n).unwrap_or_default()).map_err(|e| Error::Write(e.to_string()))?;
            total = total.saturating_add(n as u64);
            progress(total);
        }
    }
}

/// A WPD id constant (a static wide string) as a `String`.
fn take_static(p: PCWSTR) -> String {
    // SAFETY: WPD's id constants are static, NUL-terminated UTF-16 literals.
    unsafe { p.to_string() }.unwrap_or_default()
}

/// A date property (VT_DATE) as local `YYYY-MM-DDTHH:MM:SS`.
fn date(v: &IPortableDeviceValues, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: reading a property into a PROPVARIANT we own and clear below.
    let mut pv: PROPVARIANT = unsafe { v.GetValue(key) }.ok()?;
    // SAFETY: the union is read only as the member its type tag `vt` says it holds.
    let days = unsafe {
        let inner = &pv.Anonymous.Anonymous;
        (inner.vt == VT_DATE).then_some(inner.Anonymous.date)
    };
    // SAFETY: `pv` was filled by WPD and is cleared exactly once.
    let _ = unsafe { PropVariantClear(&mut pv) };
    crate::ole_date_to_iso(days?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn com_initializes_and_listing_never_fails_without_devices() {
        // whatever is plugged in, listing returns (it never panics or hangs)
        let r = cameras();
        assert!(r.is_ok(), "{r:?}");
    }

    #[test]
    fn a_bogus_device_id_is_an_error_not_a_crash() {
        assert!(matches!(Session::open("\\\\?\\usb#no-such-device"), Err(Error::Device(_))));
        assert!(matches!(Session::open(""), Err(Error::Device(_))));
    }

    #[test]
    fn strings_round_trip_through_wide() {
        let w = wide("DCIM/122_FUJI");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), "DCIM/122_FUJI");
        assert_eq!(take(PWSTR::null()), "");
        assert_eq!(take_static(WPD_DEVICE_OBJECT_ID), "DEVICE");
    }
}
