//! The Camera colour setting through the command that sets it without the Edit panel (`develop.merge`, as a script,
//! MCP or a preset does): the photo's next decode starts from the colour it asks for, and one undo step takes it back.

use std::sync::{Arc, Mutex};

use lightcraft_catalog::{Op, Photo, PhotoId, Source};
use lightcraft_develop::CameraColour;
use lightcraft_pipeline::SourceInfo;
use lightcraft_raster::Rgb32f;
use serde_json::json;

use crate::Session;
use crate::media::{FileLoader, LoadOptions};

#[test]
fn develop_merge_sets_the_camera_colour_and_the_decode_follows() {
    let mut s = Session::new();
    let p = Photo::new(PhotoId(1), Source::File { path: "/photos/DSC_0001.NEF".into() }, "DSC_0001.NEF", "NEF", 6048, 4032, "2026-11-02");
    s.catalog.apply(Op::AddPhoto { photo: Box::new(p) }).unwrap();
    s.execute("library.select", &json!({"ids": [1]})).unwrap();
    let seen = Arc::new(Mutex::new(Vec::<LoadOptions>::new()));
    let log = seen.clone();
    let loader: FileLoader = Arc::new(move |_, _, options| {
        log.lock().unwrap().push(options);
        Ok((Rgb32f::new(8, 8), SourceInfo { raw: true, colorimetric: true, ..SourceInfo::default() }))
    });
    s.media.file_loader = Some(loader);
    let render = |s: &mut Session| {
        let r = s.render_job(PhotoId(1), 64, 64, false, true).unwrap().run();
        s.accept(&r);
    };
    let colour = |s: &Session| s.catalog.photo(PhotoId(1)).unwrap().develop.profile.camera_colour;
    let last = |seen: &Arc<Mutex<Vec<LoadOptions>>>| seen.lock().unwrap().last().map(|o| o.colour);

    render(&mut s);
    assert_eq!(last(&seen), Some(CameraColour::Auto));
    s.execute("develop.merge", &json!({"settings": {"profile": {"camera_colour": "colorimetric"}}, "label": "Calibrated Camera Colour"})).unwrap();
    assert_eq!(colour(&s), CameraColour::Colorimetric);
    render(&mut s);
    assert_eq!(last(&seen), Some(CameraColour::Colorimetric), "decoded again with it");
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(colour(&s), CameraColour::Auto, "one undo step");
    render(&mut s);
    assert_eq!(last(&seen), Some(CameraColour::Auto), "and back to the camera-JPEG fit");
    assert_eq!(seen.lock().unwrap().len(), 3);
}
