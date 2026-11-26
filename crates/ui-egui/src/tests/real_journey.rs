//! One continuous real-file journey. Stage as `tests::real_journey` in lightcraft-ui-egui.
//! No synthetic catalog, file-probe/loader mock, memory store, host picker, or app process.
//!
//! Its executor names the private tmp and a fresh case directory; the server's job runner (`task.py`) binds
//! no private tmp of its own, so without them the test fails and says how to run it. No ordinary run
//! has them, so the test is `#[ignore]`d: an ordinary `cargo test` skips it rather than failing on
//! a missing environment. Make a private directory (mode 700, not under `/tmp`), then pass both
//! variables to the job and ask for ignored tests, for example:
//!
//! ```text
//! mkdir -m 700 -p $PRIVATE
//! python3 /home/codex/work/maintenance/task.py run --job-id … --task … --kind build --source <checkout> --evidence … \
//!   --env TMPDIR=$PRIVATE --env LIGHTCRAFT_REAL_JOURNEY_ROOT=$PRIVATE/lc-real-journey-<unique> \
//!   -- cargo test -p lightcraft-ui-egui --lib -- --ignored tests::real_journey --test-threads=1
//! ```

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lightcraft_catalog::{Photo, PhotoId, Source};
use lightcraft_engine::Session;
use lightcraft_engine::export::{ExportOptions, export_photo};
use serde_json::{Value, json};

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const REQUEST: Duration = Duration::from_secs(20);
const SETTLE: Duration = Duration::from_secs(30);
const EXPOSURE: f64 = 1.25;

fn ask(h: &mut Headless, method: &str, params: Value) -> Value {
    let r = h.request(method, params.clone(), REQUEST);
    assert_eq!(r["ok"], true, "{method} {params}: {r}");
    assert!(h.settle(SETTLE), "{method}: UI did not settle within the bounded wait");
    r["result"].clone()
}

fn execute(h: &mut Headless, command: &str, params: Value) -> Value {
    ask(h, "engine.execute", json!({"command": command, "params": params}))
}

/// What to do when the executor bound neither variable.
const HOW_TO_RUN: &str = "bind TMPDIR to a private tmp (mode 700, not /tmp) and LIGHTCRAFT_REAL_JOURNEY_ROOT to a fresh \
     $TMPDIR/lc-real-journey-<unique>: task.py run ... --env TMPDIR=$PRIVATE --env \
     LIGHTCRAFT_REAL_JOURNEY_ROOT=$PRIVATE/lc-real-journey-<unique> -- cargo test -p lightcraft-ui-egui --lib -- --ignored \
     tests::real_journey (see the module docs)";

/// The executor must bind TMPDIR to its admitted private tmp and pass a fresh direct child.
/// No fallback to /tmp or a user library, no reuse, and no recursive deletion on failure.
fn fresh_root() -> PathBuf {
    let tmp = std::env::var_os("TMPDIR").expect(HOW_TO_RUN);
    let tmp = PathBuf::from(tmp);
    assert!(tmp.is_absolute());
    let tmp = tmp.canonicalize().expect("private TMPDIR must already exist");
    assert_ne!(tmp, Path::new("/tmp"), "never use the shared system tmp");
    let root = PathBuf::from(std::env::var_os("LIGHTCRAFT_REAL_JOURNEY_ROOT").expect(HOW_TO_RUN));
    assert!(root.is_absolute());
    assert_eq!(root.parent(), Some(tmp.as_path()), "case must be a direct child of private TMPDIR");
    let name = root.file_name().unwrap().to_str().unwrap();
    assert!(name.starts_with("lc-real-journey-") && name.len() <= 100);
    // create_dir is intentionally create-new: an existing path/symlink is an error, not cleanup.
    std::fs::create_dir(&root).expect("fresh case directory only; preserve prior runs");
    assert_eq!(root.canonicalize().unwrap(), root);
    root
}

fn fixture(path: &Path, variant: usize) -> Vec<u8> {
    let image = lightcraft_raster::Rgba8::from_fn(64, 48, |x, y| {
        // Dark, non-neutral, non-clipping gradients; each fixture has distinct bytes and pixels.
        [(24 + x + variant * 19) as u8, (16 + y + variant * 11) as u8, (20 + (x + y) / 3 + variant * 7) as u8, 255]
    });
    let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(path, &bytes).unwrap();
    bytes
}

fn headless(session: Session, out: &Path) -> Headless {
    let destination = out.to_path_buf();
    let services = Services {
        local_places: Some(Vec::new()), // don't enumerate the host's home folder
        write: Some(Box::new(move |path, bytes| {
            let path = Path::new(path);
            if path.parent() != Some(destination.as_path()) || path.extension().and_then(|s| s.to_str()) != Some("png") {
                return Err("journey export escaped its private PNG output folder".into());
            }
            if bytes.len() > 1_048_576 {
                return Err("tiny-fixture output exceeded 1 MiB".into());
            }
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| e.to_string())?;
            file.write_all(bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())
        })),
        ..Default::default() // no native picker, external opener, Photos action, or background writer
    };
    Headless::new(LightcraftApp::new(session, services), [1400.0, 900.0], 1.0)
}

fn photo(h: &Headless, id: PhotoId) -> Photo {
    h.app.session.catalog.photo(id).expect("exact imported photo id").as_ref().clone()
}

fn selected(h: &Headless, chosen: PhotoId, other: PhotoId) {
    assert_eq!(h.app.session.selection.ids, vec![chosen]);
    assert_eq!(h.app.session.selection.active, Some(chosen));
    assert!(!h.app.session.selection.ids.contains(&other));
    assert_eq!(h.app.session.selection.state_of(chosen), lightcraft_engine::SelectionState::Active);
    assert_eq!(h.app.session.selection.state_of(other), lightcraft_engine::SelectionState::NotSelected);
}

fn copied_path(p: &Photo, library: &Path, expected: &[u8]) -> PathBuf {
    let Source::File { path } = &p.source else { panic!("import must use a real file") };
    let path = PathBuf::from(path).canonicalize().unwrap();
    assert!(path.starts_with(library.join("Originals")));
    assert_eq!(std::fs::read(&path).unwrap(), expected, "copied original bytes for {}", p.file_name);
    assert_eq!((p.width, p.height), (64, 48));
    assert!(p.content_hash.as_ref().is_some_and(|hash| hash.len() == 32));
    path
}

fn png_pixels(bytes: &[u8]) -> lightcraft_raster::Rgb32f {
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let header = lightcraft_codecs::read_header(bytes).unwrap();
    assert_eq!((header.width, header.height), (32, 24));
    assert_eq!(header.format, lightcraft_codecs::Format::Png);
    let decoded = lightcraft_codecs::decode(bytes, Default::default()).unwrap();
    assert_eq!((decoded.image.width, decoded.image.height), (32, 24));
    assert!(decoded.image.data.iter().flatten().all(|v| v.is_finite()));
    decoded.image
}

#[test]
#[ignore = "needs a private TMPDIR and LIGHTCRAFT_REAL_JOURNEY_ROOT; run with --ignored (see the module docs)"]
#[cfg(not(target_arch = "wasm32"))]
fn persistent_selected_import_develop_undo_redo_reopen_export() {
    let root = fresh_root();
    let incoming = root.join("incoming");
    let library = root.join("library");
    let out = root.join("export");
    std::fs::create_dir(&incoming).unwrap();
    std::fs::create_dir(&out).unwrap();
    let chosen_input = incoming.join("chosen.png");
    let other_input = incoming.join("unchosen.png");
    let excluded_input = incoming.join("not-imported.png");
    let chosen_bytes = fixture(&chosen_input, 0);
    let other_bytes = fixture(&other_input, 1);
    let excluded_bytes = fixture(&excluded_input, 2);
    assert_ne!(chosen_bytes, other_bytes);

    let mut session = Session::new().with_fs();
    assert!(session.open_library(&library, false).unwrap().created);
    assert_eq!(session.catalog.len(), 0, "no demo seed or memory-only catalog");
    let mut h = headless(session, &out);
    // Explicit engine action: selected paths, not a directory scan or a host file picker.
    let imported = execute(
        &mut h,
        "library.import",
        json!({
            "paths": [chosen_input.to_string_lossy(), other_input.to_string_lossy()], "mode": "copy"
        }),
    );
    assert_eq!(imported["scanned"], 2, "{imported}");
    assert_eq!(imported["imported"].as_array().unwrap().len(), 2, "{imported}");
    assert_eq!(imported["failed"], json!([]), "{imported}");
    assert_eq!(imported["duplicates"], json!([]), "{imported}");
    assert_eq!(h.app.session.catalog.len(), 2);
    assert!(!h.app.session.catalog.photos().any(|p| p.file_name == "not-imported.png"));
    let chosen = h.app.session.catalog.photos().find(|p| p.file_name == "chosen.png").unwrap().id;
    let other = h.app.session.catalog.photos().find(|p| p.file_name == "unchosen.png").unwrap().id;
    assert_ne!(chosen, other);
    let chosen_copy = copied_path(&photo(&h, chosen), &library, &chosen_bytes);
    let other_copy = copied_path(&photo(&h, other), &library, &other_bytes);
    assert_ne!(chosen_copy, other_copy);
    let untouched = photo(&h, other);

    ask(&mut h, "ui.set", json!({"view": "photoGrid", "right": "edit"}));
    // Actual pointer activation of the exact photo's registered grid widget.
    ask(&mut h, "ui.clickWidget", json!({"id": format!("thumb:{}", chosen.0)}));
    selected(&h, chosen, other);
    execute(&mut h, "photo.rate", json!({"rating": 4})); // implicit selection, NOT explicit ids
    assert_eq!(photo(&h, chosen).rating, 4);
    assert_eq!(photo(&h, other), untouched, "rating must not affect the unchosen photo");
    execute(&mut h, "library.filter", json!({"rating": 4}));
    assert_eq!(h.app.session.visible_cloned(), vec![chosen]);
    selected(&h, chosen, other);

    // Explicit presentation action to the real Develop panel, not a native shortcut assertion.
    ask(&mut h, "ui.set", json!({"view": "detail", "right": "edit"}));
    h.app.ui.open_sections = vec!["light".into()];
    assert!(h.settle(SETTLE), "Develop panel did not settle");
    let options_json = json!({"format": "png", "longEdge": 32, "bitDepth": 8, "colorSpace": "srgb", "metadata": "none"});
    let options = ExportOptions::from_params(&options_json).unwrap();
    let baseline = export_photo(&mut h.app.session, chosen, &options, 1).unwrap();
    let baseline_pixels = png_pixels(&baseline.bytes);
    let before_edit = photo(&h, chosen);
    assert_eq!(before_edit.develop.light.exposure, 0.0);
    let undo_count = h.app.session.undo.len();
    // Actual widget click + text input + Enter, with no develop.set bypass.
    ask(&mut h, "ui.clickWidget", json!({"id": "sliderValue:light.exposure"}));
    ask(&mut h, "ui.text", json!({"text": "1.25"}));
    ask(&mut h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(photo(&h, chosen).develop.light.exposure, EXPOSURE);
    assert_eq!(h.app.session.undo.len(), undo_count + 1, "one committed typed-value edit");
    let edited = photo(&h, chosen);
    assert_ne!(edited.develop, before_edit.develop);
    assert_eq!(photo(&h, other), untouched);
    let reference = export_photo(&mut h.app.session, chosen, &options, 1).unwrap();
    let reference_pixels = png_pixels(&reference.bytes);
    assert_ne!(reference.bytes, baseline.bytes, "Develop must change encoded output");
    assert_ne!(reference_pixels, baseline_pixels, "Develop must change actual decoded pixels, not just metadata");
    let energy = |img: &lightcraft_raster::Rgb32f| img.data.iter().flatten().map(|v| f64::from(*v)).sum::<f64>();
    assert!(energy(&reference_pixels) > energy(&baseline_pixels) * 1.1, "positive Exposure must brighten the pixels");

    // Explicit engine Undo/Redo actions; no claim about OS keyboard accelerator handling.
    execute(&mut h, "edit.undo", json!({}));
    assert_eq!(photo(&h, chosen).develop, before_edit.develop);
    let undone = export_photo(&mut h.app.session, chosen, &options, 1).unwrap();
    assert_eq!(undone.bytes, baseline.bytes, "Undo restores exact rendered PNG bytes");
    assert_eq!(h.app.session.undo.len(), undo_count);
    execute(&mut h, "edit.redo", json!({}));
    assert_eq!(photo(&h, chosen).develop, edited.develop);
    let redone = export_photo(&mut h.app.session, chosen, &options, 1).unwrap();
    assert_eq!(redone.bytes, reference.bytes, "Redo restores exact rendered PNG bytes");
    assert_eq!(h.app.session.undo.len(), undo_count + 1);
    assert_eq!(photo(&h, other), untouched);
    selected(&h, chosen, other);
    let document_before_close = h.app.session.catalog.to_snapshot();
    let chosen_before_close = photo(&h, chosen);
    h.app.session.persist().unwrap();
    assert!(h.app.session.unsaved().is_none());
    h.app.session.close_library().unwrap();
    assert!(library.join("catalog.snap").is_file());
    assert!(library.join("view.json").is_file());
    let snapshot_bytes = std::fs::read(library.join("catalog.snap")).unwrap();
    assert!(!snapshot_bytes.is_empty());
    drop(h); // old app, Session, MediaCache and journal are discarded BEFORE reopening

    let mut reopened = Session::new().with_fs();
    assert!(!reopened.open_library(&library, false).unwrap().created);
    assert_eq!(reopened.library.as_ref().unwrap().dir, library.canonicalize().unwrap());
    assert_eq!(reopened.catalog.to_snapshot(), document_before_close, "whole persisted document, not a hand-built catalog");
    assert!(reopened.undo.is_empty() && reopened.redo.is_empty(), "Undo stacks are session-local, not persisted");
    let mut h = headless(reopened, &out);
    assert_eq!(photo(&h, chosen), chosen_before_close, "exact chosen identity/settings/rating/history from disk");
    assert_eq!(photo(&h, other), untouched, "exact unchosen document remains unchanged");
    selected(&h, chosen, other); // view.json selection persisted; no selection injection after reopen
    copied_path(&photo(&h, chosen), &library, &chosen_bytes);
    copied_path(&photo(&h, other), &library, &other_bytes);
    // Filters aren't in view.json: reapply deliberately, do NOT claim filter persistence.
    execute(&mut h, "library.filter", json!({"rating": 4}));
    assert_eq!(h.app.session.visible_cloned(), vec![chosen]);
    selected(&h, chosen, other);
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0, "references never wrote delivery files");
    let mut params = options_json;
    params["dir"] = json!(out.to_string_lossy());
    // No ids/path override: app.export must use the actual persisted selection.
    let exported = execute(&mut h, "app.export", params);
    let delivered = exported["files"].as_array().unwrap();
    assert_eq!(delivered.len(), 1, "{exported}");
    let target = out.join("chosen.png");
    assert_eq!(delivered[0]["path"], json!(target.to_string_lossy()), "{exported}");
    assert_eq!((delivered[0]["width"].as_u64(), delivered[0]["height"].as_u64()), (Some(32), Some(24)));
    assert_eq!(delivered[0]["bytes"], json!(reference.bytes.len()));
    let bytes = std::fs::read(&target).unwrap();
    assert_eq!(bytes, reference.bytes, "fresh-Session selected-only disk delivery is byte-for-byte the edited reference");
    assert_eq!(png_pixels(&bytes), reference_pixels);
    let files: Vec<_> = std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(files, vec![std::ffi::OsString::from("chosen.png")], "no unselected output or hidden sidecars");
    assert!(!out.join("unchosen.png").exists());
    assert!(!out.join("not-imported.png").exists());
    assert_eq!(photo(&h, chosen), chosen_before_close, "export does not edit the document");
    assert_eq!(photo(&h, other), untouched);
    assert_eq!(std::fs::read(&chosen_input).unwrap(), chosen_bytes);
    assert_eq!(std::fs::read(&other_input).unwrap(), other_bytes);
    assert_eq!(std::fs::read(&excluded_input).unwrap(), excluded_bytes);
    h.app.session.close_library().unwrap();
    drop(h);
    std::fs::write(root.join("PASS.json"), serde_json::to_vec_pretty(&json!({
        "boundary": "headless UI + real Session/FsStore + real PNG codec and private disk writer",
        "chosenId": chosen.0, "unchosenId": other.0,
        "exposure": EXPOSURE, "rating": 4, "width": 32, "height": 24,
        "exportBytes": bytes.len(), "catalogSnapshotBytes": snapshot_bytes.len(),
        "exportedFiles": ["chosen.png"], "oldSessionDroppedBeforeReopen": true,
        "widgetInput": ["thumb:<chosenId>", "sliderValue:light.exposure", "ui.text 1.25", "Enter"],
        "engineActions": ["library.import copy two paths", "photo.rate 4", "library.filter rating 4", "edit.undo", "edit.redo", "app.export selection"]
    })).unwrap()).unwrap();
    eprintln!("real journey passed; retained evidence: {}", root.display());
}
