//! Count actual prefs.json writes caused by a real Settings text-field blur.
use crate::{LightcraftApp, Services, headless::Headless};
use lightcraft_catalog::{MemStore, Store};
use lightcraft_engine::{Session, library::LibraryStores};
use serde_json::{Value, json};
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
const T: Duration = Duration::from_secs(20);
struct CountPrefs {
    inner: MemStore,
    writes: Arc<AtomicUsize>,
}
impl Store for CountPrefs {
    fn read(&mut self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.inner.write_atomic(name, data)?;
        if name == "prefs.json" {
            self.writes.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    fn append(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        self.inner.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> io::Result<()> {
        self.inner.truncate(name, len)
    }
    fn describe(&self) -> String {
        "counted Settings preferences".into()
    }
}
fn ask(h: &mut Headless, method: &str, params: Value) {
    let r = h.request(method, params, T);
    assert_eq!(r["ok"], true, "{method}: {r}");
    for _ in 0..4 {
        h.step();
    }
}
#[test]
fn tab_blur_of_real_copyright_writes_preferences_exactly_once() {
    let writes = Arc::new(AtomicUsize::new(0));
    let files = MemStore::new();
    let mut session = Session::new();
    session
        .open_library_in(
            LibraryStores {
                dir: "settings-blur-count".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(CountPrefs { inner: files.clone(), writes: Arc::clone(&writes) }),
                on_disk: false,
            },
            false,
        )
        .unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1400.0, 1100.0], 1.0);
    assert!(h.settle(T));
    ask(&mut h, "engine.execute", json!({"command":"app.settings","params":{"tab":"import"}}));
    let before = writes.load(Ordering::SeqCst);
    ask(&mut h, "ui.clickWidget", json!({"id":"field:settings.copyright"}));
    ask(&mut h, "ui.text", json!({"text":"Exactly Once Blur"}));
    assert_eq!(writes.load(Ordering::SeqCst), before, "typing does not persist the draft");
    ask(&mut h, "ui.key", json!({"key":"Tab"}));
    assert_eq!(h.app.session.import_defaults.copyright, "Exactly Once Blur");
    assert_eq!(writes.load(Ordering::SeqCst), before + 1, "one native Away commit writes prefs exactly once");
    for _ in 0..12 {
        h.step();
    }
    ask(&mut h, "ui.key", json!({"key":"Tab","shift":true}));
    ask(&mut h, "ui.key", json!({"key":"Escape"}));
    assert_eq!(writes.load(Ordering::SeqCst), before + 1, "settled frames and unchanged field departure do not repeat the commit");
    let bytes = files.files.lock().unwrap().get("prefs.json").cloned().unwrap();
    let saved: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(saved["import"]["copyright"], "Exactly Once Blur");
    eprintln!("SETTINGS_BLUR_PREFS_WRITES_DELTA={}", writes.load(Ordering::SeqCst) - before);
}
