//! HDR display (LR-VIEW-HDR-DISPLAY): the loupe through an HDR presenter, its SDR fallback in
//! headless screenshots, the Linux opt-in default and the monitor profile on an HDR window.

use super::*;

/// HDR display: the window paints an HDR edit's loupe through the HDR presenter, but a headless
/// screenshot rasterizes on the CPU, which drops paint callbacks, so it draws the SDR rendition
/// (review on #1221: the photo was missing from `ui.screenshot {"headless": true}`).
#[test]
fn headless_screenshots_of_an_hdr_window_draw_the_sdr_photo() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting(Arc<AtomicUsize>);
    impl crate::hdr_view::HdrPresenter for Counting {
        fn paint(&self, _: &egui::Painter, _: egui::Rect, _: &Arc<crate::hdr_view::HdrPixels>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let mut h = demo([1000.0, 700.0]);
    let t = Duration::from_secs(10);
    let paints = Arc::new(AtomicUsize::new(0));
    h.app.hdr_presenter = Some(Box::new(Counting(paints.clone())));
    h.request("ui.set", json!({"view": "detail"}), t);
    h.request("engine.execute", json!({"command": "develop.hdr", "params": {"enabled": true, "maxEv": 3}}), t);
    assert!(h.step_until(SETTLE, |h| h.app.renderer.textures.values().any(|x| x.hdr.is_some())), "no HDR rendition arrived");
    h.step();
    assert!(paints.load(Ordering::SeqCst) > 0, "the window paints the HDR rendition");
    // the HDR rendition the renderer keeps for the presenter counts in its memory figure
    let mem = h.app.renderer.memory();
    assert!(mem["textures"]["hdrBytes"].as_u64().is_some_and(|b| b > 0), "{mem}");
    let before = paints.load(Ordering::SeqCst);
    let main = h.view.ctx.clone();
    let shot = h.app.headless_screenshot(&main, true).unwrap();
    assert_eq!(paints.load(Ordering::SeqCst), before, "the CPU screenshot must not paint through the HDR callback");
    assert!(!h.app.offscreen);
    // and the photo is in it: the middle of the frame is not one flat surround colour
    let [w, hgt] = shot.size;
    let mut colours = std::collections::HashSet::new();
    for y in (hgt * 2 / 5)..(hgt * 3 / 5) {
        for x in (w * 2 / 5)..(w * 3 / 5) {
            colours.insert(shot.pixels[y * w + x]);
        }
    }
    assert!(colours.len() > 50, "the photo is missing from the headless screenshot ({} colours)", colours.len());
}

/// HDR display is off until turned on on Linux and the BSDs (it means native Wayland, without
/// file drag-and-drop, before the window can tell whether the monitor is HDR), on elsewhere;
/// settings saved without the key get the same default (review on #1221).
#[test]
fn hdr_display_is_opt_in_on_linux() {
    let linux_like = cfg!(all(unix, not(target_os = "macos")));
    assert_eq!(crate::state::AppSettings::default().hdr_display, !linux_like);
    let saved: crate::state::AppSettings = serde_json::from_value(json!({"gpu": true})).unwrap();
    assert_eq!(saved.hdr_display, !linux_like);
    #[cfg(target_os = "linux")]
    const {
        assert!(!crate::state::HDR_DISPLAY_DEFAULT)
    };
}

/// An HDR window (scRGB) is colour-managed by the system: its pixels are sRGB primaries, so a
/// monitor profile would convert them twice. The profile is set aside (kept as the setting)
/// while the window is HDR (review on #1221).
#[test]
fn an_hdr_window_sets_the_monitor_profile_aside() {
    struct Nothing;
    impl crate::hdr_view::HdrPresenter for Nothing {
        fn paint(&self, _: &egui::Painter, _: egui::Rect, _: &std::sync::Arc<crate::hdr_view::HdrPixels>) {}
    }
    let dir = std::env::temp_dir().join(format!("lc-ui-hdr-profile-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let p3 = dir.join("p3.icc");
    std::fs::write(&p3, lightcraft_codecs::icc::write_named(lightcraft_codecs::NamedSpace::DisplayP3)).unwrap();
    let mut h = demo([1000.0, 700.0]);
    let t = Duration::from_secs(10);
    let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": p3.to_string_lossy()}}), t);
    assert_eq!(r["ok"], true, "{r}");
    let p3_id = h.app.renderer.display_id();
    assert!(p3_id.is_some());
    h.app.hdr_presenter = Some(Box::new(Nothing));
    h.step();
    assert!(h.app.renderer.scrgb_window());
    assert_eq!(h.app.renderer.display_id(), None, "an HDR window shows sRGB primaries, not the monitor's");
    assert!(!h.app.ui.settings.display_profile.is_empty(), "the setting is kept");
    // choosing a profile meanwhile is kept for later, not applied
    let r = h.request("engine.execute", json!({"command": "app.displayProfile", "params": {"path": p3.to_string_lossy()}}), t);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(h.app.renderer.display_id(), None);
    // an SDR window again: the profile is back
    h.app.hdr_presenter = None;
    h.step();
    assert_eq!(h.app.renderer.display_id(), p3_id);
    let _ = std::fs::remove_dir_all(&dir);
}
