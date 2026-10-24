//! Export a photo's look as a 3D LUT (`.cube`): its global colour and tone settings (profile,
//! white balance, tone, curves, colour, mixer, point colour, B&W mix, grading, calibration)
//! baked into a table that other apps (video editors, image editors, cameras) apply to any
//! picture. The table maps display-encoded sRGB to display-encoded sRGB, the way `.cube` looks
//! are used. Settings that depend on where a pixel is (masks, texture / clarity / dehaze,
//! highlights / shadows, sharpening, noise reduction, vignette, grain, crop, lens corrections…)
//! can't be held by a LUT: they are left out and named in the result.
//!
//! The table is made by rendering an image holding every lattice colour once through the
//! regular pipeline, as a rendered (non-raw) photo, so it matches what LightCraft draws.

use lightcraft_develop::DevelopSettings;
use lightcraft_pipeline::lut::Lut3d;
use lightcraft_pipeline::{DeepSamples, OutputDepth, OutputSpace, RenderRequest, SourceInfo};
use lightcraft_raster::Rgb32f;

/// Lattice sizes offered (entries an axis): 17 (small), 33 (the common default), 65 (finest).
pub const SIZES: [usize; 3] = [17, 33, 65];
/// The size used when none is given.
pub const DEFAULT_SIZE: usize = 33;

/// `settings` with only what a LUT can hold kept, and the names of the settings that were set
/// but left out (empty when the whole look fits in a LUT).
pub fn lut_settings(settings: &DevelopSettings) -> (DevelopSettings, Vec<&'static str>) {
    let s = settings.effective();
    let d = DevelopSettings::default();
    let mut kept = DevelopSettings {
        process: s.process,
        profile: s.profile.clone(),
        treatment: s.treatment,
        wb: s.wb,
        light: s.light,
        curve: s.curve.clone(),
        color: s.color,
        mixer: s.mixer,
        point_colors: s.point_colors.clone(),
        bw_mix: s.bw_mix,
        grading: s.grading,
        calibration: s.calibration,
        ..d.clone()
    };
    // a raw's own local tone mapping is spatial
    kept.profile.camera_local_tone = false;
    // highlights and shadows are local (they lift or hold detail by its neighbourhood)
    kept.light.highlights = d.light.highlights;
    kept.light.shadows = d.light.shadows;

    let mut skipped = Vec::new();
    let mut note = |set: bool, name: &'static str| {
        if set {
            skipped.push(name);
        }
    };
    note(s.light.highlights != d.light.highlights || s.light.shadows != d.light.shadows, "highlights & shadows");
    note(s.effects != d.effects, "texture, clarity & dehaze");
    note(s.hdr != d.hdr, "HDR");
    note(s.vignette != d.vignette, "vignette");
    note(s.grain != d.grain, "grain");
    note(s.detail != d.detail, "sharpening & noise reduction");
    note(s.optics != d.optics, "lens corrections");
    note(s.geometry != d.geometry, "transform");
    note(s.crop != d.crop || s.orientation != d.orientation, "crop & rotation");
    note(!s.masks.is_empty(), "masks");
    note(!s.spots.is_empty() || !s.red_eye.is_empty(), "spot removal & red eye");
    note(s.lens_blur != d.lens_blur, "lens blur");
    note(s.enhance != d.enhance, "enhance");
    note(s.profile.camera_local_tone, "camera local tone");
    (kept, skipped)
}

/// Bake `settings` (see [`lut_settings`]) into a `size`³ table ([`SIZES`]; other sizes 2..=129
/// work too). Errors only when the render doesn't come back at the lattice image's size.
pub fn bake(settings: &DevelopSettings, size: usize) -> Result<Lut3d, String> {
    let n = size.clamp(2, 129);
    let (kept, _) = lut_settings(settings);
    let identity = Lut3d::identity(n);
    // the lattice as an n² × n picture: red along x within each block, green across blocks, blue down
    let (w, h) = (n * n, n);
    let src = Rgb32f { width: w, height: h, data: identity.data.iter().map(|c| c.map(lightcraft_color::transfer::srgb_to_linear)).collect() };
    let req = RenderRequest { space: OutputSpace::Srgb, depth: OutputDepth::U16, ..RenderRequest::fit(w, h) };
    let info = SourceInfo::default();
    let r = lightcraft_pipeline::render(&src, &info, &kept, &req);
    let Some(deep) = r.deep.filter(|d| (d.width, d.height) == (w, h)) else {
        return Err(format!("the look rendered at {} × {}, not the {w} × {h} lattice", r.image.width, r.image.height));
    };
    let DeepSamples::U16(v) = &deep.samples else { return Err("the look rendered without 16-bit samples".into()) };
    let data: Vec<[f32; 3]> = v.as_chunks::<3>().0.iter().map(|c| c.map(|x| x as f32 / 65535.0)).collect();
    if data.len() != n * n * n {
        return Err(format!("the look rendered {} colours, not {}", data.len(), n * n * n));
    }
    Ok(Lut3d { data, ..identity })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::Hsl;
    use lightcraft_geom::Point;

    /// A look touching every kind of global setting (and some local ones a LUT leaves out).
    fn look() -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.wb.temp = 15.0;
        s.wb.tint = -8.0;
        s.light.exposure = 0.4;
        s.light.contrast = 25.0;
        s.light.whites = 10.0;
        s.light.blacks = -15.0;
        s.curve.master = vec![Point { x: 0.0, y: 0.05 }, Point { x: 0.5, y: 0.45 }, Point { x: 1.0, y: 0.95 }];
        s.color.vibrance = 20.0;
        s.color.saturation = -10.0;
        s.mixer.blue = Hsl { hue: -20.0, sat: 30.0, lum: -10.0 };
        s.grading.shadows.hue = 200.0;
        s.grading.shadows.sat = 25.0;
        s.grading.highlights.hue = 40.0;
        s.grading.highlights.sat = 20.0;
        s
    }

    /// A picture of varied colours: smooth ramps and scattered saturated pixels.
    fn picture() -> lightcraft_raster::Rgba8 {
        lightcraft_raster::Image::from_fn(48, 32, |x, y| {
            let h = (x * 7919 + y * 104_729) % 251;
            if (x + y) % 5 == 0 {
                [(h * 3 % 256) as u8, (h * 7 % 256) as u8, (h * 11 % 256) as u8, 255]
            } else {
                [(x * 5) as u8, (y * 8) as u8, ((x + y) * 3) as u8, 255]
            }
        })
    }

    /// Error (in 8-bit levels) of `lut` against the render of `s` over [`picture`], sorted.
    fn errors(lut: &Lut3d, s: &DevelopSettings) -> Vec<f32> {
        let pic = picture();
        let (kept, _) = lut_settings(s);
        let req = RenderRequest { depth: OutputDepth::U16, ..RenderRequest::fit(pic.width, pic.height) };
        let r = lightcraft_pipeline::render(&pic.to_linear(), &SourceInfo::default(), &kept, &req);
        let DeepSamples::U16(want) = r.deep.unwrap().samples else { panic!() };
        let mut e: Vec<f32> = pic
            .data
            .iter()
            .zip(want.as_chunks::<3>().0)
            .flat_map(|(p, w)| {
                let got = lut.apply([p[0], p[1], p[2]].map(|v| v as f32 / 255.0));
                (0..3).map(move |k| (got[k] - w[k] as f32 / 65535.0).abs() * 255.0)
            })
            .collect();
        e.sort_by(f32::total_cmp);
        e
    }

    #[test]
    fn the_lut_reproduces_the_global_look() {
        let s = look();
        assert!(lut_settings(&s).1.is_empty());
        let stats = |n: usize| {
            let e = errors(&bake(&s, n).unwrap(), &s);
            (e.iter().sum::<f32>() / e.len() as f32, e[e.len() * 95 / 100], e[e.len() - 1])
        };
        // a table interpolates between its entries: the error falls as it gets finer, and only
        // colours at the edge of the gamut (where the render clips a channel) are off by more
        let (m17, p17, w17) = stats(17);
        let (m33, p33, w33) = stats(33);
        let (m65, p65, w65) = stats(65);
        assert!(m33 < 0.6 && p33 < 2.0, "33: mean {m33:.2}, p95 {p33:.2}");
        assert!(m65 < 0.3 && p65 < 1.0, "65: mean {m65:.2}, p95 {p65:.2}");
        assert!(m65 < m33 && m33 < m17 && w65 <= w33 && w33 <= w17, "finer is closer: {w17} {w33} {w65} / {p17}");
        // and the look is in the table (not just the renderer's own defaults)
        let base = bake(&DevelopSettings::default(), 33).unwrap();
        let lut = bake(&s, 33).unwrap();
        assert!(lut.data.iter().zip(&base.data).any(|(a, b)| (a[0] - b[0]).abs() > 0.05));
        assert!(errors(&base, &s)[errors(&base, &s).len() / 2] > 5.0, "the defaults' table doesn't match the look");
    }

    #[test]
    fn local_settings_are_left_out_and_named() {
        let mut s = look();
        s.light.shadows = 40.0;
        s.effects.clarity = 30.0;
        s.vignette.amount = -20.0;
        s.detail.sharpen_amount = 50.0;
        let (kept, skipped) = lut_settings(&s);
        assert_eq!(skipped, vec!["highlights & shadows", "texture, clarity & dehaze", "vignette", "sharpening & noise reduction"]);
        assert_eq!(kept.light.shadows, 0.0);
        assert_eq!(kept.effects, DevelopSettings::default().effects);
        assert_eq!(kept.light.exposure, 0.4, "global tone kept");
        // the bake is the same with or without them
        let a = bake(&s, 9).unwrap();
        let b = bake(&look(), 9).unwrap();
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn sizes_and_a_cube_file() {
        for n in SIZES {
            let l = bake(&look(), n).unwrap();
            assert_eq!(l.data.len(), n * n * n);
            let back = Lut3d::parse_cube(&l.to_cube(&[])).unwrap();
            assert_eq!(back.size, n);
        }
        assert_eq!(bake(&look(), 1).unwrap().size, 2, "clamped");
    }
}

#[cfg(test)]
mod command_tests {
    use serde_json::json;

    #[test]
    fn export_look_as_lut_writes_a_cube_and_can_add_it_as_a_profile() {
        let dir = std::env::temp_dir().join(format!("lc-lut-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = crate::Session::with_demo();
        s.execute("develop.set", &json!({"control": "light.exposure", "value": 0.5})).unwrap();
        s.execute("develop.set", &json!({"control": "effects.clarity", "value": 40})).unwrap();
        let path = dir.join("Warm Look");
        let r = s.execute("profile.exportLut", &json!({"path": path.to_string_lossy(), "size": 17, "addProfile": true})).unwrap();
        let file = dir.join("Warm Look.cube");
        assert_eq!(r["path"], file.to_string_lossy().as_ref(), "the extension is added");
        assert_eq!(r["size"], 17);
        assert_eq!(r["title"], "Warm Look");
        assert_eq!(r["skipped"], json!(["texture, clarity & dehaze"]));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# Not included (a LUT can't hold them): texture, clarity & dehaze"), "{}", &text[..300]);
        let lut = lightcraft_pipeline::lut::Lut3d::parse_cube(&text).unwrap();
        assert_eq!((lut.size, lut.title.as_deref()), (17, Some("Warm Look")));
        // the profile is registered and named after the look
        let id = r["profile"]["id"].as_str().unwrap().to_string();
        assert!(id.starts_with("lut:"), "{r}");
        assert!(lightcraft_pipeline::lut::get(&id).is_some());
        assert_eq!(r["profile"]["name"], "Warm Look");
        // the file is always a .cube: other extensions stay in the name
        for (asked, written) in [("look.zip", "look.zip.cube"), ("My.Look", "My.Look.cube"), ("Shout.CUBE", "Shout.CUBE")] {
            let r = s.execute("profile.exportLut", &json!({"path": dir.join(asked).to_string_lossy(), "size": 2, "addProfile": true})).unwrap();
            assert_eq!(r["path"], dir.join(written).to_string_lossy().as_ref(), "{asked}");
            assert!(r["profile"]["id"].is_string(), "{asked}: added as a profile: {r}");
        }
        // bad requests are errors, never panics
        for bad in [
            json!({}),
            json!({"path": file.to_string_lossy(), "size": 1}),
            json!({"path": file.to_string_lossy(), "size": "big"}),
            json!({"path": file.to_string_lossy(), "id": 999_999}),
        ] {
            assert!(s.execute("profile.exportLut", &bad).is_err(), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
