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

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

fn app() -> LightcraftApp {
    LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() })
}

/// A folder and a smart album that shows it: moving the album into the folder is refused.
fn refused_move(app: &mut LightcraftApp) -> serde_json::Value {
    let trips = app.session.execute("album.create", &json!({"name": "Trips", "folder": true})).unwrap()["id"].as_u64().unwrap();
    let view = app.session.execute("album.createSmart", &json!({"name": "Trips view", "rules": {"album": trips}})).unwrap()["id"].as_u64().unwrap();
    json!({"id": view, "parent": trips})
}

fn toast(app: &LightcraftApp) -> Option<&str> {
    app.ui.toast.as_ref().map(|t| t.0.as_str())
}

#[test]
fn a_refused_command_says_why_in_a_toast() {
    let mut app = app();
    let params = refused_move(&mut app);
    assert_eq!(app.act("album.move", params), None);
    let said = toast(&app).expect("a toast");
    assert!(said.contains("Trips view") && said.contains("include itself"), "{said}");
    assert!(!said.contains("album.move") && !said.contains("invalid parameters"), "words for a person, not the command's id: {said}");
    let (_, until, _) = app.ui.toast.clone().unwrap();
    assert!(until - app.last_time >= 5.0, "long enough to read a sentence");
    // agents still find the full error as the status
    assert!(app.ui.status.contains("album.move"), "{}", app.ui.status);
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
    let e = app.run("album.move", params.clone()).unwrap_err();
    assert!(e.contains("album.move") && e.contains("include itself"), "the caller gets the whole error: {e}");
    assert_eq!(toast(&app), None, "run: the caller decides what to show");
    assert_eq!(app.quiet("album.move", params), None);
    assert_eq!(toast(&app), None, "quiet: a refusal expected here");
    assert!(app.ui.status.contains("include itself"), "both leave the status for agents");
}

/// A click on a menu item goes the same way, whatever the item (before, only an export that
/// couldn't start said so).
#[test]
fn a_refused_menu_item_says_why() {
    let mut app = app();
    let params = refused_move(&mut app);
    assert_eq!(crate::menubar::act_item(&mut app, "album.move", params.clone()), None);
    assert!(toast(&app).is_some_and(|t| t.contains("include itself")), "{:?}", toast(&app));
    // `run_item` stays the raw form: the error back, nothing shown
    app.ui.toast = None;
    assert!(crate::menubar::run_item(&mut app, "album.move", params).is_err());
    assert_eq!(toast(&app), None);
    // an item that goes through answers as before
    assert!(crate::menubar::act_item(&mut app, "album.create", json!({"name": "From the menu"})).is_some());
    assert_eq!(toast(&app), None);
}

/// An interface command that wraps an engine command (Edit In, All Metadata…) hands on the
/// engine's error as text ("command `photo.editExternal` is not available right now: no photo
/// selected"): the person reads the reason only, agents the whole of it as before.
#[test]
fn an_interface_command_that_wraps_an_engine_command_says_the_reason_only() {
    let mut app = app();
    app.session.selection = Default::default();
    for (id, wrapped) in [("photo.editInExternal", "photo.editExternal"), ("dialog.allMetadata", "photo.allMetadata")] {
        app.ui.toast = None;
        let whole = app.run(id, json!({})).unwrap_err();
        assert!(whole.contains(&format!("`{wrapped}`")), "agents get the engine's error as it was: {whole}");
        assert_eq!(toast(&app), None);
        assert_eq!(app.act(id, json!({})), None);
        let said = toast(&app).expect("a toast").to_string();
        assert!(!said.contains(wrapped) && !said.contains("not available right now") && !said.contains("invalid parameters"), "{said}");
        assert!(whole.to_lowercase().ends_with(&said.to_lowercase()), "the reason, capitalised: {said} / {whole}");
        assert!(said.chars().next().is_some_and(char::is_uppercase), "{said}");
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

/// With a dialog open the keys behind it still run their commands, but nobody asked: a refused
/// one says nothing there, and says why once the dialog is gone.
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

#[test]
fn an_unknown_command_is_said_too() {
    let mut app = app();
    assert_eq!(app.act("no.suchCommand", json!({})), None);
    assert!(toast(&app).is_some_and(|t| t.contains("no.suchCommand")), "{:?}", toast(&app));
}
