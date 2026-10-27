//! Android, over JNI: the photo picker (`android/java/…/LightCraftActivity.java`), and how much
//! of each window edge the system bars, display cutout and soft keyboard cover.
//!
//! winit 0.30 drops the activity's content-rect changes, and apps targeting Android 15 are drawn
//! edge to edge, so the window isn't resized for the keyboard either. The decor view's
//! `getWindowVisibleDisplayFrame` (API 1) gives the part of the screen left visible; comparing
//! it with the window's own bounds gives the insets. This is right whether or not Android resized
//! the window (`adjustResize` on older versions), since a resized window's bounds already exclude
//! the keyboard.
#![allow(unsafe_code)] // the JavaVM and activity pointers android-activity hands out

use jni::objects::{JObject, JObjectArray, JString};
use jni::{JValue, JavaVM, jni_sig, jni_str};
use winit::platform::android::activity::AndroidApp;

/// Pixels each edge of the window loses: `[left, top, right, bottom]`.
pub fn window_insets(app: &AndroidApp) -> Option<[i32; 4]> {
    // SAFETY: android-activity's pointer to the process's JavaVM, valid for the process's life
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    let r = vm.attach_current_thread(|env| -> jni::errors::Result<[i32; 4]> {
        // SAFETY: android-activity's global reference to the activity, valid while it runs
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        let window = env.call_method(&activity, jni_str!("getWindow"), jni_sig!("()Landroid/view/Window;"), &[])?.l()?;
        let decor = env.call_method(&window, jni_str!("getDecorView"), jni_sig!("()Landroid/view/View;"), &[])?.l()?;
        // the window on screen
        let at = env.new_int_array(2)?;
        env.call_method(&decor, jni_str!("getLocationOnScreen"), jni_sig!("([I)V"), &[JValue::Object(&at)])?;
        let mut xy = [0; 2];
        at.get_region(env, 0, &mut xy)?;
        let w = env.call_method(&decor, jni_str!("getWidth"), jni_sig!("()I"), &[])?.i()?;
        let h = env.call_method(&decor, jni_str!("getHeight"), jni_sig!("()I"), &[])?.i()?;
        // what's visible of the screen
        let rect = env.new_object(jni_str!("android/graphics/Rect"), jni_sig!("()V"), &[])?;
        env.call_method(&decor, jni_str!("getWindowVisibleDisplayFrame"), jni_sig!("(Landroid/graphics/Rect;)V"), &[JValue::Object(&rect)])?;
        let mut side = |name| env.get_field(&rect, name, jni_sig!("I")).and_then(|v| v.i());
        let (left, top, right, bottom) = (side(jni_str!("left"))?, side(jni_str!("top"))?, side(jni_str!("right"))?, side(jni_str!("bottom"))?);
        Ok([(left - xy[0]).max(0), (top - xy[1]).max(0), (xy[0] + w - right).max(0), (xy[1] + h - bottom).max(0)])
    });
    match r {
        Ok(insets) => Some(insets),
        Err(e) => {
            log::warn!("window insets: {e}");
            None
        }
    }
}

fn with_activity<T>(app: &AndroidApp, f: impl FnOnce(&mut jni::Env, &JObject) -> jni::errors::Result<T>) -> jni::errors::Result<T> {
    // SAFETY: as in `window_insets`
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: as in `window_insets`
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        f(env, &activity)
    })
}

/// Show the system photo picker; the chosen photos are copied into `dir`.
pub fn pick_photos(app: &AndroidApp, dir: &str) {
    let r = with_activity(app, |env, activity| {
        let dir = env.new_string(dir)?;
        env.call_method(activity, jni_str!("pickPhotos"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&dir)])?;
        Ok(())
    });
    if let Err(e) = r {
        log::warn!("photo picker: {e}");
    }
}

/// The paths of one picked selection that has been copied in, if any.
pub fn take_picked(app: &AndroidApp) -> Option<Vec<String>> {
    take_strings(app, jni_str!("takePicked"))
}

/// The activity's `String[] method()`: null as `None`.
fn take_strings(app: &AndroidApp, method: &jni::strings::JNIStr) -> Option<Vec<String>> {
    let r = with_activity(app, |env, activity| {
        let paths = env.call_method(activity, method, jni_sig!("()[Ljava/lang/String;"), &[])?.l()?;
        if paths.is_null() {
            return Ok(None);
        }
        // SAFETY: takePicked returns a String[]
        let paths = unsafe { JObjectArray::<JString>::from_raw(env, paths.into_raw()) };
        let mut out = Vec::new();
        for i in 0..paths.len(env)? {
            let s = paths.get_element(env, i)?;
            out.push(s.try_to_string(env)?);
        }
        Ok(Some(out))
    });
    r.unwrap_or_else(|e| {
        log::warn!("picker: {e}");
        None
    })
}

/// Show the system's document picker (`kind`: 0 several files, 1 one file, 2 a folder). Files
/// are copied into `dir`, a folder's files with one of `extensions` into `dir/<its name>`;
/// [`take_documents`] has the answer.
pub fn pick_documents(app: &AndroidApp, kind: i32, dir: &str, extensions: &[&str]) -> Result<(), String> {
    with_activity(app, |env, activity| {
        let dir = env.new_string(dir)?;
        let exts = env.new_string(extensions.join(","))?;
        env.call_method(
            activity,
            jni_str!("pickDocuments"),
            jni_sig!("(ILjava/lang/String;Ljava/lang/String;)V"),
            &[JValue::Int(kind), JValue::Object(&dir), JValue::Object(&exts)],
        )?;
        Ok(())
    })
    .map_err(|e| format!("document picker: {e}"))
}

/// The paths of one finished document pick (empty: cancelled), if any.
pub fn take_documents(app: &AndroidApp) -> Option<Vec<String>> {
    take_strings(app, jni_str!("takeDocuments"))
}

/// Open a web link in the browser.
pub fn open_url(app: &AndroidApp, url: &str) -> Result<(), String> {
    with_activity(app, |env, activity| {
        let url = env.new_string(url)?;
        env.call_method(activity, jni_str!("openUrl"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&url)])?;
        Ok(())
    })
    .map_err(|e| e.to_string())
}

/// Offer `paths` (files under the app's data directory) in the system share sheet.
pub fn share(app: &AndroidApp, paths: &[String]) {
    let r = with_activity(app, |env, activity| {
        // one string, newline-separated: simpler than building a String[] over JNI
        let joined = env.new_string(paths.join("\n"))?;
        env.call_method(activity, jni_str!("share"), jni_sig!("(Ljava/lang/String;)V"), &[JValue::Object(&joined)])?;
        Ok(())
    });
    if let Err(e) = r {
        log::warn!("share: {e}");
    }
}

/// How far the copy of picked files is, while one runs: files copied, files to copy (0: not known
/// yet), and the file being copied.
pub fn copy_progress(app: &AndroidApp) -> Option<(u64, u64, String)> {
    let r = with_activity(app, |env, activity| {
        let int = |env: &mut jni::Env, name| env.call_method(activity, name, jni_sig!("()I"), &[])?.i();
        if int(env, jni_str!("copiesRunning"))? <= 0 {
            return Ok(None);
        }
        let done = int(env, jni_str!("copiedFiles"))?;
        let total = int(env, jni_str!("filesToCopy"))?;
        let name = env.call_method(activity, jni_str!("copyName"), jni_sig!("()Ljava/lang/String;"), &[])?.l()?;
        let name = if name.is_null() {
            String::new()
        } else {
            // SAFETY: copyName returns a String
            unsafe { JString::from_raw(env, name.into_raw()) }.try_to_string(env)?
        };
        Ok(Some((u64::try_from(done).unwrap_or(0), u64::try_from(total).unwrap_or(0), name)))
    });
    r.unwrap_or_else(|e| {
        log::warn!("copy progress: {e}");
        None
    })
}

/// Stop copying picked files after the current one.
pub fn cancel_copy(app: &AndroidApp) {
    if let Err(e) = with_activity(app, |env, activity| env.call_method(activity, jni_str!("cancelCopy"), jni_sig!("()V"), &[]).map(drop)) {
        log::warn!("cancel copy: {e}");
    }
}

/// The strongest `onTrimMemory` level since the last call (0: none).
pub fn take_trim_level(app: &AndroidApp) -> i32 {
    with_activity(app, |env, activity| env.call_method(activity, jni_str!("takeTrimLevel"), jni_sig!("()I"), &[])?.i()).unwrap_or(0)
}

/// Write one line to logcat (tag `LightCraft`).
pub fn logcat(level: log::Level, text: &str) {
    let prio = match level {
        log::Level::Error => ndk_sys::android_LogPriority::ANDROID_LOG_ERROR,
        log::Level::Warn => ndk_sys::android_LogPriority::ANDROID_LOG_WARN,
        log::Level::Info => ndk_sys::android_LogPriority::ANDROID_LOG_INFO,
        _ => ndk_sys::android_LogPriority::ANDROID_LOG_DEBUG,
    };
    let Ok(text) = std::ffi::CString::new(text.replace('\0', " ")) else { return };
    // SAFETY: liblog call with NUL-terminated strings that outlive it
    unsafe {
        ndk_sys::__android_log_write(prio.0 as std::ffi::c_int, c"LightCraft".as_ptr(), text.as_ptr());
    }
}

/// Woken (repainted) when the soft keyboard queues edits; see [`register_wake`].
static WAKE: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();

/// The context [`register_wake`] wakes, for other threads that have news for it.
pub fn woken() -> Option<&'static egui::Context> {
    WAKE.get()
}

/// `LightCraftActivity.nativeWake()`: the keyboard queued edits (any thread).
extern "system" fn native_wake<'local>(_env: jni::EnvUnowned<'local>, _this: JObject<'local>) {
    if let Some(ctx) = WAKE.get() {
        ctx.request_repaint();
    }
}

/// Have the activity wake `ctx` when the soft keyboard types. NativeActivity loads the library
/// itself, not through `System.loadLibrary`, so the JVM can't find `Java_…` symbols in it: the
/// native method is registered by hand.
pub fn register_wake(app: &AndroidApp, ctx: &egui::Context) {
    let _ = WAKE.set(ctx.clone());
    let r = with_activity(app, |env, activity| {
        let class = env.get_object_class(activity)?;
        // SAFETY: `native_wake` is an instance method taking no arguments and returning nothing,
        // matching `()V`
        unsafe {
            let m = jni::NativeMethod::from_raw_parts(jni_str!("nativeWake"), jni_str!("()V"), native_wake as *mut std::ffi::c_void);
            env.register_native_methods(&class, &[m])
        }
    });
    if let Err(e) = r {
        log::warn!("text input: registering the wake-up: {e}");
    }
}

/// What the soft keyboard is told about the focused text field: its text and selection (char
/// indices, as egui counts them; a password field's text comes masked), and what it's for:
/// egui's IME purpose, and whether it's a number (a drag value or slider being typed into).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Field {
    pub text: String,
    pub selection: (usize, usize),
    pub purpose: egui::IMEPurpose,
    pub numeric: bool,
}

/// `TextInput.java`'s code for what `field` is for (a password stays a password).
fn purpose_code(field: &Field) -> i32 {
    match field.purpose {
        egui::IMEPurpose::Password => 1,
        egui::IMEPurpose::Terminal => 2,
        egui::IMEPurpose::Normal if field.numeric => 3,
        egui::IMEPurpose::Normal => 0,
    }
}

/// An egui text field got focus: the soft keyboard types into the activity's text input
/// (`TextInput.java`), with composing text and suggestions (none for a password field),
/// starting from `field`.
pub fn start_text_input(app: &AndroidApp, field: &Field) {
    let r = with_activity(app, |env, activity| {
        let text = env.new_string(&field.text)?;
        let (a, b) = (field.selection.0.min(i32::MAX as usize) as i32, field.selection.1.min(i32::MAX as usize) as i32);
        let args = [JValue::Object(&text), JValue::Int(a), JValue::Int(b), JValue::Int(purpose_code(field))];
        env.call_method(activity, jni_str!("startTextInput"), jni_sig!("(Ljava/lang/String;III)V"), &args).map(drop)
    });
    if let Err(e) = r {
        log::warn!("text input: {e}");
    }
}

/// The focused field changed other than by the keyboard (a tap moved the cursor, a key, the field
/// cleared): the keyboard's copy follows.
pub fn sync_text_input(app: &AndroidApp, field: &Field) {
    let r = with_activity(app, |env, activity| {
        let text = env.new_string(&field.text)?;
        let (a, b) = (field.selection.0.min(i32::MAX as usize) as i32, field.selection.1.min(i32::MAX as usize) as i32);
        let args = [JValue::Object(&text), JValue::Int(a), JValue::Int(b)];
        env.call_method(activity, jni_str!("syncTextInput"), jni_sig!("(Ljava/lang/String;II)V"), &args).map(drop)
    });
    if let Err(e) = r {
        log::warn!("text input: {e}");
    }
}

/// No egui text field has focus: the keyboard goes away.
pub fn stop_text_input(app: &AndroidApp) {
    if let Err(e) = with_activity(app, |env, activity| env.call_method(activity, jni_str!("stopTextInput"), jni_sig!("()V"), &[]).map(drop)) {
        log::warn!("text input: {e}");
    }
}

/// The soft keyboard's edits since the last call, as egui IME events.
pub fn take_text_input(app: &AndroidApp) -> Vec<egui::ImeEvent> {
    let r = with_activity(app, |env, activity| {
        let edits = env.call_method(activity, jni_str!("takeTextInput"), jni_sig!("()[Ljava/lang/String;"), &[])?.l()?;
        if edits.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: takeTextInput returns a String[]
        let edits = unsafe { JObjectArray::<JString>::from_raw(env, edits.into_raw()) };
        let mut out = Vec::new();
        for i in 0..edits.len(env)? {
            let s = edits.get_element(env, i)?;
            out.extend(ime_event(&s.try_to_string(env)?));
        }
        Ok(out)
    });
    r.unwrap_or_else(|e| {
        log::warn!("text input: {e}");
        Vec::new()
    })
}

/// One edit queued by `TextInput.java` (`P<text>`, `C<text>`, `D<before>,<after>`) as an egui
/// IME event.
pub fn ime_event(edit: &str) -> Option<egui::ImeEvent> {
    let mut chars = edit.chars();
    let kind = chars.next()?;
    let rest = chars.as_str();
    match kind {
        'P' => Some(egui::ImeEvent::Preedit { text: rest.to_string(), active_range_chars: None }),
        'C' => Some(egui::ImeEvent::Commit(rest.to_string())),
        'D' => {
            let (b, a) = rest.split_once(',')?;
            Some(egui::ImeEvent::DeleteSurrounding { before_chars: b.parse().ok()?, after_chars: a.parse().ok()? })
        }
        _ => None,
    }
}

/// The system's CJK font (Noto Sans CJK: Hangul, kana and Han), mapped read-only: its 30 MB stay
/// clean file pages the system can drop and reload, not app memory. `index` picks the region's
/// face (0 Japanese, 1 Korean, 2 Simplified Chinese, 3 Traditional Chinese).
pub fn system_cjk_font() -> Option<(&'static [u8], u32)> {
    let path = c"/system/fonts/NotoSansCJK-Regular.ttc";
    // SAFETY: plain libc calls on a NUL-terminated path; the mapping is checked, kept for the
    // process's life (the slice is 'static) and never written: the file is on the read-only
    // system partition, so it can't change under the mapping
    unsafe {
        let fd = libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC);
        if fd < 0 {
            return None;
        }
        let mut st: libc::stat = std::mem::zeroed();
        let len = if libc::fstat(fd, &mut st) == 0 { st.st_size as usize } else { 0 };
        let p = if len > 0 { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, fd, 0) } else { libc::MAP_FAILED };
        libc::close(fd);
        if p == libc::MAP_FAILED {
            return None;
        }
        Some((std::slice::from_raw_parts(p.cast::<u8>(), len), cjk_face(&system_locale())))
    }
}

/// The Noto Sans CJK face for `locale` (BCP 47, e.g. `ko-KR`): shared Han characters take that
/// region's forms. Japanese (Noto's default face) for everything else.
fn cjk_face(locale: &str) -> u32 {
    let l = locale.replace('_', "-");
    match l.split('-').next().unwrap_or("") {
        "ko" => 1,
        "zh" if ["Hant", "TW", "HK", "MO"].iter().any(|t| l.split('-').any(|p| p == *t)) => 3,
        "zh" => 2,
        _ => 0,
    }
}

/// The system's locale (`persist.sys.locale`, else the product's), e.g. `ko-KR`.
fn system_locale() -> String {
    let get = |name: &std::ffi::CStr| {
        let mut buf = [0 as std::ffi::c_char; 92]; // PROP_VALUE_MAX
        // SAFETY: bionic's property getter writes at most PROP_VALUE_MAX bytes, NUL-terminated
        let n = unsafe { libc::__system_property_get(name.as_ptr(), buf.as_mut_ptr()) };
        // SAFETY: NUL-terminated by the call above (or all zeros)
        (n > 0).then(|| unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
    };
    get(c"persist.sys.locale").or_else(|| get(c"ro.product.locale")).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn cjk_faces_follow_the_locale() {
        use super::cjk_face;
        assert_eq!(cjk_face("ko-KR"), 1);
        assert_eq!(cjk_face("zh-Hant-TW"), 3);
        assert_eq!(cjk_face("zh-TW"), 3);
        assert_eq!(cjk_face("zh-Hans-CN"), 2);
        assert_eq!(cjk_face("ja-JP"), 0);
        assert_eq!(cjk_face("en-US"), 0);
    }

    #[test]
    fn edits_become_ime_events() {
        use super::ime_event;
        use egui::ImeEvent;
        assert_eq!(ime_event("P한글"), Some(ImeEvent::Preedit { text: "한글".into(), active_range_chars: None }));
        assert_eq!(ime_event("Chello"), Some(ImeEvent::Commit("hello".into())));
        assert_eq!(ime_event("C"), Some(ImeEvent::Commit(String::new())));
        assert_eq!(ime_event("D2,1"), Some(ImeEvent::DeleteSurrounding { before_chars: 2, after_chars: 1 }));
        assert_eq!(ime_event("D2"), None);
        assert_eq!(ime_event(""), None);
    }
}
