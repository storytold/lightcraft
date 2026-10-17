//! HDR editing: the HDR render ([`OutputDepth::F32Hdr`]) and the SDR rendition every other render uses.

use lcc_develop::DevelopSettings;
use lcc_raster::Rgb32f;

use crate::{DeepSamples, OutputDepth, RenderRequest, SourceInfo, render};

fn sunset() -> Rgb32f {
    let lib = lcc_scenes::demo_library();
    let s = lib.iter().find(|s| s.kind == lcc_scenes::Kind::OceanSunset).map(|s| s.render(240, 160));
    s.unwrap_or_else(|| lib[0].render(240, 160))
}

fn raw() -> SourceInfo {
    SourceInfo { raw: true, ..Default::default() }
}

fn hdr_on(max_ev: f64) -> DevelopSettings {
    let mut s = DevelopSettings::default();
    s.hdr.enabled = true;
    s.hdr.max_ev = max_ev;
    s
}

fn deep(src: &Rgb32f, s: &DevelopSettings, depth: OutputDepth) -> Vec<f32> {
    let req = RenderRequest { depth, ..RenderRequest::fit(240, 240) };
    match render(src, &raw(), s, &req).deep.map(|d| d.samples) {
        Some(DeepSamples::F32(v)) => v,
        other => panic!("expected float samples, got {other:?}"),
    }
}

#[test]
fn hdr_midtones_match_the_sdr_render() {
    let src = sunset();
    let hdr = deep(&src, &hdr_on(3.0), OutputDepth::F32Hdr);
    let sdr = deep(&src, &DevelopSettings::default(), OutputDepth::F32Linear);
    // pixels whose brightest channel is a midtone (bright saturated colours legitimately differ:
    // SDR desaturates them at white, HDR keeps their colour)
    let mut n = 0;
    for (h, s) in hdr.as_chunks::<3>().0.iter().zip(sdr.as_chunks::<3>().0) {
        if s.iter().all(|v| *v < 0.15) {
            for (a, b) in h.iter().zip(s) {
                assert!((a - b).abs() < 2e-3, "{h:?} vs {s:?}");
            }
            n += 1;
        }
    }
    assert!(n > 100, "the scene should have midtones and shadows ({n})");
}

#[test]
fn f32hdr_without_hdr_is_the_sdr_float_render() {
    let src = sunset();
    let s = DevelopSettings::default();
    assert_eq!(deep(&src, &s, OutputDepth::F32Hdr), deep(&src, &s, OutputDepth::F32Linear));
}

#[test]
fn sdr_renders_of_an_hdr_edit_use_the_sdr_rendition() {
    let src = sunset();
    let req = RenderRequest::fit(240, 240);
    // neutral offsets: the edit with HDR off, its highlights rolled off (issue #69)
    let plain = render(&src, &raw(), &DevelopSettings::default(), &req);
    let on = render(&src, &raw(), &hdr_on(3.0), &req);
    let rendition = render(&src, &raw(), &hdr_on(3.0).sdr_rendition(), &req);
    assert_eq!(rendition.image.data, on.image.data);
    // offsets apply to every SDR render
    let mut s = hdr_on(3.0);
    s.hdr.sdr_brightness = 50.0;
    s.hdr.sdr_highlights = -40.0;
    let brighter = render(&src, &raw(), &s, &req);
    let expect = render(&src, &raw(), &s.sdr_rendition(), &req);
    assert_eq!(brighter.image.data, expect.image.data);
    assert_ne!(brighter.image.data, plain.image.data);
    let r = s.sdr_rendition();
    assert!(!r.hdr.enabled && (r.light.exposure - 0.5).abs() < 1e-9 && r.light.highlights == -40.0);
}

/// Issue #69: an HDR edit's SDR rendition compresses the highlights an SDR edit would clip, so a bright
/// ramp keeps distinct values above the old clip point; the same edit with HDR off renders as before.
#[test]
fn sdr_rendition_rolls_highlights_off_instead_of_clipping() {
    // a horizontal ramp from mid-grey to 6 stops over grey
    let (w, h) = (256, 4);
    let mut src = Rgb32f::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = 0.18 * 2f32.powf(6.0 * x as f32 / (w - 1) as f32);
            src.data[y * w + x] = [v, v, v];
        }
    }
    let req = RenderRequest::fit(w, w);
    let mut sdr = DevelopSettings::default();
    sdr.light.exposure = 1.5;
    let mut hdr = sdr.clone();
    hdr.hdr.enabled = true;
    let row = |s: &DevelopSettings| -> Vec<u8> {
        let out = render(&src, &raw(), s, &req).image;
        out.data[..out.width].iter().map(|p| p[1]).collect()
    };
    let (old, new) = (row(&sdr), row(&hdr));
    let first_clip = old.iter().position(|v| *v == 255).expect("the SDR edit clips this ramp");
    let distinct = |r: &[u8]| {
        let mut v = r[first_clip..].to_vec();
        v.dedup();
        v.len()
    };
    assert_eq!(distinct(&old), 1, "SDR edit: flat white past its clip point");
    assert!(distinct(&new) > 10, "HDR edit's SDR rendition keeps gradation: {:?}", &new[first_clip..]);
    assert!(new.windows(2).all(|p| p[1] >= p[0]), "monotone");
    // SDR edits are untouched: no rolloff without HDR, whatever a damaged field says
    let mut damaged = sdr.clone();
    damaged.hdr.rolloff_peak = f32::NAN;
    assert_eq!(row(&damaged), old);
    assert_eq!(sdr.sdr_rendition().hdr.rolloff_peak(), 1.0);
}
