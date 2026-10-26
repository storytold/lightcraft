//! A command the interface runs and the library refuses says why, where the person looks: a
//! toast. `LightcraftApp::act` is how the interface runs a command for someone's click, drop or
//! key; the reason is shown without each widget having to. `run` stays the raw entry (agents and
//! code that handles the answer itself get the error back and nothing is shown), and `quiet` is
//! for the few places where a refusal is expected and means nothing to the person.
//!
//! Scenarios:
//! - Given a command the library refuses (a smart album into the folder it shows), when the
//!   interface runs it for the person, then a toast gives the reason in words, without the
//!   command's id, and stays long enough to read.
//! - Given a command that isn't available right now (rating with nothing selected), then the
//!   toast says why.
//! - Given a command that goes through, then nothing is said that the command didn't say itself.
//! - Given the same refusals through `run` or `quiet`, then nothing is shown; `run` gives the
//!   error back in full and agents can still read it as the status.
//! - Given a menu item that is refused, when it is clicked (`menubar::act_item`), then the toast
//!   says why; `run_item` stays the raw form.
//! - Given a command nobody knows, then that is said too.
//! - Given an interface command that wraps an engine command (Edit In…, All Metadata), when the
//!   engine refuses, then the toast gives the engine's reason only, and `run` keeps the whole
//!   error.
//! - Given a reason that starts with a file's name or a path, then its spelling is kept; only a
//!   reason that starts with a word gets a capital.
//! - Given a dialog open, when a key behind it is refused, then nothing is said.
//! - Given a command that made a toast of its own (a partial import report) and is then refused,
//!   then the toast keeps what the command said and adds the reason after it.
//!
//! Real widgets, driven headlessly:
//! - Given a photo whose original is missing, when Edit ▸ Auto is clicked, then the toast is the
//!   reason and not "Auto settings applied"; on a photo that is there, it still says so.
//! - Given nothing selected, when a preset row is clicked, then the toast is the reason and not
//!   "Preset: <name>"; with a photo selected, it still says so.
//! - Given a slider, when it is dragged, then nothing is said; when the change is refused, on
//!   every frame of the drag, then the toast says why.
//! - Given nothing selected, when 3 is pressed, or Cmd+Z with nothing to undo, then the toast is
//!   the reason and not "Rated ★★★" / "Undo"; when they go through, they still say so.
//! - Given eight Point Color samples, when the picker is clicked on the photo, then the toast is the
//!   reason (at most 8) and nothing is added.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

/// An album placed in a folder before one that is not in it: refused. (Any engine refusal will do:
/// this is the reason the tests read back, not a feature of the folder views.)
fn refused_move(app: &mut LightcraftApp) -> serde_json::Value {
    let mut make = |params: serde_json::Value| app.session.execute("album.create", &params).unwrap()["id"].as_u64().unwrap();
    let trips = make(json!({"name": "Trips", "folder": true}));
    let (rome, paris) = (make(json!({"name": "Rome"})), make(json!({"name": "Paris"})));
    json!({"id": rome, "parent": trips, "before": paris})
}

fn toast(app: &LightcraftApp) -> Option<&str> {
    app.ui.toast.as_ref().map(|t| t.0.as_str())
}

#[test]
fn a_refused_command_says_why_in_a_toast() {
    let mut app = app();
    let params = refused_move(&mut app);
    assert_eq!(app.act("album.reorder", params), None);
    let said = toast(&app).expect("a toast");
    assert!(said.contains("not inside that folder"), "{said}");
    assert!(!said.contains("album.reorder") && !said.contains("invalid parameters"), "words for a person, not the command's id: {said}");
    let (_, until, _) = app.ui.toast.clone().unwrap();
    assert!(until - app.tick_now() >= 5.0, "long enough to read a sentence");
    // agents still find the full error as the status
    assert!(app.ui.status.contains("album.reorder"), "{}", app.ui.status);
}

#[test]
fn a_command_that_isnt_available_says_why() {
    let mut app = app();
    app.session.selection = Default::default();
    assert_eq!(app.act("photo.rate", json!({"rating": 3})), None);
    let said = toast(&app).expect("a toast");
    assert!(said.to_lowercase().contains("no photo"), "{said}");
    assert!(!said.contains("photo.rate"), "{said}");
}

#[test]
fn a_command_that_goes_through_adds_nothing() {
    let mut app = app();
    let made = app.act("album.create", json!({"name": "Made"}));
    assert!(made.is_some_and(|v| v["id"].is_u64()), "its result comes back");
    assert_eq!(toast(&app), None);
}

#[test]
fn run_and_quiet_show_nothing() {
    let mut app = app();
    let params = refused_move(&mut app);
    let e = app.run("album.reorder", params.clone()).unwrap_err();
    assert!(e.contains("album.reorder") && e.contains("not inside that folder"), "the caller gets the whole error: {e}");
    assert_eq!(toast(&app), None, "run: the caller decides what to show");
    assert_eq!(app.quiet("album.reorder", params), None);
    assert_eq!(toast(&app), None, "quiet: a refusal expected here");
    assert!(app.ui.status.contains("not inside that folder"), "both leave the status for agents");
}

/// A click on a menu item goes the same way, whatever the item (before, only an export that
/// couldn't start said so).
#[test]
fn a_refused_menu_item_says_why() {
    let mut app = app();
    let params = refused_move(&mut app);
    assert_eq!(crate::menubar::act_item(&mut app, "album.reorder", params.clone()), None);
    assert!(toast(&app).is_some_and(|t| t.contains("not inside that folder")), "{:?}", toast(&app));
    // `run_item` stays the raw form: the error back, nothing shown
    app.ui.toast = None;
    assert!(crate::menubar::run_item(&mut app, "album.reorder", params).is_err());
    assert_eq!(toast(&app), None);
    // an item that goes through answers as before
    assert!(crate::menubar::act_item(&mut app, "album.create", json!({"name": "From the menu"})).is_some());
    assert_eq!(toast(&app), None);
}

/// An interface command that wraps an engine command (Edit In, All Metadata…) hands on the
/// engine's error as text ("command `photo.editExternal` is not available right now: no photo
/// selected"): the person reads the reason only, agents the whole of it as before. Edit In
/// says "Couldn't open the editor: …" itself, on every way in, and the reason replaces the raw text in that.
#[test]
fn an_interface_command_that_wraps_an_engine_command_says_the_reason_only() {
    let mut app = app();
    app.session.selection = Default::default();
    for (id, wrapped, own_prefix) in
        [("photo.editInExternal", "photo.editExternal", Some("Couldn't open the editor: ")), ("dialog.allMetadata", "photo.allMetadata", None)]
    {
        app.ui.toast = None;
        let whole = app.run(id, json!({})).unwrap_err();
        assert!(whole.contains(&format!("`{wrapped}`")), "agents get the engine's error as it was: {whole}");
        if own_prefix.is_none() {
            assert_eq!(toast(&app), None);
        }
        app.ui.toast = None;
        assert_eq!(app.act(id, json!({})), None);
        let said = toast(&app).expect("a toast").to_string();
        assert!(!said.contains(wrapped) && !said.contains("not available right now") && !said.contains("invalid parameters"), "{said}");
        let reason = crate::engine_reason(&whole).expect("the engine's text");
        assert!(said.to_lowercase().ends_with(&reason.to_lowercase()), "the reason, capitalised: {said} / {whole}");
        assert!(said.chars().next().is_some_and(char::is_uppercase), "{said}");
        if let Some(prefix) = own_prefix {
            assert_eq!(said, format!("{prefix}{}", crate::sentence(reason)), "the command's own words with the reason in them, once");
        }
    }
}

/// A reason that starts with a file's name or a path keeps its spelling; one that starts with a
/// word gets its capital.
#[test]
fn a_reason_is_capitalised_only_when_it_starts_with_a_word() {
    use crate::sentence;
    assert_eq!(sentence("no photos selected"), "No photos selected");
    assert_eq!(sentence("nothing copied: use Copy Metadata first"), "Nothing copied: use Copy Metadata first");
    for kept in
        ["img_0012.jpg is a virtual copy", "photos/a.jpg: No such file", "ßtraße", "`x` must be a number", "“Trips” is a folder", "3 photos", ""]
    {
        assert_eq!(sentence(kept), kept);
    }
}

/// With a dialog open the keys are the dialog's and reach no command behind it, so nothing is said
/// about a refusal there; once the dialog is gone, the same key says why.
#[test]
fn a_refused_key_behind_a_dialog_says_nothing() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "photoGrid"}));
    h.app.session.selection = Default::default();
    h.app.run("dialog.export", json!({})).unwrap();
    h.step();
    assert!(h.app.ui.dialog.is_some());
    h.app.ui.toast = None;
    key(&mut h, "3", false);
    assert_eq!(toast(&h.app), None);
    h.app.ui.dialog = None;
    h.step();
    key(&mut h, "3", false);
    assert!(toast(&h.app).is_some_and(|t| t.to_lowercase().contains("no photo")), "{:?}", toast(&h.app));
}

#[test]
fn the_reason_is_read_out_of_an_engine_error_that_is_already_text() {
    use crate::{Refusal, engine_reason};
    assert_eq!(engine_reason("command `photo.editExternal` is not available right now: no photo selected"), Some("no photo selected"));
    assert_eq!(engine_reason("invalid parameters for `photo.allMetadata`: no photo"), Some("no photo"));
    // the engine's own formats, so a change there shows up here
    let disabled = lightcraft_engine::EngineError::Disabled("a.b".into(), "nothing to undo".into()).to_string();
    assert_eq!(engine_reason(&disabled), Some("nothing to undo"));
    let bad = lightcraft_engine::EngineError::BadParams { cmd: "a.b-c_d".into(), msg: "`x` must be a number: got `y`".into() }.to_string();
    assert_eq!(engine_reason(&bad), Some("`x` must be a number: got `y`"), "a reason with colons and backticks is kept whole");
    assert_eq!(
        engine_reason("invalid parameters for `album.move`: command `x.y` is not available right now: busy"),
        Some("command `x.y` is not available right now: busy"),
        "only the outer wrapping is taken off"
    );
    // text that only resembles one
    for other in [
        "command line tools are missing",
        "command `photo.rate` failed: disk full",
        "command `photo rate` is not available right now: x",
        "command `` is not available right now: x",
        "invalid parameters for the export: no folder",
        "invalid parameters for `x.y`",
        "invalid parameters for `x.y`: ",
        "invalid parameters for `a`b`: c",
        "the command `x.y` is not available right now: busy",
        "select a photo to use as the reference",
        "",
    ] {
        assert_eq!(engine_reason(other), None, "{other}");
    }
    // `plain`: the reason for the person, capitalised as an engine refusal's is; the error whole
    let whole = "command `photo.editExternal` is not available right now: no photo selected";
    let r = Refusal::plain(whole.into());
    assert_eq!((r.why.as_str(), r.error.as_str()), ("No photo selected", whole));
    // words that are the interface's own stay as written
    let r = Refusal::plain("select a photo to use as the reference".into());
    assert_eq!((r.why.as_str(), r.error.as_str()), ("select a photo to use as the reference", "select a photo to use as the reference"));
}

// ---- real widgets, driven headlessly ----

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

fn headless(session: lightcraft_engine::Session, ui: serde_json::Value) -> Headless {
    let app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", ui, T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

/// Photos whose originals are gone (the files never existed), the first one selected.
fn missing_originals() -> lightcraft_engine::Session {
    use lightcraft_catalog::{Op, Photo, PhotoId, Source};
    let mut session = lightcraft_engine::Session::new();
    for id in 1..=3 {
        let source = Source::File { path: format!("/lightcraft-refusals/{id}.jpg") };
        let photo = Photo::new(PhotoId(id), source, &format!("{id}.jpg"), "JPEG", 100, 100, "2026-10-08");
        session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
    }
    session.execute("library.select", &json!({"ids": [1]})).unwrap();
    session
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{r}; widgets: {:?}", h.app.widgets.iter().map(|(w, _)| w.as_str()).collect::<Vec<_>>());
    h.step();
}

fn key(h: &mut Headless, key: &str, cmd: bool) {
    let r = h.request("ui.key", json!({"key": key, "cmd": cmd}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// Edit ▸ Auto on a photo whose original is missing: nothing changed, and the toast used to read
/// "Auto settings applied" (the success was said whatever the command answered).
#[test]
fn the_auto_button_doesnt_claim_success_when_refused() {
    let mut h = headless(missing_originals(), json!({"view": "detail", "right": "edit"}));
    let before = h.app.session.develop_of(lightcraft_catalog::PhotoId(1));
    click(&mut h, "button:auto");
    let said = toast(&h.app).expect("a toast").to_string();
    assert!(!said.contains("Auto settings applied"), "{said}");
    assert!(h.app.ui.status.contains("develop.auto"), "the refusal it is about: {}", h.app.ui.status);
    assert_eq!(h.app.session.develop_of(lightcraft_catalog::PhotoId(1)), before, "nothing changed");
}

/// And where Auto goes through, it still says so.
#[test]
fn the_auto_button_says_so_when_it_went_through() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "detail", "right": "edit"}));
    click(&mut h, "button:auto");
    assert_eq!(toast(&h.app), Some("Auto settings applied"));
}

/// A preset row clicked with nothing selected: the toast used to read "Preset: <name>".
#[test]
fn a_preset_row_doesnt_claim_success_when_refused() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "detail", "right": "edit", "presets": true}));
    let row = |h: &Headless| h.app.widgets.iter().map(|(w, _)| w.clone()).find(|w| w.starts_with("preset:"));
    let id = row(&h).expect("a preset row");
    // with a photo selected the row says what it applied
    click(&mut h, &id);
    assert!(toast(&h.app).is_some_and(|t| t.starts_with("Preset: ")), "{:?}", toast(&h.app));
    // with none, why not
    h.app.ui.toast = None;
    h.app.session.selection = Default::default();
    h.step();
    let id = row(&h).expect("a preset row with nothing selected");
    click(&mut h, &id);
    let said = toast(&h.app).expect("a toast").to_string();
    assert!(!said.starts_with("Preset: "), "{said}");
    assert!(said.to_lowercase().contains("no photo"), "{said}");
}

/// A slider drag is begin, a change per frame, end: all three say why when refused, in the one
/// toast slot, and an ordinary drag says nothing.
#[test]
fn a_refused_slider_change_says_why_and_an_ordinary_drag_says_nothing() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "detail", "right": "edit"}));
    let id = h.app.session.active().expect("an active photo");
    let before = h.app.session.develop_of(id);
    let r = h.request("ui.dragWidget", json!({"id": "slider:light.exposure", "dx": 40.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert_ne!(h.app.session.develop_of(id), before, "the drag moved Exposure");
    assert_eq!(toast(&h.app), None, "an ordinary drag says nothing");
    // a change the library refuses, as a panel's slider passes it on, on every frame of a drag
    let spec = lightcraft_develop::controls::find("light.exposure").unwrap();
    for _ in 0..3 {
        let out = crate::widgets::SliderOut { value: Some(1.0), ..Default::default() };
        crate::panels::edit::apply_slider_out(&mut h.app, spec, out, |app, v| {
            app.act("develop.set", json!({"control": "no.suchControl", "value": v}))
        });
    }
    let said = toast(&h.app).expect("a toast");
    assert!(said.contains("no.suchControl") && !said.contains("develop.set"), "{said}");
}

/// A key is a click by another name: refused, it says why instead of the success it used to
/// claim ("Rated ★★★" with nothing selected, "Undo" with nothing to undo).
#[test]
fn a_refused_key_says_why_instead_of_claiming_success() {
    let mut h = headless(missing_originals(), json!({"view": "photoGrid"}));
    // goes through: its own success toast, as before
    key(&mut h, "3", false);
    assert_eq!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 3);
    assert_eq!(toast(&h.app), Some("Rated ★★★"));
    key(&mut h, "Z", true);
    assert_eq!(h.app.session.catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().rating, 0);
    assert_eq!(toast(&h.app), Some("Undo"));
    // nothing left to undo
    h.app.ui.toast = None;
    key(&mut h, "Z", true);
    let said = toast(&h.app).expect("a toast").to_string();
    assert!(said != "Undo" && said.to_lowercase().contains("nothing to undo"), "{said}");
    // nothing selected
    h.app.ui.toast = None;
    h.app.session.selection = Default::default();
    key(&mut h, "3", false);
    let said = toast(&h.app).expect("a toast").to_string();
    assert!(!said.contains("Rated") && said.to_lowercase().contains("no photo"), "{said}");
    assert!(h.app.session.catalog.photos().all(|p| p.rating == 0), "and nothing was rated");
}

/// A library whose disk takes nothing while `full` is set: every command that changes something fails to save.
#[derive(Clone, Default)]
struct FullDisk {
    files: lightcraft_catalog::MemStore,
    full: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl lightcraft_catalog::Store for FullDisk {
    fn read(&mut self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.files.read(name)
    }
    fn write_atomic(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.full.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.write_atomic(name, data)
    }
    fn append(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        if self.full.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(std::io::Error::other("disk full"));
        }
        self.files.append(name, data)
    }
    fn truncate(&mut self, name: &str, len: u64) -> std::io::Result<()> {
        self.files.truncate(name, len)
    }
    fn describe(&self) -> String {
        "full disk".into()
    }
}

/// The flag-stepping keys (`, ⌘↑, ⌘↓) name the new flag only when the step went through: one that failed (here: the
/// library could not be saved) keeps its reason on screen, not "Unflagged" or "Flagged as …".
#[test]
fn a_refused_flag_step_key_says_why_instead_of_naming_the_flag() {
    let disk = FullDisk::default();
    let mut session = lightcraft_engine::Session::new();
    let stores = lightcraft_engine::library::LibraryStores {
        dir: "full-disk".into(),
        catalog: Box::new(disk.clone()),
        files: Box::new(lightcraft_catalog::MemStore::new()),
        on_disk: false,
    };
    session.open_library_in(stores, true).unwrap();
    let mut h = headless(session, json!({"view": "photoGrid"}));
    assert!(h.app.session.active().is_some(), "a photo to flag");
    // goes through: the new flag is named
    key(&mut h, "`", false);
    assert_eq!(toast(&h.app), Some("Flagged as Pick"));
    // (the first failed save also brings the library's own "not written to disk" notice; the keys after it are on
    // their own)
    disk.full.store(true, std::sync::atomic::Ordering::SeqCst);
    let flag = |h: &Headless| h.app.session.active().and_then(|id| h.app.session.catalog.photo(id)).map(|p| p.flag);
    for (k, cmd) in [("`", false), ("ArrowUp", true), ("ArrowDown", true)] {
        h.app.ui.toast = None;
        let before = flag(&h);
        key(&mut h, k, cmd);
        assert_ne!(flag(&h), before, "{k} reached the command (the change stays in memory)");
        let said = toast(&h.app).expect("a toast").to_string();
        assert!(said.contains("not written to disk"), "{k}: {said}");
    }
}

#[test]
fn an_unknown_command_is_said_too() {
    let mut app = app();
    assert_eq!(app.act("no.suchCommand", json!({})), None);
    assert!(toast(&app).is_some_and(|t| t.contains("no.suchCommand")), "{:?}", toast(&app));
}

/// `[` / `]` on a spot that is gone: the command gives the refusal back (and, run for a key, the
/// toast says why once) and doesn't answer with the new size as if it had changed the spot.
#[test]
fn a_brush_size_key_on_a_vanished_spot_is_refused() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "detail", "right": "remove"}));
    h.app.session.active_spot = Some(7);
    let r = h.app.run("brush.larger", json!({}));
    assert!(r.is_err(), "{r:?}");
    h.app.session.active_spot = None;
    assert!(h.app.run("brush.larger", json!({})).is_ok(), "with no spot it only sizes the brush");
}

/// File ▸ Import Profiles & Presets… with a `.cube` and a local-adjustment preset, when the
/// library's preferences can't be saved: the import's own toast (what happened to the profiles)
/// and the refusal's reason are both shown, not only the reason.
#[test]
fn a_refusal_keeps_the_toast_the_command_made() {
    let dir = std::env::temp_dir().join(format!("lc-refusal-toast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("library");
    // a prefs.json that can't be read blocks saving the preferences for the session
    std::fs::create_dir_all(lib.join("prefs.json")).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(&lib, true).unwrap();
    let mut app = LightcraftApp::new(session, Services { png: None, ..Default::default() });
    let cube = dir.join("Warm.cube");
    let mut text = String::from("TITLE \"Warm\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                text += &format!("{r} {g} {b}\n");
            }
        }
    }
    std::fs::write(&cube, text).unwrap();
    let effect = dir.join("Soft.lceffect");
    std::fs::write(&effect, json!({"effects": [{"name": "Soft"}]}).to_string()).unwrap();
    let paths = [cube.to_string_lossy().to_string(), effect.to_string_lossy().to_string()];
    app.ui.toast = None;
    assert_eq!(app.act("file.importPresets", json!({"paths": paths})), None, "the preset half is refused");
    let said = toast(&app).expect("a toast").to_string();
    assert!(said.starts_with("Import failed: ") && said.contains("prefs.json couldn't be read"), "what the import said is kept: {said}");
    assert_eq!(said.matches("couldn't be read").count(), 1, "the reason is not said twice: {said}");
    // the same refusal again while its toast is still up: the command's new toast is kept too
    assert_eq!(app.act("file.importPresets", json!({"paths": paths})), None);
    let again = toast(&app).expect("a toast");
    assert!(again.starts_with("Import failed: "), "{again}");
}

fn refusal(error: &str, why: &str) -> crate::Refusal {
    crate::Refusal { error: error.into(), why: why.into(), kind: lightcraft_engine::ErrorKind::Other }
}

/// The command's toast comes first, the reason after it; a toast that is the raw error, or
/// already says the reason (in either capitalisation), becomes the reason once.
#[test]
fn a_refused_commands_own_toast_and_the_reason_are_joined_once() {
    let r = refusal("command `x.y` is not available right now: no photo selected", "No photo selected");
    assert_eq!(crate::refusal_toast(None, &r), "No photo selected");
    assert_eq!(crate::refusal_toast(Some("Imported 2 profiles".into()), &r), "Imported 2 profiles · No photo selected");
    assert_eq!(crate::refusal_toast(Some(r.error.clone()), &r), "No photo selected");
    assert_eq!(crate::refusal_toast(Some("Failed: no photo selected".into()), &r), "Failed: no photo selected");
}

/// The Point Color picker, clicked on the photo after eight samples exist: the library refuses a
/// ninth, and the click used to drop that without a word.
#[test]
fn a_refused_point_color_pick_says_why() {
    let mut h = headless(lightcraft_engine::Session::with_demo(), json!({"view": "detail", "right": "edit", "tool": "pointColor"}));
    for i in 0..8 {
        let r = h.app.session.execute("pointColor.pick", &json!({"x": 0.3 + 0.05 * f64::from(i), "y": 0.5}));
        assert!(r.is_ok(), "sample {i}: {r:?}");
    }
    let r = h.request("ui.click", json!({"x": 500.0, "y": 450.0}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let said = toast(&h.app).expect("a toast");
    assert!(said.to_lowercase().contains("at most 8"), "{said}");
    assert!(!said.contains("pointColor.pick"), "{said}");
}
