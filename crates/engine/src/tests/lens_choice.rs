//! Choosing a photo's lens profile by hand (issue #1049): the choice lists the camera mount's lenses, gives the photo
//! that profile as a chosen correction, and keeps it through Reload from Disk, a database lookup and a render.

use std::sync::Arc;

use lightcraft_catalog::{MediaKind, Op, Photo, PhotoId, Source};
use lightcraft_develop::{EmbeddedLens, EmbeddedWarp, LensSource};
use lightcraft_geom::Orientation;

use crate::Session;
use crate::lens_profiles::{choices, chosen};

const DB: &str = r#"<lensdatabase>
  <camera><maker>Testcam</maker><model>TC-1</model><mount>T</mount><cropfactor>1</cropfactor></camera>
  <lens><maker>Testcam</maker><model>Testcam 50mm f/1.8</model><mount>T</mount><cropfactor>1</cropfactor>
    <calibration><distortion model="poly3" focal="50" k1="-0.01" /></calibration></lens>
  <lens><maker>Testcam</maker><model>Testcam 18-55mm f/3.5-5.6</model><mount>T</mount><cropfactor>1</cropfactor>
    <calibration><distortion model="poly3" focal="18" k1="-0.02" /><distortion model="poly3" focal="55" k1="0.01" /></calibration></lens>
  <lens><maker>Othercam</maker><model>Othercam 35mm f/2</model><mount>O</mount><cropfactor>1</cropfactor>
    <calibration><distortion model="poly3" focal="35" k1="-0.01" /></calibration></lens></lensdatabase>"#;

fn db() -> lightcraft_lensdb::LensDb {
    lightcraft_lensdb::LensDb::parse(DB).unwrap()
}

fn raw(id: u64, lens: &str) -> Photo {
    let mut p =
        Photo::new(PhotoId(id), Source::File { path: format!("/photos/{id}.cr3") }, &format!("{id}.cr3"), "CR3", 600, 400, "2026-10-01T00:00:00");
    p.kind = MediaKind::Raw;
    p.meta.make = "Testcam".into();
    p.meta.camera = "Testcam TC-1".into();
    p.meta.lens = lens.into();
    p.meta.focal_mm = Some(18.0);
    p
}

/// What the photo's file says when it is read: no lens correction of its own, its size and focal length.
fn file(focal: Option<f32>) -> crate::media::ProbeInfo {
    let mut meta = lightcraft_catalog::Meta { focal_mm: focal, ..Default::default() };
    meta.camera = "Testcam TC-1".into();
    crate::media::ProbeInfo { width: 600, height: 400, orientation: Some(Orientation::Normal), meta, ..Default::default() }
}

#[test]
fn the_choice_is_the_camera_mounts_lenses_by_name() {
    let db = db();
    assert_eq!(choices(&db, "Testcam", "Testcam TC-1"), ["Testcam 18-55mm f/3.5-5.6", "Testcam 50mm f/1.8"], "the mount's, sorted");
    assert_eq!(choices(&db, "Nobody", "X-1").len(), 3, "a camera the database doesn't know: every lens");
}

#[test]
fn a_chosen_profile_is_read_for_the_photos_focal_length() {
    let db = db();
    let cam = ("Testcam", "Testcam TC-1");
    let zoom = "Testcam 18-55mm f/3.5-5.6";
    let at = |f: f64| chosen(&db, zoom, cam, Some(f), None, (600.0, 400.0), Orientation::Normal).unwrap();
    assert_eq!(at(18.0).source, LensSource::Chosen);
    assert!(at(18.0).warp.is_some());
    assert_ne!(at(18.0), at(55.0), "the zoom's correction follows the focal length");
    // a prime gives its own focal length to a photo that records none; a zoom can't
    assert!(chosen(&db, "Testcam 50mm f/1.8", cam, None, None, (600.0, 400.0), Orientation::Normal).is_ok());
    assert!(chosen(&db, zoom, cam, None, None, (600.0, 400.0), Orientation::Normal).unwrap_err().contains("no focal length"));
    assert!(chosen(&db, "Testcam 85mm", cam, Some(85.0), None, (600.0, 400.0), Orientation::Normal).unwrap_err().contains("no lens profile named"));
}

/// The photo gets the chosen correction, its Lens field names it and the correction is switched on, for its virtual
/// copies too, as one undo step.
#[test]
fn choosing_gives_the_photo_and_its_copies_the_profile_in_one_undo_step() {
    let mut s = Session::new();
    let mut copy = raw(2, "");
    copy.copy_of = Some(PhotoId(1));
    for p in [raw(1, ""), copy] {
        s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    }
    let ops = crate::cmd::lens_choice::ops_for(&s, PhotoId(1), &db(), "Testcam 18-55mm f/3.5-5.6", &file(Some(18.0))).unwrap();
    s.commit("Choose Lens Profile", Op::Batch { ops }).unwrap();
    for id in [1, 2] {
        let p = s.catalog.photo(PhotoId(id)).unwrap();
        assert_eq!(p.embedded_lens.map(|l| l.source), Some(LensSource::Chosen), "photo {id}");
        assert_eq!(p.meta.lens, "Testcam 18-55mm f/3.5-5.6");
        assert!(p.develop.optics.lens_profile);
    }
    s.execute("edit.undo", &serde_json::json!({})).unwrap();
    let p = s.catalog.photo(PhotoId(1)).unwrap();
    assert!(p.embedded_lens.is_none() && p.meta.lens.is_empty() && !p.develop.optics.lens_profile, "undo takes it all back");
    // only raws
    let mut jpeg = raw(3, "");
    jpeg.kind = MediaKind::Image;
    s.catalog.apply(Op::AddPhoto { photo: Box::new(jpeg) }).unwrap();
    assert!(crate::cmd::lens_choice::ops_for(&s, PhotoId(3), &db(), "Testcam 50mm f/1.8", &file(None)).is_err());
}

fn chosen_lens() -> EmbeddedLens {
    EmbeddedLens {
        warp: Some(EmbeddedWarp { planes: [[1.0, -0.02, 0.0, 0.0, 0.0, 0.0]; 3], radius: 0.6, ..Default::default() }),
        source: LensSource::Chosen,
        ..Default::default()
    }
}

/// Reload from Disk and a database lookup read the file again; a chosen profile stays, whatever the file says.
#[test]
fn a_chosen_profile_is_kept_when_the_file_is_read_again() {
    let mut p = raw(1, "Testcam 18-55mm f/3.5-5.6");
    p.embedded_lens = Some(chosen_lens());
    let db_lens = EmbeddedLens { source: LensSource::Database, ..chosen_lens() };
    for from_file in [None, Some(db_lens)] {
        let info = crate::media::ProbeInfo { embedded_lens: from_file, ..file(Some(18.0)) };
        assert!(crate::cmd::convert::lens_ops(PhotoId(1), &p, &info).is_empty(), "Reload keeps it ({from_file:?})");
    }
    let mut s = Session::new();
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let again = crate::cmd::lens_refresh::missing(&s.catalog, &db(), None, true);
    assert!(again.is_empty(), "a lookup of every database profile again leaves it out");
}

/// A render uses the chosen profile, over the file's own correction too, and a change of it changes the render keys.
#[test]
fn a_render_uses_the_chosen_profile() {
    use crate::media::DecodedSource;
    let img = Arc::new(lightcraft_raster::Rgb32f::new(4, 4));
    let file_lens = EmbeddedLens { source: LensSource::File, ..chosen_lens() };
    let decoded =
        DecodedSource::new(img, Some(lightcraft_pipeline::SourceInfo { lens: Some(EmbeddedLens { warp: None, ..file_lens }), ..Default::default() }));
    let header = |l: Option<EmbeddedLens>| lightcraft_pipeline::SourceInfo { lens: l, ..Default::default() };
    assert_eq!(decoded.info_or(header(Some(chosen_lens()))).lens, Some(chosen_lens()), "chosen over the file's own");
    // and a chosen profile is in the render keys: choosing one (or another) is never shown from an old render
    let mut s = Session::new();
    let mut p = raw(1, "");
    p.develop = Arc::new(lightcraft_develop::DevelopSettings {
        optics: lightcraft_develop::Optics { lens_profile: true, ..Default::default() },
        ..(*p.develop).clone()
    });
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    let id = PhotoId(1);
    let key = |s: &mut Session| s.render_job(id, 800, 600, false, true).unwrap().key;
    let none = key(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(chosen_lens())) }).unwrap();
    let one = key(&mut s);
    s.catalog.apply(Op::SetEmbeddedLens { id, lens: Some(Box::new(EmbeddedLens { warp: None, ..chosen_lens() })) }).unwrap();
    let other = key(&mut s);
    assert!(none != one && one != other, "{none:?} {one:?} {other:?}");
}
