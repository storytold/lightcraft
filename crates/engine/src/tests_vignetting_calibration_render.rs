//! A photo's flat-field map renders from the catalog's table: the session hands the render the maps the photo's
//! settings reference (`Session::job_info`), and removing the reference photo changes nothing.

use std::sync::Arc;

use lcc_catalog::{Op, Photo, PhotoId, Source, VignettingMapRecord};
use lcc_develop::{EmbeddedVignettingMap, VignettingMapId, VignettingMapOrigin};
use lcc_pipeline::SourceInfo;
use lcc_raster::Rgb32f;
use lcc_raw::gaintable::SourcePlacement;

const W: usize = 240;
const H: usize = 160;

/// EV darker than the centre at sensor position `(x, y)`.
fn falloff(x: f64, y: f64) -> f64 {
    let (dx, dy) = ((x / W as f64 - 0.5) * 2.0, (y / H as f64 - 0.5) * 2.0 * H as f64 / W as f64);
    let d2 = (dx * dx + dy * dy) / (1.0 + (H as f64 / W as f64).powi(2));
    1.2 * d2 * d2 + 0.3 * d2
}

fn the_map() -> EmbeddedVignettingMap {
    let grid = [48usize, 32];
    let ev: Vec<f64> = (0..grid[0] * grid[1])
        .map(|i| falloff(((i % grid[0]) as f64 + 0.5) * W as f64 / grid[0] as f64, ((i / grid[0]) as f64 + 0.5) * H as f64 / grid[1] as f64))
        .collect();
    let source = VignettingMapOrigin { sha1: Some("a9993e364706816aba3e25717850c26c9cd0d89d".into()), ..Default::default() };
    EmbeddedVignettingMap::from_ev([W, H], grid, None, source, &ev).unwrap()
}

/// A session with one raw (a uniform grey scene seen through the fall-off) and its reference photo, the map in the
/// catalog, and the raw's settings referencing it at `amount`.
fn session(amount: f64) -> (crate::Session, PhotoId, PhotoId, VignettingMapId) {
    let mut s = crate::Session::new();
    s.media.file_loader = Some(Arc::new(move |_, _, _| {
        let mut img = Rgb32f::new(W, H);
        for (i, px) in img.data.iter_mut().enumerate() {
            let g = (-falloff((i % W) as f64 + 0.5, (i / W) as f64 + 0.5)).exp2() as f32;
            *px = [0.18 * g; 3];
        }
        let placement = SourcePlacement { active_size: Some([W, H]), ..SourcePlacement::new([0.0, 0.0, 1.0, 1.0], lcc_geom::Orientation::Normal) };
        Ok((img, SourceInfo { raw: true, placement: Some(placement), ..SourceInfo::default() }))
    }));
    let add = |s: &mut crate::Session, name: &str| {
        let id = s.catalog.alloc_photo_id();
        let p = Photo::new(id, Source::File { path: format!("flat-field-render-{name}.nef") }, name, "NEF", W as u32, H as u32, "2026-01-01");
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
        id
    };
    let reference = add(&mut s, "reference");
    let photo = add(&mut s, "photo");
    let map = the_map();
    let id = VignettingMapId::of(&map);
    let record = Box::new(VignettingMapRecord { map, photo: Some(reference), added: String::new(), withdrawn: false });
    s.catalog.apply(Op::AddVignettingMap { id: id.clone(), record }).unwrap();
    let mut d = (*s.catalog.photo(photo).unwrap().develop).clone();
    d.optics.vignetting_calibration = Some(id.clone());
    d.optics.vignetting_calibration_amount = amount;
    s.catalog.apply(Op::SetDevelop { id: photo, settings: Arc::new(d), label: "Flat Field".into(), edited: None }).unwrap();
    (s, photo, reference, id)
}

/// The rendered photo's corner over its centre, in 8-bit output levels.
fn corner_over_centre(s: &mut crate::Session, id: PhotoId) -> f64 {
    let r = s.render_job(id, W, H, false, true).unwrap().run();
    let img = r.rendered.unwrap().image;
    let at = |x: usize, y: usize| f64::from(img.data[y * img.width + x][1]);
    at(2, 2) / at(img.width / 2, img.height / 2)
}

#[test]
fn a_photo_renders_its_map_from_the_catalog() {
    let (mut s, photo, _, id) = session(100.0);
    assert!(s.job_info(s.catalog.photo(photo).unwrap()).vignetting_maps.is_some(), "the render gets the map");
    let corrected = corner_over_centre(&mut s, photo);
    let (mut off, photo_off, _, _) = session(0.0);
    let uncorrected = corner_over_centre(&mut off, photo_off);
    eprintln!("corner / centre: {corrected:.3} with the map, {uncorrected:.3} at amount 0");
    assert!((corrected - 1.0).abs() < 0.03, "{corrected}");
    assert!(uncorrected < 0.75, "{uncorrected}");
    // a photo whose settings reference a map the catalog doesn't have renders without it
    let mut d = (*s.catalog.photo(photo).unwrap().develop).clone();
    d.optics.vignetting_calibration = VignettingMapId::parse(&"0".repeat(64));
    s.catalog.apply(Op::SetDevelop { id: photo, settings: Arc::new(d), label: "x".into(), edited: None }).unwrap();
    assert!(s.job_info(s.catalog.photo(photo).unwrap()).vignetting_maps.is_none());
    assert!(corner_over_centre(&mut s, photo) < 0.75);
    assert!(s.catalog.vignetting_map(&id).is_some());
}

#[test]
fn removing_the_reference_photo_keeps_the_correction() {
    let (mut s, photo, reference, id) = session(100.0);
    let before = corner_over_centre(&mut s, photo);
    s.catalog.apply(Op::RemovePhoto { id: reference }).unwrap();
    assert!(s.catalog.vignetting_map(&id).is_some());
    let after = corner_over_centre(&mut s, photo);
    assert!((after - before).abs() < 1e-9, "{before} → {after}");
}
