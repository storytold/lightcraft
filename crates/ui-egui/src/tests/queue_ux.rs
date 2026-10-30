//! The waiting line for imports and folder syncs, as the person sees it: every row says which folder it is, the
//! import and Synchronize dialogs say they will wait, a drop that is refused says why, and what the line does with its
//! tasks is told, never silent.

use std::time::Duration;

use lightcraft_engine::activity::Cancel;
use lightcraft_engine::import::ImportCandidate;
use serde_json::json;

use crate::headless::Headless;
use crate::import::ImportDialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(60);

fn app() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.settle(T);
    h
}

/// A review of `n` photos from `folder`, as the scan hands it over.
fn review(folder: &str, n: usize) -> ImportDialog {
    let candidates = (0..n)
        .map(|i| ImportCandidate {
            path: format!("{folder}/IMG_{i:03}.jpg"),
            name: format!("IMG_{i:03}.jpg"),
            format: "JPEG".into(),
            ..Default::default()
        })
        .collect();
    let mut d = ImportDialog::new(candidates);
    d.sources = vec![folder.to_string()];
    d
}

fn sync_dialog(name: &str) -> crate::state::Dialog {
    crate::state::Dialog::SynchronizeFolder {
        path: format!("/photos/{name}"),
        name: name.into(),
        disk: false,
        counts: Some(crate::state::SyncCounts { new: 1, ..Default::default() }),
        import_new: true,
        relink_moved: true,
        remove_missing: false,
        read_metadata: false,
    }
}

fn says(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t == text)
}

fn open_review(h: &mut Headless, d: ImportDialog) {
    h.app.ui.dialog = Some(crate::state::Dialog::Import { opts: Box::new(d) });
    for _ in 0..4 {
        h.step();
    }
}

/// Given an import running, when reviews of different folders are confirmed and a folder is dropped, then each waiting
/// task says which folder it is (several sources are counted rather than named) and what else is known: how many
/// photos, and how they come in.
#[test]
fn waiting_imports_say_which_folder_they_are() {
    let mut h = app();
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    let mut move_it = review("/photos/Lisbon", 1);
    move_it.copy = true;
    move_it.move_files = true;
    let mut several = review("/photos/Faro", 2);
    several.sources = vec!["/photos/Faro".into(), "/photos/Oslo".into(), "/photos/Evora".into()];
    crate::import::start(&mut h.app, &review("/photos/Rome", 3)).expect("waits");
    crate::import::start(&mut h.app, &move_it).expect("waits");
    crate::import::start(&mut h.app, &several).expect("waits");
    crate::import::start_paths(&mut h.app, vec!["/photos/Porto/".into()]).expect("waits");
    let rows = h.app.session.activity.waiting();
    let said: Vec<(&str, &str)> = rows.iter().map(|r| (r.detail.as_str(), r.facts.as_str())).collect();
    assert_eq!(
        said,
        vec![("Rome", "3 photos · Add in place"), ("Lisbon", "1 photo · Move"), ("3 items", "2 photos · Add in place"), ("Porto", "Add in place")]
    );
}

/// Given a waiting import, then its row in the activity stack names the folder and says what is known beside when it
/// starts.
#[test]
fn a_waiting_row_gives_the_folder_and_what_is_known_beside_when_it_starts() {
    let mut h = app();
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    crate::import::start(&mut h.app, &review("/photos/Rome", 3)).expect("waits");
    for _ in 0..4 {
        h.step();
    }
    let text = h.painted_text();
    assert!(text.iter().any(|t| t == "Rome · 3 photos · Add in place"), "{text:?}");
    assert!(text.iter().any(|t| t == " · Starts next"), "{text:?}");
    let said = crate::panels::activity::cross_name(&h.app.session.activity.waiting()[0]);
    assert_eq!(said, "Remove from Queue: Importing · Rome", "✕ is named for the folder");
}

/// Given an import that runs, then its row names its folder beside how far along it is.
#[test]
fn the_running_import_names_its_folder() {
    let dir = std::env::temp_dir().join(format!("lc-queue-ux-{}", std::process::id())).join("Rome");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..30 {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut s = lightcraft_engine::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|p: &str| {
        std::thread::sleep(Duration::from_millis(100));
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).expect("starts");
    assert!(h.step_until(T, |h| h.app.session.activity.list().iter().any(|t| t.kind == "import" && !t.waiting)), "the row appears");
    let row = h.app.session.activity.list().into_iter().find(|t| t.kind == "import").unwrap();
    assert_eq!(row.detail, "Rome");
    h.app.session.activity.cancel_all();
    h.step_until(T, |h| h.app.import.is_none());
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// Given an import that waited, when it starts, then its running row keeps naming the folder.
#[test]
fn an_import_that_waited_names_its_folder_when_it_runs() {
    let dir = std::env::temp_dir().join(format!("lc-queue-ux-waited-{}", std::process::id())).join("Rome");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..30 {
        std::fs::write(dir.join(format!("IMG_{i:03}.jpg")), format!("not really a jpeg {i}")).unwrap();
    }
    let mut s = lightcraft_engine::Session::new();
    s.media.file_probe = Some(std::sync::Arc::new(|p: &str| {
        std::thread::sleep(Duration::from_millis(100));
        Ok(lightcraft_engine::media::ProbeInfo {
            width: 60,
            height: 40,
            format: "JPEG".into(),
            content_hash: Some(p.to_string()),
            ..Default::default()
        })
    }));
    let mut h = Headless::new(LightcraftApp::new(s, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    let hold = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    crate::import::start_paths(&mut h.app, vec![dir.to_string_lossy().to_string()]).expect("waits");
    drop(hold);
    assert!(h.step_until(T, |h| h.app.session.activity.list().iter().any(|t| t.kind == "import" && !t.waiting)), "it starts");
    let row = h.app.session.activity.list().into_iter().find(|t| t.kind == "import").unwrap();
    assert_eq!(row.detail, "Rome");
    h.app.session.activity.cancel_all();
    h.step_until(T, |h| h.app.import.is_none());
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// Given an import running, when a Synchronize Folder is confirmed, then its waiting row names the folder.
#[test]
fn a_waiting_synchronize_names_its_folder() {
    let mut h = app();
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    let r = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog("Rome"));
    assert!(r.as_ref().is_ok_and(|v| v.get("queued").is_some()), "{r:?}");
    let rows = h.app.session.activity.waiting();
    assert_eq!((rows[0].kind, rows[0].detail.as_str(), rows[0].facts.as_str()), ("sync", "Rome", ""));
}

/// Given the library lane free, then the review's button says "Import" and nothing says it will wait. Given an
/// import running, then it says "Queue" and names what it starts after; a move is told as one.
#[test]
fn the_review_says_when_the_import_will_wait() {
    let mut h = app();
    open_review(&mut h, review("/photos/Rome", 3));
    assert!(says(&h, "Import 3 Photos"), "{:?}", h.painted_text());
    assert!(!h.painted_text().iter().any(|t| t.starts_with("Starts after")), "{:?}", h.painted_text());
    let running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    running.detail("Oslo");
    h.step();
    h.step();
    assert!(says(&h, "Queue 3 Photos"), "{:?}", h.painted_text());
    assert!(says(&h, "Starts after: Importing · Oslo"), "{:?}", h.painted_text());
    let mut d = review("/photos/Rome", 1);
    d.copy = true;
    d.move_files = true;
    open_review(&mut h, d);
    assert!(says(&h, "Queue Move 1 Photo"), "{:?}", h.painted_text());
}

/// Given a free lane, then nothing is ahead; given an import running with another waiting behind it, then a task
/// asked for now starts after the running one, as it is named on its row.
#[test]
fn what_a_new_task_starts_after_is_the_running_one() {
    let mut h = app();
    assert_eq!(crate::import::lane_ahead_text(&h.app), None);
    let hold = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    hold.detail("Oslo");
    crate::import::start(&mut h.app, &review("/photos/Lisbon", 1)).expect("waits");
    assert_eq!(crate::import::lane_ahead_text(&h.app).as_deref(), Some("Importing · Oslo"));
}

/// The same for the single-folder Synchronize dialog.
#[test]
fn the_synchronize_dialog_says_when_it_will_wait() {
    let mut h = app();
    h.app.ui.dialog = Some(sync_dialog("Rome"));
    h.step();
    h.step();
    assert!(says(&h, "Synchronize"), "{:?}", h.painted_text());
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    h.step();
    h.step();
    assert!(says(&h, "Queue Synchronize"), "{:?}", h.painted_text());
    assert!(says(&h, "Starts after: Importing"), "{:?}", h.painted_text());
}

/// Given a finished scan whose review waits behind another dialog, a folder being scanned for the review, or a folder
/// being read for the Local view, when photos are dropped or started, then each says its own reason, in the window's
/// language, and not "an import is running".
#[test]
fn a_refused_drop_says_why_per_cause_in_the_windows_language() {
    let mut h = app();
    assert_eq!(crate::import::drop_refusal(&h.app), None);
    h.app.review_pending = Some(crate::state::Dialog::Import { opts: Box::new(review("/photos/Rome", 1)) });
    let pending = crate::import::start_paths(&mut h.app, vec!["/photos/Oslo".into()]).expect_err("refused");
    assert_eq!(pending, "An import review is waiting to open: drop photos when it is closed");
    h.app.review_pending = None;
    let scan = crate::import::stall_scan_of(&mut h.app, false);
    let reviewing = crate::import::start_paths(&mut h.app, vec!["/photos/Oslo".into()]).expect_err("refused");
    drop(scan);
    h.app.scan = None;
    let scan = crate::import::stall_scan_of(&mut h.app, true);
    let reading = crate::import::drop_refusal(&h.app).expect("refused");
    drop(scan);
    h.app.scan = None;
    assert_eq!(reviewing, "A folder is still being scanned for the import review: drop photos when it has finished");
    assert_eq!(reading, "A folder is still being read for the Local view: drop photos when it has finished");
    assert!(![&pending, &reviewing, &reading].iter().any(|t| t.contains("An import is running")));
    // the same, in German: from the catalog's own text
    crate::i18n::set_language(crate::i18n::Locale::De);
    h.app.review_pending = Some(crate::state::Dialog::Import { opts: Box::new(review("/photos/Rome", 1)) });
    let german = crate::import::drop_refusal(&h.app).expect("refused");
    crate::i18n::set_language(crate::i18n::Locale::En);
    assert_eq!(german, crate::i18n::Locale::De.tr("An import review is waiting to open: drop photos when it is closed"));
    assert_ne!(german, pending, "translated");
}

fn library_app(tag: &str) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-queue-ux-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = lightcraft_engine::Session::new().with_fs();
    session.open_library(dir.join("first"), false).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), [1200.0, 800.0], 1.0);
    h.settle(T);
    (h, dir)
}

/// A preview build running with another waiting behind it: the line holds one task.
fn hold_a_line(h: &mut Headless) -> lightcraft_engine::activity::TaskGuard {
    let running = h.app.session.activity.start("previews", "Building previews", Cancel::Yes);
    let request = lightcraft_engine::activity::Request { command: "library.buildPreviews".into(), params: json!({}), ..Default::default() };
    h.app.session.activity.enqueue("previews", "Building previews", request).unwrap();
    running
}

fn library_dir(h: &Headless) -> Option<std::path::PathBuf> {
    h.app.session.library.as_ref().map(|l| l.dir.clone())
}

fn click(h: &mut Headless, id: &str) {
    h.step();
    h.step();
    assert_eq!(h.request("ui.clickWidget", json!({"id": id}), T)["ok"], true, "{id}");
    for _ in 0..4 {
        h.step();
    }
}

/// Given a task waiting in line, when another library is opened, then the person is asked first, as quitting asks:
/// Cancel keeps the library and the line; Switch Library goes, drops the line and says how many were dropped.
#[test]
fn switching_libraries_asks_before_dropping_the_line() {
    let (mut h, dir) = library_app("switch-asks");
    let _running = hold_a_line(&mut h);
    let first = library_dir(&h);
    let r = h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    assert_eq!(r["asked"], true, "{r}");
    h.step();
    h.step();
    let said = "1 task is waiting in line: Building previews. Switching libraries drops it.";
    assert!(h.painted_text().iter().any(|t| t == said), "{:?}", h.painted_text());
    assert_eq!((library_dir(&h), h.app.session.activity.waiting().len()), (first.clone(), 1), "nothing changed yet");
    click(&mut h, "button:switchCancel");
    assert_eq!((library_dir(&h), h.app.session.activity.waiting().len()), (first, 1), "cancel keeps both");
    assert!(h.app.switch_prompt.is_none());
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks again");
    click(&mut h, "button:switchAnyway");
    assert_eq!(library_dir(&h), Some(dir.join("second")));
    assert!(h.app.session.activity.waiting().is_empty());
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Waiting tasks dropped: 1"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the switch question up for one waiting task, when another task is put in line before Switch Library is clicked,
/// then nothing is dropped: it asks again, naming both.
#[test]
fn a_task_put_in_line_while_the_question_is_up_is_asked_about_too() {
    let (mut h, dir) = library_app("switch-grows");
    let _running = hold_a_line(&mut h);
    let first = library_dir(&h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    h.step();
    let request = lightcraft_engine::activity::Request { command: "library.buildPreviews".into(), params: json!({}), ..Default::default() };
    h.app.session.activity.enqueue("previews", "Building previews", request).unwrap();
    click(&mut h, "button:switchAnyway");
    assert_eq!((library_dir(&h), h.app.session.activity.waiting().len()), (first, 2), "nothing was dropped");
    let asked = h.app.switch_prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default();
    assert!(asked.starts_with("2 tasks are waiting in line"), "{asked}");
    click(&mut h, "button:switchAnyway");
    assert_eq!(library_dir(&h), Some(dir.join("second")), "agreed to both");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the switch question up for one waiting task, when the task running ahead of it ends, then the waiting one stays
/// in line (it would start, and then escape the drop the question names): Switch Library still switches and drops it.
#[test]
fn the_line_holds_while_the_switch_question_is_up() {
    let (mut h, dir) = library_app("switch-holds");
    let running = hold_a_line(&mut h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    h.step();
    drop(running);
    for _ in 0..6 {
        h.step();
    }
    assert!(h.app.switch_prompt.is_some(), "still asking");
    assert_eq!(h.app.session.activity.waiting().len(), 1, "the waiting build did not start while the question was up");
    click(&mut h, "button:switchAnyway");
    assert_eq!(library_dir(&h), Some(dir.join("second")), "the switch went through");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a switch question that is about a library no longer open (a control call switched while the window was hidden, so
/// no frame drew the question and dropped it), when the task ahead ends and only the logic tick runs, then the waiting task
/// starts: a stale question holds nothing.
#[test]
fn a_stale_switch_question_does_not_hold_the_line() {
    let (mut h, dir) = library_app("switch-stale-hold");
    let running = hold_a_line(&mut h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    let generation = h.app.session.library_generation();
    h.app.switch_prompt.as_mut().expect("asked").generation = generation.wrapping_add(1);
    drop(running);
    let ctx = h.view.ctx.clone();
    h.app.logic(&ctx);
    assert!(h.app.session.activity.waiting().is_empty(), "the waiting build started");
    assert!(h.step_until(T, |h| h.app.session.activity.list().is_empty()), "and it ended before the test did");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a task waiting in line and a library that can't be opened, when the person agrees to drop the line and the open
/// fails, then the old library is opened again and its line is still there: nothing was switched, so nothing is dropped.
#[test]
fn a_switch_that_fails_keeps_the_line() {
    let (mut h, dir) = library_app("switch-fails");
    let _running = hold_a_line(&mut h);
    let first = library_dir(&h);
    std::fs::create_dir_all(dir.join("broken")).unwrap();
    std::fs::write(dir.join("broken").join("catalog.snap"), b"not a catalog").unwrap();
    let r = h.app.run("app.openLibrary", json!({"path": dir.join("broken").to_string_lossy(), "dropWaiting": true}));
    assert!(r.is_err(), "the broken library does not open: {r:?}");
    assert_eq!(library_dir(&h), first, "the old library is open again");
    assert_eq!(h.app.session.activity.waiting().len(), 1, "its line was not dropped");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the switch question with focus moved to Switch Library, when a task waiting in line is replaced by another with
/// the same name before Switch Library is pressed, then the question comes back with the same words and focus back on the
/// safe Cancel, not on the button that was just used.
#[test]
fn a_switch_question_asked_again_in_the_same_words_resets_focus_to_cancel() {
    let (mut h, dir) = library_app("switch-focus");
    let _running = hold_a_line(&mut h);
    let first = library_dir(&h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    h.step();
    h.step();
    let said = h.app.switch_prompt.as_ref().map(|p| p.text.clone()).unwrap_or_default();
    let focused = |h: &Headless| h.view.ctx.memory(|m| m.focused());
    let hit =
        |h: &Headless, id: &str| h.app.widget_hits.iter().rev().find(|(w, _)| w == id).map(|(_, hit)| hit.id).unwrap_or_else(|| panic!("no {id}"));
    let (cancel, anyway) = (hit(&h, "button:switchCancel"), hit(&h, "button:switchAnyway"));
    assert_eq!(focused(&h), Some(cancel), "the safe Cancel starts focused");
    assert_eq!(h.request("ui.key", json!({"key": "Tab"}), T)["ok"], true);
    assert_eq!(focused(&h), Some(anyway), "Tab moved focus to Switch Library");
    // the waiting task is taken out and another of the same name put in: the question reads the same
    let old = h.app.session.activity.waiting()[0].id;
    h.app.session.activity.cancel(old).unwrap();
    let request = lightcraft_engine::activity::Request { command: "library.buildPreviews".into(), params: json!({}), ..Default::default() };
    h.app.session.activity.enqueue("previews", "Building previews", request).unwrap();
    assert_eq!(h.request("ui.key", json!({"key": "Space"}), T)["ok"], true);
    h.step();
    h.step();
    assert_eq!((library_dir(&h), h.app.session.activity.waiting().len()), (first, 1), "nothing was dropped");
    assert_eq!(h.app.switch_prompt.as_ref().map(|p| p.text.as_str()), Some(said.as_str()), "asked again in the same words");
    let cancel_after = hit(&h, "button:switchCancel");
    assert_eq!(focused(&h), Some(cancel_after), "focus is back on the safe Cancel");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given Open Library with a task waiting, then it asks too and Cancel leaves everything as it was.
#[test]
fn opening_a_library_asks_before_dropping_the_line() {
    let (mut h, dir) = library_app("open-asks");
    let mut other = lightcraft_engine::Session::new().with_fs();
    other.open_library(dir.join("other"), false).unwrap();
    other.close_library().unwrap();
    drop(other);
    let _running = hold_a_line(&mut h);
    let first = library_dir(&h);
    let r = h.app.run("app.openLibrary", json!({"path": dir.join("other").to_string_lossy()})).expect("asks");
    assert_eq!(r["asked"], true, "{r}");
    click(&mut h, "button:switchCancel");
    assert_eq!((library_dir(&h), h.app.session.activity.waiting().len()), (first, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a control call that has already agreed (`dropWaiting`), then the library switches without a question and the
/// toast says how many waiting tasks were dropped. With nothing waiting there is no question and no toast.
#[test]
fn a_switch_that_agreed_to_drop_the_line_says_how_many_went() {
    let (mut h, dir) = library_app("switch-agreed");
    let quiet = h.app.run("app.newLibrary", json!({"path": dir.join("quiet").to_string_lossy()})).expect("opens");
    assert!(quiet.get("asked").is_none(), "{quiet}");
    assert!(h.app.ui.toast.as_ref().is_none_or(|t| !t.0.contains("dropped")), "{:?}", h.app.ui.toast);
    let _running = hold_a_line(&mut h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy(), "dropWaiting": true})).expect("opens");
    assert_eq!(library_dir(&h), Some(dir.join("second")));
    assert_eq!(h.app.ui.toast.as_ref().map(|t| t.0.as_str()), Some("Waiting tasks dropped: 1"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given a line already full, when an import or a synchronize is asked for, then the refusal is the app's translated
/// sentence, not the engine's English one.
#[test]
fn a_full_line_is_refused_in_the_language_of_the_window() {
    let mut h = app();
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    for _ in 0..lightcraft_engine::activity::MAX_WAITING {
        let request = lightcraft_engine::activity::Request { command: "library.buildPreviews".into(), params: json!({}), ..Default::default() };
        h.app.session.activity.enqueue("previews", "Building previews", request).unwrap();
    }
    crate::i18n::set_language(crate::i18n::Locale::De);
    let import = crate::import::start(&mut h.app, &review("/photos/Rome", 1)).expect_err("refused");
    let dropped = crate::import::start_paths(&mut h.app, vec!["/photos/Oslo".into()]).expect_err("refused");
    let sync = crate::panels::dialogs::confirm_dialog(&mut h.app, &sync_dialog("Rome")).expect_err("refused");
    let german = crate::i18n::tr_format!("Too many tasks are waiting ({n}): wait for some to finish", n = lightcraft_engine::activity::MAX_WAITING);
    crate::i18n::set_language(crate::i18n::Locale::En);
    let english = crate::panels::activity::line_full_text();
    assert_eq!(english, "Too many tasks are waiting (100): wait for some to finish");
    for said in [import, dropped, sync] {
        assert!(!said.contains("waiting already"), "{said}");
        assert_eq!(said, german, "German, from the catalog");
        assert_ne!(said, english, "German, not English");
    }
}

/// Given a waiting import whose folder name is far longer than the stack is wide, then its row stays inside the stack
/// and the timing is whole: the name and the facts are what give way (cut with "…").
#[test]
fn a_waiting_row_with_long_facts_stays_inside_the_stack_with_its_timing_whole() {
    let long = "A_very_long_folder_name_for_a_holiday_in_the_old_town_with_all_the_family_2026";
    let mut h = app();
    let _running = h.app.session.activity.start("import", "Importing", Cancel::Yes);
    crate::import::start(&mut h.app, &review("/photos/Rome", 3)).expect("waits");
    crate::import::start(&mut h.app, &review(&format!("/photos/{long}"), 3)).expect("waits");
    crate::i18n::set_language(crate::i18n::Locale::De);
    for _ in 0..4 {
        h.step();
    }
    let after = crate::i18n::tr_format!("Starts after {n} more", n = 1);
    let painted = h.painted_text();
    crate::i18n::set_language(crate::i18n::Locale::En);
    let id = h.app.session.activity.waiting()[1].id;
    let at = |what: &str| h.app.widgets.iter().find(|(w, _)| *w == format!("activity:{what}:{id}")).map(|(_, r)| *r).expect("drawn");
    let (row, timing) = (at("row"), at("starts"));
    assert!(timing.right() <= row.right() + 0.5, "the timing ends inside its row: {timing:?} in {row:?}");
    // (rows are laid out in the stack's width less its frame: 280 less a margin and a stroke each side)
    assert!(row.width() <= crate::panels::activity::WIDTH - 18.0 + 0.5, "{row:?}");
    assert!(painted.iter().any(|t| t.ends_with(&after)), "the timing is whole: {painted:?}");
}

/// Given a Local folder being read, then a review asked for now says it starts after that read, as its row says it.
#[test]
fn the_dialog_names_a_local_folder_read_it_waits_for() {
    let mut h = app();
    let _scan = crate::import::stall_scan_of(&mut h.app, true);
    assert_eq!(crate::import::lane_ahead_text(&h.app).as_deref(), Some("Reading folder"));
}

/// Given tasks waiting in line, when a Lightroom catalog is imported into a new library, then it is refused before
/// anything is made or asked: the question about the line would otherwise leave the catalog merged into the library
/// that stays open.
#[test]
fn a_lightroom_import_into_a_new_library_is_refused_while_tasks_wait() {
    let (mut h, dir) = library_app("lr-new");
    let _running = hold_a_line(&mut h);
    std::fs::write(dir.join("old.lrcat"), b"not a catalog").unwrap();
    let first = library_dir(&h);
    let e = h
        .app
        .run(
            "file.importLightroom",
            json!({"path": dir.join("old.lrcat").to_string_lossy(), "into": "new", "location": dir.to_string_lossy(), "name": "fresh"}),
        )
        .expect_err("refused");
    assert!(e.contains("waiting"), "{e}");
    assert!(h.app.switch_prompt.is_none() && !dir.join("fresh").exists(), "nothing asked or made");
    assert_eq!(library_dir(&h), first);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the library chooser up (Open Library… from the banner) and a task waiting, when a library is chosen, then
/// the question comes first and the chooser stays; Switch Library closes it.
#[test]
fn the_library_chooser_stays_until_the_switch_is_confirmed() {
    let (mut h, dir) = library_app("chooser");
    let mut other = lightcraft_engine::Session::new().with_fs();
    other.open_library(dir.join("other"), false).unwrap();
    other.close_library().unwrap();
    drop(other);
    let _running = hold_a_line(&mut h);
    let mut chooser = crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings);
    // (as the banner's Open Library… sets it: the chooser belongs to the library that is open)
    if let Some(c) = chooser.chooser.as_mut() {
        c.over = Some(h.app.session.library_generation());
    }
    h.app.library_problem = Some(chooser);
    let drawn = |h: &Headless, id: &str| h.app.widgets.iter().any(|(w, _)| *w == id);
    for _ in 0..4 {
        h.step();
    }
    assert!(drawn(&h, "button:libraryOpen"), "the list is drawn before the question");
    crate::panels::library_problem::open(&mut h.app, Some(dir.join("other").to_string_lossy().into_owned()));
    assert!(h.app.switch_prompt.is_some(), "asked");
    for _ in 0..4 {
        h.step();
    }
    assert!(!drawn(&h, "button:libraryOpen"), "the list is not drawn under the question");
    // (the list takes no keys while the question is up: Return would open the list's selection, but it goes to the
    // question, whose focus rests on Cancel; Cancel keeps the library and the list)
    let _ = h.request("ui.key", json!({"key": "Enter"}), T);
    h.step();
    h.step();
    assert!(h.app.library_problem.is_some(), "the chooser stays while the question is answered");
    assert!(h.app.switch_prompt.is_none(), "Return went to the question and it answered Cancel");
    assert_ne!(library_dir(&h), Some(dir.join("other")), "nothing was opened by the list's own keys");
    // asked again, and answered with its button
    crate::panels::library_problem::open(&mut h.app, Some(dir.join("other").to_string_lossy().into_owned()));
    assert!(h.app.switch_prompt.is_some(), "asked again");
    click(&mut h, "button:switchAnyway");
    assert_eq!(library_dir(&h), Some(dir.join("other")));
    assert!(h.app.library_problem.is_none(), "closed once the library opened");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given many tasks waiting, then the question names the first few and counts the rest, so it fits its window.
#[test]
fn the_switch_question_names_only_the_first_few_waiting_tasks() {
    let (mut h, dir) = library_app("switch-many");
    let _running = hold_a_line(&mut h);
    for _ in 0..6 {
        let request = lightcraft_engine::activity::Request { command: "library.buildPreviews".into(), params: json!({}), ..Default::default() };
        h.app.session.activity.enqueue("previews", "Building previews", request).unwrap();
    }
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    let text = h.app.switch_prompt.as_ref().map(|p| p.text.clone()).expect("asked");
    assert!(text.starts_with("7 tasks are waiting in line: ") && text.contains(", +2 more. Switching"), "{text}");
    assert_eq!(text.matches("Building previews").count(), 5, "{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the question about the waiting tasks up over an open library, when a shortcut's key is pressed, then the
/// library is not acted on: the question has the keyboard until it is answered.
#[test]
fn the_switch_question_takes_the_keyboard() {
    let (mut h, dir) = library_app("switch-keys");
    let _running = hold_a_line(&mut h);
    let panel = h.app.ui.left_panel;
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    h.step();
    h.step();
    // (the question is a prompt: the barrier refuses a key aimed past it, or drops it; either way nothing is acted on)
    let _ = h.request("ui.key", json!({"key": "F7"}), T);
    h.step();
    h.step();
    assert_eq!(h.app.ui.left_panel, panel, "F7 (Left Panel) did nothing under the question");
    click(&mut h, "button:switchCancel");
    assert_eq!(h.request("ui.key", json!({"key": "F7"}), T)["ok"], true);
    h.step();
    h.step();
    assert_ne!(h.app.ui.left_panel, panel, "and works again once it is answered");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the library chooser up and the question asked from it, when the chosen library can't be opened after Switch
/// Library, then the chooser stays and shows the error, as it does for a failed open without the question.
#[test]
fn a_failed_switch_from_the_chooser_shows_its_error_in_the_chooser() {
    let (mut h, dir) = library_app("chooser-fails");
    let mut other = lightcraft_engine::Session::new().with_fs();
    other.open_library(dir.join("other"), false).unwrap();
    other.close_library().unwrap();
    drop(other);
    let _running = hold_a_line(&mut h);
    let mut chooser = crate::panels::library_problem::LibraryProblem::choose_later(&h.app.ui.settings);
    if let Some(c) = chooser.chooser.as_mut() {
        c.over = Some(h.app.session.library_generation());
    }
    h.app.library_problem = Some(chooser);
    h.step();
    let target = dir.join("other");
    crate::panels::library_problem::open(&mut h.app, Some(target.to_string_lossy().into_owned()));
    assert!(h.app.switch_prompt.is_some(), "asked");
    // (it can't be opened by the time the question is answered)
    std::fs::remove_dir_all(&target).unwrap();
    std::fs::write(&target, b"not a library").unwrap();
    let first = library_dir(&h);
    click(&mut h, "button:switchAnyway");
    assert_eq!(library_dir(&h), first, "the library stays open");
    assert_eq!(h.app.session.activity.waiting().len(), 1, "and its line");
    let problem = h.app.library_problem.as_ref().expect("the chooser stays");
    assert!(!problem.error.is_empty(), "it says why");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the question up, when a control call switches libraries anyway (`dropWaiting`), then the question goes: it was
/// about the library that is gone, and answering it would drop the new library's tasks unasked.
#[test]
fn the_switch_question_goes_when_another_library_is_opened() {
    let (mut h, dir) = library_app("switch-stale");
    let _running = hold_a_line(&mut h);
    h.app.run("app.newLibrary", json!({"path": dir.join("second").to_string_lossy()})).expect("asks");
    assert!(h.app.switch_prompt.is_some());
    h.app.run("app.newLibrary", json!({"path": dir.join("third").to_string_lossy(), "dropWaiting": true})).expect("switches");
    assert_eq!(library_dir(&h), Some(dir.join("third")));
    h.step();
    h.step();
    assert!(h.app.switch_prompt.is_none(), "the question is about the first library");
    assert!(!h.app.widgets.iter().any(|(w, _)| *w == "button:switchAnyway"), "and is not drawn");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Given the review's button for each verb, then the count comes as a label's value in French, Italian, Brazilian
/// Portuguese, Indonesian and Swedish, never straight after the verb's noun ("Mettre le déplacement en file 3 photos").
#[test]
fn the_review_button_puts_the_count_after_a_label() {
    use crate::i18n::{Locale, set_language};
    for locale in [Locale::Fr, Locale::It, Locale::PtBr, Locale::Id, Locale::Sv] {
        set_language(locale);
        for verb in ["Import", "Move", "Queue", "Queue Move"] {
            let said = crate::panels::dialogs::photo_count_label(verb, 3);
            let verb = locale.tr(verb);
            assert!(said.starts_with(verb) && !said.starts_with(&format!("{verb} 3")), "{locale:?}: {said}");
        }
    }
    set_language(Locale::En);
}
