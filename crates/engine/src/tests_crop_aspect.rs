//! Issue #2253: a crop ratio keeps the orientation asked for. A named ratio ("2x3", "4x5",
//! "original") takes the crop's present orientation, landscape or portrait after X, as in
//! Lightroom; a custom `[w, h]` is used as given, so 2 × 3 is a portrait crop.

use serde_json::{Value, json};

use crate::Session;

/// The demo photo, landscape, with its crop's width and height in pixels.
fn crop_px(s: &Session) -> (f64, f64) {
    let p = s.catalog.photo(s.active().unwrap()).unwrap();
    let (w, h) = (p.width as f64, p.height as f64);
    let (w, h) = if p.develop.orientation.swaps_axes() { (h, w) } else { (w, h) };
    let r = p.develop.crop.geometry.rect_px(w, h);
    (r.width(), r.height())
}

fn landscape_photo() -> Session {
    let mut s = Session::with_demo();
    let id = s.catalog.photos().find(|p| p.width > p.height && !p.develop.orientation.swaps_axes()).map(|p| p.id).unwrap();
    s.selection = crate::Selection { ids: vec![id], active: Some(id) };
    s
}

fn aspect(s: &mut Session, p: Value) {
    s.execute("crop.aspect", &p).unwrap();
}

fn ratio(s: &Session) -> f64 {
    let (w, h) = crop_px(s);
    w / h
}

/// A named ratio follows the crop (and X keeps it portrait); a custom ratio is used as given. The window
/// reapplies the last ratio to an untouched photo with `orientation: "crop"`: on a landscape crop it lands
/// landscape, whatever photo it was chosen on. A bad `orientation` is refused on every path, the lock-as-is
/// ones ("current", an unlocked "toggle") included, and whether it is a wrong word or not a string at all;
/// nothing changes.
#[test]
fn ratios_keep_the_orientation_asked_for_and_a_bad_orientation_is_refused() {
    let mut s = landscape_photo();
    aspect(&mut s, json!({"aspect": "2x3"}));
    assert!((ratio(&s) - 1.5).abs() < 0.01, "landscape crop: {}", ratio(&s));
    s.execute("crop.rotateAspect", &json!({})).unwrap();
    assert!((ratio(&s) - 2.0 / 3.0).abs() < 0.01, "X turns it portrait: {}", ratio(&s));
    // another named ratio after X stays portrait (it used to turn back to the image's landscape)
    aspect(&mut s, json!({"aspect": "4x5"}));
    assert!((ratio(&s) - 0.8).abs() < 0.01, "still portrait: {}", ratio(&s));
    aspect(&mut s, json!({"aspect": "16x9"}));
    assert!((ratio(&s) - 9.0 / 16.0).abs() < 0.01, "{}", ratio(&s));

    // a custom ratio is used as given
    aspect(&mut s, json!({"aspect": [2, 3]}));
    assert!((ratio(&s) - 2.0 / 3.0).abs() < 0.01, "custom 2 × 3 is portrait: {}", ratio(&s));
    aspect(&mut s, json!({"aspect": [3, 2]}));
    assert!((ratio(&s) - 1.5).abs() < 0.01, "{}", ratio(&s));

    // `orientation` overrides (the crop is landscape now)
    aspect(&mut s, json!({"aspect": [2, 3], "orientation": "crop"}));
    assert!((ratio(&s) - 1.5).abs() < 0.01, "follows the crop: {}", ratio(&s));
    aspect(&mut s, json!({"aspect": "2x3", "orientation": "given"}));
    assert!((ratio(&s) - 2.0 / 3.0).abs() < 0.01, "as given: {}", ratio(&s));

    // a bad one is refused before anything is locked
    let before = s.develop_of(s.selection.active.unwrap()).unwrap().crop;
    for aspect in [json!("current"), json!("toggle"), json!("2x3"), json!([2, 3]), json!("free")] {
        for orientation in [json!("sideways"), json!(1), json!(true), json!(null)] {
            let e = s.execute("crop.aspect", &json!({"aspect": aspect, "orientation": orientation})).unwrap_err();
            assert!(e.to_string().contains("orientation"), "{aspect} {orientation}: {e}");
            assert_eq!(s.develop_of(s.selection.active.unwrap()).unwrap().crop, before, "{aspect} {orientation}: untouched");
        }
    }
    // the valid words still lock as is
    aspect(&mut s, json!({"aspect": "current", "orientation": "crop"}));
    assert!(s.develop_of(s.selection.active.unwrap()).unwrap().crop.aspect.is_some());
}
