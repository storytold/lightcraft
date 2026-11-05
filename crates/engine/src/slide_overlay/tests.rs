use super::*;

fn layout(w: usize, h: usize) -> Layout {
    lightcraft_slideshow::layout(&lightcraft_slideshow::SlideSettings::default().merged(&json!({"margins": 0.1})).unwrap(), 1.5, w, h)
}

fn photo() -> Photo {
    let mut p = crate::rename::sample_photo();
    p.rating = 3;
    p.meta.caption = "Morning fog".into();
    p.meta.shutter = "1/250".into();
    p.meta.aperture = Some(8.0);
    p.meta.focal_mm = Some(50.0);
    p
}

#[test]
fn tokens_expand_like_the_rename_tokens_plus_slide_ones() {
    let p = photo();
    assert_eq!(expand("{caption}", &p), "Morning fog");
    assert_eq!(expand("{exposure}", &p), "1/250 s at f/8");
    assert_eq!(expand("{focal} · ISO {iso}", &p), format!("50 mm · ISO {}", p.meta.iso.unwrap_or_default()));
    assert_eq!(expand("{file}", &p), p.file_name);
    assert_eq!(expand("{name}", &p), crate::rename::expand_tokens("{name}", &p, 1, 1), "rename tokens work");
    assert_eq!(expand("{date}", &p), crate::rename::expand_tokens("{date:%Y-%m-%d}", &p, 1, 1));
    assert_eq!(expand("{rating}", &p), "3");
    // unknown and slide-less tags show as typed, and are reported
    assert_eq!(expand("{sparkle} {seq} {", &p), "{sparkle} {seq} {");
    assert_eq!(unknown_tokens("{title} {sparkle} {seq} {é}"), vec!["{sparkle}", "{seq}", "{é}"]);
    // a photo without metadata: empty values, no panic
    let mut bare = p.clone();
    bare.meta = Default::default();
    assert_eq!(expand("[{exposure}|{camera}|{caption}|{focal}]", &bare), "[|||]");
}

#[test]
fn nine_anchors_place_inside_the_slide_with_their_offsets() {
    let l = layout(1000, 600);
    let short = 600.0;
    for anchor in ANCHORS {
        for relative_to in [RelativeTo::Slide, RelativeTo::Photo] {
            let at = |ox: f64, oy: f64| Placement { anchor, offset_x: ox, offset_y: oy, relative_to }.place(&l, 100.0, 40.0);
            let b = at(0.0, 0.0);
            assert!(b.x0 >= 0.0 && b.y0 >= 0.0 && b.x1 <= 1000.0 && b.y1 <= 600.0, "{anchor:?} {relative_to:?}: {b:?}");
            assert_eq!((b.width(), b.height()), (100.0, 40.0));
            let c = if relative_to == RelativeTo::Slide { l.slide } else { l.visible };
            let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
            let id = anchor_id(anchor);
            let want_x = if id.ends_with("eft") {
                c.x0 + 50.0
            } else if id.ends_with("ight") {
                c.x1 - 50.0
            } else {
                (c.x0 + c.x1) / 2.0
            };
            let want_y = if id.starts_with("top") {
                c.y0 + 20.0
            } else if id.starts_with("bottom") {
                c.y1 - 20.0
            } else {
                (c.y0 + c.y1) / 2.0
            };
            assert!((cx - want_x).abs() < 1e-9 && (cy - want_y).abs() < 1e-9, "{id} {relative_to:?}: {b:?}");
            // an inward offset moves it by that share of the shorter side
            let (sx, sy) = (if id.ends_with("ight") { -0.02 } else { 0.02 }, if id.starts_with("bottom") { -0.03 } else { 0.03 });
            let m = at(sx, sy);
            assert!((m.x0 - b.x0 - sx * short).abs() < 1e-9 && (m.y0 - b.y0 - sy * short).abs() < 1e-9, "{id}: {m:?}");
        }
    }
    // pushed off the slide: kept on it
    let b = Placement { anchor: Anchor::BottomRight, offset_x: 0.5, offset_y: 0.5, relative_to: RelativeTo::Slide }.place(&l, 100.0, 40.0);
    assert_eq!((b.x1, b.y1), (1000.0, 600.0));
}

fn blank(w: usize, h: usize) -> Rgba8 {
    Image::filled(w, h, [0, 0, 0, 255])
}

fn lit(img: &Rgba8, r: &Rect, min: u8) -> usize {
    let (x0, y0, x1, y1) = pixels(r, img.width, img.height);
    (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| img.get(x, y)[1] >= min).count()
}

#[test]
fn rating_stars_count_matches_the_rating() {
    let l = layout(800, 500);
    let s = OverlaySettings::default().merged(&json!({"ratingStars": {"enabled": true, "color": "#00ff00", "size": 0.05}})).unwrap();
    let mut p = photo();
    let mut widths = Vec::new();
    for rating in 0..=5u8 {
        p.rating = rating;
        let o = SlideOverlays::resolve(&s, Some(&p));
        let boxes = o.boxes(&l);
        let mut img = blank(800, 500);
        o.draw_front(&mut img, &l);
        let Some((_, b)) = boxes.iter().find(|(k, _)| *k == Kind::RatingStars) else {
            assert_eq!(rating, 0, "no stars only for an unrated photo");
            assert_eq!(lit(&img, &l.slide, 1), 0);
            continue;
        };
        widths.push(b.width());
        // count the stars: separate runs of lit columns along the box's middle row
        let y = ((b.y0 + b.y1) / 2.0) as usize;
        let mut runs = 0;
        let mut was = false;
        for x in 0..800 {
            let on = img.get(x, y)[1] > 128;
            runs += usize::from(on && !was);
            was = on;
        }
        assert_eq!(runs, usize::from(rating), "{rating} stars");
    }
    assert!(widths.windows(2).all(|w| w[1] > w[0]));
}

#[test]
fn text_with_a_shadow_draws_the_shadow_at_its_angle() {
    let l = layout(800, 500);
    let s = OverlaySettings::default()
        .merged(&json!({"texts": [{"text": "Shadow", "size": 0.08, "color": "#ffffff", "opacity": 1, "anchor": "center", "offsetX": 0, "offsetY": 0,
            "shadow": true, "shadowOpacity": 1, "shadowOffset": 0.02, "shadowRadius": 0, "shadowAngle": -90}]}))
        .unwrap();
    let o = SlideOverlays::resolve(&s, Some(&photo()));
    let b = o.boxes(&l)[0].1;
    let mut grey = Image::filled(800, 500, [128, 128, 128, 255]);
    o.draw_front(&mut grey, &l);
    // straight down by 10 px: dark pixels below the text box's glyphs, none above
    let dark = |r: Rect| {
        let (x0, y0, x1, y1) = pixels(&r, 800, 500);
        (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| grey.get(x, y)[0] < 40).count()
    };
    assert!(dark(Rect { y0: b.y1 - 2.0, y1: b.y1 + 10.0, ..b }) > 20, "the shadow falls below");
    assert_eq!(dark(Rect { y0: b.y0 - 12.0, y1: b.y0, ..b }), 0, "none above");
    // without the shadow: no dark pixels at all
    let off = s.merged(&json!({"texts": [{"shadow": false}]})).unwrap();
    let mut grey2 = Image::filled(800, 500, [128, 128, 128, 255]);
    SlideOverlays::resolve(&off, Some(&photo())).draw_front(&mut grey2, &l);
    assert!(grey2.data.iter().all(|p| p[0] >= 128));
}

#[test]
fn identity_plate_image_draws_at_its_opacity_and_a_missing_one_is_reported() {
    let dir = std::env::temp_dir().join(format!("lc-plate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("plate.png");
    let red = Image::filled(40, 20, [255u8, 0, 0, 255]);
    let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&red), &lightcraft_codecs::EncodeMeta::default()).unwrap();
    std::fs::write(&path, png).unwrap();
    let l = layout(800, 500);
    let s = OverlaySettings::default()
        .merged(&json!({"identityPlate": {"enabled": true, "image": path.to_string_lossy(), "size": 0.1, "opacity": 0.5, "anchor": "topLeft", "offsetX": 0, "offsetY": 0}}))
        .unwrap();
    assert!(s.warnings().is_empty());
    let o = SlideOverlays::resolve(&s, None);
    let b = o.boxes(&l)[0].1;
    assert_eq!((b.x0, b.y0, b.width(), b.height()), (0.0, 0.0, 100.0, 50.0), "50 px high (10 % of 500), its aspect kept");
    let mut img = blank(800, 500);
    o.draw_front(&mut img, &l);
    let p = img.get(50, 25);
    assert!((p[0] as i32 - 128).abs() <= 2 && p[1] == 0, "half-opaque red over black: {p:?}");
    assert_eq!(img.get(150, 25)[0], 0, "only inside its box");
    // behind the image: the photo covers it
    let behind = s.merged(&json!({"identityPlate": {"behind": true, "anchor": "center"}})).unwrap();
    let photo_img = Image::filled(30, 20, [0u8, 0, 255, 255]);
    let ob = SlideOverlays::resolve(&behind, None);
    let (img, l2) = lightcraft_slideshow::compose_with(&photo_img, None, &lightcraft_slideshow::SlideSettings::default(), 800, 500, &mut |i, l| {
        ob.draw_behind(i, l)
    });
    let _ = l2;
    assert_eq!(img.get(400, 250), [0, 0, 255, 255], "the photo is on top");
    // a missing file: reported, nothing drawn, no panic
    let gone = s.merged(&json!({"identityPlate": {"image": dir.join("nope.png").to_string_lossy()}})).unwrap();
    assert_eq!(gone.warnings().len(), 1);
    let mut img = blank(800, 500);
    SlideOverlays::resolve(&gone, None).draw_front(&mut img, &l);
    assert!(img.data.iter().all(|p| p[0] == 0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn overlays_are_resolution_independent() {
    let s = OverlaySettings::default()
        .merged(&json!({
            "identityPlate": {"enabled": true, "text": "Studio", "size": 0.06},
            "ratingStars": {"enabled": true, "size": 0.04},
            "texts": [{"text": "{caption}", "size": 0.04, "anchor": "bottomLeft", "offsetX": 0.02, "offsetY": -0.02, "relativeTo": "photo"}],
        }))
        .unwrap();
    let o = SlideOverlays::resolve(&s, Some(&photo()));
    let (a, b) = (o.boxes(&layout(800, 500)), o.boxes(&layout(1600, 1000)));
    assert_eq!(a.len(), 3);
    for ((ka, ra), (kb, rb)) in a.iter().zip(&b) {
        assert_eq!(ka, kb);
        for (va, vb) in [(ra.x0, rb.x0), (ra.y0, rb.y0), (ra.x1, rb.x1), (ra.y1, rb.y1)] {
            assert!((va * 2.0 - vb).abs() <= 4.0, "{ka:?}: {ra:?} vs {rb:?}");
        }
    }
}

#[test]
fn watermark_takes_the_export_insets_and_rotation() {
    let s = OverlaySettings::default().merged(&json!({"watermark": {"inset": 0.1, "insetY": 0.2, "rotation": 90}})).unwrap();
    assert_eq!((s.watermark.inset_x, s.watermark.inset_y, s.watermark.rotation), (0.1, 0.2, 90));
    let s = s.merged(&json!({"watermark": {"insetX": 0.05}})).unwrap();
    assert_eq!((s.watermark.inset_x, s.watermark.inset_y), (0.05, 0.2));
    assert!(OverlaySettings::default().merged(&json!({"watermark": {"rotation": 45}})).is_err());
}

#[test]
fn hostile_settings_are_refused_or_clamped_and_never_panic() {
    let d = OverlaySettings::default();
    for bad in [
        json!({"identityPlate": {"size": "big"}}),
        json!({"identityPlate": {"anchor": "middle"}}),
        json!({"identityPlate": 3}),
        json!({"ratingStars": {"color": "#zzzzzz"}}),
        json!({"texts": {"text": "a"}}),
        json!({"texts": [{"relativeTo": "page"}]}),
        json!({"texts": [{"sparkle": 1}]}),
        json!({"texts": vec![json!({}); MAX_TEXTS + 1]}),
        json!({"removeText": 0}),
        json!({"removeText": -1}),
        json!({"watermark": {"size": 9}}),
        json!({"watermark": 5}),
        json!({"showWatermark": "yes"}),
        json!({"glitter": true}),
        json!([1]),
    ] {
        assert!(d.merged(&bad).is_err(), "{bad}");
    }
    let s = d
        .merged(&json!({"identityPlate": {"size": 1e300, "opacity": -3, "offsetX": -1e9}, "ratingStars": {"size": 0}, "addText": {"size": -1, "shadowAngle": 900, "shadowRadius": 7}}))
        .unwrap();
    assert_eq!((s.identity_plate.size, s.identity_plate.opacity, s.identity_plate.place.offset_x), (PLATE_SIZE_MAX, 0.0, -OFFSET_MAX));
    assert_eq!(s.rating_stars.size, SIZE_MIN);
    assert_eq!((s.texts[0].size, s.texts[0].shadow.angle, s.texts[0].shadow.radius), (SIZE_MIN, 180.0, SHADOW_RADIUS_MAX));
    assert!(d.merged(&json!({"removeText": 0})).is_err());
    assert!(s.merged(&json!({"removeText": 0})).unwrap().texts.is_empty());
    // round trips (command JSON and prefs.json)
    assert_eq!(d.merged(&s.to_json()).unwrap(), s);
    let back: OverlaySettings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
    assert_eq!(back, s);
    // NaN / ∞ from a hand-edited prefs file
    let mut wild = s.clone();
    wild.identity_plate.size = f64::NAN;
    wild.texts[0].place.offset_y = f64::INFINITY;
    wild.watermark.size = f32::NAN;
    let w = wild.sanitized();
    assert_eq!((w.identity_plate.size, w.texts[0].place.offset_y, w.watermark.size), (IdentityPlate::default().size, 0.0, Watermark::default().size));
    // drawing hostile text at every size: empty, huge, CJK, RTL, control characters
    let huge = "W".repeat(50_000);
    for text in ["", " ", "\n\n", huge.as_str(), "日本語のテキスト", "שלום עולם", "مرحبا", "a\u{0}b\u{7}c\r\n", "{caption}{"] {
        let s = d
            .merged(&json!({"identityPlate": {"enabled": true, "text": text, "size": PLATE_SIZE_MAX}, "ratingStars": {"enabled": true}, "addText": {"text": text, "size": TEXT_SIZE_MAX, "shadow": true, "shadowRadius": SHADOW_RADIUS_MAX}, "showWatermark": true, "watermark": text}))
            .unwrap();
        assert!(s.identity_plate.text.chars().count() <= TEXT_MAX_CHARS);
        let mut bare = photo();
        bare.meta = Default::default();
        for (w, h) in [(1, 1), (17, 9), (320, 200)] {
            let l = lightcraft_slideshow::layout(&lightcraft_slideshow::SlideSettings::default(), f64::NAN, w, h);
            let o = SlideOverlays::resolve(&s, Some(&bare));
            let mut img = blank(l.slide.width() as usize, l.slide.height() as usize);
            o.draw_behind(&mut img, &l);
            o.draw_front(&mut img, &l);
            for (_, b) in o.boxes(&l) {
                assert!(b.x0.is_finite() && b.y1.is_finite());
            }
        }
    }
}

/// Huge text and stars on a big slide cost what shows: glyphs off the slide aren't rasterised.
#[test]
fn huge_overlays_on_a_big_slide_stay_cheap() {
    let long = "Wide ".repeat(400);
    let s = OverlaySettings::default()
        .merged(&json!({
            "ratingStars": {"enabled": true, "size": STAR_SIZE_MAX},
            "addText": {"text": long, "size": TEXT_SIZE_MAX, "shadow": true, "shadowRadius": SHADOW_RADIUS_MAX, "anchor": "topLeft", "offsetX": 0, "offsetY": 0},
        }))
        .unwrap();
    let mut p = photo();
    p.rating = 5;
    let l = lightcraft_slideshow::layout(&lightcraft_slideshow::SlideSettings::default(), 1.5, 4000, 2500);
    let mut img = blank(4000, 2500);
    let t = std::time::Instant::now();
    SlideOverlays::resolve(&s, Some(&p)).draw_front(&mut img, &l);
    assert!(t.elapsed().as_secs_f64() < 20.0, "{:?}", t.elapsed());
    assert!(img.get(100, 300)[0] > 0, "the text's start shows");
}

/// CJK text draws with the craft-fonts faces (skips without them).
#[test]
fn cjk_text_uses_the_craft_fonts() {
    if crate::fonts::cjk(crate::fonts::CRAFT_FONTS).next().is_none() {
        eprintln!("skipped: built without craft-fonts (CRAFT_FONTS_DIR)");
        return;
    }
    let cjk = crate::text::TextLayout::new("日本語", 40.0, false, crate::fonts::CRAFT_FONTS).unwrap();
    let tofu = crate::text::TextLayout::new("日本語", 40.0, false, &[]).unwrap();
    let count = |t: &crate::text::TextLayout| {
        let mut n = 0usize;
        t.draw(0.0, 0.0, |_, _, c| n += usize::from(c > 0.5));
        n
    };
    assert_ne!(count(&cjk), count(&tofu), "real glyphs, not missing-glyph boxes");
}
