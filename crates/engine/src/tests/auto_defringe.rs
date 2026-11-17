//! `develop.autoDefringe`: Auto Defringe sets the Defringe sliders from the purple and green fringes it measures on
//! the active photo, as one undo step, and leaves a photo without fringes as it was.

use lightcraft_catalog::PhotoId;
use serde_json::json;

use crate::Session;

/// Give photo `id` a decoded source (thumbnail and preview): a bright square on a dark ground, with a purple rim
/// on the dark side of its edges when `fringed`.
fn scene(s: &mut Session, id: PhotoId, fringed: bool) {
    use crate::media::SourceLevel;
    let (n, a, b, w) = (1800usize, 600usize, 1200usize, 2usize);
    let img = lightcraft_raster::Rgb32f::from_fn(n, n, |x, y| {
        let inside = (a..b).contains(&x) && (a..b).contains(&y);
        let near = (a - w..b + w).contains(&x) && (a - w..b + w).contains(&y);
        if inside {
            [0.9; 3]
        } else if near && fringed {
            [0.12, 0.02, 0.22]
        } else {
            [0.02; 3]
        }
    });
    let img = std::sync::Arc::new(img);
    let info = lightcraft_pipeline::SourceInfo::default();
    s.media.insert_with_info(id, SourceLevel::Thumb, img.clone(), info.clone());
    s.media.insert_with_info(id, SourceLevel::Preview, img, info);
}

fn active_demo(fringed: bool) -> (Session, PhotoId) {
    let mut s = Session::with_demo();
    let id = s.visible_cloned()[0];
    scene(&mut s, id, fringed);
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    (s, id)
}

#[test]
fn auto_defringe_sets_purple_from_the_photo_in_one_undo_step() {
    let (mut s, id) = active_demo(true);
    let before = s.develop_of(id).unwrap().optics;
    let r = s.execute("develop.autoDefringe", &json!({})).unwrap();
    assert_eq!(r["found"], true, "{r}");
    let o = s.develop_of(id).unwrap().optics;
    assert!(o.defringe_purple_amount > 0.0 && o.defringe_green_amount == 0.0, "{r}");
    assert!(o.remove_ca && o.defringe_protect, "Remove CA and Protect Colours on: {o:?}");
    assert_eq!(
        (o.defringe_purple_amount, o.defringe_purple_hue_lo, o.defringe_purple_hue_hi),
        (r["purpleAmount"].as_f64().unwrap(), r["purpleHueLo"].as_f64().unwrap(), r["purpleHueHi"].as_f64().unwrap())
    );
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().optics, before, "one undo step puts Defringe back");
}

/// Without fringes, Auto Defringe only turns Remove Chromatic Aberration on; a second time it changes nothing.
#[test]
fn auto_defringe_without_fringes_only_removes_chromatic_aberration() {
    let (mut s, id) = active_demo(false);
    let before = s.develop_of(id).unwrap().optics;
    let r = s.execute("develop.autoDefringe", &json!({})).unwrap();
    assert_eq!(r, json!({"found": false, "removeCa": true}));
    let o = s.develop_of(id).unwrap().optics;
    assert!(o.remove_ca && !o.defringe_protect, "{o:?}");
    assert_eq!((o.defringe_purple_amount, o.defringe_green_amount), (before.defringe_purple_amount, before.defringe_green_amount));
    let again = s.develop_of(id).unwrap();
    assert_eq!(s.execute("develop.autoDefringe", &json!({})).unwrap(), json!({"found": false, "removeCa": false}));
    assert_eq!(s.develop_of(id).unwrap(), again, "nothing changed");
}
