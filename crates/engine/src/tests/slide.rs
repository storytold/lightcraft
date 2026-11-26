//! Slideshow slides: `slideshow.layout` (validated, saved with the library) and
//! `slideshow.preview` (a slide composed with the layout, as a PNG).

use std::path::PathBuf;

use serde_json::json;

use crate::Session;

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lc-slide-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "a PNG");
    let be = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    (be(16), be(20))
}

fn base64_decode(text: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in text.bytes().take_while(|c| *c != b'=') {
        acc = (acc << 6) | A.iter().position(|a| *a == c).unwrap() as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    out
}

#[test]
fn slide_layout_is_validated_and_persists_with_the_library() {
    let lib = temp_dir("layout");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("slideshow.layout", &json!({})).unwrap();
        assert_eq!(r["marginLeft"], 0.0, "defaults: {r}");
        assert_eq!(r["backdropColor"], "#000000");
        let r = s
            .execute(
                "slideshow.layout",
                &json!({"marginTop": 0.1, "strokeBorder": true, "strokeColor": [255, 255, 255], "castShadow": true, "backdropColor": "#203040", "aspect": "4:3"}),
            )
            .unwrap();
        assert_eq!((r["marginLeft"].as_f64(), r["marginBottom"].as_f64()), (Some(0.1), Some(0.1)), "linked margins: {r}");
        assert_eq!(r["strokeColor"], "#ffffff");
        // a bad value changes nothing (the good keys beside it included)
        for bad in [json!({"marginLeft": "wide", "zoomToFill": true}), json!({"backdropColor": "red"}), json!({"glitter": 1})] {
            assert!(s.execute("slideshow.layout", &bad).is_err(), "{bad}");
        }
        assert!(!s.slide_layout.zoom_to_fill);
        // out of range: clamped
        let r = s.execute("slideshow.layout", &json!({"linkMargins": false, "marginRight": 9, "shadowOpacity": -1})).unwrap();
        assert_eq!((r["marginRight"].as_f64(), r["marginLeft"].as_f64(), r["shadowOpacity"].as_f64()), (Some(0.45), Some(0.1), Some(0.0)));
        drop(s);
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("slideshow.layout", &json!({})).unwrap();
    assert_eq!((r["marginRight"].as_f64(), r["aspect"].as_str(), r["backdropColor"].as_str()), (Some(0.45), Some("4:3"), Some("#203040")));
    assert_eq!(r["linkMargins"], false);
    drop(s);
    // hand-edited values out of range are clamped when the library opens
    std::fs::write(lib.join("prefs.json"), r#"{"slideLayout": {"marginTop": 1e9, "shadowAngle": 720, "strokeWidth": -3}}"#).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!((s.slide_layout.margin_top, s.slide_layout.shadow_angle, s.slide_layout.stroke_width), (0.45, 0.0, 0.0));
    // and the playback settings beside them are untouched by a layout change
    let before = s.slideshow.clone();
    s.execute("slideshow.layout", &json!({"zoomToFill": true})).unwrap();
    assert_eq!(s.slideshow, before);
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn slide_preview_returns_the_composed_slide_at_the_asked_size() {
    let mut s = Session::with_demo();
    let photos = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [photos[0].0]})).unwrap();
    s.execute("slideshow.layout", &json!({"margins": 0.1, "backdropColor": "#ff0000"})).unwrap();
    let r = s.execute("slideshow.preview", &json!({"width": 320, "height": 200})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64(), r["id"].as_u64()), (Some(320), Some(200), Some(photos[0].0)));
    assert_eq!(r["cell"], json!([20.0, 20.0, 300.0, 180.0]), "10 % of the 200 px short side");
    let png = base64_decode(r["png"].as_str().unwrap());
    assert_eq!(png_size(&png), (320, 200));
    // written to a file instead
    let dir = temp_dir("preview");
    let path = dir.join("slide.png");
    let r = s.execute("slideshow.preview", &json!({"id": photos[1].0, "width": 240, "path": path.to_string_lossy()})).unwrap();
    assert_eq!(r["path"], path.to_string_lossy().as_ref());
    assert!(r.get("png").is_none());
    assert_eq!(png_size(&std::fs::read(&path).unwrap()), (240, 150), "the height from the aspect preview (screen: 16:10)");
    s.execute("slideshow.layout", &json!({"aspect": "16:9"})).unwrap();
    let r = s.execute("slideshow.preview", &json!({"height": 90})).unwrap();
    assert_eq!((r["width"].as_u64(), r["height"].as_u64()), (Some(160), Some(90)));
    // the pixels: red margins, the photo inside
    let img = s.slide_now(&s.slide_layout.clone(), photos[0], 320, 200).unwrap();
    assert_eq!((img.width, img.height), (320, 200));
    assert_eq!(img.get(3, 3), [255, 0, 0, 255]);
    assert_eq!(img.get(160, 196), [255, 0, 0, 255]);
    assert_ne!(img.get(160, 100), [255, 0, 0, 255], "the photo in the cell");
    // refused: bad sizes, a photo that isn't there
    for bad in [json!({"width": 4}), json!({"height": 100000}), json!({"width": "big"}), json!({"id": 999_999}), json!({"id": "one"})] {
        assert!(s.execute("slideshow.preview", &bad).is_err(), "{bad}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_background_image_and_the_slide_key() {
    let mut s = Session::with_demo();
    let photos = s.visible_cloned();
    let plain = s.slide_layout.clone();
    let job = s.slide_job(photos[0], 320, 200, None).unwrap();
    let loupe = s.loupe_job(photos[0], 320, 200, true).unwrap();
    assert_ne!(job.key, loupe.key, "a slide is not the photo's render");
    // a background image fills the slide around the photo
    let bg =
        s.slide_layout.merged(&json!({"margins": 0.2, "backdropColor": "#ff0000", "backgroundImage": photos[2].0, "backgroundOpacity": 1})).unwrap();
    let img = s.slide_now(&bg, photos[0], 320, 200).unwrap();
    assert_ne!(img.get(3, 3), [255, 0, 0, 255], "the background photo, not the colour");
    // it is rendered once and reused
    assert!(s.slide_backdrop.lock().unwrap().is_some());
    let other = s.slide_job_with(&bg, photos[1], 320, 200, None, false).unwrap();
    assert_ne!(other.key, s.slide_job_with(&plain, photos[1], 320, 200, None, false).unwrap().key);
    // a background photo that isn't in the catalog: the colour alone
    let gone = bg.merged(&json!({"backgroundImage": 987_654})).unwrap();
    let img = s.slide_now(&gone, photos[0], 320, 200).unwrap();
    assert_eq!(img.get(3, 3), [255, 0, 0, 255]);
}

#[test]
fn slide_overlays_are_validated_and_persist_with_the_library() {
    let lib = temp_dir("overlays");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("slideshow.overlays", &json!({})).unwrap();
        assert_eq!(r["identityPlate"]["enabled"], false, "defaults: {r}");
        assert_eq!(r["texts"], json!([]));
        assert!(s.slides_plain());
        let r = s
            .execute(
                "slideshow.overlays",
                &json!({"identityPlate": {"enabled": true, "text": "Studio Nord"}, "ratingStars": {"enabled": true}, "addText": {"text": "{caption} {bogus}"}, "showWatermark": true, "watermark": "© Ann"}),
            )
            .unwrap();
        assert_eq!(r["texts"].as_array().map(Vec::len), Some(1));
        assert_eq!(r["watermark"]["text"], "© Ann");
        assert_eq!(r["warnings"].as_array().map(Vec::len), Some(1), "the unknown token is reported: {r}");
        assert!(!s.slides_plain(), "overlays make playback compose its slides");
        // a bad value changes nothing (the good keys beside it included)
        for bad in
            [json!({"identityPlate": {"size": "big"}, "showWatermark": false}), json!({"texts": [{"anchor": "nowhere"}]}), json!({"removeText": 5})]
        {
            assert!(s.execute("slideshow.overlays", &bad).is_err(), "{bad}");
        }
        assert!(s.slide_overlays.show_watermark);
        drop(s);
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    let r = s.execute("slideshow.overlays", &json!({})).unwrap();
    assert_eq!((r["identityPlate"]["text"].as_str(), r["texts"][0]["text"].as_str()), (Some("Studio Nord"), Some("{caption} {bogus}")));
    // the layout beside it is untouched by an overlay change
    let before = s.slide_layout.clone();
    s.execute("slideshow.overlays", &json!({"removeText": 0})).unwrap();
    assert_eq!(s.slide_layout, before);
    drop(s);
    // hand-edited values out of range are clamped when the library opens
    std::fs::write(lib.join("prefs.json"), r#"{"slideOverlays": {"identityPlate": {"size": 1e9, "opacity": -2}, "texts": [{"size": 1e300}]}}"#)
        .unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!((s.slide_overlays.identity_plate.size, s.slide_overlays.identity_plate.opacity), (crate::slide_overlay::PLATE_SIZE_MAX, 0.0));
    assert_eq!(s.slide_overlays.texts[0].size, crate::slide_overlay::TEXT_SIZE_MAX);
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn slide_preview_draws_the_overlays() {
    let mut s = Session::with_demo();
    let photos = s.visible_cloned();
    s.execute("library.select", &json!({"ids": [photos[0].0]})).unwrap();
    s.execute("photo.rate", &json!({"rating": 4, "ids": [photos[0].0]})).unwrap();
    s.execute("slideshow.layout", &json!({"margins": 0.2, "backdropColor": "#000000"})).unwrap();
    let before = s.slide_job(photos[0], 320, 200, None).unwrap().key;
    let plain = s.slide_now(&s.slide_layout.clone(), photos[0], 640, 400).unwrap();
    s.execute(
        "slideshow.overlays",
        &json!({
            "identityPlate": {"enabled": true, "text": "LC", "size": 0.1, "color": "#00ff00", "opacity": 1, "anchor": "topLeft", "offsetX": 0.01, "offsetY": 0.01},
            "ratingStars": {"enabled": true, "color": "#ffff00", "size": 0.05, "anchor": "bottomRight", "offsetX": -0.01, "offsetY": -0.01, "relativeTo": "slide"},
            "addText": {"text": "{file}", "size": 0.05, "color": "#ffffff", "opacity": 1, "anchor": "bottom", "offsetX": 0, "offsetY": -0.01},
        }),
    )
    .unwrap();
    assert_ne!(s.slide_job(photos[0], 320, 200, None).unwrap().key, before, "overlays change the slide's render key");
    let r = s.execute("slideshow.preview", &json!({"width": 640, "height": 400})).unwrap();
    let kinds: Vec<&str> = r["overlays"].as_array().unwrap().iter().filter_map(|o| o["kind"].as_str()).collect();
    assert_eq!(kinds, ["identityPlate", "ratingStars", "text0"]);
    let img = s.slide_now(&s.slide_layout.clone(), photos[0], 640, 400).unwrap();
    let count = |img: &lightcraft_raster::Rgba8, b: &serde_json::Value, want: fn([u8; 4]) -> bool| {
        let v: Vec<usize> = (0..4).map(|i| b[i].as_f64().unwrap() as usize).collect();
        (v[1]..v[3]).flat_map(|y| (v[0]..v[2]).map(move |x| (x, y))).filter(|&(x, y)| want(img.get(x, y))).count()
    };
    for (i, (kind, want)) in [
        ("identityPlate", (|p: [u8; 4]| p[1] > 200 && p[0] < 60) as fn([u8; 4]) -> bool),
        ("ratingStars", |p| p[0] > 200 && p[1] > 200 && p[2] < 60),
        ("text0", |p| p[0] > 200 && p[1] > 200 && p[2] > 200),
    ]
    .into_iter()
    .enumerate()
    {
        let b = &r["overlays"][i]["box"];
        assert!(count(&img, b, want) > 10, "{kind} is drawn in its box {b}");
        assert_eq!(count(&plain, b, want), 0, "{kind}: not there without overlays");
    }
}

#[test]
fn slide_titles_are_validated_persist_and_preview() {
    let lib = temp_dir("titles");
    {
        let mut s = Session::new().with_fs();
        s.open_library(&lib, false).unwrap();
        let r = s.execute("slideshow.titles", &json!({})).unwrap();
        assert_eq!((r["intro"]["enabled"].as_bool(), r["ending"]["enabled"].as_bool()), (Some(false), Some(false)), "off by default: {r}");
        assert_eq!(r["intro"]["content"], "identityPlate");
        assert_eq!(r["intro"]["duration"], serde_json::Value::Null, "the slide duration");
        let r = s
            .execute(
                "slideshow.titles",
                &json!({"intro": {"enabled": true, "color": "#102030", "content": "text", "text": "Iceland 2026", "scale": 0.12, "duration": 3}, "ending": {"enabled": true, "color": [255, 255, 255]}}),
            )
            .unwrap();
        assert_eq!((r["intro"]["color"].as_str(), r["intro"]["duration"].as_f64()), (Some("#102030"), Some(3.0)));
        assert_eq!(r["ending"]["color"], "#ffffff");
        // a bad value changes nothing, the good keys beside it included
        for bad in [
            json!({"intro": {"enabled": false, "scale": "huge"}}),
            json!({"ending": {"content": "credits"}}),
            json!({"intro": {"duration": -1, "color": "blue"}}),
            json!({"middle": {}}),
        ] {
            assert!(s.execute("slideshow.titles", &bad).is_err(), "{bad}");
        }
        assert!(s.slide_titles.intro.enabled);
        // out of range: clamped
        let r = s.execute("slideshow.titles", &json!({"ending": {"scale": 4, "duration": 1e6}})).unwrap();
        assert_eq!((r["ending"]["scale"].as_f64(), r["ending"]["duration"].as_f64()), (Some(0.5), Some(crate::slideshow::SLIDE_MAX)));
        // a plate image that can't be read is reported
        s.execute("slideshow.overlays", &json!({"identityPlate": {"image": "/no/such/plate.png"}})).unwrap();
        let r = s.execute("slideshow.titles", &json!({})).unwrap();
        let w = r["warnings"].as_array().unwrap();
        assert!(w.iter().any(|w| w.as_str().unwrap().contains("ending screen")), "{r}");
        drop(s);
    }
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert_eq!(s.slide_titles.intro.text, "Iceland 2026", "saved with the library");
    assert_eq!(s.slide_titles.durations(4.0), (Some(3.0), Some(crate::slideshow::SLIDE_MAX)));
    drop(s);
    // hand-edited values out of range are clamped when the library opens
    std::fs::write(lib.join("prefs.json"), r#"{"slideTitles": {"intro": {"enabled": true, "scale": -4, "duration": 1e99}}}"#).unwrap();
    let mut s = Session::new().with_fs();
    s.open_library(&lib, false).unwrap();
    assert!(s.slide_titles.intro.enabled);
    assert_eq!((s.slide_titles.intro.scale, s.slide_titles.intro.duration), (crate::slide_overlay::SIZE_MIN, Some(crate::slideshow::SLIDE_MAX)));
    drop(s);
    let _ = std::fs::remove_dir_all(&lib);
}

#[test]
fn slide_preview_renders_a_title_screen() {
    let mut s = Session::with_demo();
    s.execute(
        "slideshow.titles",
        &json!({"ending": {"color": "#00ff00", "content": "text", "text": "Fin", "overrideColor": true, "textColor": "#ff0000", "scale": 0.2}}),
    )
    .unwrap();
    let r = s.execute("slideshow.preview", &json!({"title": "ending", "width": 400, "height": 250})).unwrap();
    assert_eq!(
        (r["title"].as_str(), r["enabled"].as_bool(), r["width"].as_u64(), r["height"].as_u64()),
        (Some("ending"), Some(false), Some(400), Some(250))
    );
    let png = base64_decode(r["png"].as_str().unwrap());
    assert_eq!(png_size(&png), (400, 250));
    let b: Vec<f64> = r["plate"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!((b[3] - b[1] - 50.0).abs() < 1.5, "20 % of the 250 px short side: {b:?}");
    assert!(((b[0] + b[2]) / 2.0 - 200.0).abs() < 1.0 && ((b[1] + b[3]) / 2.0 - 125.0).abs() < 1.0, "centred: {b:?}");
    // the same pixels as playback composes
    let img = s.title_compose(crate::slideshow::Title::Ending, 400, 250).compose();
    assert_eq!(img.get(3, 3), [0, 255, 0, 255]);
    assert!(img.data.iter().any(|p| p[0] > 200 && p[1] < 60), "the red text");
    // no photo needed; refused: an unknown title, an id with it, bad sizes
    s.execute("library.select", &json!({"ids": []})).unwrap();
    assert!(s.execute("slideshow.preview", &json!({"title": "intro"})).is_ok());
    for bad in [json!({"title": "credits"}), json!({"title": 1}), json!({"title": "intro", "id": 1}), json!({"title": "intro", "width": 3})] {
        assert!(s.execute("slideshow.preview", &bad).is_err(), "{bad}");
    }
}
