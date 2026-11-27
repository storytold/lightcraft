//! Headless tests of the Export dialog's dropdowns: Format, Amount and Metadata are a borderless
//! dropdown, Color space, Sharpen and Resize to a boxed one, instead of rows of buttons.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::state::Dialog;
use crate::{LightcraftApp, Services};
use lightcraft_engine::export::{
    ExportFormat as F, ExportOptions, MetadataPolicy as M, OutputSpace as C, ResizeMode as R, SharpenAmount as A, SharpenFor as S,
};

const T: Duration = Duration::from_secs(20);

/// The demo library with the Export dialog open.
fn export_dialog() -> Headless {
    let services = Services { png: None, ..Default::default() };
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), services);
    let mut h = Headless::new(app, [1600.0, 1200.0], 1.0);
    let r = h.request("engine.execute", json!({"command": "dialog.export", "params": {}}), T);
    assert_eq!(r["ok"], true, "{r}");
    if let Some(Dialog::Export { full_size, .. }) = &mut h.app.ui.dialog {
        *full_size = false;
    }
    h.step();
    h
}

fn opts(h: &Headless) -> &ExportOptions {
    match &h.app.ui.dialog {
        Some(Dialog::Export { opts, .. }) => opts,
        _ => panic!("the Export dialog is open"),
    }
}

fn has_widget(h: &Headless, prefix: &str) -> bool {
    h.app.widgets.iter().any(|(id, _)| id.starts_with(prefix))
}

fn click(h: &mut Headless, id: &str) {
    let r = h.request("ui.clickWidget", json!({"id": id}), T);
    assert_eq!(r["ok"], true, "{id}: {r}");
}

/// Open the dropdown `opener` (`dropdown:{id}` or `combo:{id}`) and choose its item `key`.
fn choose(h: &mut Headless, opener: &str, id: &str, key: &str) {
    click(h, &format!("{opener}:{id}"));
    h.step();
    click(h, &format!("{id}:{key}"));
    h.step();
    assert!(!has_widget(h, &format!("{id}:{key}")), "{id}: the menu closed");
}

/// Each row is a dropdown, with no row of buttons left, and choosing an item sets the option the
/// button did; Amount shows only while Sharpen is on, as before.
#[test]
fn export_choices_are_dropdowns() {
    let mut h = export_dialog();
    for (opener, id) in [
        ("dropdown", "exportFormat"),
        ("combo", "exportColorSpace"),
        ("combo", "exportResizeMode"),
        ("combo", "exportSharpen"),
        ("dropdown", "exportMetadata"),
    ] {
        assert!(has_widget(&h, &format!("{opener}:{id}")), "{opener}:{id}");
        assert!(!has_widget(&h, &format!("button:{id}-")), "no row of {id} buttons");
    }
    choose(&mut h, "dropdown", "exportFormat", "tiff");
    assert_eq!(opts(&h).format, F::Tiff);
    choose(&mut h, "combo", "exportColorSpace", "adobeRgb");
    assert_eq!(opts(&h).color_space, C::AdobeRgb);
    choose(&mut h, "combo", "exportSharpen", "none");
    assert_eq!(opts(&h).sharpen, S::None);
    assert!(!has_widget(&h, "dropdown:exportSharpenAmount"), "no Amount without sharpening");
    choose(&mut h, "combo", "exportSharpen", "matte");
    assert_eq!(opts(&h).sharpen, S::Matte);
    choose(&mut h, "dropdown", "exportSharpenAmount", "high");
    assert_eq!(opts(&h).sharpen_amount, A::High);
    choose(&mut h, "dropdown", "exportMetadata", "copyright");
    assert_eq!(opts(&h).metadata, M::Copyright);
    choose(&mut h, "dropdown", "exportFormat", "original");
    assert_eq!(opts(&h).format, F::Original);
    assert!(!has_widget(&h, "combo:exportSharpen"), "original files have no output options");
}

/// Rendered DNG is a DNG option, so the Format dropdown says DNG for it, and choosing DNG again
/// keeps the rendered kind.
#[test]
fn rendered_dng_shows_as_dng() {
    let mut h = export_dialog();
    choose(&mut h, "dropdown", "exportFormat", "dng");
    click(&mut h, "button:exportDngKind-1");
    h.step();
    assert_eq!(opts(&h).format, F::RenderedDng);
    choose(&mut h, "dropdown", "exportFormat", "dng");
    assert_eq!(opts(&h).format, F::RenderedDng, "DNG again keeps Rendered photo");
}

/// Resize to, now built on the shared boxed dropdown, still gives a new unit a sensible value.
#[test]
fn resize_mode_still_resets_the_value_for_a_new_unit() {
    let mut h = export_dialog();
    choose(&mut h, "combo", "exportResizeMode", "percent");
    let Some(Dialog::Export { resize, .. }) = &h.app.ui.dialog else { panic!("dialog") };
    assert_eq!(resize.mode, R::Percent);
    assert!(resize.value <= 100.0, "a percentage, not a pixel count: {}", resize.value);
    choose(&mut h, "combo", "exportResizeMode", "longEdge");
    let Some(Dialog::Export { resize, .. }) = &h.app.ui.dialog else { panic!("dialog") };
    assert!(resize.value > 100.0, "back to pixels: {}", resize.value);
}
