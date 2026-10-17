//! Headless tests of the video UI: the control bar's requests on click and drag (from a fake
//! state), the grid badge for a video record, and develop controls greyed out for video only.

use egui::{Event, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use lcc_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use super::*;

struct Bar {
    s: VideoUiState,
    requests: Vec<Request>,
    track: Rect,
}

fn bar(s: VideoUiState) -> Harness<'static, Bar> {
    let mut ready = false;
    let mut h = Harness::builder().with_size(vec2(800.0, 60.0)).build_ui_state(
        move |ui, b: &mut Bar| {
            if !ready {
                crate::theme::install_fonts(ui.ctx());
                crate::theme::apply(ui.ctx());
                ready = true;
                return;
            }
            let out = control_bar(ui, &b.s);
            b.track = out.track;
            b.requests.extend(out.requests);
        },
        Bar { s, requests: Vec::new(), track: Rect::NOTHING },
    );
    h.run_steps(3);
    h
}

fn clip() -> VideoUiState {
    VideoUiState { duration: 100.0, current: 20.0, trim_in: 10.0, trim_out: Some(80.0), ..Default::default() }
}

fn drag(h: &mut Harness<'_, Bar>, from: Pos2, to: Pos2) {
    h.input_mut().events.push(Event::PointerMoved(from));
    h.run();
    h.input_mut().events.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Default::default() });
    h.run();
    for i in 1..=4 {
        h.input_mut().events.push(Event::PointerMoved(from + (to - from) * (i as f32 / 4.0)));
        h.run();
    }
    h.input_mut().events.push(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Default::default() });
    h.run();
}

#[test]
fn buttons_emit_their_commands() {
    let mut h = bar(clip());
    h.get_by_label("Play").click();
    h.run();
    h.get_by_label("Next Frame").click();
    h.run();
    h.get_by_label("Previous Frame").click();
    h.run();
    h.get_by_label("Set Poster Frame").click();
    h.run();
    let r = &h.state().requests;
    assert_eq!(
        r,
        &vec![
            ("video.togglePlay", json!({})),
            ("video.stepFrame", json!({"delta": 1})),
            ("video.stepFrame", json!({"delta": -1})),
            ("video.setPoster", json!({"time": 20.0})),
        ]
    );
}

#[test]
fn playing_shows_pause() {
    let h = bar(VideoUiState { playing: true, ..clip() });
    assert!(h.query_by_label("Pause").is_some());
    assert!(h.query_by_label("Play").is_none());
}

#[test]
fn clicking_the_track_seeks() {
    let mut h = bar(clip());
    let track = h.state().track;
    let at = pos2(track.left() + track.width() * 0.5, track.center().y);
    drag(&mut h, at, at);
    let seek = h.state().requests.iter().find(|(c, _)| *c == "video.seek").cloned();
    let t = seek.and_then(|(_, p)| p["time"].as_f64()).unwrap();
    assert!((t - 50.0).abs() < 0.5, "{t}");
}

#[test]
fn dragging_the_trim_handles_sets_trim() {
    let mut h = bar(clip());
    let track = h.state().track;
    let s = clip();
    let xin = time_x(track, &s, 10.0);
    drag(&mut h, pos2(xin, track.center().y), pos2(time_x(track, &s, 30.0), track.center().y));
    let (_, p) = h.state().requests.iter().rev().find(|(c, _)| *c == "video.setTrim").cloned().unwrap();
    assert!((p["in"].as_f64().unwrap() - 30.0).abs() < 0.5, "{p}");
    assert_eq!(p["out"], json!(80.0));
    assert!(!h.state().requests.iter().any(|(c, _)| *c == "video.seek"), "a handle drag doesn't scrub");
    // the end dragged to the clip's end is "no trim"
    h.state_mut().requests.clear();
    let xout = time_x(track, &s, 80.0);
    drag(&mut h, pos2(xout, track.center().y), pos2(track.right() + 40.0, track.center().y));
    let (_, p) = h.state().requests.iter().rev().find(|(c, _)| *c == "video.setTrim").cloned().unwrap();
    assert_eq!(p, json!({"in": 10.0, "out": null}));
}

#[test]
fn times_and_state_parsing() {
    assert_eq!(format_time(65.4), "1:05");
    assert_eq!(format_time(3725.0), "1:02:05");
    assert_eq!(format_time(f64::NAN), "0:00");
    assert_eq!(format_time(-3.0), "0:00");
    let s = VideoUiState::from_value(&json!({"current": 3.0, "playing": true, "trimOut": null}), &clip());
    assert_eq!((s.current, s.playing, s.trim_out, s.duration), (3.0, true, None, 100.0));
    let bad = VideoUiState { duration: f64::NAN, trim_in: 50.0, trim_out: Some(-1.0), ..Default::default() };
    assert_eq!(bad.trim(), (0.0, 0.0));
}

fn video(id: u64) -> Photo {
    let mut p =
        Photo::new(PhotoId(id), Source::File { path: format!("/video-ui-test/CLIP_{id}.mp4") }, "CLIP.mp4", "MP4", 1920, 1080, "2026-01-01T00:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(65.0);
    p
}

#[test]
fn grid_badge_only_for_video() {
    let mut photo = video(7);
    let still = {
        let mut p = photo.clone();
        p.kind = MediaKind::Image;
        p
    };
    assert_eq!(spoken_suffix(&photo), ", video, 1:05");
    assert_eq!(spoken_suffix(&still), "");
    for (p, want) in [(still, false), (photo.clone(), true)] {
        let mut ready = false;
        let mut h = Harness::builder().with_size(vec2(220.0, 170.0)).build_ui_state(
            move |ui, found: &mut bool| {
                if !ready {
                    crate::theme::install_fonts(ui.ctx());
                    crate::theme::apply(ui.ctx());
                    ready = true;
                    return;
                }
                crate::widgets::take_registry(ui.ctx());
                grid_badge(ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 150.0)), &p);
                *found = crate::widgets::take_registry(ui.ctx()).iter().any(|(k, _)| k == "badge:video:7");
            },
            false,
        );
        h.run_steps(3);
        let found = *h.state();
        assert_eq!(found, want);
    }
    photo.duration = None;
    assert_eq!(spoken_suffix(&photo), ", video");
}

#[test]
fn grid_cell_shows_the_badge() {
    let mut s = lcc_engine::Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(video(1)) }).unwrap();
    let app = crate::LccApp::new(s, crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1000.0, 700.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::PhotoGrid;
    h.settle(std::time::Duration::from_secs(30));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "badge:video:1"), "video badge drawn");
}

#[test]
fn allowed_controls() {
    for ok in ["light.exposure", "wb.temp", "color.saturation", "curve", "mixer", "grading"] {
        assert!(control_allowed(ok), "{ok}");
    }
    for no in ["effects.clarity", "detail.nrLuminance", "optics.distortion", "enhance.denoise", "calibration.redHue", "hdr.sdrBrightness"] {
        assert!(!control_allowed(no), "{no}");
    }
    assert!(section_allowed("light") && section_allowed("color"));
    for no in ["effects", "detail", "optics", "calibration"] {
        assert!(!section_allowed(no));
    }
}

#[test]
fn gate_disables_only_when_blocked() {
    for blocked in [false, true] {
        let mut clicks = 0;
        let mut h = Harness::builder().with_size(vec2(200.0, 80.0)).build_ui_state(
            move |ui, n: &mut i32| {
                video_gate(ui, blocked, |ui| {
                    if ui.button("Clarity").clicked() {
                        *n += 1;
                    }
                });
            },
            clicks,
        );
        h.run();
        h.get_by_label("Clarity").click();
        h.run();
        clicks = *h.state();
        assert_eq!(clicks, if blocked { 0 } else { 1 });
    }
}

/// With a video active, the Detail section's sliders are disabled; with a photo they aren't.
#[test]
fn edit_panel_greys_out_for_video_only() {
    for kind in [MediaKind::Image, MediaKind::Video] {
        let mut s = lcc_engine::Session::new();
        let mut p = video(1);
        p.kind = kind;
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        s.execute("library.select", &json!({"ids": [1]})).unwrap();
        let mut app = crate::LccApp::new(s, crate::Services { png: None, ..Default::default() });
        app.ui.toggle_section("detail");
        let mut ready = false;
        let mut h = Harness::builder().with_size(vec2(360.0, 3000.0)).build_ui_state(
            move |ui, app: &mut crate::LccApp| {
                if !ready {
                    crate::theme::install_fonts(ui.ctx());
                    crate::theme::apply(ui.ctx());
                    ready = true;
                    return;
                }
                crate::widgets::take_registry(ui.ctx());
                egui::ScrollArea::vertical().show(ui, |ui| crate::panels::edit::show(app, ui, PhotoId(1)));
            },
            app,
        );
        h.run_steps(4);
        let node = h.get_by_label("Smoothness");
        let disabled = egui_kittest::kittest::NodeT::accesskit_node(&node).is_disabled();
        assert_eq!(disabled, kind == MediaKind::Video, "{kind:?}");
    }
}

/// A clip (no `VideoMeta`, missing file) and a still, the clip active in Develop.
fn develop_clip() -> crate::headless::Headless {
    let mut s = lcc_engine::Session::new();
    // an undecodable file: the loupe shows the placeholder poster
    let dir = std::env::temp_dir().join(format!("lcc-video-ui-gating-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("CLIP.mp4");
    std::fs::write(&path, b"not an mp4").unwrap();
    let mut clip = video(1);
    clip.source = Source::File { path: path.to_string_lossy().into_owned() };
    s.catalog.apply(Op::AddPhoto { photo: Box::new(clip) }).unwrap();
    let still = Photo::new(PhotoId(2), Source::Demo { scene: 0 }, "still.jpg", "JPEG", 64, 48, "");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(still) }).unwrap();
    s.execute("library.select", &json!({"ids": [1], "active": 1})).unwrap();
    let app = crate::LccApp::new(s, crate::Services { png: None, ..Default::default() });
    let mut h = crate::headless::Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = crate::state::ViewMode::Detail;
    h.app.ui.right = crate::state::RightPanel::Edit;
    h
}

#[test]
fn photo_only_panels_and_tools_are_blocked_for_a_clip() {
    let mut h = develop_clip();
    h.settle(std::time::Duration::from_secs(30));
    for id in ["panel.remove", "panel.masking", "panel.redeye", "tool.brush", "mask.add"] {
        assert!(!crate::menus::ui_enabled(&h.app, id), "{id}");
        let e = h.app.run(id, json!({})).unwrap_err();
        assert!(e.contains("video"), "{id}: {e}");
    }
    assert_eq!(h.app.ui.right, crate::state::RightPanel::Edit, "no photo-only panel opened");
    assert!(crate::menus::ui_enabled(&h.app, "panel.edit"));
    // a clip can be cropped
    assert!(crate::menus::ui_enabled(&h.app, "panel.crop"));
    // a still takes them again
    h.app.run("library.select", json!({"ids": [2], "active": 2})).unwrap();
    assert!(crate::menus::ui_enabled(&h.app, "panel.crop"));
    h.app.run("panel.crop", json!({})).unwrap();
}

#[test]
fn develop_on_a_clip_without_video_meta_draws_the_histogram_and_metadata() {
    let mut h = develop_clip();
    h.settle(std::time::Duration::from_secs(30));
    assert!(h.app.widgets.iter().any(|(w, _)| w == "histogram"), "histogram drawn");
    let main = h.app.renderer.textures.get(&crate::render::Slot::Main).expect("the poster rendered");
    assert!(main.photo == PhotoId(1) && main.histogram.is_some(), "histogram of the poster frame");
    h.app.ui.right = crate::state::RightPanel::Info;
    h.settle(std::time::Duration::from_secs(5));
}

#[test]
fn metadata_shows_video_fields() {
    use crate::panels::metadata::value;
    let mut p = video(3);
    assert_eq!(value(&p, "videoDuration"), "1:05");
    assert_eq!(value(&p, "resolution"), "1920 × 1080");
    assert_eq!(value(&p, "codec"), "", "not probed yet");
    p.video = Some(lcc_catalog::VideoMeta {
        duration: 65.0,
        fps_num: 30000,
        fps_den: 1001,
        width: 1280,
        height: 720,
        codec: "h264".into(),
        has_audio: true,
        trim_in: 2.0,
        trim_out: Some(30.0),
        poster_time: None,
    });
    assert_eq!(value(&p, "frameRate"), "29.97 fps");
    assert_eq!(value(&p, "codec"), "H264");
    assert_eq!(value(&p, "resolution"), "1280 × 720");
    assert_eq!(value(&p, "audio"), "Yes");
    assert_eq!(value(&p, "trim"), "0:02 – 0:30");
    p.video.as_mut().unwrap().fps_den = 0;
    assert_eq!(value(&p, "frameRate"), "", "a zero denominator is no frame rate, not a panic");
    p.kind = MediaKind::Image;
    assert_eq!(value(&p, "codec"), "");
}
