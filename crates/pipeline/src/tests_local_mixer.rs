//! Pixel tests of a mask's local Color Mixer (`Mask::mixer`, LR-MASK-MIXER).

use lightcraft_develop::{DevelopSettings, Hsl, Mask, MaskComponent, MaskOp, MaskShape, Mixer, Treatment};
use lightcraft_geom::Point;
use lightcraft_raster::{Rgb32f, Rgba8};

use crate::{RenderRequest, SourceInfo, render};

const W: usize = 240;
const H: usize = 120;

/// Every hue across the width (saturated colours, in linear sRGB as Rec.709 primaries), darker towards the top.
fn colours() -> Rgb32f {
    Rgb32f::from_fn(W, H, |x, y| lightcraft_color::perceptual::hsv_to_rgb(x as f32 / W as f32 * 360.0, 0.85, 0.08 + 0.7 * y as f32 / H as f32))
}

/// A Color Mixer that moves several bands (hue, saturation and luminance).
fn strong() -> Mixer {
    Mixer {
        orange: Hsl { hue: 60.0, sat: 40.0, lum: -30.0 },
        green: Hsl { hue: -50.0, sat: -80.0, lum: 40.0 },
        blue: Hsl { hue: 30.0, sat: -100.0, lum: -50.0 },
        magenta: Hsl { hue: 0.0, sat: 70.0, lum: 20.0 },
        ..Default::default()
    }
}

/// Settings with one hard mask left of `edge` (x as a fraction of the width; past 1 it covers everything) and the
/// local Color Mixer `mixer`.
fn masked(edge: f64, mixer: Mixer) -> DevelopSettings {
    let shape = MaskShape::Linear { start: Point::new(edge, 0.5), end: Point::new(edge + 0.01, 0.5) };
    let mut s = DevelopSettings::default();
    s.masks.push(Mask { id: 1, components: vec![MaskComponent { name: None, op: MaskOp::Add, invert: false, shape }], mixer, ..Default::default() });
    s
}

fn shot(src: &Rgb32f, s: &DevelopSettings) -> Rgba8 {
    render(src, &SourceInfo::default(), s, &RenderRequest::fit(W, H)).image
}

/// Pixels that differ between `a` and `b` in columns `x0..x1`.
fn differ(a: &Rgba8, b: &Rgba8, x0: usize, x1: usize) -> usize {
    (0..H).flat_map(|y| (x0..x1).map(move |x| y * W + x)).filter(|&i| a.data[i] != b.data[i]).count()
}

#[test]
fn a_full_mask_color_mixer_renders_as_the_global_mixer() {
    let src = colours();
    let global = DevelopSettings { mixer: strong(), ..Default::default() };
    let (g, l) = (shot(&src, &global), shot(&src, &masked(2.0, strong())));
    assert_eq!(differ(&g, &l, 0, W), 0, "same mixer, same maths");
    let changed = differ(&g, &shot(&src, &DevelopSettings::default()), 0, W);
    assert!(changed > W * H / 4, "the mixer changes the colours ({changed} px)");
}

#[test]
fn the_local_color_mixer_works_inside_the_mask_only() {
    let src = colours();
    let plain = shot(&src, &DevelopSettings::default());
    let img = shot(&src, &masked(0.45, strong()));
    assert_eq!(differ(&img, &plain, (W as f64 * 0.47) as usize, W), 0, "nothing changes outside the mask");
    let inside = differ(&img, &plain, 0, (W as f64 * 0.44) as usize);
    assert!(inside > W * H / 8, "the mask's mixer applies inside ({inside} px)");
}

#[test]
fn a_neutral_local_mixer_changes_nothing() {
    let src = colours();
    let s = masked(0.45, Mixer::default());
    assert_eq!(shot(&src, &s).data, shot(&src, &DevelopSettings::default()).data);
}

#[test]
fn the_mask_amount_scales_the_local_mixer() {
    let src = colours();
    let plain = shot(&src, &DevelopSettings::default());
    let full = shot(&src, &masked(2.0, strong()));
    let mut half = masked(2.0, strong());
    half.masks[0].adjust.amount = 50.0;
    let half = shot(&src, &half);
    let dist =
        |a: &Rgba8, b: &Rgba8| a.data.iter().zip(&b.data).map(|(p, q)| (0..3).map(|k| (p[k] as i32 - q[k] as i32).abs()).sum::<i32>()).sum::<i32>();
    let (d_half, d_full) = (dist(&half, &plain), dist(&full, &plain));
    assert!(d_half > 0 && d_half < d_full, "half the Amount, part of the change: {d_half} vs {d_full}");
}

#[test]
fn black_and_white_ignores_the_local_mixer() {
    let src = colours();
    let mut bw = masked(2.0, strong());
    bw.treatment = Treatment::Bw;
    let plain = DevelopSettings { treatment: Treatment::Bw, ..Default::default() };
    assert_eq!(shot(&src, &bw).data, shot(&src, &plain).data);
}

#[test]
fn the_local_mixer_is_written_only_when_set_and_reads_back() {
    let mut m = Mask::default();
    let json = serde_json::to_string(&m).unwrap();
    assert!(!json.contains("mixer"), "a neutral mixer isn't written: {json}");
    let back: Mask = serde_json::from_str(&json).unwrap();
    assert!(back.mixer.is_neutral(), "a mask without it reads as neutral");
    m.mixer = strong();
    let back: Mask = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
    assert_eq!(back.mixer, strong());
}
