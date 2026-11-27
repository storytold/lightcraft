//! Texts that were drawn in English whatever the UI language (#911): the Layout Image toasts, the
//! export dialog's DNG and Original notes, Visualize HDR, and a face's Add name label.

use std::time::Duration;

use serde_json::json;

use crate::LightcraftApp;
use crate::headless::Headless;
use crate::i18n::{Locale, set_language};

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(120);

/// The demo library with the UI in German, in `view` with `right` as the right panel.
fn german(view: &str, right: &str) -> Headless {
    let services = crate::Services { png: None, ..Default::default() };
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    app.ui.settings.gpu = false;
    app.ui.language = Locale::De;
    set_language(Locale::De);
    let mut h = Headless::new(app, [1400.0, 900.0], 1.0);
    let r = h.request("ui.set", json!({"view": view, "right": right}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    h
}

fn back_to_english(h: &mut Headless) {
    h.app.ui.language = Locale::En;
    set_language(Locale::En);
}

fn shows(h: &Headless, text: &str) -> bool {
    h.painted_text().iter().any(|t| t.contains(text))
}

fn toast(h: &Headless) -> Option<String> {
    h.app.ui.toast.as_ref().map(|t| t.0.clone())
}

/// A small PNG to use as the layout image.
fn layout_png() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lc-ui-language-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let img = lightcraft_raster::Rgba8::from_fn(40, 30, |_, y| if y < 10 { [255, 0, 255, 255] } else { [0, 0, 0, 0] });
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&img), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    let path = dir.join("layout.png");
    std::fs::write(&path, png).unwrap();
    path
}

#[test]
fn the_layout_image_toasts_are_in_the_ui_language() {
    let mut h = german("detail", "edit");
    let path = layout_png();
    let r = h.request("engine.execute", json!({"command": "view.chooseLayoutImage", "params": {"path": path.to_string_lossy()}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(toast(&h).as_deref(), Some("Layoutbild ein"));
    let r = h.request("engine.execute", json!({"command": "view.layoutImage", "params": {"show": false}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(toast(&h).as_deref(), Some("Layoutbild aus"));
    back_to_english(&mut h);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn the_export_dialogs_dng_and_original_notes_are_in_the_ui_language() {
    use lightcraft_engine::export::ExportFormat;
    let mut h = german("photoGrid", "none");
    h.app.services.pick_folder = Some(Box::new(|| None));
    let r = h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    for (format, german) in
        [(ExportFormat::Dng, "RAW-Fotos als DNG mit eingebetteten Bearbeitungen"), (ExportFormat::Original, "Unveränderte Originaldateien")]
    {
        if let Some(crate::state::Dialog::Export { opts, .. }) = &mut h.app.ui.dialog {
            opts.format = format;
        }
        h.step();
        h.step();
        assert!(
            shows(&h, german),
            "{format:?}: {:?}",
            h.painted_text().iter().filter(|t| t.contains("DNG") || t.contains("XMP")).collect::<Vec<_>>()
        );
    }
    back_to_english(&mut h);
}

#[test]
fn visualize_hdr_is_in_the_ui_language() {
    let mut h = german("detail", "edit");
    let r = h.request("engine.execute", json!({"command": "develop.merge", "params": {"settings": {"hdr": {"enabled": true}}}}), T);
    assert_eq!(r["ok"], true, "{r}");
    assert!(
        h.step_until(T, |h| shows(h, "HDR visualisieren")),
        "the HDR settings slide open: {:?}",
        h.painted_text().iter().filter(|t| t.contains("HDR")).collect::<Vec<_>>()
    );
    back_to_english(&mut h);
}

#[test]
fn a_faces_add_name_label_is_in_the_ui_language() {
    use lightcraft_catalog::Op;
    let mut h = german("detail", "none");
    let id = h.app.session.active().unwrap();
    let mut meta = h.app.session.catalog.photo(id).unwrap().meta.clone();
    meta.regions = vec![lightcraft_meta::Region {
        rect: lightcraft_geom::Rect { x0: 0.3, y0: 0.2, x1: 0.55, y1: 0.6 },
        kind: lightcraft_meta::RegionKind::Face,
        name: None,
        description: None,
    }];
    h.app.session.commit("setup", Op::SetMeta { id, meta: Box::new(meta) }).unwrap();
    h.settle(SETTLE);
    // pointing at the face shows the invitation to name it
    h.request("ui.pointer", json!({"events": [{"kind": "move", "x": 0.42, "y": 0.4}]}), T);
    assert!(h.step_until(T, |h| shows(h, "Namen hinzufügen")), "{:?}", h.painted_text());
    back_to_english(&mut h);
}
