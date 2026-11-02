//! LRC-DEV-AUTOSYNC (N-A3, test notes 2026-10-24 note 18): Auto Sync sets every selected photo to
//! the active photo's value, and where white balance is a mode or another scale, each photo applies
//! it to itself. As Shot gives each photo its own camera white, Auto each photo's own from its
//! pixels, a light preset its numbers. A typed or dragged Temp is the same number on each photo of
//! the same scale (the same Kelvin on raws edited in Kelvin; an old relative-scale value converted
//! first, never read as Kelvin), moves a photo of another scale (a JPEG beside a raw) by as much as
//! the active photo, and leaves each photo's own tint.

use lightcraft_catalog::PhotoId;
use lightcraft_develop::{WbMode, WbScale, WhiteBalance};
use serde_json::json;

use crate::Session;

/// Give photo `id` a decoded raw source (thumbnail and preview) with as-shot white `(temp, tint)`;
/// `legacy`: a camera whose settings without a scale were written on the relative scale; `warm`
/// tints its pixels, so Auto finds another white on each photo.
fn raw_as_shot(s: &mut Session, id: PhotoId, temp: f64, tint: f64, legacy: bool, warm: f32) {
    use crate::media::SourceLevel;
    let mut img = lightcraft_raster::Rgb32f::new(48, 32);
    for (i, px) in img.data.iter_mut().enumerate() {
        let v = 0.1 + 0.6 * ((i % 48) as f32 / 48.0);
        *px = [v * warm, v, v / warm];
    }
    let img = std::sync::Arc::new(img);
    let info =
        lightcraft_pipeline::SourceInfo { raw: true, legacy_relative_wb: legacy, as_shot_temp: temp, as_shot_tint: tint, ..Default::default() };
    s.media.insert_with_info(id, SourceLevel::Thumb, img.clone(), info.clone());
    s.media.insert_with_info(id, SourceLevel::Preview, img, info);
}

/// The white balance photo `id` renders with (Kelvin, tint).
fn wb(s: &Session, id: PhotoId) -> (f64, f64) {
    lightcraft_pipeline::local::effective_wb(&s.source_info(id), &s.develop_of(id).unwrap())
}

fn close(x: (f64, f64), y: (f64, f64)) -> bool {
    (x.0 - y.0).abs() < 1.0 && (x.1 - y.1).abs() < 0.5
}

/// Three demo photos (a active, b, c) selected, Auto Sync on, each a raw of another as-shot white;
/// c still holds an old relative-scale white (7000 there meant "a little warmer than as shot").
fn three_raws() -> (Session, [PhotoId; 3]) {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(3).collect();
    let (a, b, c) = (ids[0], ids[1], ids[2]);
    raw_as_shot(&mut s, a, 5200.0, 4.0, false, 1.0);
    raw_as_shot(&mut s, b, 3300.0, -12.0, false, 1.4);
    raw_as_shot(&mut s, c, 6100.0, 8.0, true, 0.7);
    let mut d = (*s.develop_of(c).unwrap()).clone();
    d.wb = WhiteBalance { mode: WbMode::Custom, temp: 7000.0, tint: 0.0, scale: WbScale::Legacy };
    s.catalog.apply(lightcraft_catalog::Op::SetDevelop { id: c, settings: std::sync::Arc::new(d), label: "x".into(), edited: None }).unwrap();
    s.execute("library.select", &json!({"ids": [a.0, b.0, c.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    (s, [a, b, c])
}

/// As Shot and Auto are modes: each photo gets its own camera white, or its own Auto white worked
/// out on its own pixels, never the active photo's numbers. A light preset gives its numbers.
#[test]
fn auto_sync_white_balance_modes_apply_to_each_photo() {
    let (mut s, [a, b, c]) = three_raws();
    // As Shot: each photo's own camera white
    s.execute("develop.wb", &json!({"mode": "asShot"})).unwrap();
    for (id, own) in [(a, (5200.0, 4.0)), (b, (3300.0, -12.0)), (c, (6100.0, 8.0))] {
        assert!(close(wb(&s, id), own), "As Shot on photo {}: {:?}, its own is {own:?}", id.0, wb(&s, id));
        assert_eq!(s.develop_of(id).unwrap().wb.mode, WbMode::AsShot);
    }
    // a light preset: its numbers on every raw
    s.execute("develop.wb", &json!({"mode": "tungsten"})).unwrap();
    for id in [a, b, c] {
        assert!(close(wb(&s, id), (2850.0, 0.0)), "Tungsten on photo {}: {:?}", id.0, wb(&s, id));
    }
    // Auto: each photo's own, from its own pixels
    s.execute("develop.wb", &json!({"mode": "auto"})).unwrap();
    for id in [b, c] {
        let src = s.source_now(id, crate::media::SourceLevel::Thumb).unwrap();
        let own = lightcraft_pipeline::auto::auto_wb(&src, &s.source_info(id));
        assert!(close(wb(&s, id), own), "Auto on photo {}: {:?}, its own Auto is {own:?}", id.0, wb(&s, id));
        assert_eq!(s.develop_of(id).unwrap().wb.mode, WbMode::Auto);
    }
    assert!(!close(wb(&s, a), wb(&s, b)), "not the active photo's Auto copied over: {:?}", wb(&s, a));
    // one undo step puts every photo back
    s.execute("edit.undo", &json!({})).unwrap();
    for id in [a, b, c] {
        assert!(close(wb(&s, id), (2850.0, 0.0)), "undo, photo {}: {:?}", id.0, wb(&s, id));
    }
}

/// A Temp typed or dragged from As Shot: the same Kelvin on every raw, each keeping its own tint;
/// the photo holding an old relative-scale value gets the Kelvin too, never read on that scale.
#[test]
fn auto_sync_temp_is_the_same_kelvin_and_keeps_each_photos_tint() {
    let (mut s, [a, b, c]) = three_raws();
    s.execute("develop.wb", &json!({"mode": "asShot"})).unwrap();
    // typed
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 5600.0})).unwrap();
    assert!(close(wb(&s, a), (5600.0, 4.0)), "a: {:?}", wb(&s, a));
    assert!(close(wb(&s, b), (5600.0, -12.0)), "b keeps its own tint: {:?}", wb(&s, b));
    assert!(close(wb(&s, c), (5600.0, 8.0)), "c keeps its own tint: {:?}", wb(&s, c));
    // a drag, synced on release
    s.execute("develop.beginInteraction", &json!({"label": "Temp"})).unwrap();
    for v in [5000.0, 4600.0] {
        s.execute("develop.set", &json!({"control": "wb.temp", "value": v})).unwrap();
    }
    s.execute("develop.endInteraction", &json!({})).unwrap();
    assert!(close(wb(&s, b), (4600.0, -12.0)) && close(wb(&s, c), (4600.0, 8.0)), "b {:?} c {:?}", wb(&s, b), wb(&s, c));
    // a Tint move: the same tint on every photo, each keeping its Kelvin (all at 4600 here)
    s.execute("develop.set", &json!({"control": "wb.tint", "value": 10.0})).unwrap();
    for id in [a, b, c] {
        assert!(close(wb(&s, id), (4600.0, 10.0)), "photo {}: {:?}", id.0, wb(&s, id));
    }
}

/// A photo still holding an old relative-scale value: a Temp synced from a photo in Kelvin reaches
/// it as Kelvin, never as a number on the relative scale (where 6500 is as shot), which turned
/// photos blue (test notes 2026-10-15). Main already got this right; the guard keeps it so.
#[test]
fn auto_sync_temp_never_lands_on_an_old_relative_scale() {
    let (mut s, [_, _, c]) = three_raws();
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 5600.0})).unwrap();
    assert!((wb(&s, c).0 - 5600.0).abs() < 1.0, "c renders at {:?}", wb(&s, c));
    assert_eq!(s.develop_of(c).unwrap().wb.scale, WbScale::Kelvin);
}

/// A JPEG selected with a raw has no "same" Kelvin (its white balance is relative to its own
/// colours): it moves by as much as the raw did. Before, it got the raw's Kelvin number, read on
/// its own scale.
#[test]
fn auto_sync_temp_moves_a_jpeg_by_as_much_as_the_raw() {
    let mut s = Session::with_demo();
    let ids: Vec<PhotoId> = s.visible_cloned().into_iter().take(2).collect();
    let (a, b) = (ids[0], ids[1]);
    raw_as_shot(&mut s, a, 5200.0, 4.0, false, 1.0);
    // b: a JPEG (no raw white balance scale: 6500 / 0 = its own colours)
    let img = std::sync::Arc::new(lightcraft_raster::Rgb32f::filled(48, 32, [0.4, 0.4, 0.4]));
    let info = lightcraft_pipeline::SourceInfo { raw: false, as_shot_temp: 6500.0, as_shot_tint: 0.0, ..Default::default() };
    s.media.insert_with_info(b, crate::media::SourceLevel::Thumb, img.clone(), info.clone());
    s.media.insert_with_info(b, crate::media::SourceLevel::Preview, img, info);
    assert!(!s.source_info(b).raw);
    s.execute("library.select", &json!({"ids": [a.0, b.0], "active": a.0})).unwrap();
    s.execute("develop.autoSync", &json!({"on": true})).unwrap();
    s.execute("develop.wb", &json!({"mode": "asShot"})).unwrap();
    let before = wb(&s, b);
    s.execute("develop.set", &json!({"control": "wb.temp", "value": 5600.0})).unwrap();
    assert!(close(wb(&s, b), (before.0 + 400.0, before.1)), "the JPEG: {:?}, was {before:?}", wb(&s, b));
}

/// The other photos' pixels for Auto are read together (in parallel, review of #3571): in the
/// order asked, each photo its own, and a photo the library doesn't have is an error, not a panic.
#[test]
fn sources_for_auto_come_back_in_order_and_per_photo() {
    let (mut s, [a, b, c]) = three_raws();
    let want: Vec<_> = [b, c, a].iter().map(|&x| s.source_now(x, crate::media::SourceLevel::Thumb).unwrap()).collect();
    let got = s.sources_now(&[b, PhotoId(u64::MAX), c, a], crate::media::SourceLevel::Thumb);
    assert_eq!(got.iter().map(|(x, _)| *x).collect::<Vec<_>>(), vec![b, PhotoId(u64::MAX), c, a]);
    assert!(got[1].1.is_err());
    for (i, k) in [0, 2, 3].into_iter().enumerate() {
        let (img, info) = got[k].1.as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(img, &want[i]), "photo {k}");
        // the facts its decoder gave (the as-shot whites differ per photo here)
        assert_eq!(info.as_shot_temp, s.source_info(got[k].0).as_shot_temp, "photo {k}");
    }
    assert_eq!(got[0].1.as_ref().unwrap().1.as_shot_temp, 3300.0, "b's own");
}
