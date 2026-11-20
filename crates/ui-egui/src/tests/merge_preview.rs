//! Headless tests of the Photo Merge dialog's preview: click to zoom in, drag to pan, click to fit.

use std::time::Duration;

use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(120);

fn rect(h: &Headless, id: &str) -> egui::Rect {
    h.app.widgets.iter().find(|(w, _)| w == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no widget {id}"))
}

fn press(h: &mut Headless, pos: egui::Pos2, pressed: bool) {
    h.app.synthetic.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
}

fn click_at(h: &mut Headless, pos: egui::Pos2) {
    h.app.synthetic.push(egui::Event::PointerMoved(pos));
    press(h, pos, true);
    press(h, pos, false);
    h.settle(T);
}

#[test]
fn a_click_on_the_merge_preview_zooms_in_and_a_drag_pans() {
    let dir = std::env::temp_dir().join(format!("lc-merge-zoom-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, bytes) in lightcraft_merge::synth::bracket_dngs(1600, 1200, &[-2.0, 0.0, 2.0]).unwrap().into_iter().enumerate() {
        let p = dir.join(format!("IMG_{i}.dng"));
        std::fs::write(&p, bytes).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    crate::merge::open(&mut h.app, "merge.hdr").unwrap();
    assert!(h.step_until(T, |h| h.app.merge.preview.is_some() && h.app.merge.preview_task.is_none()), "the preview rendered");
    h.settle(T);
    assert_eq!(h.app.merge.zoom, None, "the preview opens fitted");

    let area = rect(&h, "merge.preview");
    // right of and above the middle: a point of the fitted image whatever its aspect
    let at = area.center() + egui::vec2(area.width() * 0.2, area.height() * -0.1);
    click_at(&mut h, at);
    let (u, v) = h.app.merge.zoom.expect("the click zoomed in");
    assert!(u > 0.55 && v < 0.5, "zoomed in on the point clicked ({u}, {v})");

    // drag left: the image follows the pointer, so a point further right comes to the middle
    let from = area.center();
    let to = from - egui::vec2(60.0, 0.0);
    h.app.synthetic.push(egui::Event::PointerMoved(from));
    press(&mut h, from, true);
    h.step();
    for i in 1..=6 {
        h.app.synthetic.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    press(&mut h, to, false);
    h.settle(T);
    let (u2, v2) = h.app.merge.zoom.expect("still zoomed in after a drag");
    assert!(u2 > u + 0.01 && (v2 - v).abs() < 1e-3, "the drag panned right ({u}, {v}) → ({u2}, {v2})");

    click_at(&mut h, area.center());
    assert_eq!(h.app.merge.zoom, None, "a second click fits the preview again");
    let _ = std::fs::remove_dir_all(dir);
}

/// Three overlapping panorama photos imported and selected, and the Panorama dialog open on them
/// with its preview made (this computer has the memory for its full size). The directory is the
/// photos'.
fn panorama_dialog(tag: &str) -> (Headless, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("lc-merge-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut paths = Vec::new();
    for (i, v) in lightcraft_merge::synth::pano_views(800, 600, 700.0, &[-30.0, 0.0, 30.0]).unwrap().into_iter().enumerate() {
        let png =
            lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&v.to_srgb8()), &lightcraft_codecs::EncodeMeta::default()).unwrap();
        let p = dir.join(format!("P_{i}.png"));
        std::fs::write(&p, png).unwrap();
        paths.push(p.to_string_lossy().to_string());
    }
    let mut session = lightcraft_engine::Session::new().with_fs();
    let r = session.execute("library.import", &json!({"paths": paths})).unwrap();
    let ids: Vec<u64> = r["imported"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    session.execute("library.select", &json!({"ids": ids})).unwrap();
    let mut h = Headless::new(LightcraftApp::new(session, Services { png: None, ..Default::default() }), [1200.0, 800.0], 1.0);
    h.settle(T);
    reopen(&mut h);
    (h, dir)
}

/// Open the Panorama dialog again and wait for its preview.
fn reopen(h: &mut Headless) {
    crate::merge::open(&mut h.app, "merge.panorama").unwrap();
    assert!(h.step_until(T, |h| h.app.merge.preview.is_some() && h.app.merge.preview_task.is_none()), "the preview rendered");
    h.settle(T);
}

/// The preview's forecast, as a computer with other memory makes it.
fn set_forecast(h: &mut Headless, f: serde_json::Value) {
    if let Some(p) = h.app.merge.preview.as_mut() {
        p.3["forecast"] = f;
    }
    h.settle(T);
}

/// Tick "Merge at the smaller size".
fn agree(h: &mut Headless) {
    let r = h.request("ui.clickWidget", json!({"id": "merge.fitMemory"}), T);
    assert!(r.get("error").is_none(), "{r}");
    h.settle(T);
}

/// Whether the final merge started may go under 40 MP (`fitMemory`); it is stopped.
fn started_fit_memory(h: &mut Headless) -> bool {
    let fit = h.app.merge.final_task.as_ref().map(|t| t.job.fit_memory).expect("a merge started");
    if let Some(t) = &h.app.merge.final_task {
        t.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    assert!(h.step_until(T, |h| h.app.merge.final_task.is_none()));
    fit
}

/// A forecast of 2.4 MP from 6.1 MP of photos, which the merge makes only under 40 MP.
fn smaller() -> serde_json::Value {
    json!({"megapixels": 2.4, "nativeMegapixels": 6.1, "limit": "memory", "fitMemory": true})
}

/// A panorama whose full size won't fit memory is merged only once its dialog has said the size it
/// will be made at and the user has agreed to it: Merge waits for the preview (which works out the
/// size), then for "Merge at the smaller size" to be ticked, and the merge it starts may then be
/// made under 40 MP to fit (`fitMemory`). With the memory for its full size, nothing is asked;
/// without the dialog nobody agreed, and Merge with Last Settings never goes under 40 MP.
#[test]
fn a_panorama_made_smaller_for_memory_is_agreed_to_before_it_merges() {
    // the forecast holds for one memory budget: other tests mustn't change it under this one
    let _budget = crate::test_budget::BudgetGuard::hold();
    let opts = crate::merge::MergeDialog::for_command("merge.panorama");
    let (mut h, dir) = panorama_dialog("smaller");
    let forecast = h.app.merge.preview.as_ref().map(|p| p.3["forecast"].clone()).unwrap_or_default();
    assert!(forecast["limit"].is_null(), "the full size fits here: {forecast}");
    assert_eq!(crate::merge::holdup(&h.app, &opts), None);
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "merge.fitMemory"), "no notice");
    crate::merge::open(&mut h.app, "merge.panorama").unwrap();
    assert!(crate::merge::holdup(&h.app, &opts).is_some(), "Merge waits for the preview's size");
    reopen(&mut h);

    set_forecast(&mut h, smaller());
    assert_eq!(
        crate::merge::smaller_notice(&smaller()).as_deref(),
        Some("Not enough memory for the full size, 6.1 MP: the panorama will be made at about 2.4 MP")
    );
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert!(r["error"].as_str().is_some_and(|e| e.contains("agree to its size first")), "not agreed: {r}");
    assert!(h.app.merge.final_task.is_none() && h.app.ui.dialog.is_some(), "nothing merged, the dialog still open");
    agree(&mut h);
    assert!(h.app.merge.smaller_agreed, "ticked");
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["result"]["started"], true, "{r}");
    assert!(started_fit_memory(&mut h), "made smaller to fit, as agreed");

    crate::merge::start_last(&mut h.app, "merge.panorama").unwrap();
    assert!(!started_fit_memory(&mut h), "Merge with Last Settings: never under 40 MP");
    let _ = std::fs::remove_dir_all(dir);
}

/// Agreeing to a smaller panorama answers the size its preview named, and only that: one made
/// smaller but no smaller than 40 MP keeps that floor when agreed to, so nothing far smaller than
/// named can come of it; and when the memory budget or the dialog's photos change, the preview is
/// redone and agreeing starts over.
#[test]
fn agreeing_to_a_smaller_panorama_holds_only_for_the_size_named() {
    let _budget = crate::test_budget::BudgetGuard::hold();
    let opts = crate::merge::MergeDialog::for_command("merge.panorama");
    let (mut h, dir) = panorama_dialog("agreed");
    set_forecast(&mut h, json!({"megapixels": 100.0, "nativeMegapixels": 200.0, "limit": "memory", "fitMemory": false}));
    agree(&mut h);
    let r = h.request("ui.dialog.confirm", json!({}), T);
    assert_eq!(r["result"]["started"], true, "{r}");
    assert!(!started_fit_memory(&mut h), "at its usual floor");

    reopen(&mut h);
    assert!(!h.app.merge.smaller_agreed, "agreed to the last dialog's size, not this one's");
    set_forecast(&mut h, smaller());
    agree(&mut h);
    assert!(h.app.merge.smaller_agreed && crate::merge::holdup(&h.app, &opts).is_none());
    // the memory budget changes (Settings, an agent), the merge's memory with it
    lightcraft_engine::memory::set_budget(lightcraft_engine::memory::budget() + (64 << 20));
    assert!(crate::merge::holdup(&h.app, &opts).is_some(), "Merge waits for a preview of this memory");
    h.step();
    assert!(h.step_until(T, |h| h.app.merge.preview_task.is_none() && crate::merge::holdup(&h.app, &opts).is_none()), "redone");
    assert!(!h.app.merge.smaller_agreed, "agreed to the size the old memory allowed, not this");
    // the photos the dialog merges change (a batch moving on to its next set): the forecast was of others
    h.app.merge.ids.pop();
    assert!(crate::merge::holdup(&h.app, &opts).is_some(), "Merge waits for a preview of these photos");
    let _ = std::fs::remove_dir_all(dir);
}

/// A panorama that can't be made even smaller says why under the preview; there is nothing to
/// tick, and Merge stays off.
#[test]
fn a_panorama_that_cant_be_made_says_why_and_merge_stays_off() {
    let _budget = crate::test_budget::BudgetGuard::hold();
    let opts = crate::merge::MergeDialog::for_command("merge.panorama");
    let (mut h, dir) = panorama_dialog("refused");
    set_forecast(&mut h, json!({"refused": "not enough memory for this panorama: …"}));
    assert!(!h.app.widgets.iter().any(|(w, _)| w == "merge.fitMemory"), "nothing to agree to");
    h.app.merge.smaller_agreed = true;
    let held = crate::merge::holdup(&h.app, &opts).unwrap_or_default();
    assert!(held.starts_with("This panorama can't be merged: not enough memory"), "{held}");
    let _ = std::fs::remove_dir_all(dir);
}
