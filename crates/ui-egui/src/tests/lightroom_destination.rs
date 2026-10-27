//! Issue #2672: before File ▸ Import Lightroom Catalog… starts, a window asks whether the catalog
//! merges into the open library or goes into a new library (named after the catalog, next to the
//! open one), unless Settings (or the window's "Don't ask again") says to always merge.
//!
//! The catalogs here are fake (`.lrcat` files that aren't SQLite): what is checked is where the
//! import is started, which library is open when it starts, and that the read then fails cleanly.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::headless::Headless;
use crate::state::{Dialog, LightroomImport};
use crate::{LightcraftApp, Services, UiState};

const T: Duration = Duration::from_secs(20);

/// A folder of its own per test: `Library` (a LightCraft library, demo photos in it unless
/// `empty`) and `Lightroom/Travel.lrcat` (a fake catalog) side by side.
struct Fixture {
    /// The test's own folder.
    root: PathBuf,
    /// `root/Library`.
    lib: PathBuf,
    /// `root/Lightroom/Travel.lrcat`.
    lrcat: PathBuf,
}

impl Fixture {
    /// Make the folders and the fake catalog (an earlier run's leftovers removed).
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("lc-ui-lrdest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let lr = root.join("Lightroom");
        std::fs::create_dir_all(&lr).unwrap();
        let lrcat = lr.join("Travel.lrcat");
        std::fs::write(&lrcat, b"not really SQLite").unwrap();
        Fixture { lib: root.join("Library"), root, lrcat }
    }

    /// The desktop app on `Library`, with a folder dialog (for Choose…) and screenshots.
    fn app(&self, empty: bool) -> Headless {
        let mut session = lightcraft_engine::Session::new().with_fs();
        session.open_library(&self.lib, !empty).unwrap();
        let png = |img: &lightcraft_raster::Rgba8| {
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
        };
        // (a folder dialog for the window's Choose…, and what a screenshot needs)
        let services = Services {
            pick_folder: Some(Box::new(|| None)),
            png: Some(Box::new(png)),
            write: Some(Box::new(|p: &str, b: &[u8]| std::fs::write(p, b).map_err(|e| e.to_string()))),
            ..Default::default()
        };
        let mut h = Headless::new(LightcraftApp::new(session, services), [1200.0, 800.0], 1.0);
        h.step();
        h
    }

    /// The catalog's path as text.
    fn lrcat(&self) -> String {
        self.lrcat.to_string_lossy().into_owned()
    }
}

impl Drop for Fixture {
    /// Remove the folders.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// The widget `id` was drawn last frame.
fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Click the widget `id` through the control channel, then draw a frame.
fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
    h.step();
}

/// The open library's folder.
fn open_dir(h: &Headless) -> Option<PathBuf> {
    h.app.session.library.as_ref().map(|l| l.dir.clone())
}

/// The window's state: (new library chosen, name, location, don't ask).
fn destination(h: &Headless) -> Option<(bool, String, String, bool)> {
    match &h.app.ui.dialog {
        Some(Dialog::LightroomDestination { new_library, name, location, dont_ask, .. }) => {
            Some((*new_library, name.clone(), location.clone(), *dont_ask))
        }
        _ => None,
    }
}

/// Let the import of the fake catalog end (it fails to read) and return what it reported.
fn import_ends(h: &mut Headless, lrcat: &Path) -> Value {
    assert!(h.step_until(T, |h| h.app.lightroom.is_none()), "the import never ended");
    let last = h.app.lightroom_last.clone().unwrap_or_default();
    assert_eq!(last["path"], json!(lrcat), "the import was of this catalog: {last}");
    last
}

/// A headless screenshot into `$LIGHTCRAFT_TEST_SHOTS`, when it is set.
fn shot(h: &mut Headless, name: &str) {
    if let Some(p) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") {
        let r = h.request("ui.screenshot", json!({"path": Path::new(&p).join(name).to_string_lossy(), "headless": true}), T);
        assert_eq!(r["ok"], true, "{r}");
    }
}

/// The window appears before anything starts, with its choices; Merge imports into the open library.
#[test]
fn the_window_asks_and_merge_imports_into_the_open_library() {
    let f = Fixture::new("merge");
    let mut h = f.app(false);
    let photos = h.app.session.catalog.len();
    assert!(photos > 0);

    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    assert_eq!(r, json!({"asking": true, "path": f.lrcat(), "into": "new"}));
    h.step();
    h.step();
    // a library with photos: the window opens on a new library, named after the catalog, next to the open one
    assert_eq!(destination(&h), Some((true, "Travel".into(), f.root.to_string_lossy().into_owned(), false)));
    for id in [
        "radio:lightroomDestination-open",
        "radio:lightroomDestination-new",
        "field:lightroomLibraryName",
        "field:lightroomLibraryLocation",
        "button:lightroomLibraryChoose",
        "check:lightroomDontAsk",
        "button:dialogOk",
        "button:dialogCancel",
    ] {
        assert!(has(&h, id), "{id} is missing");
    }
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none(), "nothing starts before the answer");
    shot(&mut h, "lightroom-destination-2672.png");

    // Merge into the open library
    click(&mut h, "radio:lightroomDestination-open");
    assert_eq!(destination(&h).map(|d| d.0), Some(false));
    click(&mut h, "button:dialogOk");
    assert!(destination(&h).is_none());
    assert!(h.app.lightroom.is_some() || h.app.lightroom_last.is_some(), "the import started");
    import_ends(&mut h, &f.lrcat);
    assert_eq!(open_dir(&h), Some(f.lib.clone()), "into the library open before");
    assert_eq!(h.app.session.catalog.len(), photos);
    assert!(!f.root.join("Travel").exists(), "no library was made");
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask, "it asks again next time");
}

/// Create a new library: it is made next to the open one, named after the catalog, and opened
/// before the import starts there.
#[test]
fn create_a_new_library_opens_it_before_the_import_starts() {
    let f = Fixture::new("new");
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    h.step();
    assert_eq!(destination(&h).map(|d| d.0), Some(true));
    // ui.dialog.confirm answers what the window's button does
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], true, "{r}");
    let new = f.root.join("Travel");
    let result = &r["result"];
    assert_eq!(result["library"]["path"], json!(new.to_string_lossy()), "{r}");
    assert_eq!(result["library"]["name"], "Travel", "{r}");
    assert_eq!(result["path"], json!(f.lrcat()), "{r}");
    // the new library is the open one, and it was when the import started (the import's
    // snapshot is taken in the same call, after the switch)
    assert_eq!(open_dir(&h), Some(new.clone()));
    assert!(lightcraft_engine::library::is_library(&new));
    assert_eq!(h.app.ui.settings.library_path, new.to_string_lossy(), "opened at the next launch");
    assert!(h.app.ui.settings.recent_libraries.contains(&f.lib.to_string_lossy().into_owned()), "the old one is a recent library");
    import_ends(&mut h, &f.lrcat);
    assert_eq!(open_dir(&h), Some(new));
}

/// Cancel: no import, no library made, the open library stays.
#[test]
fn cancel_does_nothing() {
    let f = Fixture::new("cancel");
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    h.step();
    assert!(destination(&h).is_some());
    click(&mut h, "button:dialogCancel");
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    assert_eq!(open_dir(&h), Some(f.lib.clone()));
    assert!(!f.root.join("Travel").exists());
}

/// An empty library has nothing to merge with: the window opens on Merge.
#[test]
fn an_empty_library_opens_the_window_on_merge() {
    let f = Fixture::new("empty");
    let mut h = f.app(true);
    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    assert_eq!(r["into"], "open", "{r}");
    h.step();
    assert!(matches!(&h.app.ui.dialog, Some(Dialog::LightroomDestination { new_library: false, photos: 0, .. })), "{:?}", h.app.ui.dialog);
    // (the window in French, which translates it: the language is this test thread's own)
    if std::env::var_os("LIGHTCRAFT_TEST_SHOTS").is_some() {
        h.app.run("app.language.french", json!({})).unwrap();
        h.step();
        h.step();
        shot(&mut h, "lightroom-destination-empty-fr-2672.png");
        h.app.run("app.language.english", json!({})).unwrap();
    }
}

/// "Always merge into the open library" imports without the window.
#[test]
fn always_merge_skips_the_window() {
    let f = Fixture::new("always");
    let mut h = f.app(false);
    h.app.ui.settings.lightroom_import = LightroomImport::AlwaysMerge;
    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    h.step();
    assert!(destination(&h).is_none());
    import_ends(&mut h, &f.lrcat);
    assert_eq!(open_dir(&h), Some(f.lib.clone()));
}

/// "Don't ask again" (with Merge only) sets "Always merge", which is saved in `ui.json`.
#[test]
fn dont_ask_again_merges_from_then_on_and_is_saved() {
    let f = Fixture::new("dontask");
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    h.step();
    // greyed out while a new library is chosen: it only speaks of merging
    click(&mut h, "check:lightroomDontAsk");
    assert_eq!(destination(&h).map(|d| d.3), Some(false));
    click(&mut h, "radio:lightroomDestination-open");
    click(&mut h, "check:lightroomDontAsk");
    assert_eq!(destination(&h).map(|d| d.3), Some(true));
    click(&mut h, "button:dialogOk");
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::AlwaysMerge);
    import_ends(&mut h, &f.lrcat);
    // saved with the UI state (ui.json) and read back
    let saved = serde_json::to_string(&h.app.ui).unwrap();
    assert!(saved.contains(r#""lightroomImport":"alwaysMerge""#), "{saved}");
    let back: UiState = serde_json::from_str(&saved).unwrap();
    assert_eq!(back.settings.lightroom_import, LightroomImport::AlwaysMerge);
    // the next import doesn't ask
    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    assert!(destination(&h).is_none());
    import_ends(&mut h, &f.lrcat);
}

/// Settings written before the setting existed, or hand-edited, ask (the default).
#[test]
fn old_and_odd_settings_files_ask() {
    let old: UiState = serde_json::from_str(r#"{"settings": {"startupView": "detail"}}"#).unwrap();
    assert_eq!(old.settings.lightroom_import, LightroomImport::Ask);
    let odd: UiState = serde_json::from_str(r#"{"settings": {"lightroomImport": "sometimes", "startupView": "detail"}}"#).unwrap();
    assert_eq!(odd.settings.lightroom_import, LightroomImport::Ask);
    assert_eq!(odd.settings.startup_view, crate::state::StartupView::Detail, "the rest of the file still loads");
}

/// Settings ▸ General ▸ When importing a Lightroom catalog switches between the two.
#[test]
fn settings_general_switches_the_question_off_and_on() {
    let f = Fixture::new("settings");
    let mut h = f.app(false);
    h.app.run("app.settings", json!({})).unwrap();
    h.step();
    h.step();
    click(&mut h, "button:settingsLightroomImport-1");
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::AlwaysMerge);
    shot(&mut h, "settings-lightroom-import-2672.png");
    click(&mut h, "button:settingsLightroomImport-0");
    assert_eq!(h.app.ui.settings.lightroom_import, LightroomImport::Ask);
}

/// A new library's folder must be new or empty (File ▸ New Library…'s rule): the window says so
/// and stays open to pick another name; nothing is imported and the library stays.
#[test]
fn a_folder_that_isnt_empty_is_refused_and_the_window_stays() {
    let f = Fixture::new("notempty");
    std::fs::create_dir_all(f.root.join("Travel")).unwrap();
    std::fs::write(f.root.join("Travel").join("notes.txt"), b"mine").unwrap();
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    h.step();
    // the free name is offered…
    assert_eq!(destination(&h).map(|d| d.1), Some("Travel 2".to_string()));
    // …but the user types the taken one
    if let Some(Dialog::LightroomDestination { name, .. }) = h.app.ui.dialog.as_mut() {
        *name = "Travel".into();
    }
    click(&mut h, "button:dialogOk");
    assert!(destination(&h).is_some(), "the window stays to be corrected");
    let toast = h.app.ui.toast.clone().map(|t| t.0).unwrap_or_default();
    assert!(toast.contains("isn't empty"), "{toast}");
    assert_eq!(open_dir(&h), Some(f.lib.clone()));
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    // the same through the control channel
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["ok"], false, "{r}");
    assert!(destination(&h).is_some());
}

/// `into` acts without asking (agents, scripts); bad params are refused before anything happens.
#[test]
fn into_acts_without_the_window() {
    let f = Fixture::new("into");
    let mut h = f.app(false);
    for bad in [json!({"path": f.lrcat(), "into": "elsewhere"}), json!({"path": f.lrcat(), "into": "new", "location": "relative/dir"})] {
        let err = h.app.run("file.importLightroom", bad.clone()).unwrap_err();
        assert!(err.contains("file.importLightroom"), "{bad}: {err}");
    }
    // a catalog that isn't there doesn't leave an empty library behind
    let missing = f.root.join("Gone.lrcat");
    let err = h.app.run("file.importLightroom", json!({"path": missing.to_string_lossy(), "into": "new"})).unwrap_err();
    assert!(err.contains("no Lightroom catalog"), "{err}");
    assert!(!f.root.join("Gone").exists());
    assert!(destination(&h).is_none() && h.app.lightroom_last.is_none());

    let elsewhere = f.root.join("Libraries");
    let r = h
        .app
        .run("file.importLightroom", json!({"path": f.lrcat(), "into": "new", "name": "Trips", "location": elsewhere.to_string_lossy()}))
        .unwrap();
    assert_eq!(r["library"]["name"], "Trips", "{r}");
    assert_eq!(open_dir(&h), Some(elsewhere.join("Trips")));
    assert!(destination(&h).is_none());
    import_ends(&mut h, &f.lrcat);

    // a new library while another import runs: refused before anything is made
    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat(), "into": "open"})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    let err = h.app.run("file.importLightroom", json!({"path": f.lrcat(), "into": "new", "name": "Again"})).unwrap_err();
    assert!(err.contains("already running"), "{err}");
    assert!(!elsewhere.join("Again").exists());
    // and the window isn't shown for an answer that couldn't be carried out
    assert!(h.app.run("file.importLightroom", json!({"path": f.lrcat()})).is_err());
    assert!(destination(&h).is_none());
    import_ends(&mut h, &f.lrcat);
    assert_eq!(open_dir(&h), Some(elsewhere.join("Trips")));
}

/// A session in memory (no library on disk) has nothing to put a new library next to: it merges
/// without asking, as before.
#[test]
fn a_library_in_memory_merges_without_asking() {
    let f = Fixture::new("memory");
    let mut h = Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1200.0, 800.0], 1.0);
    h.step();
    let r = h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    assert_eq!(r["running"], true, "{r}");
    assert!(destination(&h).is_none());
    import_ends(&mut h, &f.lrcat);
}

/// Another library opened while the window was up (an agent, the control channel): the window
/// doesn't merge into it.
#[test]
fn merge_is_refused_when_another_library_was_opened_meanwhile() {
    let f = Fixture::new("stale");
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    click(&mut h, "radio:lightroomDestination-open");
    let other = f.root.join("Other");
    let dialog = h.app.ui.dialog.take();
    h.app.run("app.openLibrary", json!({"path": other.to_string_lossy()})).unwrap();
    let stale = dialog.clone();
    h.app.ui.dialog = dialog;
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "the window closes: the library it named is gone");
    // confirm's own error, not the last toast: another notice (the library switch's, on macOS)
    // can replace the toast before it is read
    let stale = stale.unwrap();
    let err = crate::lightroom_destination::confirm(&mut h.app, &stale).unwrap_err();
    assert!(err.contains("nothing was imported"), "{err}");
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    assert_eq!(open_dir(&h), Some(other));
}

/// The same for *Create a new library* (CodeRabbit on #2685): a window left over from before a
/// library switch makes no library and imports nothing.
#[test]
fn a_new_library_is_refused_when_another_library_was_opened_meanwhile() {
    let f = Fixture::new("stale-new");
    let mut h = f.app(false);
    h.app.run("file.importLightroom", json!({"path": f.lrcat()})).unwrap();
    h.step();
    click(&mut h, "radio:lightroomDestination-new");
    let other = f.root.join("Other");
    let dialog = h.app.ui.dialog.take();
    let target = match &dialog {
        Some(crate::state::Dialog::LightroomDestination { name, location, .. }) => std::path::Path::new(location).join(name),
        _ => panic!("the destination window is up"),
    };
    h.app.run("app.openLibrary", json!({"path": other.to_string_lossy()})).unwrap();
    let stale = dialog.clone();
    h.app.ui.dialog = dialog;
    h.step();
    click(&mut h, "button:dialogOk");
    assert!(h.app.ui.dialog.is_none(), "the window closes: the library it named is gone");
    // confirm's own error, not the last toast: another notice (the library switch's, on macOS)
    // can replace the toast before it is read
    let stale = stale.unwrap();
    let err = crate::lightroom_destination::confirm(&mut h.app, &stale).unwrap_err();
    assert!(err.contains("nothing was imported"), "{err}");
    assert!(h.app.lightroom.is_none() && h.app.lightroom_last.is_none());
    assert_eq!(open_dir(&h), Some(other));
    assert!(!target.exists(), "no library was made at {}", target.display());
}
