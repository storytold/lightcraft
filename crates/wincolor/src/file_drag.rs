//! Windows Shell drag source for existing files. No clipboard or file writes.
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::null_mut;
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, GlobalFree, POINT, S_OK};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, IDropSource, IDropSource_Impl, OleInitialize, OleUninitialize};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{SHCreateDataObject, SHDoDragDrop};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, GetWindowThreadProcessId, SM_SWAPBUTTON, WindowFromPoint};
use windows::core::{BOOL, HRESULT, implement};

struct Ole;
impl Drop for Ole {
    fn drop(&mut self) {
        // SAFETY: this guard is created only after successful initialization on this thread.
        unsafe { OleUninitialize() };
    }
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if keys.contains(MK_LBUTTON) {
            return S_OK;
        }
        // An outbound drag must never come back as an inbound import into this process.
        let mut point = POINT::default();
        let mut pid = 0;
        // SAFETY: point and pid are writable outputs; WindowFromPoint supplies the HWND.
        unsafe {
            if GetCursorPos(&mut point).is_err() || GetWindowThreadProcessId(WindowFromPoint(point), Some(&mut pid)) == 0 {
                return DRAGDROP_S_CANCEL;
            }
        }
        if pid == std::process::id() { DRAGDROP_S_CANCEL } else { DRAGDROP_S_DROP }
    }

    fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

fn primary_down() -> bool {
    // SAFETY: no pointers; the sign bit gives the current physical button state.
    unsafe {
        let key = if GetSystemMetrics(SM_SWAPBUTTON) == 0 { VK_LBUTTON } else { VK_RBUTTON };
        GetAsyncKeyState(i32::from(key.0)) < 0
    }
}

fn file_data(paths: &[PathBuf]) -> Result<IDataObject, String> {
    // DROPFILES: offset (20), POINT, fNC, fWide (TRUE), followed by UTF-16 names.
    let mut bytes = vec![20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0];
    for path in paths {
        for unit in path.as_os_str().encode_wide() {
            if unit == 0 {
                return Err("invalid original path".into());
            }
            bytes.extend(unit.to_le_bytes());
        }
        bytes.extend([0, 0]);
    }
    bytes.extend([0, 0]);
    // SAFETY: the Shell constructs a managed data object without any Shell item identifiers.
    let data: IDataObject = unsafe { SHCreateDataObject(None, None, None::<&IDataObject>) }.map_err(|e| e.to_string())?;
    let format =
        FORMATETC { cfFormat: CF_HDROP.0, ptd: null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0.cast_unsigned() };
    // SAFETY: allocate exactly the number of initialized bytes copied below.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) }.map_err(|e| e.to_string())?;
    // SAFETY: memory is an owned movable allocation.
    let ptr = unsafe { GlobalLock(memory) }.cast::<u8>();
    if ptr.is_null() {
        // SAFETY: the lock failed, so the allocation is unlocked and still ours.
        let _ = unsafe { GlobalFree(Some(memory)) };
        return Err("could not lock drag data".into());
    }
    // SAFETY: ptr points to a locked allocation of bytes.len() bytes, disjoint from bytes.
    unsafe {
        ptr.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        let _ = GlobalUnlock(memory);
    }
    let medium = STGMEDIUM { tymed: TYMED_HGLOBAL.0.cast_unsigned(), u: STGMEDIUM_0 { hGlobal: memory }, ..Default::default() };
    // SAFETY: the format and unlocked medium match; ownership transfers only on success.
    if let Err(error) = unsafe { data.SetData(&format, &medium, true) } {
        // SAFETY: on failure the allocation still belongs to the source.
        let _ = unsafe { GlobalFree(Some(memory)) };
        return Err(error.to_string());
    }
    Ok(data)
}

/// Start an OLE copy-only drag on the desktop UI thread. Release and Escape are owned by OLE
/// until it returns. No user file is changed by this source, whatever effect the target reports.
pub fn drag_original_files(paths: &[PathBuf]) -> Result<(), String> {
    if paths.is_empty() || !primary_down() {
        return Ok(());
    }
    // SAFETY: reserve parameter is null; the guard balances success on the same thread.
    unsafe { OleInitialize(None) }.map_err(|e| e.to_string())?;
    let _ole = Ole;
    let data = file_data(paths)?;
    if !primary_down() {
        return Ok(());
    }
    let source: IDropSource = DropSource.into();
    // SAFETY: data and source stay alive through the call; the UI thread is OLE-initialized.
    unsafe { SHDoDragDrop(None, &data, &source, DROPEFFECT_COPY) }.map_err(|e| e.to_string())?;
    Ok(())
}
