//! Catalogued videos in the UI (plan/video-import-plan.md, Filone B): a clip reads as a clip —
//! format and running time on its grid cell, a card instead of an error in the loupe, and
//! Duration in Info — although this build can't decode or play one yet.

use std::time::Duration;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::state::ViewMode;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_secs(60);

/// Check whether a widget or element with the given identifier is registered.
fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

/// Encode a test RGBA8 image as PNG bytes.
fn png(img: &lightcraft_raster::Rgba8) -> Vec<u8> {
    lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(img), &lightcraft_codecs::EncodeMeta::default()).unwrap_or_default()
}

/// Save a headless screenshot under `$LIGHTCRAFT_TEST_SHOTS` when that variable asks for one (the
/// project's way of looking at UI work).
fn shot(h: &mut Headless, name: &str) {
    let Some(dir) = std::env::var_os("LIGHTCRAFT_TEST_SHOTS") else { return };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).expect("create the screenshot folder");
    let path = dir.join(name);
    let r = h.request("ui.screenshot", json!({"path": path.to_string_lossy(), "headless": true}), T);
    assert_eq!(r["ok"], true, "{r}");
}

/// Select a photo or video by ID in the test application and wait for the UI to settle.
fn select(h: &mut Headless, id: PhotoId) {
    let r = h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id.0]}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
}

/// Two clips (one under an hour, one over), a still and a clip without a reported duration.
/// Their files don't exist, so a thumbnail render fails fast — which for a video must not read as
/// a broken photo.
fn video_app(view: ViewMode) -> Headless {
    let mut session = lightcraft_engine::Session::new();
    for (id, name, kind, duration, format) in [
        (1u64, "clip.mp4", MediaKind::Video, Some(84.0), "MP4"),
        (2, "photo.jpg", MediaKind::Image, None, "JPEG"),
        (3, "long.mov", MediaKind::Video, Some(4530.0), "MOV"),
        (4, "unknown.mp4", MediaKind::Video, None, "MP4"),
    ] {
        let path = format!("/lightcraft-video-test/{name}");
        let mut p = Photo::new(PhotoId(id), Source::File { path }, name, format, 1920, 1080, "2026-05-04T10:00:00");
        p.kind = kind;
        p.duration = duration;
        session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let app = LightcraftApp::new(
        session,
        Services {
            png: Some(Box::new(png)),
            write: Some(Box::new(|path: &str, bytes: &[u8]| std::fs::write(path, bytes).map_err(|e| e.to_string()))),
            write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
            ..Default::default()
        },
    );
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = view;
    h.app.ui.thumb_size = 180.0;
    h.settle(SETTLE);
    h
}

/// `MM:SS` under an hour and `H:MM:SS` from an hour up, seconds truncated (a player never promises
/// a second that doesn't play), and an unusable number comes back empty rather than as a time.
#[test]
fn a_duration_reads_as_minutes_and_seconds() {
    use crate::widgets::format_duration;
    assert_eq!(format_duration(0.0), "00:00");
    assert_eq!(format_duration(15.0), "00:15");
    assert_eq!(format_duration(84.9), "01:24");
    assert_eq!(format_duration(222.0), "03:42");
    assert_eq!(format_duration(3599.99), "59:59");
    assert_eq!(format_duration(3600.0), "1:00:00");
    assert_eq!(format_duration(4530.0), "1:15:30");
    assert_eq!(format_duration(100.0 * 3600.0), "100:00:00");
    for hostile in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.4] {
        assert_eq!(format_duration(hostile), "", "{hostile} is not a running time");
    }
}

/// Both grids draw the format and the running time on a clip's cell, and nothing on a still's.
#[test]
fn grid_cells_mark_videos_with_their_format_and_running_time() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut h = video_app(view);
        assert!(has(&h, "thumb:1") && has(&h, "thumb:2") && has(&h, "thumb:3"), "{view:?}: the cells are drawn");
        assert!(has(&h, "badge:videoDuration:1"), "{view:?}: clip.mp4 (1:24) has a duration badge");
        assert!(has(&h, "badge:videoFormat:1"), "{view:?}: clip.mp4 names its container");
        assert!(has(&h, "badge:videoDuration:3"), "{view:?}: long.mov (1:15:30) has one");
        assert!(!has(&h, "badge:videoDuration:4"), "{view:?}: a clip without a duration shows no time");
        assert!(!has(&h, "badge:videoDuration:2") && !has(&h, "badge:videoFormat:2"), "{view:?}: a still has neither badge");
        shot(&mut h, &format!("video-{view:?}.png"));
    }
}

#[test]
fn grids_announce_video_even_without_a_duration() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut h = video_app(view);
        let ctx = h.view.ctx.clone();
        ctx.enable_accesskit();
        let mut labels = Vec::new();
        for frame in 0..3 {
            let raw = crate::headless::HeadlessView::raw_input(egui::vec2(1200.0, 800.0), 1.0, h.time() + frame as f64, Vec::new());
            let out = ctx.run_ui(raw, |ui| h.app.ui(ui));
            if let Some(update) = out.platform_output.accesskit_update {
                labels.extend(update.nodes.into_iter().filter_map(|(_, node)| node.label().map(str::to_string)));
            }
        }
        let video = crate::i18n::tr("Video");
        assert!(labels.iter().any(|label| label == &format!("unknown.mp4, {video}")), "{view:?}: {labels:?}");
        assert!(labels.iter().any(|label| label == &format!("clip.mp4, {video} 01:24")), "{view:?}: {labels:?}");
        assert!(labels.iter().any(|label| label == "photo.jpg"));
    }
}

#[test]
fn culling_views_show_video_cards_without_scheduling_frames() {
    for command in ["view.compare", "view.reference", "view.survey"] {
        let mut h = video_app(ViewMode::PhotoGrid);
        h.app.run("library.select", json!({"ids": [1, 3], "active": 3})).unwrap();
        h.app.run(command, json!({})).unwrap();
        h.settle(SETTLE);
        assert!(has(&h, "cull:1:videoCard"), "{command}: the first clip is not a permanent skeleton");
        assert!(has(&h, "cull:3:videoCard"), "{command}: the second clip is not a permanent skeleton");
        assert_eq!(h.app.renderer.in_flight(), 0, "{command}: no frame decoder jobs");
        assert!(!h.app.widgets.iter().any(|(id, _)| id.starts_with("compare:rendering:")));
        shot(&mut h, &format!("video-{}.png", command.replace('.', "-")));
    }
}

/// The filter bar's Videos kind keeps the clips and drops the stills.
#[test]
fn the_videos_filter_keeps_only_clips() {
    let mut h = video_app(ViewMode::PhotoGrid);
    let r = h.request("engine.execute", json!({"command": "library.filter", "params": {"kind": "video"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let visible = h.app.session.visible().to_vec();
    assert_eq!(visible.len(), 3, "the three clips, no still");
    let ids: Vec<u64> = visible.iter().map(|p| p.0).collect();
    for id in [1, 3, 4] {
        assert!(ids.contains(&id), "clip {id} is in the Videos view");
    }
    for id in visible {
        assert_eq!(h.app.session.catalog.photo(id).map(|p| p.kind), Some(MediaKind::Video));
    }
}

/// Opening a clip in the loupe shows its card — name and running time, and what this build does
/// with it — instead of "This photo can't be opened" and the failure pill.
#[test]
fn the_loupe_shows_a_video_card_instead_of_a_render_error() {
    let mut h = video_app(ViewMode::Detail);
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "canvas:videoCard"), "the card stands in for the frame");
    assert_eq!(h.app.loupe_shown.map(|l| l.1), Some("video"), "ui.inspect says what the loupe shows");
    assert!(!has(&h, "loupe:renderFailed"), "a catalogued video is not a broken photo");
    assert!(has(&h, "info:duration"), "Info lists the clip's running time");
    assert_eq!(h.app.renderer.in_flight(), 0, "no render reads the clip's file to fail");
    assert!(h.app.renderer.failure(crate::render::Slot::Main).is_none());
    shot(&mut h, "video-loupe.png");
}

/// A still keeps the old behaviour: no video card, and Info has no Duration row.
#[test]
fn a_still_has_no_video_card_or_duration_row() {
    let mut h = video_app(ViewMode::Detail);
    select(&mut h, PhotoId(2));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "canvas:videoCard"));
    assert!(!has(&h, "info:duration"));
}

/// A very small loupe pane (e.g. docked or small window) never crashes video_card with f32::clamp bounds.
#[test]
fn video_card_never_panics_on_small_pane() {
    let mut session = lightcraft_engine::Session::new();
    let mut p = Photo::new(PhotoId(1), Source::File { path: "/test.mp4".into() }, "test.mp4", "MP4", 1920, 1080, "2026-05-04T10:00:00");
    p.kind = MediaKind::Video;
    p.duration = Some(10.0);
    session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let app = LightcraftApp::new(session, Services::default());
    // Small window: height 100px (canvas height < 120px)
    let mut h = Headless::new(app, [300.0, 100.0], 1.0);
    select(&mut h, PhotoId(1));
    h.app.ui.view = ViewMode::Detail;
    h.settle(SETTLE);
    assert!(has(&h, "canvas:videoCard"), "the card is drawn even in small pane without panicking");
}
