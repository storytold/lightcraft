//! Import review sizes change the real grid and its preview resolution, preserving culling.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::import::ImportDialog;
use crate::render::Slot;
use crate::state::{Dialog, UiState};
use crate::{LightcraftApp, Services};
use lightcraft_engine::import::ImportCandidate;

const T: Duration = Duration::from_secs(30);

/// Size buckets follow physical pixels, with finite fallbacks and a fixed allocation ceiling.
#[test]
fn import_thumb_edges_follow_size_and_display_density() {
    use crate::import::thumb_edge;
    assert_eq!(thumb_edge(116.0, 1.0), 192);
    assert_eq!(thumb_edge(320.0, 1.0), 384);
    assert_eq!(thumb_edge(320.0, 2.0), 768);
    assert_eq!(thumb_edge(320.0, 3.0), 1024);
    assert_eq!(thumb_edge(320.0, f32::MAX), 1024);
    assert_eq!(thumb_edge(f32::NAN, f32::NAN), 192);
    assert_eq!(thumb_edge(116.0, -1.0), 192);
}

/// An isolated import dialog with real preview jobs and a disposable procedural source file.
struct Review {
    h: Headless,
    dir: std::path::PathBuf,
}

impl Review {
    /// Open sixty candidates at the requested display density, including duplicate/error badges.
    fn new(scale: f32) -> Self {
        Self::with_dimensions(scale, 1600, 1200)
    }

    fn with_dimensions(scale: f32, width: usize, height: usize) -> Self {
        let dir = std::env::temp_dir().join(format!("lc-import-thumbnails-{}-{}", std::process::id(), lightcraft_preview::next_tick()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = lightcraft_raster::Rgba8 {
            width,
            height,
            data: (0..width * height).map(|i| [((i % width) / 7) as u8, ((i / width) / 5) as u8, 100, 255]).collect(),
        };
        let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
        let path = dir.join("original.png");
        std::fs::write(&path, &bytes).unwrap();
        let mut candidates: Vec<_> = (0..60)
            .map(|i| ImportCandidate {
                path: path.to_string_lossy().into(),
                name: format!("a-long-photo-name-{i:02}.png"),
                format: "PNG".into(),
                width: width as u32,
                height: height as u32,
                file_size: bytes.len() as u64,
                ..Default::default()
            })
            .collect();
        candidates[1].duplicate = Some("content".into());
        candidates[2].error = Some("Unreadable test candidate".into());
        let mut app = LightcraftApp::new(lightcraft_engine::Session::new().with_fs(), Services::default());
        app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(candidates)) });
        let mut h = Headless::new(app, [1366.0, 1000.0], scale);
        assert!(h.settle(T));
        Self { h, dir }
    }

    /// Find an automation widget, failing the test if the dialog no longer exposes it.
    fn rect(&self, id: &str) -> egui::Rect {
        self.h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("missing {id}"))
    }

    /// Snapshot every candidate's checked state while requiring the review to remain open.
    fn checks(&self) -> Vec<bool> {
        let Some(Dialog::Import { opts }) = &self.h.app.ui.dialog else { panic!("import review closed") };
        opts.checked.clone()
    }

    /// Drive the real control dispatcher and fail on any unsuccessful response.
    fn request(&mut self, method: &str, params: serde_json::Value) {
        let response = self.h.request(method, params, T);
        assert_eq!(response["ok"], true, "{method}: {response}");
    }

    /// Set thumbnail size through its command and wait for the resulting previews to settle.
    fn size(&mut self, size: f32) {
        self.request("engine.execute", json!({"command": "view.importThumbSize", "params": {"size": size}}));
        assert!(self.h.settle(T));
    }
}

impl Drop for Review {
    /// Release import preview jobs before removing the temporary source directory.
    fn drop(&mut self) {
        self.h.app.ui.dialog = None;
        self.h.app.renderer.forget_imports();
        self.h.settle(T);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Pointer input must resize and reflow tiles without changing culling, badges or saved size.
#[test]
fn import_thumbnail_slider_reflows_without_changing_selection_or_badges() {
    let mut review = Review::new(1.0);
    assert_eq!(review.rect("import:0").width(), 116.0);
    review.request("ui.clickWidget", json!({"id": "import:0"}));
    let checked = review.checks();
    assert!(!checked[0] && !checked[1] && !checked[2]);
    // Real pointer input on the slider, including its end stops.
    review.request("ui.clickWidget", json!({"id": "slider:importThumbSize", "fx": 0.0}));
    assert!(review.h.settle(T));
    assert_eq!(review.rect("import:0").width(), 80.0);
    let slider = review.rect("slider:importThumbSize");
    review.request("ui.dragWidget", json!({"id": "slider:importThumbSize", "toX": slider.right(), "toY": slider.center().y}));
    assert!(review.h.settle(T));
    assert_eq!(review.rect("import:0").width(), 320.0);
    assert_eq!(review.checks(), checked);
    let text = review.h.painted_text().join("\n");
    assert!(text.contains("Duplicate") && text.contains("Unreadable"), "badges remain visible: {text}");
    assert!(review.rect("import:3").top() > review.rect("import:0").top(), "larger tiles wrap into more rows");
    review.size(80.0);
    assert_eq!(review.rect("import:3").top(), review.rect("import:0").top());
    assert_eq!(review.checks(), checked);
    // The preference survives closing/reopening the review and a settings round trip.
    let saved = serde_json::to_value(&review.h.app.ui).unwrap();
    let restored: UiState = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.sanitized().import_thumb_size, 80.0);
}

/// Dense displays need sharper previews; shrinking reuses them and scrolling bounds retention.
#[test]
fn import_thumbnail_previews_upgrade_for_density_and_stay_bounded_when_scrolling() {
    let mut review = Review::new(2.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].size, [384, 288]);
    review.size(320.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].size, [768, 576]);
    // Shrinking keeps the sharp preview instead of replacing it with a smaller decode.
    let sharp = review.h.app.renderer.textures[&Slot::Import(0)].key;
    review.size(116.0);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].key, sharp);
    review.size(320.0);
    // Scrolling a large card retains only the nearby tiles and drops obsolete requests.
    review.request("ui.hoverWidget", json!({"id": "import:0"}));
    review.request("ui.scroll", json!({"dy": -20000.0}));
    assert!(review.h.settle(T));
    assert!(!review.h.app.renderer.textures.contains_key(&Slot::Import(0)));
    let imports: Vec<_> = review.h.app.renderer.textures.keys().filter_map(|s| if let Slot::Import(i) = s { Some(*i) } else { None }).collect();
    assert!(!imports.is_empty() && imports.len() < 20, "bounded visible previews: {imports:?}");
    for i in imports {
        assert!(review.h.app.widgets.iter().any(|(w, _)| w == &format!("import:{i}")));
    }
}

/// A preview smaller than its requested bucket is still built only once while the grid is idle.
#[test]
fn import_thumbnail_small_sources_do_not_rebuild_jobs_every_frame() {
    let mut review = Review::with_dimensions(2.0, 64, 48);
    assert_eq!(review.h.app.renderer.textures[&Slot::Import(0)].size, [64, 48]);
    for size in [116.0, 320.0, 80.0] {
        review.size(size);
        let built = review.h.app.renderer.import_jobs_built;
        for _ in 0..60 {
            review.h.step();
        }
        assert_eq!(review.h.app.renderer.import_jobs_built, built, "small sources already satisfy their requested bucket");
    }
}

/// Closing the review releases large previews through the button, Escape and control channel.
#[test]
fn import_thumbnail_close_releases_previews_for_all_cancel_paths() {
    for dismissal in ["button", "escape", "control"] {
        let mut review = Review::new(1.0);
        assert!(review.h.app.renderer.textures.contains_key(&Slot::Import(0)));
        match dismissal {
            "escape" => review.request("ui.key", json!({"key": "Escape"})),
            "control" => review.request("ui.dialog.cancel", json!({})),
            _ => review.request("ui.clickWidget", json!({"id": "button:dialogCancel"})),
        }
        assert!(review.h.settle(T));
        assert!(review.h.app.ui.dialog.is_none());
        assert!(review.h.app.renderer.textures.keys().all(|slot| !matches!(slot, Slot::Import(_))));
        assert!(!review.h.app.renderer.is_pending(Slot::Import(0)));
    }
}

/// Invalid commands preserve the setting; legacy and non-finite preferences restore safely.
#[test]
fn import_thumbnail_size_rejects_bad_commands_and_restores_old_preferences() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    assert_eq!(app.run("view.importThumbSize", json!({})).unwrap()["size"], 116.0);
    for bad in [json!("large"), json!(null), json!([120]), json!(1e300)] {
        assert!(app.run("view.importThumbSize", json!({"size": bad})).is_err());
        assert_eq!(app.ui.import_thumb_size, 116.0);
    }
    assert_eq!(app.run("view.importThumbSize", json!({"size": -10})).unwrap()["size"], 80.0);
    assert_eq!(app.run("view.importThumbSize", json!({"size": 1e6})).unwrap()["size"], 320.0);
    let legacy: UiState = serde_json::from_value(json!({})).unwrap();
    assert_eq!(legacy.import_thumb_size, 116.0);
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let state = UiState { import_thumb_size: bad, ..Default::default() }.sanitized();
        assert_eq!(state.import_thumb_size, 116.0);
    }
}

/// A selected import slider keeps keyboard nudges after idle frames without changing candidate checks.
#[test]
fn import_thumbnail_slider_keeps_the_shared_keyboard_contract() {
    let mut review = Review::new(1.0);
    let checked = review.checks();
    review.request("ui.clickWidget", json!({"id": "slider:importThumbSize", "fx": 0.4}));
    assert!(review.h.settle(T));
    for _ in 0..6 {
        review.h.step();
    }
    let before = review.h.app.ui.import_thumb_size;
    review.request("ui.key", json!({"key": "ArrowUp"}));
    assert!(review.h.settle(T));
    assert_eq!(review.h.app.ui.import_thumb_size, before + 1.0);
    assert_eq!(review.checks(), checked);
}

/// Escape cancels the numeric draft before a later Escape dismisses the enclosing review.
#[test]
fn import_thumbnail_exact_entry_escape_keeps_the_review_open() {
    let mut review = Review::new(1.0);
    let checked = review.checks();
    for (key, expected) in [("Escape", 116.0), ("Enter", 240.0)] {
        review.request("ui.clickWidget", json!({"id": "sliderValue:importThumbSize"}));
        review.request("ui.key", json!({"key": "a", "cmd": true}));
        review.request("ui.text", json!({"text": "240"}));
        review.request("ui.key", json!({"key": key}));
        assert!(review.h.settle(T));
        assert_eq!(review.h.app.ui.import_thumb_size, expected);
        assert_eq!(review.checks(), checked, "the review remains open after exact-value editing");
    }
    review.request("ui.key", json!({"key": "Escape"}));
    assert!(review.h.app.ui.dialog.is_none(), "a later Escape still dismisses the review");
}

/// Every shipped language translates the slider and command labels, with the real control visible at high density.
#[test]
fn import_thumbnail_labels_are_localized_in_every_shipped_language() {
    use crate::i18n::Locale;
    let command =
        crate::menus::UI_COMMANDS.iter().find(|command| command.0 == "view.importThumbSize").expect("the import-size command is registered");
    assert_eq!(command.1, "Import Thumbnail Size", "the catalog key matches the command's actual label");
    for theme in ["charcoal", "paper"] {
        for scale in [1.0, 2.25] {
            let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services { png: None, ..Default::default() });
            app.ui.dialog = Some(Dialog::Import { opts: Box::new(ImportDialog::new(Vec::new())) });
            let mut h = Headless::new(app, [960.0, 720.0], scale);
            assert!(h.app.run("view.theme", json!({"theme": theme})).is_ok());
            for locale in Locale::ALL {
                for source in ["Thumbnail size", "Import Thumbnail Size", "Click to type a value, drag to change it"] {
                    assert!(!locale.tr(source).is_empty());
                    if *locale != Locale::En {
                        assert_ne!(locale.tr(source), source, "{locale:?}: {source}");
                    }
                }
                let result = h.request("ui.set", json!({"language": locale.code()}), T);
                assert_eq!(result["ok"], true, "{result}");
                assert!(h.settle(T));
                assert!(h.painted_text().iter().any(|text| text == locale.tr("Thumbnail size")), "{locale:?}: translated slider label");
                let track = h.app.widgets.iter().find(|(id, _)| id == "slider:importThumbSize").expect("the import slider is visible").1;
                let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, h.size);
                assert!(viewport.contains_rect(track), "{locale:?} {theme} {scale}: slider stays inside the viewport");
            }
        }
    }
}
