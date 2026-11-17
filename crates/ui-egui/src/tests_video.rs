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
    video_app_with(view, None)
}

/// [`video_app`] whose session decodes posters with `video_loader` (as `with_fs` installs one).
fn video_app_with(view: ViewMode, video_loader: Option<lightcraft_engine::media::FileLoader>) -> Headless {
    let mut session = lightcraft_engine::Session::new();
    session.media.video_loader = video_loader;
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

/// A still with a stray duration stays a still: no clip badges and no "Video" in its spoken
/// description.
#[test]
fn a_still_with_a_stray_duration_is_not_a_video() {
    let mut session = lightcraft_engine::Session::new();
    let mut p = Photo::new(
        PhotoId(1),
        Source::File { path: "/lightcraft-video-test/still.jpg".into() },
        "still.jpg",
        "JPEG",
        1920,
        1080,
        "2026-05-04T10:00:00",
    );
    p.duration = Some(84.0);
    session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let app = LightcraftApp::new(
        session,
        Services {
            png: Some(Box::new(png)),
            write: Some(Box::new(|path: &str, bytes: &[u8]| std::fs::write(path, bytes).map_err(|e| e.to_string()))),
            write_new: Some(Box::new(lightcraft_engine::export::write_file_new)),
            ..Default::default()
        },
    );
    let spoken = crate::panels::grid::cell_description(&app, app.session.catalog.photo(PhotoId(1)).unwrap());
    assert!(!spoken.contains(crate::i18n::tr("Video")), "{spoken}");
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.app.ui.view = ViewMode::PhotoGrid;
    h.app.ui.thumb_size = 180.0;
    h.settle(SETTLE);
    assert!(!has(&h, "badge:videoDuration:1") && !has(&h, "badge:videoFormat:1"), "a still gets no video badges");
}

/// In a square cell without a thumbnail, the running time hangs above the placeholder's badge
/// bar (the cell's lower margin is lower), so the rating bar cannot cover it.
#[test]
fn a_square_video_without_a_thumbnail_keeps_its_time_above_the_badge_bar() {
    let mut h = video_app(ViewMode::SquareGrid);
    h.app.ui.show_filenames = false;
    h.settle(SETTLE);
    let rect = |name: &str| h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r);
    let thumb = rect("thumb:1").expect("the cell");
    let duration = rect("badge:videoDuration:1").expect("the running time");
    // The placeholder is the image area inset 20 and its 24 px badge bar starts at cell.bottom()-54.
    assert!(duration.bottom() <= thumb.bottom() - 54.0, "{duration:?} in {thumb:?}");
}

/// At the smallest thumbnails a clip's two pills never cover each other: the running time stays
/// and the container gives way when they would touch (square grid, filename line hidden).
#[test]
fn small_square_cells_keep_the_format_and_duration_pills_apart() {
    let mut h = video_app(ViewMode::SquareGrid);
    h.app.ui.show_filenames = false;
    for size in [crate::state::THUMB_SIZE_MIN, crate::state::THUMB_SIZE_MIN + 20.0, 180.0] {
        h.app.ui.thumb_size = size;
        h.settle(SETTLE);
        let rect = |name: &str| h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r);
        for id in [1, 3] {
            let duration = rect(&format!("badge:videoDuration:{id}"));
            if let (Some(d), Some(f)) = (duration, rect(&format!("badge:videoFormat:{id}"))) {
                assert!(!d.intersects(f), "{size}: clip {id}'s pills overlap: {d:?} {f:?}");
            }
            assert!(duration.is_some(), "{size}: clip {id} shows its running time");
        }
    }
}

/// A virtual copy's tag holds the cell's top right corner: a clip's container pill moves below it
/// rather than under it, in both grids (the square grid with its filename line hidden).
#[test]
fn a_video_copy_keeps_its_format_badge_clear_of_the_copy_tag() {
    for view in [ViewMode::PhotoGrid, ViewMode::SquareGrid] {
        let mut h = video_app(view);
        h.app.ui.show_filenames = false;
        let id = PhotoId(1);
        let mut copy = h.app.session.catalog.photo(id).unwrap().as_ref().clone();
        copy.id = PhotoId(9);
        copy.copy_name = Some("Copy 1".into());
        h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(copy) }).unwrap();
        h.settle(SETTLE);
        let rect = |name: &str| h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r);
        let (Some(thumb), Some(badge)) = (rect("thumb:9"), rect("badge:videoFormat:9")) else {
            panic!("{view:?}: the copy's cell and container badge are drawn");
        };
        let (thumb1, badge1) = (rect("thumb:1").expect("the original's cell"), rect("badge:videoFormat:1").expect("the original's badge"));
        // the copy tag is 20 points tall: the copy's pill sits at least that much lower in its cell
        let lowered = (badge.top() - thumb.top()) - (badge1.top() - thumb1.top());
        assert!(lowered >= 20.0, "{view:?}: {badge:?} in {thumb:?} vs {badge1:?} in {thumb1:?}");
        assert!(thumb.contains_rect(badge), "{view:?}: {badge:?} stays on its cell {thumb:?}");
        shot(&mut h, &format!("video-copy-{view:?}.png"));
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
    // no video loader: a build without decoding says so on the card itself (no render did)
    let text = h.view.painted_text().join("\n");
    if lightcraft_codecs::video_stream::video_decoding_available() {
        assert!(text.contains(crate::i18n::tr("Video catalogued — playback and trim are coming in a later update")), "{text}");
    } else {
        let why = crate::i18n::tr(lightcraft_codecs::video_stream::NO_VIDEO_DECODING);
        assert!(text.contains(&crate::i18n::tr_format!("No video frame: {reason}", reason = why)), "{text}");
    }
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

/// A video loader that counts its calls and answers with a flat blue-grey picture, or `fail`.
fn poster_loader(calls: std::sync::Arc<std::sync::atomic::AtomicUsize>, fail: Option<&'static str>) -> lightcraft_engine::media::FileLoader {
    std::sync::Arc::new(move |_path: &str, max_edge: usize, _with| {
        calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(why) = fail {
            return Err(why.into());
        }
        let (w, h) = (320.min(max_edge), 180.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.1, 0.2, 0.45]; w * h] }, Default::default()))
    })
}

/// With video decoding, a clip's grid cell, filmstrip and loupe show its poster frame: no card,
/// no failure.
#[test]
fn posters_show_in_the_grid_and_the_loupe() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut h = video_app_with(ViewMode::PhotoGrid, Some(poster_loader(calls.clone(), None)));
    for id in [1, 3, 4] {
        assert!(h.app.renderer.thumb(PhotoId(id)).is_some(), "clip {id} has its poster in the grid");
    }
    shot(&mut h, "video-posters-grid.png");
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "canvas:videoCard"), "the poster stands in for the card");
    assert!(matches!(h.app.loupe_shown.map(|l| l.1), Some("render" | "preview" | "thumb")), "{:?}", h.app.loupe_shown);
    assert!(!has(&h, "notice:videoFrame:info"));
    assert!(!has(&h, "loupe:renderFailed"));
    shot(&mut h, "video-posters-loupe.png");
}

/// A clip whose frames can't be decoded keeps its card, which says why, as Info does; the failed
/// poster isn't asked for again frame after frame.
#[test]
fn an_undecodable_clip_keeps_its_card_and_says_why_once() {
    const WHY: &str = "Apple ProRes video can't be decoded yet";
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut h = video_app_with(ViewMode::PhotoGrid, Some(poster_loader(calls.clone(), Some(WHY))));
    assert!(h.app.renderer.thumb(PhotoId(1)).is_none());
    assert_eq!(h.app.renderer.failure(crate::render::Slot::Thumb(PhotoId(1))), Some(WHY));
    // the grid marks a failed clip as it marks a failed photo
    let marks = |h: &Headless| h.view.painted_text().iter().filter(|t| t.as_str() == "!").count();
    assert_eq!(marks(&h), 4, "the three clips' cells are marked, as the still's (its file is missing too)");
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(has(&h, "canvas:videoCard"));
    assert!(marks(&h) >= 4, "and so does the filmstrip");
    assert!(has(&h, "notice:videoFrame:info"), "Info says why the clip shows no frame");
    assert!(!has(&h, "loupe:renderFailed"), "a clip without a frame is not a broken photo");
    let asked = calls.load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..30 {
        h.step();
    }
    h.settle(SETTLE);
    assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), asked, "a failed poster is not retried every frame");
    shot(&mut h, "video-undecodable-loupe.png");
}

/// A portrait clip's poster is narrower than its square cell: the running time sits on the
/// picture's own corner, not out at the cell's edge beside it.
#[test]
fn a_portrait_poster_keeps_its_running_time_on_the_picture() {
    let portrait: lightcraft_engine::media::FileLoader = std::sync::Arc::new(|_path: &str, max_edge: usize, _with| {
        let (w, h) = (180.min(max_edge), 320.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.3, 0.3, 0.3]; w * h] }, Default::default()))
    });
    let mut h = video_app_with(ViewMode::SquareGrid, Some(portrait));
    h.app.ui.show_filenames = false;
    let mut p = h.app.session.catalog.photo(PhotoId(1)).unwrap().as_ref().clone();
    (p.width, p.height) = (1080, 1920);
    h.app.session.catalog.apply(Op::RemovePhoto { id: PhotoId(1) }).unwrap();
    h.app.session.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    h.settle(SETTLE);
    assert!(h.app.renderer.thumb(PhotoId(1)).is_some(), "the poster is drawn");
    let rect = |name: &str| h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r);
    let cell = rect("thumb:1").expect("the cell");
    let badge = rect("badge:videoDuration:1").expect("the running time");
    // the 9:16 picture is at most the cell's height × 9/16 wide, centred in the cell
    let half = cell.height() * 9.0 / 32.0;
    assert!(badge.right() <= cell.center().x + half + 0.5, "{badge:?} past the picture's right edge in {cell:?}");
    assert!(badge.left() >= cell.center().x - half - 0.5, "{badge:?} past the picture's left edge in {cell:?}");
}

/// The filmstrip puts a portrait clip's running time on its poster too, not beside it.
#[test]
fn a_portrait_poster_keeps_its_running_time_on_the_picture_in_the_filmstrip() {
    let portrait: lightcraft_engine::media::FileLoader = std::sync::Arc::new(|_path: &str, max_edge: usize, _with| {
        let (w, h) = (180.min(max_edge), 320.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.3, 0.3, 0.3]; w * h] }, Default::default()))
    });
    let mut h = video_app_with(ViewMode::Detail, Some(portrait));
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "edit", "filmstrip": true}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    let rect = |name: &str| h.app.widgets.iter().find(|(w, _)| w == name).map(|(_, r)| *r);
    let cell = rect("film:1").expect("the filmstrip cell");
    let badge = rect("badge:filmDuration:1").expect("the running time");
    // the cell's image area is inset 10 at the sides, 22 at the top and 8 at the bottom; the
    // 9:16 picture is centred in it, at most its height × 9/16 wide
    let area_h = cell.height() - 30.0;
    let centre = cell.center().x;
    let half = area_h * 9.0 / 32.0;
    assert!(badge.right() <= centre + half + 0.5 && badge.left() >= centre - half - 0.5, "{badge:?} beside the picture in {cell:?}");
}

/// A poster the activity row's ✕ stopped is no lasting failure: no mark, no warning, and it is
/// asked for again (and shows) rather than waiting for the catalog to change.
#[test]
fn a_stopped_poster_is_asked_for_again() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = calls.clone();
    let loader: lightcraft_engine::media::FileLoader = std::sync::Arc::new(move |_path: &str, max_edge: usize, _with| {
        // every clip's first decode is stopped
        if counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3 {
            return Err(lightcraft_engine::files::VIDEO_STOPPED.into());
        }
        let (w, h) = (320.min(max_edge), 180.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.1, 0.2, 0.45]; w * h] }, Default::default()))
    });
    let h = video_app_with(ViewMode::PhotoGrid, Some(loader));
    assert!(calls.load(std::sync::atomic::Ordering::Relaxed) > 3, "asked for again");
    for id in [1, 3, 4] {
        assert!(h.app.renderer.failure(crate::render::Slot::Thumb(PhotoId(id))).is_none(), "clip {id}: no lasting failure");
        assert!(h.app.renderer.thumb(PhotoId(id)).is_some(), "clip {id} shows its poster after all");
    }
    assert!(!h.app.renderer.failure_messages.iter().any(|m| m.contains("Stopped")), "no warning: {:?}", h.app.renderer.failure_messages);
}

/// While the loupe's own render of a clip fails (or hasn't come), its thumbnail's poster stands
/// in: no card over a picture that is there.
#[test]
fn the_thumbnails_poster_stands_in_for_a_failed_loupe_render() {
    let loader: lightcraft_engine::media::FileLoader = std::sync::Arc::new(move |_path: &str, max_edge: usize, _with| {
        // every render bigger than a thumbnail fails: only the grid's poster exists
        if max_edge > 512 {
            return Err("the clip's file can't be read".into());
        }
        let (w, h) = (320.min(max_edge), 180.min(max_edge));
        Ok((lightcraft_raster::Rgb32f { width: w, height: h, data: vec![[0.1, 0.2, 0.45]; w * h] }, Default::default()))
    });
    let mut h = video_app_with(ViewMode::PhotoGrid, Some(loader));
    assert!(h.app.renderer.thumb(PhotoId(1)).is_some());
    select(&mut h, PhotoId(1));
    let r = h.request("ui.set", json!({"view": "detail", "right": "info", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.settle(SETTLE);
    assert!(!has(&h, "canvas:videoCard"));
    // the loupe's own pictures gone (its render failed, its stand-in not back yet): the grid's
    // poster stands in for one frame, no card flashes up over it
    h.app.renderer.textures.remove(&crate::render::Slot::Main);
    h.app.renderer.textures.remove(&crate::render::Slot::Preview);
    h.step();
    assert!(h.app.renderer.thumb(PhotoId(1)).is_some());
    assert!(!has(&h, "canvas:videoCard"), "the thumbnail's poster shows, not the card");
    assert!(!has(&h, "loupe:renderFailed"));
}
