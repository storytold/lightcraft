//! The import review says what its scan left out: one line, and only when it left something out.
//!
//! * Files in the folder that are not photos (a text file beside the pictures): the review lists
//!   the pictures and says how many files were skipped.
//! * A folder tree deeper than a scan goes: the review says that there may be more photos.
//! * A folder of pictures only: no line at all.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

use crate::dialogs::import::{ImportDialog, scan_note};
use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(120);

const STOPPED: &str = "Stopped at the scan limit: there may be more photos";

/// A scratch folder that goes away with the test, however it ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("lc-ui-scan-note-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn write_png(path: &Path, seed: u8) {
    let (w, h) = (24usize, 16usize);
    let data: Vec<[u8; 4]> = (0..w * h).map(|i| [(i % w * 9) as u8, (i / w * 13) as u8, seed, 255]).collect();
    let img = lightcraft_raster::Rgba8 { width: w, height: h, data };
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn headless() -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services { png: None, ..Default::default() });
    Headless::new(app, [1300.0, 900.0], 1.0)
}

/// File → Import from Folder… for `dir`, and wait for the review to open.
fn open_review(h: &mut Headless, dir: &Scratch) {
    let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.0.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(h.step_until(T, |h| matches!(h.app.ui.dialog, Some(Dialog::Import { .. }))), "no import review: {:?}", h.app.ui.dialog);
}

fn review(h: &mut Headless) -> &mut ImportDialog {
    let Some(Dialog::Import { opts }) = &mut h.app.ui.dialog else { panic!("no import review") };
    opts
}

/// The review painted a text that is exactly `line`.
fn shows(h: &Headless, line: &str) -> bool {
    h.painted_text().iter().any(|t| t == line)
}

/// The review painted any of the scan's notes.
fn shows_any_note(h: &Headless) -> bool {
    h.painted_text().iter().any(|t| t.contains(STOPPED) || t.contains("could not be read") || t.contains("skipped (not a supported photo type)"))
}

fn has_note_widget(h: &mut Headless) -> bool {
    h.request("ui.widgets", json!({}), T).to_string().contains("label:importScanNote")
}

/// A text file beside the pictures: the review lists the pictures, and says one file was skipped.
#[test]
fn the_review_says_how_many_files_were_skipped() {
    let dir = Scratch::new("skipped");
    write_png(&dir.0.join("a.png"), 1);
    write_png(&dir.0.join("trip").join("b.png"), 2);
    std::fs::write(dir.0.join("notes.txt"), b"not a photo").unwrap();
    let mut h = headless();
    open_review(&mut h, &dir);
    let d = review(&mut h);
    assert_eq!(d.candidates.len(), 2);
    assert_eq!((d.scan_limited, d.unreadable, d.unsupported), (false, 0, 1));
    h.step();
    assert!(shows(&h, "1 file skipped (not a supported photo type)"), "{:?}", h.painted_text());
    assert!(has_note_widget(&mut h), "the line is a registered widget");
}

/// Nothing to import but a file that is not a photo: no review opens, and the message says why
/// nothing was found, the same skipped count the review would have shown.
#[test]
fn an_empty_scan_says_what_it_left_out() {
    let dir = Scratch::new("empty-skipped");
    std::fs::write(dir.0.join("notes.txt"), b"not a photo").unwrap();
    let mut h = headless();
    assert_eq!(scan_empty_folder(&mut h, &dir), "No photos found · 1 file skipped (not a supported photo type)");
    assert!(!matches!(h.app.ui.dialog, Some(Dialog::Import { .. })), "no review for a scan with no photos");
}

/// A folder with nothing in it: the plain message, as before.
#[test]
fn an_empty_folder_keeps_the_plain_message() {
    let dir = Scratch::new("empty");
    let mut h = headless();
    assert_eq!(scan_empty_folder(&mut h, &dir), "No photos found");
}

/// File → Import from Folder… for `dir` (no photos in it): the message the scan leaves.
fn scan_empty_folder(h: &mut Headless, dir: &Scratch) -> String {
    let r = h.request("engine.execute", json!({"command": "file.addFolder", "params": {"path": dir.0.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(h.step_until(SETTLE, |h| h.app.ui.toast.is_some()), "no message: {:?}", h.app.ui.toast);
    h.app.ui.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default()
}

/// XMP sidecars beside the pictures are read with them, so they are not "skipped": no note for
/// them, but a text file in the same folder still gets its line.
#[test]
fn xmp_sidecars_do_not_make_the_review_say_files_were_skipped() {
    let dir = Scratch::new("sidecars");
    write_png(&dir.0.join("a.png"), 1);
    std::fs::write(dir.0.join("a.xmp"), b"x").unwrap();
    std::fs::write(dir.0.join("b.XMP"), b"x").unwrap();
    let mut h = headless();
    open_review(&mut h, &dir);
    let d = review(&mut h);
    assert_eq!((d.candidates.len(), d.scan_limited, d.unreadable, d.unsupported), (1, false, 0, 0));
    h.step();
    assert!(!shows_any_note(&h), "{:?}", h.painted_text());
    assert!(!has_note_widget(&mut h));

    std::fs::write(dir.0.join("notes.txt"), b"not a photo").unwrap();
    h.app.ui.dialog = None;
    open_review(&mut h, &dir);
    assert_eq!(review(&mut h).unsupported, 1, "only the text file");
    h.step();
    assert!(shows(&h, "1 file skipped (not a supported photo type)"), "{:?}", h.painted_text());
}

/// A tree deeper than a scan goes: the review says that there may be more photos.
#[test]
fn the_review_says_when_the_scan_stopped_at_its_limit() {
    let dir = Scratch::new("deep");
    write_png(&dir.0.join("top.png"), 1);
    let mut deepest = dir.0.clone();
    for _ in 0..lightcraft_engine::walk::MAX_DEPTH {
        deepest = deepest.join("d");
    }
    std::fs::create_dir_all(&deepest).unwrap();
    let mut h = headless();
    open_review(&mut h, &dir);
    let d = review(&mut h);
    assert_eq!((d.candidates.len(), d.scan_limited, d.unreadable, d.unsupported), (1, true, 0, 0));
    h.step();
    assert!(shows(&h, STOPPED), "{:?}", h.painted_text());
}

/// Nothing left out: no line, and no widget for it.
#[test]
fn the_review_has_no_note_when_the_scan_left_nothing_out() {
    let dir = Scratch::new("clean");
    write_png(&dir.0.join("a.png"), 1);
    write_png(&dir.0.join("trip").join("b.png"), 2);
    let mut h = headless();
    open_review(&mut h, &dir);
    let d = review(&mut h);
    assert_eq!((d.candidates.len(), d.scan_limited, d.unreadable, d.unsupported), (2, false, 0, 0));
    h.step();
    assert!(!shows_any_note(&h), "{:?}", h.painted_text());
    assert!(!has_note_widget(&mut h));
}

/// The line follows the counters while the review is open, and is written in the language in use.
#[test]
fn the_note_follows_the_counters_and_the_language() {
    let dir = Scratch::new("follow");
    write_png(&dir.0.join("a.png"), 1);
    let mut h = headless();
    open_review(&mut h, &dir);
    assert!(!shows_any_note(&h));

    let all = format!("{STOPPED} · 2 folders could not be read · 3 files skipped (not a supported photo type)");
    {
        let d = review(&mut h);
        (d.scan_limited, d.unreadable, d.unsupported) = (true, 2, 3);
    }
    h.step();
    h.step();
    assert!(shows(&h, &all), "{:?}", h.painted_text());

    {
        let d = review(&mut h);
        (d.scan_limited, d.unreadable, d.unsupported) = (false, 1, 1);
    }
    h.step();
    h.step();
    assert!(shows(&h, "1 folder could not be read · 1 file skipped (not a supported photo type)"), "{:?}", h.painted_text());

    {
        let d = review(&mut h);
        (d.scan_limited, d.unreadable, d.unsupported) = (true, 1, 1);
    }
    assert_eq!(h.request("engine.execute", json!({"command": "app.language.german"}), T)["ok"], true);
    h.settle(SETTLE);
    let german = "Am Suchlimit angehalten: Es gibt möglicherweise weitere Fotos · Ordner, die nicht gelesen werden konnten: 1 · Übersprungene Dateien (kein unterstützter Fototyp): 1";
    assert!(shows(&h, german), "{:?}", h.painted_text());
    assert_eq!(h.request("engine.execute", json!({"command": "app.language.english"}), T)["ok"], true);

    {
        let d = review(&mut h);
        (d.scan_limited, d.unreadable, d.unsupported) = (false, 0, 0);
    }
    h.step();
    h.step();
    assert!(!shows_any_note(&h), "back to nothing left out: {:?}", h.painted_text());
}

/// The wording, one part per thing left out, joined into one line; singular and plural.
#[test]
fn the_note_names_each_thing_that_was_left_out() {
    let note = |scan_limited, unreadable, unsupported| scan_note(&ImportDialog { scan_limited, unreadable, unsupported, ..Default::default() });
    assert_eq!(note(false, 0, 0), None);
    assert_eq!(note(true, 0, 0).as_deref(), Some(STOPPED));
    assert_eq!(note(false, 1, 0).as_deref(), Some("1 folder could not be read"));
    assert_eq!(note(false, 4, 0).as_deref(), Some("4 folders could not be read"));
    assert_eq!(note(false, 0, 1).as_deref(), Some("1 file skipped (not a supported photo type)"));
    assert_eq!(note(false, 0, 12).as_deref(), Some("12 files skipped (not a supported photo type)"));
    assert_eq!(
        note(true, 2, 2).as_deref(),
        Some(format!("{STOPPED} · 2 folders could not be read · 2 files skipped (not a supported photo type)").as_str())
    );
}
