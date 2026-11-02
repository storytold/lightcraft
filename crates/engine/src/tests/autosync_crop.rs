//! LR-SYNC-PREVIOUS (N-A3, test notes 2026-10-25 note 15): a crop synced to a photo turned the
//! other way keeps what it means there: how tight it is, where it sits, its ratio lock turned to the
//! photo's own orientation. A landscape never gets a portrait-shaped crop, nor a vertical a
//! landscape one.

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;
use crate::sync_crop::synced_crop;

/// A crop of `frac` of the largest of shape `shape` at `angle` on a `w × h` frame, centred at the
/// normalized point `c` (moved inside), with `aspect` locked or free.
#[allow(clippy::too_many_arguments)]
fn crop_on(
    w: f64,
    h: f64,
    shape: f64,
    frac: f64,
    c: (f64, f64),
    angle: f64,
    aspect: Option<(u32, u32)>,
    flips: (bool, bool),
) -> lightcraft_develop::Crop {
    let f = lightcraft_geom::crop_fit_angle(w, h, angle, Some(shape)).rect_px(w, h);
    let r = lightcraft_geom::Rect::from_center(lightcraft_geom::Point::new(c.0 * w, c.1 * h), f.width() * frac, f.height() * frac);
    let g = lightcraft_geom::CropGeometry { rect: r.scale(1.0 / w, 1.0 / h), angle }.constrained(w, h);
    lightcraft_develop::Crop { geometry: g, aspect, flip_h: flips.0, flip_v: flips.1 }
}

/// A crop synced from a vertical to a landscape (and back) keeps its tightness and position: 80 %
/// of the largest crop of its shape on the vertical is 80 % of the largest of the turned shape on
/// the landscape, its centre at the same normalized point, moved in only as far as the frame needs.
#[test]
fn a_synced_crop_keeps_its_tightness_and_position_across_orientations() {
    let (v, l) = ((4000.0, 6000.0), (6000.0, 4000.0));
    for aspect in [Some((2, 3)), None] {
        let src = crop_on(v.0, v.1, 2.0 / 3.0, 0.8, (0.5, 1.0 / 3.0), 0.0, aspect, (false, false));
        let out = synced_crop(&src, v, l);
        let r = out.geometry.rect_px(l.0, l.1);
        let largest = lightcraft_geom::crop_fit_angle(l.0, l.1, 0.0, Some(1.5)).rect_px(l.0, l.1);
        assert!((r.width() / r.height() - 1.5).abs() < 1e-6, "{aspect:?}: turned to 3:2, got {}", r.width() / r.height());
        assert!((r.width() / largest.width() - 0.8).abs() < 1e-6, "{aspect:?}: 80 % of the largest, got {}", r.width() / largest.width());
        assert!((r.center().x / l.0 - 0.5).abs() < 1e-6, "{aspect:?}: centred across");
        let sc = src.geometry.rect.center();
        assert!((r.center().y / l.1 - sc.y).abs() < 1e-6, "{aspect:?}: the same height in the frame, {} vs {}", r.center().y / l.1, sc.y);
        assert!(out.geometry.is_within_image(l.0, l.1));
        if aspect.is_some() {
            assert_eq!(out.aspect, Some((3, 2)));
        }
    }
    // a centre that would put the crop off the frame moves in only as far as it must
    let src = crop_on(v.0, v.1, 2.0 / 3.0, 0.9, (0.5, 0.45), 0.0, Some((2, 3)), (false, false));
    let mut low = src;
    let (w, h) = (low.geometry.rect.width(), low.geometry.rect.height());
    low.geometry.rect = lightcraft_geom::Rect::new(0.5 - w / 2.0, 1.0 - h, 0.5 + w / 2.0, 1.0);
    let out = synced_crop(&low, v, l);
    let r = out.geometry.rect_px(l.0, l.1);
    assert!((r.y1 - l.1).abs() < 1e-3, "down against the bottom edge: {r:?}");
    assert!(out.geometry.is_within_image(l.0, l.1));
    // a crop with room to keep its place keeps it exactly, normalized
    let src = crop_on(l.0, l.1, 1.5, 0.5, (0.3, 0.6), 0.0, Some((3, 2)), (false, false));
    let out = synced_crop(&src, l, v);
    let r = out.geometry.rect_px(v.0, v.1);
    assert!((r.center().x / v.0 - 0.3).abs() < 1e-6 && (r.center().y / v.1 - 0.6).abs() < 1e-6, "{:?}", r.center());
    assert_eq!(out.aspect, Some((2, 3)));
    // same shape: the very rectangle
    assert_eq!(synced_crop(&src, l, (3000.0, 2000.0)), src);
}

/// Whatever the crop, a landscape never gets a portrait-shaped crop and a vertical never a
/// landscape one: ratio locked or free, at a straighten angle, with flips, from either orientation;
/// and the crop always stays inside the photo.
#[test]
fn a_synced_crop_never_gets_the_other_orientation() {
    let frames = [(4000.0, 6000.0), (6000.0, 4000.0), (3000.0, 4000.0), (5472.0, 3648.0)];
    for a in frames {
        for o in frames {
            for (shape, aspect) in [(2.0 / 3.0, Some((2, 3))), (1.5, Some((3, 2))), (0.8, None), (1.7, None), (1.0, Some((1, 1)))] {
                for angle in [0.0, 3.0, -7.5, 20.0] {
                    for flips in [(false, false), (true, false), (false, true), (true, true)] {
                        for (frac, c) in [(1.0, (0.5, 0.5)), (0.7, (0.2, 0.8)), (0.4, (0.9, 0.1))] {
                            let src = crop_on(a.0, a.1, shape, frac, c, angle, aspect, flips);
                            let out = synced_crop(&src, a, o);
                            let r = out.geometry.rect_px(o.0, o.1);
                            let s = r.width() / r.height();
                            let what = format!("{a:?} → {o:?}, shape {shape} {aspect:?}, {angle}°, {flips:?}, {frac} at {c:?}: {s}");
                            assert!(out.geometry.is_within_image(o.0, o.1), "inside: {what}");
                            assert_eq!((out.flip_h, out.flip_v), flips, "{what}");
                            assert_eq!(out.geometry.angle, angle, "{what}");
                            if (a.0 / a.1 - o.0 / o.1).abs() <= 0.005 * a.0 / a.1 {
                                continue;
                            }
                            let wide = o.0 > o.1;
                            if (shape - 1.0f64).abs() > 1e-3 {
                                assert_eq!(s > 1.0, wide, "the photo's own orientation: {what}");
                            }
                            if let Some((x, y)) = out.aspect.filter(|(x, y)| x != y) {
                                assert_eq!(x > y, wide, "the locked ratio turned too: {what}");
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Photo `id`'s frame as its crop sees it now.
fn frame(s: &Session, id: PhotoId) -> (f64, f64) {
    s.crop_frame(id, s.develop_of(id).unwrap().orientation)
}

/// In the app: Auto Sync of a locked crop, low in the frame and a little straightened, to a photo
/// turned the other way (rotated a quarter). The other photo gets the lock turned to its own
/// orientation and a crop of the same tightness at the same place in its own frame. On main it got
/// the active photo's numbers as they were: a landscape lock on a vertical (or the reverse).
#[test]
fn auto_sync_of_a_crop_reaches_a_photo_turned_the_other_way() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    if (frame(&s, b).0 > frame(&s, b).1) == (frame(&s, a).0 > frame(&s, a).1) {
        s.execute("photo.rotateRight", &json!({"ids": [b.0]})).unwrap();
    }
    let ((aw, ah), (bw, bh)) = (frame(&s, a), frame(&s, b));
    assert_ne!(aw > ah, bw > bh, "b is turned the other way");
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let (lock, shape) = if aw > ah { ((3, 2), 1.5) } else { ((2, 3), 2.0 / 3.0) };
    let mut d = (*s.develop_of(a).unwrap()).clone();
    d.crop = crop_on(aw, ah, shape, 0.6, (0.5, 0.7), 2.0, Some(lock), (false, false));
    let want = synced_crop(&d.crop, (aw, ah), (bw, bh));
    s.set_develop(a, d, "Crop").unwrap();
    let cb = s.develop_of(b).unwrap().crop;
    assert_eq!(cb.aspect, Some((lock.1, lock.0)), "the lock turned to b's orientation");
    let r = cb.geometry.rect_px(bw, bh);
    assert_eq!(r.width() > r.height(), bw > bh, "b's own orientation: {r:?} on {bw}×{bh}");
    let largest = lightcraft_geom::crop_fit_angle(bw, bh, 2.0, Some(1.0 / shape)).rect_px(bw, bh);
    assert!((r.width() / largest.width() - 0.6).abs() < 0.01, "as tight as on a: {}", r.width() / largest.width());
    assert!(cb.geometry.is_within_image(bw, bh));
    assert_eq!(cb, want);
    // one undo puts b back
    s.execute("edit.undo", &json!({})).unwrap();
    assert_ne!(s.develop_of(b).unwrap().crop, want);
}

/// A square photo has no orientation: a synced crop keeps its own there, lock and shape, as tight
/// and placed as on the source (review of #3572: it was turned to a vertical).
#[test]
fn a_crop_synced_to_a_square_photo_keeps_its_own_orientation() {
    let (l, sq) = ((6000.0, 4000.0), (4000.0, 4000.0));
    let src = crop_on(l.0, l.1, 1.5, 0.7, (0.5, 0.5), 0.0, Some((3, 2)), (false, false));
    let out = synced_crop(&src, l, sq);
    assert_eq!(out.aspect, Some((3, 2)));
    let r = out.geometry.rect_px(sq.0, sq.1);
    assert!((r.width() / r.height() - 1.5).abs() < 1e-6, "{r:?}");
    assert!(out.geometry.is_within_image(sq.0, sq.1));
    let v = (4000.0, 6000.0);
    let src = crop_on(v.0, v.1, 2.0 / 3.0, 0.7, (0.5, 0.5), 0.0, Some((2, 3)), (false, false));
    assert_eq!(synced_crop(&src, v, sq).aspect, Some((2, 3)));
}

/// With Auto Sync on, a change of the ratio lock alone (no crop moved) leaves each other photo's
/// own crop rectangle and turns the lock to its orientation (review of #3572: the whole crop was
/// replaced by the active photo's).
#[test]
fn auto_sync_of_a_ratio_lock_alone_keeps_each_photos_crop() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    if (frame(&s, b).0 > frame(&s, b).1) == (frame(&s, a).0 > frame(&s, a).1) {
        s.execute("photo.rotateRight", &json!({"ids": [b.0]})).unwrap();
    }
    let ((aw, ah), (bw, bh)) = (frame(&s, a), frame(&s, b));
    // b has a crop of its own, free
    let mut db = (*s.develop_of(b).unwrap()).clone();
    db.crop = crop_on(bw, bh, bw / bh, 0.5, (0.3, 0.4), 0.0, None, (false, false));
    let own = db.crop.geometry;
    s.set_develop(b, db, "Crop").unwrap();
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    let lock = if aw > ah { (3, 2) } else { (2, 3) };
    let mut d = (*s.develop_of(a).unwrap()).clone();
    d.crop.aspect = Some(lock);
    s.set_develop(a, d, "Aspect").unwrap();
    let cb = s.develop_of(b).unwrap().crop;
    assert_eq!(cb.geometry, own, "b keeps its own crop");
    assert_eq!(cb.aspect, Some((lock.1, lock.0)), "the lock turned to b's orientation");
}
