//! Face skin on a real photo with the real MediaPipe models: the eyes and the mouth are holes in it,
//! the cheeks are skin. Needs `LIGHTCRAFT_MEDIAPIPE_MODELS` (a folder with
//! `selfie_multiclass_256x256.tflite` and `face_landmarker.task`) and `LIGHTCRAFT_FACESKIN_PHOTO`
//! (a JPEG with a frontal face, eyes and mouth open); skipped without them.

use lightcraft_segment::people::{FaceModel, PeopleModel, PeopleRequest, people_mask};

#[test]
fn face_skin_leaves_eyes_and_mouth_out_on_a_real_photo() {
    let (Some(dir), Some(photo)) = (std::env::var_os("LIGHTCRAFT_MEDIAPIPE_MODELS"), std::env::var_os("LIGHTCRAFT_FACESKIN_PHOTO")) else {
        lightcraft_testkit::skip!(Model, "set LIGHTCRAFT_MEDIAPIPE_MODELS and LIGHTCRAFT_FACESKIN_PHOTO");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let img = image::open(photo).unwrap().to_rgb8();
    let img = image::imageops::resize(
        &img,
        img.width() * 1024 / img.width().max(img.height()),
        img.height() * 1024 / img.width().max(img.height()),
        image::imageops::FilterType::Triangle,
    );
    let (w, h) = (img.width() as usize, img.height() as usize);
    let rgb = img.into_raw();
    let people = PeopleModel::load(&dir).unwrap();
    let faces = FaceModel::load(&dir).unwrap().faces(&rgb, w, h).unwrap();
    let map = people.segment(&rgb, w, h).unwrap();
    let with = faces.iter().filter(|f| f.landmarks.is_some()).count();
    eprintln!("{} faces, {with} with landmarks", faces.len());
    assert!(with >= 1, "a frontal face is found and landmarked");
    let req = PeopleRequest::parse(0, &["Face Skin".into()]).unwrap();
    let m = people_mask(&req, &rgb, w, h, Some(&map), &faces).unwrap();
    assert!(m.rect.is_none());
    let at = |p: [f32; 2]| {
        let (i, j) = ((p[0] / w as f32 * m.side as f32) as usize, (p[1] / h as f32 * m.side as f32) as usize);
        m.logits[j.min(m.side - 1) * m.side + i.min(m.side - 1)]
    };
    for f in faces.iter().filter_map(|f| f.landmarks.as_ref()) {
        let mouth = [(f[13][0] + f[14][0]) / 2.0, (f[13][1] + f[14][1]) / 2.0];
        let (eye_r, eye_l, cheek_r, cheek_l) = (f[468], f[473], f[205], f[425]);
        eprintln!("eyes {:.1} {:.1}, mouth {:.1}, cheeks {:.1} {:.1}", at(eye_r), at(eye_l), at(mouth), at(cheek_r), at(cheek_l));
        assert!(at(eye_r) < 0.0 && at(eye_l) < 0.0, "eyes are not skin");
        assert!(at(mouth) < 0.0, "the mouth is not skin");
        assert!(at(cheek_r).max(at(cheek_l)) > 0.0, "a cheek is skin");
    }
}
