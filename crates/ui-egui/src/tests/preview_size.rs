//! #1897: preferences are commands, persist safely, and constrain actual preview plans.

use serde_json::json;

use crate::{LightcraftApp, Services, state::AppSettings};

/// The public command accepts a percentage and -1 sentinel that old fixed choices could not express.
#[test]
fn preview_size_command_supports_custom_percentage_and_pixel_limit() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    let result = app.run("app.previewSize", json!({"percent": 37, "pixelLimit": 1700}));
    assert_eq!(result, Ok(json!({"percent": 37, "pixelLimit": 1700})));
    assert_eq!(app.run("app.previewSize", json!({"pixelLimit": -1})), Ok(json!({"percent": 37, "pixelLimit": -1})));
    assert_eq!(app.run("app.previewSize", json!({})), Ok(json!({"percent": 37, "pixelLimit": -1})));
}

/// Bad automation input never partially changes the other, valid preference.
#[test]
fn preview_size_command_refuses_invalid_arguments_atomically() {
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    for params in [
        json!({"percent": 101}),
        json!({"percent": -1}),
        json!({"percent": 2.5}),
        json!({"percent": "50"}),
        json!({"percent": null}),
        json!({"percent": 50, "pixelLimit": 0}),
        json!({"percent": 50, "pixelLimit": -2}),
        json!({"percent": 50, "pixelLimit": 12.5}),
        json!({"percent": 50, "pixelLimit": 4294967296_u64}),
        json!({"percent": 50, "pixelLimit": "-1"}),
        json!({"percent": 50, "pixels": 1000}),
        json!([]),
        json!(true),
    ] {
        let before = app.ui.settings.clone();
        assert!(app.run("app.previewSize", params.clone()).is_err(), "{params}");
        assert_eq!(app.ui.settings, before, "{params}");
    }
}

/// Both caps apply to the photo's dimensions, before step rounding, without raising a small view.
#[test]
fn percentage_and_pixel_caps_choose_the_smaller_safe_size() {
    let mut settings = AppSettings { preview_percent: 37, ..Default::default() };
    assert_eq!(settings.loupe_edge(6000.0, 6000, 16384), 2220);
    settings.preview_limit = 1700;
    assert_eq!(settings.loupe_edge(6000.0, 6000, 16384), 1700);
    assert_eq!(settings.loupe_edge(900.0, 6000, 16384), 960);
    assert_eq!(settings.loupe_edge(6000.0, 6000, 1024), 1024);
    for percent in 0..=100 {
        settings.preview_percent = percent;
        for native in [0, 1, 8, 99, 100, 6000, usize::MAX] {
            let edge = settings.loupe_edge(f32::INFINITY, native, 16384);
            assert!((8..=1700).contains(&edge), "{percent}, {native}: {edge}");
            assert!(edge <= native.max(8));
        }
    }
    settings.preview_percent = 0;
    assert_eq!(settings.loupe_edge(6000.0, 6000, 16384), 8);
}

/// Legacy values, custom caps, missing fields and malformed values do not lose unrelated settings.
#[test]
fn preview_size_preferences_preserve_legacy_and_custom_values() {
    for limit in [0, 1600, 2560, 3840, 5120] {
        let old: AppSettings = serde_json::from_value(json!({"previewLimit": limit})).unwrap();
        assert_eq!(old.preview_percent, 100);
        assert_eq!(old.preview_limit, limit);
    }
    let mut ui = crate::UiState::default();
    ui.settings.preview_percent = 37;
    ui.settings.preview_limit = 1700;
    let back: crate::UiState = serde_json::from_value(serde_json::to_value(&ui).unwrap()).unwrap();
    assert_eq!(back.sanitized().settings, ui.settings);
    for invalid in [json!(null), json!("bad"), json!(-1), json!(2.5), json!(4294967296_u64)] {
        let restored: AppSettings = serde_json::from_value(json!({"previewPercent": invalid, "previewLimit": invalid, "gpu": false})).unwrap();
        assert_eq!((restored.preview_percent, restored.preview_limit, restored.gpu), (100, 0, false));
    }
}

/// An intentional fit cap must not be bypassed by the separate native-detail window renderer.
#[test]
fn percentage_caps_fit_but_keeps_the_existing_sharp_zoom_contract() {
    let settings = AppSettings { preview_percent: 25, ..Default::default() };
    let fit =
        crate::region::ViewSizes { drawn_long: 2800.0, canvas_long: 2800.0, native_long: 6000, texture_side: 16384, draft: None, windows: true };
    assert_eq!(crate::region::plan(&settings, fit), crate::region::LoupePlan { main_edge: 1500, window_edge: None });
    let zoom = crate::region::plan(&settings, crate::region::ViewSizes { drawn_long: 6000.0, ..fit });
    assert_eq!(zoom, crate::region::LoupePlan { main_edge: 1500, window_edge: Some(6000) });
}

/// Real slider/field input changes the shared preferences; invalid text and Escape do not.
#[test]
fn preview_size_settings_controls_support_keyboard_editing_and_cancellation() {
    use std::time::Duration;
    let mut h = crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1000.0, 760.0], 1.0);
    let t = Duration::from_secs(20);
    let ask = |h: &mut crate::headless::Headless, method, params| {
        let r = h.request(method, params, t);
        assert_eq!(r["ok"], true, "{r}");
    };
    ask(&mut h, "engine.execute", json!({"command": "app.settings", "params": {"tab": "performance"}}));
    ask(&mut h, "ui.clickWidget", json!({"id": "slider:settingsPreviewPercent", "fx": 0.2}));
    assert!(h.app.ui.settings.preview_percent < 100);
    let before = h.app.ui.settings.preview_percent;
    // Native control requests leave idle frames between clicking and pressing a key.
    // Focus must survive those frames, as it does while a person pauses to read the value.
    for _ in 0..6 {
        h.step();
    }
    ask(&mut h, "ui.key", json!({"key": "ArrowUp"}));
    assert!(h.app.ui.settings.preview_percent > before, "the slider is keyboard adjustable");
    for (text, expected, end) in [("1700", 1700, "Enter"), ("-1", 0, "Tab"), ("0", 0, "Enter"), ("invalid", 0, "Enter"), ("3200", 0, "Escape")] {
        ask(&mut h, "ui.clickWidget", json!({"id": "field:settingsPreviewPixels"}));
        ask(&mut h, "ui.key", json!({"key": "a", "cmd": true}));
        ask(&mut h, "ui.text", json!({"text": text}));
        ask(&mut h, "ui.key", json!({"key": end}));
        assert_eq!(h.app.ui.settings.preview_limit, expected, "{text} + {end}");
    }
    let saved: crate::UiState = serde_json::from_value(serde_json::to_value(&h.app.ui).unwrap()).unwrap();
    assert_eq!(saved.sanitized().settings, h.app.ui.settings);
}

/// The new command is discoverable by connected agents with its accepted parameter units.
#[test]
fn preview_size_command_is_discoverable() {
    let app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    let commands = crate::control::all_commands(&app);
    let command = commands.as_array().unwrap().iter().find(|c| c["id"] == "app.previewSize").unwrap();
    assert_eq!(command["enabled"], true);
    assert!(command["params"].as_str().unwrap().contains("pixelLimit"));
    assert!(command["params"].as_str().unwrap().contains("4294967295"));
}

/// A connected update replaces an in-progress field's stale draft before Return or Tab can commit it.
#[test]
fn preview_size_external_updates_replace_an_active_pixel_edit() {
    use std::time::Duration;
    let mut h = crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::new(), Services::default()), [1000.0, 760.0], 1.0);
    let t = Duration::from_secs(20);
    let mut ask = |method, params| {
        let r = h.request(method, params, t);
        assert_eq!(r["ok"], true, "{r}");
        r["result"].clone()
    };
    ask("engine.execute", json!({"command":"app.settings","params":{"tab":"performance"}}));
    for end in ["Enter", "Tab"] {
        ask("engine.execute", json!({"command":"app.previewSize","params":{"pixelLimit":987}}));
        ask("ui.clickWidget", json!({"id":"field:settingsPreviewPixels"}));
        ask("ui.key", json!({"key":"a","cmd":true}));
        ask("ui.text", json!({"text":"1700"}));
        ask("engine.execute", json!({"command":"app.previewSize","params":{"pixelLimit":1777}}));
        ask("ui.key", json!({"key":end}));
        assert_eq!(ask("engine.execute", json!({"command":"app.previewSize"}))["pixelLimit"], 1777, "{end}");
    }
}

/// System Info describes the unlimited sentinel in words and gives units only to positive limits.
#[test]
fn preview_size_system_info_distinguishes_unlimited_and_pixel_caps() {
    crate::i18n::set_language(crate::i18n::Locale::En);
    let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
    for (limit, expected) in [(-1, "37% / No pixel limit"), (1700, "37% / 1700 px")] {
        app.run("app.previewSize", json!({"percent":37,"pixelLimit":limit})).unwrap();
        let info = app.run("app.systemInfo", json!({"open":false})).unwrap();
        let row = info.as_array().unwrap().iter().find(|r| r["label"] == "Preview size").unwrap();
        assert_eq!(row["value"], expected);
    }
}

/// Compare and Reference respect caps at fit, while retaining native detail after zooming in.
#[test]
fn preview_size_caps_compare_and_reference_whole_frames() {
    use std::time::Duration;
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.run("app.previewSize", json!({"percent": 100, "pixelLimit": 113})).unwrap();
    let mut h = crate::headless::Headless::new(app, [1000.0, 760.0], 1.0);
    for mode in ["view.compare", "view.reference"] {
        h.app.run(mode, json!({})).unwrap();
        h.app.run("view.zoomFit", json!({})).unwrap();
        h.settle(Duration::from_secs(120));
        for pane in 0..2 {
            let tex = h.app.renderer.textures.get(&crate::render::Slot::Compare(pane)).unwrap();
            assert!(tex.size[0].max(tex.size[1]) <= 113, "{mode}: {:?}", tex.size);
            assert!(!h.app.renderer.textures.contains_key(&crate::render::Slot::CompareRegion(pane)));
        }
        h.app.run("view.zoom100", json!({})).unwrap();
        h.settle(Duration::from_secs(120));
        assert!(h.app.renderer.textures.contains_key(&crate::render::Slot::CompareRegion(0)), "{mode}: sharp window");
    }
}

/// An 8 px preview's unavoidable aspect rounding must not disable native detail.
#[test]
fn preview_size_tiny_whole_frames_keep_sharp_zoom_windows() {
    use std::time::Duration;
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut h = crate::headless::Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("app.previewSize", json!({"percent": 0, "pixelLimit": -1})).unwrap();
    for mode in ["view.detail", "view.compare", "view.reference"] {
        h.app.run(mode, json!({})).unwrap();
        h.app.run("view.zoomFit", json!({})).unwrap();
        h.settle(Duration::from_secs(120));
        h.app.run("view.zoom100", json!({})).unwrap();
        h.settle(Duration::from_secs(120));
        let slot = if mode == "view.detail" { crate::render::Slot::Region } else { crate::render::Slot::CompareRegion(0) };
        assert!(h.app.renderer.textures.contains_key(&slot), "{mode}: an 8 px whole frame must not suppress sharp zoom");
    }
}

/// Explicit percentage zoom remains sharp even when that image fits inside the pane.
#[test]
fn preview_size_fitting_percentage_zoom_keeps_sharp_windows() {
    use std::time::Duration;
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    let mut h = crate::headless::Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.run("app.previewSize", json!({"percent": 100, "pixelLimit": 113})).unwrap();
    for mode in ["view.detail", "view.compare", "view.reference"] {
        h.app.run(mode, json!({})).unwrap();
        h.app.ui.zoom = crate::state::Zoom::Percent(5.0);
        h.settle(Duration::from_secs(120));
        let slot = if mode == "view.detail" { crate::render::Slot::Region } else { crate::render::Slot::CompareRegion(0) };
        assert!(h.app.renderer.textures.contains_key(&slot), "{mode}: explicit percentage zoom must stay sharp inside its pane");
    }
}

/// Quantization tolerance follows the short edge, preserving genuine embedded-preview crops.
#[test]
fn preview_size_texture_aspects_allow_quantization_without_hiding_crops() {
    use crate::region::texture_aspect;
    for (size, aspect) in [([8, 5], 1.5), ([5, 8], 2.0 / 3.0), ([37, 25], 1.5), ([8, 1], 100.0)] {
        assert_eq!(texture_aspect(size, aspect), aspect, "{size:?}");
    }
    assert_eq!(texture_aspect([8, 4], 1.5), 2.0);
    assert_eq!(texture_aspect([400, 400], 1.5), 1.0);
    assert_eq!(texture_aspect([600, 300], 1.5), 2.0);
    for frame in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_eq!(texture_aspect([8, 5], frame), 1.6);
    }
    assert!(texture_aspect([0, 0], 1.5).is_finite());
}

/// The actual painted meshes must fill the frame, including Before/After and wipe views.
#[test]
fn preview_size_tiny_detail_textures_fill_the_painted_frame() {
    use crate::render::Slot;
    use crate::state::BeforeAfter;
    use std::time::Duration;
    let mut app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default());
    app.ui.filmstrip = false;
    app.run("app.previewSize", json!({"percent": 0, "pixelLimit": -1})).unwrap();
    app.run("view.detail", json!({})).unwrap();
    app.run("view.zoomFit", json!({})).unwrap();
    let mut h = crate::headless::Headless::new(app, [1000.0, 760.0], 1.0);
    for mode in
        [BeforeAfter::Off, BeforeAfter::Original, BeforeAfter::SideBySide, BeforeAfter::TopBottom, BeforeAfter::Split, BeforeAfter::SplitTopBottom]
    {
        h.app.ui.before_after = mode;
        h.settle(Duration::from_secs(120));
        h.step();
        let image = h.app.image_rect.unwrap();
        for slot in [Slot::Main, Slot::Before] {
            if (slot == Slot::Main && mode == BeforeAfter::Original) || (slot == Slot::Before && mode == BeforeAfter::Off) {
                continue;
            }
            let tex = h.app.renderer.textures.get(&slot).unwrap();
            let painted = h
                .view
                .painted_shapes()
                .into_iter()
                .find_map(|shape| match shape {
                    egui::epaint::Shape::Mesh(mesh) if mesh.texture_id == tex.tex.id() => Some(mesh.calc_bounds()),
                    _ => None,
                })
                .unwrap();
            let mut expected = image.size();
            if slot == Slot::Before && mode == BeforeAfter::Split {
                expected.x *= 0.5;
            } else if slot == Slot::Before && mode == BeforeAfter::SplitTopBottom {
                expected.y *= 0.5;
            }
            assert!((painted.size() - expected).abs().max_elem() < 0.6, "{mode:?}/{slot:?}: painted {:?}, frame {expected:?}", painted.size());
        }
    }
}

/// Changing both controls resizes the real loupe texture and leaves the photo's edits untouched.
#[test]
fn preview_size_changes_actual_loupe_renders_without_changing_edits() {
    use std::time::Duration;
    let mut h =
        crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1000.0, 760.0], 1.0);
    let t = Duration::from_secs(20);
    h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), t);
    h.settle(Duration::from_secs(120));
    let id = h.app.session.active().unwrap();
    let edits = h.app.session.catalog.photo(id).unwrap().develop.clone();
    let native = {
        let photo = h.app.session.catalog.photo(id).unwrap();
        let frame =
            lightcraft_pipeline::geometry::Frame::with_lens(photo.width as usize, photo.height as usize, &edits, true, photo.embedded_lens.as_ref());
        frame.native_size().0.max(frame.native_size().1).round() as usize
    };
    for (percent, pixels, maximum) in [(5, -1, native * 5 / 100), (100, 113, 113), (0, -1, 8)] {
        let r = h.request("engine.execute", json!({"command": "app.previewSize", "params": {"percent": percent, "pixelLimit": pixels}}), t);
        assert_eq!(r["ok"], true, "{r}");
        h.settle(Duration::from_secs(120));
        let texture = h.app.renderer.textures.get(&crate::render::Slot::Main).unwrap();
        assert!(texture.size[0].max(texture.size[1]) <= maximum.max(8), "{percent}% / {pixels}: {:?}", texture.size);
        assert!(h.app.region_view.is_none(), "fit should respect the chosen preview cap");
        assert_eq!(h.app.session.catalog.photo(id).unwrap().develop, edits);
        h.app.ui.before_after = crate::state::BeforeAfter::SideBySide;
        h.settle(Duration::from_secs(120));
        for slot in [crate::render::Slot::Main, crate::render::Slot::Before] {
            let texture = h.app.renderer.textures.get(&slot).unwrap();
            assert!(texture.size[0].max(texture.size[1]) <= maximum.max(8), "{slot:?}: {:?}", texture.size);
        }
        h.app.ui.before_after = crate::state::BeforeAfter::Off;
    }
}

/// Slideshow playback uses the same whole-photo caps, independently of its screen size.
#[test]
fn preview_size_caps_slideshow_photo_renders() {
    use std::time::Duration;
    let mut h =
        crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1000.0, 760.0], 1.0);
    h.app.run("app.previewSize", json!({"percent": 100, "pixelLimit": 113})).unwrap();
    h.app.run("view.slideshow", json!({"interval": 60, "preparePreviews": false})).unwrap();
    let id = h.app.session.active().unwrap();
    let slot = crate::render::Slot::Slide(lightcraft_engine::slideshow::Item::Photo(id));
    assert!(h.step_until(Duration::from_secs(120), |h| h.app.renderer.textures.contains_key(&slot)));
    let texture = h.app.renderer.textures.get(&slot).unwrap();
    assert!(texture.size[0].max(texture.size[1]) <= 113, "{:?}", texture.size);
    h.app.run("slideshow.end", json!({})).unwrap();
}

/// Settings' slide composer uses the active photo's percentage instead of an unbounded native size.
#[test]
fn preview_size_caps_slideshow_settings_preview() {
    use crate::render::{RenderOffload, Slot};
    use lightcraft_engine::media::{RenderJob, RenderResult};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    struct Capture(Rc<RefCell<Option<RenderJob>>>);
    impl RenderOffload for Capture {
        fn try_start(&mut self, slot: Slot, job: RenderJob) -> Option<RenderJob> {
            if slot == Slot::SlidePreview {
                *self.0.borrow_mut() = Some(job);
            }
            None
        }
        fn finished(&mut self) -> Vec<(Slot, RenderResult, f64)> {
            Vec::new()
        }
    }

    let mut h =
        crate::headless::Headless::new(LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services::default()), [1000.0, 760.0], 1.0);
    let photo = h.app.session.catalog.photo(h.app.session.active().unwrap()).unwrap();
    let frame = lightcraft_pipeline::geometry::Frame::with_lens(
        photo.width as usize,
        photo.height as usize,
        &photo.develop,
        true,
        photo.embedded_lens.as_ref(),
    );
    let native = frame.native_size().0.max(frame.native_size().1).round() as usize;
    let maximum = lightcraft_engine::cmd::previews::percentage_edge(native, 1);
    let captured = Rc::new(RefCell::new(None));
    h.app.renderer.set_offload(Box::new(Capture(Rc::clone(&captured))));
    h.app.run("app.previewSize", json!({"percent": 1, "pixelLimit": -1})).unwrap();
    h.app.run("app.settings", json!({"tab": "slideshow"})).unwrap();
    assert!(h.step_until(Duration::from_secs(20), |_| captured.borrow().is_some()));
    let job = captured.borrow();
    let job = job.as_ref().unwrap();
    // The photo render is capped; the composed slide canvas keeps its own resolution for text/overlays.
    assert!(job.slide.is_some());
    assert!(job.request.max_w.max(job.request.max_h) <= maximum, "{:?} > {maximum}", job.request);
}

/// Every shipped language translates the new messages and keeps both controls inside a narrow dialog.
#[test]
fn preview_size_localized_controls_fit_at_normal_and_high_scaling() {
    use std::time::Duration;
    const MESSAGES: &[&str] = &[
        "Pixel limit",
        "No pixel limit",
        "Percentage of the photo's long edge. The smaller limit wins; -1 means no pixel limit. 0% keeps a minimum preview. Smaller previews are faster.",
        "Limits apply to whole-frame previews. Zoomed detail stays sharp; exports and 1:1 preview builds keep their own sizes.",
        "Enter a positive pixel limit, or -1 for no limit.",
    ];
    for &language in crate::i18n::Locale::ALL {
        crate::i18n::set_language(language);
        for message in MESSAGES {
            if language != crate::i18n::Locale::En {
                assert_ne!(crate::i18n::tr(message), *message, "{language:?}: {message}");
            }
        }
        for scale in [1.0, 2.25] {
            for theme in [crate::theme::ThemeKind::Charcoal, crate::theme::ThemeKind::Paper] {
                let mut app = LightcraftApp::new(lightcraft_engine::Session::new(), Services::default());
                app.ui.language = language;
                app.ui.settings.select_theme(theme);
                let mut h = crate::headless::Headless::new(app, [960.0, 720.0], scale);
                let r = h.request("engine.execute", json!({"command": "app.settings", "params": {"tab": "performance"}}), Duration::from_secs(20));
                assert_eq!(r["ok"], true, "{r}");
                let widgets = h.request("ui.widgets", json!({}), Duration::from_secs(20));
                for id in ["slider:settingsPreviewPercent", "field:settingsPreviewPixels"] {
                    let widget = widgets["result"].as_array().unwrap().iter().find(|w| w["id"] == id).unwrap();
                    let rect = widget["rect"].as_array().unwrap();
                    let (x, y, w, hh) = (rect[0].as_f64().unwrap(), rect[1].as_f64().unwrap(), rect[2].as_f64().unwrap(), rect[3].as_f64().unwrap());
                    assert!(x >= 0.0 && y >= 0.0 && x + w <= 960.1 && y + hh <= 720.1, "{language:?}/{theme:?}/{scale}/{id}: {rect:?}");
                }
            }
        }
    }
    crate::i18n::set_language(crate::i18n::Locale::En);
}
