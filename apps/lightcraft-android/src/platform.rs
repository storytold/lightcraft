//! The Android ABI boundary. All application logic is safe Rust.
use android_activity::AndroidApp;
use jni::{
    JavaVM,
    objects::{GlobalRef, JObject, JString, JValue},
};
use std::sync::Arc;

pub struct Bridge {
    vm: JavaVM,
    activity: GlobalRef,
    // Keeps the AndroidApp and its VM/activity references alive across worker calls.
    _app: AndroidApp,
}

impl Bridge {
    pub fn new(app: AndroidApp) -> Result<Arc<Self>, String> {
        // SAFETY: AndroidApp provides the live process JavaVM; retaining app and a
        // global activity reference keeps both valid for every attached worker.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }.map_err(|e| e.to_string())?;
        let activity = {
            let env = vm.attach_current_thread().map_err(|e| e.to_string())?;
            // SAFETY: this is AndroidApp's live global Activity reference. JObject
            // does not delete it; new_global_ref creates our own owned reference.
            let borrowed = unsafe { JObject::from_raw(app.activity_as_ptr().cast()) };
            env.new_global_ref(borrowed).map_err(|e| e.to_string())?
        };
        Ok(Arc::new(Self { vm, activity, _app: app }))
    }

    /// Java always returns a JSON envelope; exceptions are translated and cleared.
    pub fn call(&self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut env = self.vm.attach_current_thread().map_err(|e| e.to_string())?;
        // A render thread can already be permanently attached. A scoped JNI local
        // frame avoids accumulating two references on every event-poll frame.
        let text = env
            .with_local_frame(8, |env| -> jni::errors::Result<String> {
                let arg = env.new_string(request.to_string())?;
                let object =
                    env.call_method(self.activity.as_obj(), "bridge", "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&arg)])?.l()?;
                let result = JString::from(object);
                Ok(env.get_string(&result)?.into())
            })
            .map_err(|e| {
                let _ = env.exception_clear();
                format!("Android bridge: {e}")
            })?;
        let reply: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if let Some(error) = reply.get("error").and_then(|v| v.as_str()) {
            return Err(error.into());
        }
        Ok(reply)
    }

    pub fn read(&self, uri: &str) -> Result<Vec<u8>, String> {
        super::storage::validate_uri(uri)?;
        let reply = self.call(serde_json::json!({"op":"read", "uri":uri}))?;
        let path = reply.get("path").and_then(|v| v.as_str()).ok_or("Android returned no source file")?;
        let result = std::fs::read(path).map_err(|e| e.to_string());
        let _ = std::fs::remove_file(path);
        result
    }
}

// SAFETY: android-activity resolves this exact ABI symbol, with a valid AndroidApp.
// The symbol is defined once in this cdylib; unwinding never crosses the boundary.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    let error_path = app.internal_data_path().map(|p| p.join("startup-error.txt"));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::app::run(app)));
    let error = match result {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(e),
        Err(_) => Some("Unexpected panic during Android startup or rendering".into()),
    };
    if let Some(error) = error {
        if let Some(path) = error_path {
            let _ = std::fs::write(path, &error);
        }
        log::error!("LightCraft startup failed: {error}");
    }
}
