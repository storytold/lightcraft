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

/// The journey's private folders and fixture files, made once before any session opens.
struct Stage {
    library: PathBuf,
    out: PathBuf,
    chosen_input: PathBuf,
    other_input: PathBuf,
    excluded_input: PathBuf,
    chosen_bytes: Vec<u8>,
    other_bytes: Vec<u8>,
    excluded_bytes: Vec<u8>,
}

fn stage(root: &Path) -> Stage {
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
    Stage { library, out, chosen_input, other_input, excluded_input, chosen_bytes, other_bytes, excluded_bytes }
}

/// The journey's own library and what each phase checks against.
struct Journey {
    stage: Stage,
    chosen: PhotoId,
    other: PhotoId,
    untouched: Photo,
    options: ExportOptions,
    options_json: Value,
}

/// What the develop edit produced, so Undo, Redo and the export compare against it.
struct Rendered {
    baseline: Vec<u8>,
    reference: Vec<u8>,
    reference_pixels: lightcraft_raster::Rgb32f,
    before_edit: Photo,
    edited: Photo,
    undo_count: usize,
}

/// What survives the close and is checked again after reopening.
struct Persisted {
    chosen_before_close: Photo,
    snapshot_bytes: Vec<u8>,
}

/// Total light in a rendered image: a brighter render must sum higher.
fn energy(img: &lightcraft_raster::Rgb32f) -> f64 {
    img.data.iter().flatten().map(|v| f64::from(*v)).sum()
}

/// Open a real library in the journey's private folders and import the chosen files through the
/// engine; the third fixture stays out.
fn open_and_import(stage: Stage) -> (Headless, Journey) {
    let mut session = Session::new().with_fs();
    assert!(session.open_library(&stage.library, false).unwrap().created);
    assert_eq!(session.catalog.len(), 0, "no demo seed or memory-only catalog");
    let mut h = headless(session, &stage.out);
    // Explicit engine action: selected paths, not a directory scan or a host file picker.
    let paths = [stage.chosen_input.to_string_lossy(), stage.other_input.to_string_lossy()];
    let imported = execute(&mut h, "library.import", json!({"paths": paths, "mode": "copy"}));
    assert_eq!(imported["scanned"], 2, "{imported}");
    assert_eq!(imported["imported"].as_array().unwrap().len(), 2, "{imported}");
    assert_eq!(imported["failed"], json!([]), "{imported}");
    assert_eq!(imported["duplicates"], json!([]), "{imported}");
    assert_eq!(h.app.session.catalog.len(), 2);
    assert!(!h.app.session.catalog.photos().any(|p| p.file_name == "not-imported.png"));
    let chosen = h.app.session.catalog.photos().find(|p| p.file_name == "chosen.png").unwrap().id;
    let other = h.app.session.catalog.photos().find(|p| p.file_name == "unchosen.png").unwrap().id;
    assert_ne!(chosen, other);
    let chosen_copy = copied_path(&photo(&h, chosen), &stage.library, &stage.chosen_bytes);
    let other_copy = copied_path(&photo(&h, other), &stage.library, &stage.other_bytes);
    assert_ne!(chosen_copy, other_copy);
    let untouched = photo(&h, other);
    let options_json = json!({"format": "png", "longEdge": 32, "bitDepth": 8, "colorSpace": "srgb", "metadata": "none"});
    let options = ExportOptions::from_params(&options_json).unwrap();
    (h, Journey { stage, chosen, other, untouched, options, options_json })
}

/// Rate the chosen photo by pointer, filter on the rating, and keep the selection.
fn rate_and_filter(h: &mut Headless, j: &Journey) {
    ask(h, "ui.set", json!({"view": "photoGrid", "right": "edit"}));
    // Actual pointer activation of the exact photo's registered grid widget.
    ask(h, "ui.clickWidget", json!({"id": format!("thumb:{}", j.chosen.0)}));
    selected(h, j.chosen, j.other);
    execute(h, "photo.rate", json!({"rating": 4})); // implicit selection, NOT explicit ids
    assert_eq!(photo(h, j.chosen).rating, 4);
    assert_eq!(photo(h, j.other), j.untouched, "rating must not affect the unchosen photo");
    execute(h, "library.filter", json!({"rating": 4}));
    assert_eq!(h.app.session.visible_cloned(), vec![j.chosen]);
    selected(h, j.chosen, j.other);
}

/// Render the baseline, type the exposure into the real slider field, and render the reference.
fn develop(h: &mut Headless, j: &Journey) -> Rendered {
    // Explicit presentation action to the real Develop panel, not a native shortcut assertion.
    ask(h, "ui.set", json!({"view": "detail", "right": "edit"}));
    h.app.ui.open_sections = vec!["light".into()];
    assert!(h.settle(SETTLE), "Develop panel did not settle");
    let baseline = export_photo(&mut h.app.session, j.chosen, &j.options, 1).unwrap();
    let baseline_pixels = png_pixels(&baseline.bytes);
    let before_edit = photo(h, j.chosen);
    assert_eq!(before_edit.develop.light.exposure, 0.0);
    let undo_count = h.app.session.undo.len();
    // Actual widget click + text input + Enter, with no develop.set bypass.
    ask(h, "ui.clickWidget", json!({"id": "sliderValue:light.exposure"}));
    ask(h, "ui.text", json!({"text": "1.25"}));
    ask(h, "ui.key", json!({"key": "Enter"}));
    assert_eq!(photo(h, j.chosen).develop.light.exposure, EXPOSURE);
    assert_eq!(h.app.session.undo.len(), undo_count + 1, "one committed typed-value edit");
    let edited = photo(h, j.chosen);
    assert_ne!(edited.develop, before_edit.develop);
    assert_eq!(photo(h, j.other), j.untouched);
    let reference = export_photo(&mut h.app.session, j.chosen, &j.options, 1).unwrap();
    let reference_pixels = png_pixels(&reference.bytes);
    assert_ne!(reference.bytes, baseline.bytes, "Develop must change encoded output");
    assert_ne!(reference_pixels, baseline_pixels, "Develop must change actual decoded pixels, not just metadata");
    assert!(energy(&reference_pixels) > energy(&baseline_pixels) * 1.1, "positive Exposure must brighten the pixels");
    Rendered { baseline: baseline.bytes, reference: reference.bytes, reference_pixels, before_edit, edited, undo_count }
}

/// Explicit engine Undo/Redo actions; no claim about OS keyboard accelerator handling.
fn undo_redo(h: &mut Headless, j: &Journey, r: &Rendered) {
    execute(h, "edit.undo", json!({}));
    assert_eq!(photo(h, j.chosen).develop, r.before_edit.develop);
    let undone = export_photo(&mut h.app.session, j.chosen, &j.options, 1).unwrap();
    assert_eq!(undone.bytes, r.baseline, "Undo restores exact rendered PNG bytes");
    assert_eq!(h.app.session.undo.len(), r.undo_count);
    execute(h, "edit.redo", json!({}));
    assert_eq!(photo(h, j.chosen).develop, r.edited.develop);
    let redone = export_photo(&mut h.app.session, j.chosen, &j.options, 1).unwrap();
    assert_eq!(redone.bytes, r.reference, "Redo restores exact rendered PNG bytes");
    assert_eq!(h.app.session.undo.len(), r.undo_count + 1);
    assert_eq!(photo(h, j.other), j.untouched);
    selected(h, j.chosen, j.other);
}

/// Persist, close and drop the old app, then reopen a fresh session on the same folders.
fn persist_and_reopen(mut h: Headless, j: &Journey) -> (Headless, Persisted) {
    let document_before_close = h.app.session.catalog.to_snapshot();
    let chosen_before_close = photo(&h, j.chosen);
    h.app.session.persist().unwrap();
    assert!(h.app.session.unsaved().is_none());
    h.app.session.close_library().unwrap();
    assert!(j.stage.library.join("catalog.snap").is_file());
    assert!(j.stage.library.join("view.json").is_file());
    let snapshot_bytes = std::fs::read(j.stage.library.join("catalog.snap")).unwrap();
    assert!(!snapshot_bytes.is_empty());
    drop(h); // old app, Session, MediaCache and journal are discarded BEFORE reopening

    let mut reopened = Session::new().with_fs();
    assert!(!reopened.open_library(&j.stage.library, false).unwrap().created);
    assert_eq!(reopened.library.as_ref().unwrap().dir, j.stage.library.canonicalize().unwrap());
    assert_eq!(reopened.catalog.to_snapshot(), document_before_close, "whole persisted document, not a hand-built catalog");
    assert!(reopened.undo.is_empty() && reopened.redo.is_empty(), "Undo stacks are session-local, not persisted");
    let mut h = headless(reopened, &j.stage.out);
    assert_eq!(photo(&h, j.chosen), chosen_before_close, "exact chosen identity/settings/rating/history from disk");
    assert_eq!(photo(&h, j.other), j.untouched, "exact unchosen document remains unchanged");
    selected(&h, j.chosen, j.other); // view.json selection persisted; no selection injection after reopen
    copied_path(&photo(&h, j.chosen), &j.stage.library, &j.stage.chosen_bytes);
    copied_path(&photo(&h, j.other), &j.stage.library, &j.stage.other_bytes);
    // Filters aren't in view.json: reapply deliberately, do NOT claim filter persistence.
    execute(&mut h, "library.filter", json!({"rating": 4}));
    assert_eq!(h.app.session.visible_cloned(), vec![j.chosen]);
    selected(&h, j.chosen, j.other);
    assert_eq!(std::fs::read_dir(&j.stage.out).unwrap().count(), 0, "references never wrote delivery files");
    (h, Persisted { chosen_before_close, snapshot_bytes })
}

/// Export the persisted selection through the app command and check the one delivered file.
fn export_selection(h: &mut Headless, j: &Journey, r: &Rendered, p: &Persisted) -> Vec<u8> {
    let mut params = j.options_json.clone();
    params["dir"] = json!(j.stage.out.to_string_lossy());
    // No ids/path override: app.export must use the actual persisted selection.
    let exported = execute(h, "app.export", params);
    let delivered = exported["files"].as_array().unwrap();
    assert_eq!(delivered.len(), 1, "{exported}");
    let target = j.stage.out.join("chosen.png");
    assert_eq!(delivered[0]["path"], json!(target.to_string_lossy()), "{exported}");
    assert_eq!((delivered[0]["width"].as_u64(), delivered[0]["height"].as_u64()), (Some(32), Some(24)));
    assert_eq!(delivered[0]["bytes"], json!(r.reference.len()));
    let bytes = std::fs::read(&target).unwrap();
    assert_eq!(bytes, r.reference, "fresh-Session selected-only disk delivery is byte-for-byte the edited reference");
    assert_eq!(png_pixels(&bytes), r.reference_pixels);
    let files: Vec<_> = std::fs::read_dir(&j.stage.out).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(files, vec![std::ffi::OsString::from("chosen.png")], "no unselected output or hidden sidecars");
    assert!(!j.stage.out.join("unchosen.png").exists());
    assert!(!j.stage.out.join("not-imported.png").exists());
    assert_eq!(photo(h, j.chosen), p.chosen_before_close, "export does not edit the document");
    assert_eq!(photo(h, j.other), j.untouched);
    assert_eq!(std::fs::read(&j.stage.chosen_input).unwrap(), j.stage.chosen_bytes);
    assert_eq!(std::fs::read(&j.stage.other_input).unwrap(), j.stage.other_bytes);
    assert_eq!(std::fs::read(&j.stage.excluded_input).unwrap(), j.stage.excluded_bytes);
    h.app.session.close_library().unwrap();
    bytes
}

/// The evidence the executor keeps after the journey passed.
fn write_evidence(root: &Path, j: &Journey, p: &Persisted, delivered: &[u8]) {
    let evidence = json!({
        "boundary": "headless UI + real Session/FsStore + real PNG codec and private disk writer",
        "chosenId": j.chosen.0, "unchosenId": j.other.0,
        "exposure": EXPOSURE, "rating": 4, "width": 32, "height": 24,
        "exportBytes": delivered.len(), "catalogSnapshotBytes": p.snapshot_bytes.len(),
        "exportedFiles": ["chosen.png"], "oldSessionDroppedBeforeReopen": true,
        "widgetInput": ["thumb:<chosenId>", "sliderValue:light.exposure", "ui.text 1.25", "Enter"],
        "engineActions": ["library.import copy two paths", "photo.rate 4", "library.filter rating 4", "edit.undo", "edit.redo", "app.export selection"]
    });
    std::fs::write(root.join("PASS.json"), serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
}

#[test]
#[ignore = "needs a private TMPDIR and LIGHTCRAFT_REAL_JOURNEY_ROOT; run with --ignored (see the module docs)"]
#[cfg(not(target_arch = "wasm32"))]
fn persistent_selected_import_develop_undo_redo_reopen_export() {
    let root = fresh_root();
    let (mut h, j) = open_and_import(stage(&root));
    rate_and_filter(&mut h, &j);
    let rendered = develop(&mut h, &j);
    undo_redo(&mut h, &j, &rendered);
    let (mut h, persisted) = persist_and_reopen(h, &j);
    let delivered = export_selection(&mut h, &j, &rendered, &persisted);
    drop(h);
    write_evidence(&root, &j, &persisted, &delivered);
    eprintln!("real journey passed; retained evidence: {}", root.display());
}
