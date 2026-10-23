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

fn has(h: &Headless, id: &str) -> bool {
    h.app.widgets.iter().any(|(w, _)| w == id)
}

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

/// Opening a clip in the loupe shows its card — name and running time, and what this build does
/// with it — instead of "This photo can't be opened" and the failure pill.
#[test]
fn the_loupe_shows_a_video_card_instead_of_a_render_error() {
    let mut h = video_app(ViewMode::Detail);
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "canvas:videoCard"), "the card stands in for the frame");
    assert_eq!(h.app.loupe_shown.map(|l| l.1), Some("video"), "ui.inspect says what the loupe shows");
    assert!(!has(&h, "loupe:renderFailed"), "a catalogued video is not a broken photo");
    assert!(has(&h, "info:duration"), "Info lists the clip's running time");
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
