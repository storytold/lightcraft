//! Notes 3, 11 and 31 of 2026-10-24: what the photographer sees is always their edit. A photo with
//! edits never shows the camera's JPEG (unedited and uncropped) in the loupe or a cell, not while a
//! slider moves, not when the user comes back to it, not after a sync; its last rendered picture
//! (any size, scaled) or a render with its settings comes first. Only a photo without edits uses
//! the camera JPEG as its cheap first picture.

use std::sync::Arc;
use std::time::{Duration, Instant};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use serde_json::json;

use crate::headless::Headless;
use crate::render::Slot;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

/// A headless app with `n` raws (camera JPEG instant, raw decodes taking `decode` each).
fn raws(n: u64, decode: Duration) -> Headless {
    let mut s = lightcraft_engine::Session::new();
    s.media.preview_loader = Some(Arc::new(|_: &str, edge: usize| {
        let w = edge.clamp(1, 96);
        let h = (w * 2 / 3).max(1);
        Some(lightcraft_raster::Rgba8 { width: w, height: h, data: vec![[255, 0, 255, 255]; w * h] })
    }));
    s.media.file_loader = Some(Arc::new(move |_, _| {
        std::thread::sleep(decode);
        Ok((lightcraft_raster::Rgb32f::new(24, 16), Default::default()))
    }));
    for id in 1..=n {
        let mut p =
            Photo::new(PhotoId(id), Source::File { path: format!("shoot/r{id}.cr3") }, &format!("r{id}.cr3"), "CR3", 8192, 5464, "2026-10-24");
        p.kind = lightcraft_catalog::MediaKind::Raw;
        // as imported: the camera's defaults, no edits
        p.develop = Arc::new(p.camera_defaults());
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let app = LightcraftApp::new(s, Services { png: None, ..Default::default() });
    Headless::new(app, [1400.0, 900.0], 1.0)
}

fn exec(h: &mut Headless, command: &str, params: serde_json::Value) {
    let r = h.request("engine.execute", json!({"command": command, "params": params}), T);
    assert_eq!(r["ok"], true, "{command}: {r}");
}

/// Step frames until `done`, at most `max`; every frame's loupe picture goes to `seen`.
fn frames(h: &mut Headless, max: Duration, seen: &mut Vec<(PhotoId, &'static str)>, done: impl Fn(&Headless) -> bool) -> bool {
    let t0 = Instant::now();
    while t0.elapsed() < max {
        h.step();
        seen.extend(h.app.loupe_shown);
        if done(h) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    false
}

fn rendered(h: &Headless, id: PhotoId) -> bool {
    h.app.loupe_shown == Some((id, "render")) && !h.app.renderer.is_pending(Slot::Main)
}

/// Open photo `id` in the loupe, edited (exposure and a crop), and wait for its render.
fn edited_on_screen(h: &mut Headless, id: u64) {
    exec(h, "library.select", json!({"ids": [id]}));
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(h, "develop.set", json!({"values": {"light.exposure": 1.0}}));
    let p = h.app.session.catalog.photo(PhotoId(id)).unwrap();
    let mut d = (*p.develop).clone();
    d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.2, y0: 0.2, x1: 0.8, y1: 0.8 };
    h.app.session.set_develop(PhotoId(id), d, "Crop").unwrap();
    let mut seen = Vec::new();
    assert!(frames(h, T, &mut seen, |h| rendered(h, PhotoId(id))), "the edit is rendered: {:?}", h.app.loupe_shown);
}

/// Note 3: moving a slider on an edited, cropped photo kept flipping the loupe to the camera JPEG.
#[test]
fn a_slider_drag_on_an_edited_photo_never_shows_the_camera_jpeg() {
    let mut h = raws(6, Duration::from_millis(300));
    edited_on_screen(&mut h, 3);
    let id = PhotoId(3);
    let mut seen = Vec::new();
    h.app.run("develop.beginInteraction", json!({"label": "Exposure"})).unwrap();
    for i in 0..12 {
        exec(&mut h, "develop.set", json!({"values": {"light.exposure": 1.0 + f64::from(i) * 0.1}}));
        // whatever lets the loupe's own texture go mid-drag (a lost GPU texture, a display change,
        // a cache reset): what stands in must still be the edit
        if i == 5 {
            h.app.renderer.release(Slot::Main);
        }
        frames(&mut h, Duration::from_millis(120), &mut seen, |_| false);
    }
    h.app.run("develop.endInteraction", json!({})).unwrap();
    assert!(frames(&mut h, T, &mut seen, |h| rendered(h, id)), "the last value is rendered");
    let camera: Vec<_> = seen.iter().filter(|(p, s)| *p == id && *s == "embedded").collect();
    assert!(camera.is_empty(), "frames that showed the unedited camera JPEG: {} of {}", camera.len(), seen.len());
    // (the frame the texture went, and the next while its stand-in is fetched from memory)
    let blank = seen.iter().filter(|(p, s)| *p == id && *s == "none").count();
    assert!(blank <= 3, "{blank} frames without a picture");
}

/// Notes 3/11/31 combined: "land on any photo that I just edited and you see the camera JPG".
#[test]
fn coming_back_to_an_edited_photo_shows_its_edit_first() {
    let mut h = raws(6, Duration::from_millis(400));
    edited_on_screen(&mut h, 3);
    let id = PhotoId(3);
    // away to an unedited photo (its camera JPEG is fine there), and back
    exec(&mut h, "library.next", json!({}));
    let mut seen = Vec::new();
    frames(&mut h, Duration::from_millis(600), &mut seen, |_| false);
    exec(&mut h, "library.previous", json!({}));
    seen.clear();
    assert!(frames(&mut h, T, &mut seen, |h| rendered(h, id) || h.app.loupe_shown == Some((id, "cached"))));
    let first = seen.iter().find(|(p, _)| *p == id).copied();
    assert!(matches!(first, Some((_, "cached" | "render" | "last"))), "the first picture of the edited photo: {first:?}");
    let camera: Vec<_> = seen.iter().filter(|(p, s)| *p == id && *s == "embedded").collect();
    assert!(camera.is_empty(), "the camera JPEG stood in for the edit: {camera:?}");
}

/// Note 11 with note 3: the photo's settings changed while it wasn't on screen (Auto Sync, Sync,
/// a preset on the selection): its preview for the new settings isn't there yet, and stepping to it
/// showed the camera JPEG. Its last rendered picture comes first, then the render.
#[test]
fn a_photo_synced_while_away_shows_its_last_render_not_the_camera_jpeg() {
    let mut h = raws(6, Duration::from_millis(400));
    edited_on_screen(&mut h, 3);
    let id = PhotoId(3);
    exec(&mut h, "library.next", json!({}));
    let mut seen = Vec::new();
    frames(&mut h, Duration::from_millis(600), &mut seen, |_| false);
    // synced from photo 4: another exposure for photo 3
    let mut d = (*h.app.session.catalog.photo(id).unwrap().develop).clone();
    d.light.exposure = -0.7;
    h.app.session.set_develop(id, d, "Sync").unwrap();
    exec(&mut h, "library.previous", json!({}));
    seen.clear();
    assert!(frames(&mut h, T, &mut seen, |h| rendered(h, id)), "its render comes");
    let first = seen.iter().find(|(p, _)| *p == id).copied();
    assert!(matches!(first, Some((_, "last" | "render" | "cached"))), "the first picture of the synced photo: {first:?}");
    let camera: Vec<_> = seen.iter().filter(|(p, s)| *p == id && *s == "embedded").collect();
    assert!(camera.is_empty(), "the camera JPEG stood in for the edit: {camera:?}");
}

/// An unedited raw still gets its camera JPEG at once (the cull fix stays).
#[test]
fn an_unedited_raw_still_shows_its_camera_jpeg_first() {
    let mut h = raws(4, Duration::from_millis(800));
    exec(&mut h, "library.select", json!({"ids": [2]}));
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let mut seen = Vec::new();
    assert!(frames(&mut h, Duration::from_secs(3), &mut seen, |h| h.app.loupe_shown == Some((PhotoId(2), "embedded"))), "{seen:?}");
}

/// An edited raw that was never rendered (edited by a preset at import, synced before it was ever
/// seen): its first picture is a fast draft with its settings made from the camera JPEG, at once,
/// in the loupe and the grid, never the camera JPEG itself, while the raw is decoded.
#[test]
fn an_edited_raw_never_rendered_shows_a_draft_of_its_edit_at_once() {
    let mut h = raws(6, Duration::from_millis(1500));
    for id in 1..=6 {
        let mut d = (*h.app.session.catalog.photo(PhotoId(id)).unwrap().develop).clone();
        d.light.exposure = 0.8;
        d.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.25, y0: 0.0, x1: 0.75, y1: 1.0 };
        h.app.session.set_develop(PhotoId(id), d, "Preset").unwrap();
    }
    exec(&mut h, "library.select", json!({"ids": [2]}));
    let r = h.request("ui.set", json!({"view": "detail"}), T);
    assert_eq!(r["ok"], true, "{r}");
    let mut seen = Vec::new();
    assert!(frames(&mut h, Duration::from_secs(1), &mut seen, |h| h.app.loupe_shown == Some((PhotoId(2), "draft"))), "{seen:?}");
    assert!(!seen.iter().any(|(_, s)| *s == "embedded"), "the camera JPEG: {seen:?}");
    // the filmstrip cells too: drafts, no camera JPEG
    let film = frames(&mut h, Duration::from_secs(2), &mut seen, |h| h.app.thumb_first.summary().empty_now == 0);
    assert!(film, "filmstrip cells empty: {:?}", h.app.thumb_first.summary());
    for id in 1..=6 {
        if let Some(t) = h.app.renderer.thumb(PhotoId(id)) {
            assert_ne!(t.quick, Some(lightcraft_engine::media::QuickSource::Embedded), "photo {id}'s cell");
        }
    }
}

/// Whether the loupe's zoom window on screen is the one it wants for photo `id`.
fn window_shown(h: &Headless, id: PhotoId) -> bool {
    h.app.region_view.is_some_and(|v| v.photo == id && h.app.renderer.textures.get(&Slot::Region).is_some_and(|t| t.key == v.key && t.photo == id))
}

/// Step frames until photo `id`'s zoom window is up (at most `T`): whether a render of a window
/// was asked for meanwhile (one kept or made ahead is shown without one).
fn rendered_to_window(h: &mut Headless, id: PhotoId) -> bool {
    let t0 = Instant::now();
    let mut rendered = h.app.renderer.is_pending(Slot::Region);
    while !window_shown(h, id) && t0.elapsed() < T {
        h.step();
        rendered |= h.app.renderer.is_pending(Slot::Region);
    }
    assert!(window_shown(h, id), "photo {id:?}'s window: {:?}", h.app.region_view);
    rendered
}

/// Culling at 100 %: stepping to the next photo and back rendered every window again, each time
/// "Rendering…" for as long as a raw takes to decode. The next photo's window at the same place is
/// made ahead while the user looks at this one, and the windows shown before are kept: each step
/// shows its window without rendering one.
#[test]
fn zoomed_in_stepping_on_and_back_shows_each_window_at_once() {
    let mut h = raws(4, Duration::from_millis(300));
    exec(&mut h, "library.select", json!({"ids": [2]}));
    let r = h.request("ui.set", json!({"view": "detail", "right": "none", "filmstrip": false}), T);
    assert_eq!(r["ok"], true, "{r}");
    exec(&mut h, "view.zoom100", json!({}));
    let mut seen = Vec::new();
    assert!(frames(&mut h, T, &mut seen, |h| window_shown(h, PhotoId(2))), "photo 2's window: {:?}", h.app.region_view);
    // while photo 2 is looked at, the windows of the photos after and before it are made
    // (until the renderer says they all are)
    let quiet = frames(&mut h, T, &mut seen, |h| h.app.renderer.windows_ahead_done.is_some_and(|k| h.app.region_view.is_some_and(|v| v.key == k)));
    assert!(quiet, "the windows ahead made: {:?}", h.app.renderer.kept_windows());
    assert!(h.app.renderer.kept_windows().0 >= 2, "the photos on either side kept: {:?}", h.app.renderer.kept_windows());
    // (in the library's order: the photos on either side of photo 2)
    let order = h.app.session.visible_cloned();
    let at = order.iter().position(|p| *p == PhotoId(2)).unwrap();
    let (next, prev) = (order[at + 1], order[at - 1]);
    exec(&mut h, "library.next", json!({}));
    assert!(!rendered_to_window(&mut h, next), "the step to the next photo shows the window made ahead");
    exec(&mut h, "library.previous", json!({}));
    assert!(!rendered_to_window(&mut h, PhotoId(2)), "back to photo 2: its kept window");
    exec(&mut h, "library.previous", json!({}));
    assert!(!rendered_to_window(&mut h, prev), "on to the photo before it: made ahead from photo 2");
    let (count, bytes) = h.app.renderer.kept_windows();
    assert!(count <= crate::render::KEPT_WINDOWS && bytes <= crate::render::KEPT_WINDOW_BYTES, "{count} windows, {bytes} bytes");
}
