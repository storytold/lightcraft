use super::*;
use crate::{OutputDepth, OutputSpace, PixelWindow, Proof};
use lightcraft_geom::Point;

fn source() -> Arc<Rgb32f> {
    Arc::new(Rgb32f::from_fn(128, 32, |x, y| {
        let v = 0.01 + x as f32 / 500.0;
        [v, v * (0.3 + y as f32 / 50.0), v * 0.4]
    }))
}
fn bent() -> Vec<Point> {
    vec![Point::new(0.0, 0.04), Point::new(0.25, 0.65), Point::new(1.0, 0.95)]
}
fn sample(s: &DevelopSettings, ch: CurveInput) -> Rendered {
    render(&source(), &SourceInfo::default(), s, &RenderRequest::fit(128, 32), None, ch)
}

#[test]
fn input_follows_exposure_but_not_its_own_curve_or_later_curves() {
    let base = DevelopSettings::default();
    for ch in [CurveInput::Parametric, CurveInput::Master, CurveInput::Red, CurveInput::Green, CurveInput::Blue] {
        let mut s = base.clone();
        if ch != CurveInput::Parametric {
            s.curve.lights = 45.0;
        }
        if !matches!(ch, CurveInput::Parametric | CurveInput::Master) {
            s.curve.master = bent();
        }
        let before = sample(&s, ch).histogram;
        let key = ch.settings_key(&s);
        if ch == CurveInput::Parametric {
            s.curve.shadows = 65.0;
            s.curve.split_mid = 60.0;
        }
        if matches!(ch, CurveInput::Parametric | CurveInput::Master) {
            s.curve.master = bent();
        }
        s.curve.red = bent();
        s.curve.green = bent();
        s.curve.blue = bent();
        s.curve.refine_saturation = 0.0;
        s.grain.amount = 90.0;
        assert_eq!(sample(&s, ch).histogram, before, "own and later curves: {ch:?}");
        assert_eq!(ch.settings_key(&s), key, "do not rerender for this curve's drag: {ch:?}");
        s.light.exposure = 1.0;
        assert_ne!(sample(&s, ch).histogram, before, "upstream exposure: {ch:?}");
        assert_ne!(ch.settings_key(&s), key);
    }
    let mut changed = base.clone();
    changed.curve.master = bent();
    let req = RenderRequest::fit(128, 32);
    let a = crate::render(&source(), &SourceInfo::default(), &base, &req);
    let b = crate::render(&source(), &SourceInfo::default(), &changed, &req);
    assert_ne!(a.histogram, b.histogram, "the separate output histogram must follow the curve");
    assert_ne!(a.image, b.image);
}

#[test]
fn each_curve_sees_exactly_its_predecessors_including_profile_contributions() {
    let mut s = DevelopSettings::default();
    s.profile.id = "lc.film.golden-hour".into();
    s.curve.shadows = 45.0;
    s.curve.master = bent();
    s.curve.red = vec![Point::new(0.0, 0.0), Point::new(0.6, 0.2), Point::new(1.0, 1.0)];
    s.curve.green = bent();
    s.curve.blue = bent();
    let req = RenderRequest::fit(128, 32);
    let mut images = Vec::new();
    for ch in [CurveInput::Parametric, CurveInput::Master, CurveInput::Red, CurveInput::Green, CurveInput::Blue] {
        let input = sample(&s, ch);
        // In this fixture, remove every operation after the desired boundary explicitly.
        // Expanding the profile first preserves its upstream tone/colour and preceding curves.
        let mut previous = crate::profiles::effective(&s).into_owned();
        previous.profile.id = "lc.color".into();
        previous.curve = ch.predecessors(&previous.curve);
        let reference = crate::render(&source(), &SourceInfo::default(), &previous, &req);
        assert_eq!(input.image, reference.image, "boundary {ch:?}");
        images.push(input.image);
    }
    assert_ne!(images[0], images[1], "parametric contributes before master");
    assert_ne!(images[1], images[2], "master contributes before RGB");
    assert_eq!(images[2], images[3], "the RGB curves have a common predecessor");
    assert_eq!(images[3], images[4]);
    assert_ne!(sample(&s, CurveInput::Red).histogram.r, sample(&s, CurveInput::Red).histogram.b);
}

#[test]
fn grain_lut_and_proof_warning_colours_are_after_the_sample() {
    let s = DevelopSettings::default();
    let before = sample(&s, CurveInput::Master);
    let mut changed = s.clone();
    changed.grain.amount = 100.0;
    changed.profile.id = "lut:curve-input-regression".into();
    crate::lut::register(
        &changed.profile.id,
        crate::lut::Lut3d { size: 2, data: vec![[0.9, 0.1, 0.8]; 8], domain_min: [0.0; 3], domain_max: [1.0; 3], title: None },
    );
    let after = sample(&changed, CurveInput::Master);
    let output = crate::render(&source(), &SourceInfo::default(), &changed, &RenderRequest::fit(128, 32));
    crate::lut::unregister(&changed.profile.id);
    assert_eq!(before.histogram, after.histogram);
    assert_ne!(output.image, after.image);
    let mut req = RenderRequest::fit(128, 32);
    req.proof = Some(Proof { space: OutputSpace::Srgb, dest_warning: true, display_warning: true, ..Default::default() });
    let warning = render(&source(), &SourceInfo::default(), &s, &req, None, CurveInput::Master);
    req.proof.as_mut().unwrap().dest_warning = false;
    req.proof.as_mut().unwrap().display_warning = false;
    assert_eq!(warning.histogram, render(&source(), &SourceInfo::default(), &s, &req, None, CurveInput::Master).histogram);
}

#[test]
fn crop_changes_the_sample_but_zoom_windows_do_not_and_sizes_are_bounded() {
    let mut s = DevelopSettings::default();
    let req = RenderRequest::fit(128, 32);
    let whole = render(&source(), &SourceInfo::default(), &s, &req, None, CurveInput::Master);
    let window = RenderRequest { window: Some(PixelWindow { x: 32, y: 0, w: 32, h: 32 }), ..req };
    assert_eq!(whole.histogram, render(&source(), &SourceInfo::default(), &s, &window, None, CurveInput::Master).histogram);
    s.crop.geometry.rect = lightcraft_geom::Rect::new(0.5, 0.0, 1.0, 1.0);
    let cropped = render(&source(), &SourceInfo::default(), &s, &req, None, CurveInput::Master);
    assert_ne!(whole.histogram, cropped.histogram);
    let uncropped = RenderRequest { apply_crop: false, ..req };
    assert_eq!(whole.histogram, render(&source(), &SourceInfo::default(), &s, &uncropped, None, CurveInput::Master).histogram);
    let huge = RenderRequest::fit(usize::MAX, usize::MAX);
    let bounded = render(&source(), &SourceInfo::default(), &s, &huge, None, CurveInput::Master);
    assert!(bounded.image.width <= EDGE && bounded.image.height <= EDGE);
    assert!(bounded.histogram.total <= (EDGE * EDGE) as u32);
}

#[test]
fn hdr_preview_uses_its_sdr_rendition_and_bins_do_not_alias_headroom_to_white() {
    let mut s = DevelopSettings::default();
    s.hdr.enabled = true;
    s.hdr.sdr_brightness = -50.0;
    let preview = sample(&s, CurveInput::Master);
    assert_eq!(preview.histogram, sample(&s.sdr_rendition(), CurveInput::Master).histogram);
    s.hdr.sdr_brightness = 50.0;
    assert_ne!(preview.histogram, sample(&s, CurveInput::Master).histogram);
    let samples = [[0.0; 3], [0.5; 3], [1.0; 3], [1.01; 3], [f32::NAN; 3], [f32::INFINITY; 3], [-0.1; 3]];
    let h = of_encoded(&samples, [0.25, 0.5, 0.25]);
    assert_eq!(h.r.iter().sum::<u32>(), 3);
    assert_eq!(h.r[0], 1);
    assert_eq!(h.r[128], 1);
    assert_eq!(h.r[255], 1);
    assert_eq!(h.luma, h.r);
    let src = Arc::new(Rgb32f::from_fn(8, 8, |_, _| [4.0; 3]));
    let req = RenderRequest { depth: OutputDepth::F32Hdr, ..RenderRequest::fit(8, 8) };
    let high = render(&src, &SourceInfo::default(), &s, &req, None, CurveInput::Parametric);
    assert_eq!(high.histogram.r.iter().sum::<u32>(), 0, "HDR values lie beyond the curve axes");
}

#[test]
fn diagnostic_and_normal_renders_share_spatial_cache_without_changing_pixels() {
    let src = source();
    let mut s = DevelopSettings::default();
    s.curve.master = bent();
    s.curve.lights = 60.0;
    s.grain.amount = 50.0;
    let info = SourceInfo::default();
    let req = RenderRequest::fit(128, 32);
    let cache = StageCache::default();
    let before = crate::render_cached(&src, &info, &s, &req, &cache);
    let a = render(&src, &info, &s, &req, Some(&cache), CurveInput::Red);
    let b = render(&src, &info, &s, &req, None, CurveInput::Red);
    let after = crate::render_cached(&src, &info, &s, &req, &cache);
    assert_eq!(a.histogram, b.histogram);
    assert_eq!(before.image, after.image);
    assert_eq!(before.histogram, after.histogram);
}

#[test]
fn input_uses_the_display_primaries_and_effective_edit() {
    let src = source();
    let info = SourceInfo::default();
    let mut s = DevelopSettings::default();
    s.light.exposure = 1.0;
    s.curve.lights = 55.0;
    s.curve.master = bent();
    s.set_section_enabled("light", false);
    let req = RenderRequest::fit(128, 32);
    let disabled = render(&src, &info, &s, &req, None, CurveInput::Red);
    let mut effective = s.effective().into_owned();
    effective.curve = CurveInput::Red.predecessors(&effective.curve);
    assert_eq!(disabled.image, crate::render(&src, &info, &effective, &req).image);
    let mut previous = DevelopSettings::default();
    previous.curve.lights = 45.0;
    let srgb = render(&src, &info, &previous, &req, None, CurveInput::Red);
    for space in [OutputSpace::DisplayP3, OutputSpace::Rec2020] {
        let display = RenderRequest { display: crate::DisplaySpace::of(space, 7), ..req };
        let input = render(&src, &info, &previous, &display, None, CurveInput::Red);
        let normal = crate::render(&src, &info, &previous, &display);
        assert_eq!(input.image, normal.image, "the prefix is encoded in the actual preview primaries");
        assert_ne!(input.histogram, srgb.histogram);
        let proof = RenderRequest { proof: Some(Proof { space: OutputSpace::Srgb, ..Default::default() }), ..display };
        let proof_input = render(&src, &info, &previous, &proof, None, CurveInput::Red);
        assert_eq!(proof_input.image, crate::render(&src, &info, &previous, &proof).image);
    }
}
