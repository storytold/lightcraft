use super::*;
use proptest::prelude::*;

/// A uniform picture of one Y′CbCr colour.
struct Uniform {
    y: Vec<u16>,
    c: [Vec<u16>; 2],
    w: usize,
    h: usize,
    cw: usize,
    bits: u8,
    chroma: Chroma,
}

fn uniform(w: usize, h: usize, bits: u8, chroma: Chroma, ycc: [u16; 3]) -> Uniform {
    let (cw, ch) = chroma.plane_size(w, h);
    Uniform { y: vec![ycc[0]; w * h], c: [vec![ycc[1]; cw * ch], vec![ycc[2]; cw * ch]], w, h, cw, bits, chroma }
}

impl Uniform {
    fn picture(&self) -> Picture<'_> {
        Picture {
            width: self.w,
            height: self.h,
            bit_depth: self.bits,
            chroma: self.chroma,
            y: Plane { samples: Samples::U16(&self.y), stride: self.w },
            cb: Plane { samples: Samples::U16(&self.c[0]), stride: self.cw },
            cr: Plane { samples: Samples::U16(&self.c[1]), stride: self.cw },
        }
    }
}

fn srgb(desc: ColourDescription, bits: u8, ycc: [u16; 3]) -> [u8; 4] {
    let u = uniform(4, 4, bits, Chroma::C420, ycc);
    let conv = Converter::new(desc, bits, 1920, 1080).unwrap();
    let mut out = vec![[0u8; 4]; 16];
    conv.to_srgb8(&u.picture(), &mut out).unwrap();
    assert!(out.iter().all(|p| *p == out[0]), "a uniform picture converts to one colour");
    out[0]
}

fn linear(desc: ColourDescription, bits: u8, ycc: [u16; 3]) -> [f32; 3] {
    let u = uniform(4, 4, bits, Chroma::C420, ycc);
    let conv = Converter::new(desc, bits, 1920, 1080).unwrap();
    let mut out = vec![[0f32; 3]; 16];
    conv.to_linear_rec2020(&u.picture(), &mut out).unwrap();
    out[0]
}

fn near(a: [u8; 4], b: [u8; 3], tol: i32) -> bool {
    (0..3).all(|i| (i32::from(a[i]) - i32::from(b[i])).abs() <= tol)
}

#[test]
fn limited_range_8_bit_bt709_black_white_and_primaries() {
    let d = ColourDescription::BT709;
    assert_eq!(srgb(d, 8, [16, 128, 128]), [0, 0, 0, 255]);
    assert_eq!(srgb(d, 8, [235, 128, 128]), [255, 255, 255, 255]);
    // BT.709 limited-range 75 % bars are the textbook check; full-intensity red is (63, 102, 240)
    assert!(near(srgb(d, 8, [63, 102, 240]), [255, 0, 0], 2), "{:?}", srgb(d, 8, [63, 102, 240]));
    assert!(near(srgb(d, 8, [173, 42, 26]), [0, 255, 0], 2), "{:?}", srgb(d, 8, [173, 42, 26]));
    assert!(near(srgb(d, 8, [32, 240, 118]), [0, 0, 255], 2), "{:?}", srgb(d, 8, [32, 240, 118]));
}

#[test]
fn full_range_uses_the_whole_code_range() {
    let d = ColourDescription { full_range: true, ..ColourDescription::BT709 };
    assert_eq!(srgb(d, 8, [0, 128, 128]), [0, 0, 0, 255]);
    assert_eq!(srgb(d, 8, [255, 128, 128]), [255, 255, 255, 255]);
    assert!(near(srgb(d, 8, [128, 128, 128]), [128, 128, 128], 1));
}

#[test]
fn ten_bit_limited_range_scales_by_four() {
    let d = ColourDescription::BT709;
    assert_eq!(srgb(d, 10, [64, 512, 512]), [0, 0, 0, 255]);
    assert_eq!(srgb(d, 10, [940, 512, 512]), [255, 255, 255, 255]);
    assert_eq!(srgb(d, 10, [63 * 4, 102 * 4, 240 * 4]), srgb(d, 8, [63, 102, 240]));
}

#[test]
fn linear_output_puts_sdr_white_at_one_and_grey_on_the_srgb_curve() {
    let w = linear(ColourDescription::BT709, 8, [235, 128, 128]);
    assert!(w.iter().all(|v| (v - 1.0).abs() < 1e-3), "{w:?}");
    let grey = linear(ColourDescription { full_range: true, ..ColourDescription::BT709 }, 8, [128, 128, 128]);
    let want = lightcraft_color::transfer::srgb_to_linear(128.0 / 255.0);
    assert!(grey.iter().all(|v| (v - want).abs() < 2e-3), "{grey:?} vs {want}");
    // pure BT.709 red is inside Rec.2020: a mix of the wider primaries, all non-negative
    let red = linear(ColourDescription::BT709, 8, [63, 102, 240]);
    assert!(red[0] > 0.5 && red[1] > 0.0 && red[2] >= 0.0, "{red:?}");
}

#[test]
fn bt2020_primaries_are_converted_not_passed_through() {
    let d = ColourDescription { primaries: 9, transfer: 14, matrix: 9, full_range: false };
    let conv = Converter::new(d, 10, 3840, 2160).unwrap();
    assert!(!conv.srgb_passthrough);
    // a Rec.2020-saturated green leaves sRGB: clipped to green, not a passthrough of the signal
    let g = srgb(d, 10, [690, 270, 160]);
    assert!(g[1] > 200 && g[0] < 60, "{g:?}");
}

#[test]
fn pq_and_hlg_are_tone_mapped_below_sdr_white() {
    let pq = ColourDescription { primaries: 9, transfer: 16, matrix: 9, full_range: false };
    let hlg = ColourDescription { transfer: 18, ..pq };
    for d in [pq, hlg] {
        assert!(Converter::new(d, 10, 3840, 2160).unwrap().is_hdr());
        let mut last = -1.0f32;
        for code in (64..=940).step_by(16) {
            let l = linear(d, 10, [code, 512, 512]);
            assert!(l.iter().all(|v| v.is_finite() && *v <= 1.0 + 1e-4), "{d:?} {code}: {l:?}");
            assert!(l[1] >= last - 1e-5, "monotonic: {d:?} {code}");
            last = l[1];
        }
        // the brightest code is near SDR white, not clipped far above it
        assert!(last > 0.9, "{d:?}: {last}");
    }
    // PQ reference white (203 cd/m², code ≈ 0.58 of the signal range) sits close to SDR white
    let code = (64.0 + 876.0 * lightcraft_color::transfer::pq_encode(203.0 / 10_000.0)) as u16;
    let ref_white = linear(pq, 10, [code, 512, 512]);
    assert!((0.85..=1.0).contains(&ref_white[1]), "{ref_white:?}");
}

#[test]
fn unspecified_code_points_follow_the_picture_size() {
    let sd = Converter::new(ColourDescription::UNSPECIFIED, 8, 720, 480).unwrap();
    assert_eq!(sd.resolved(), ColourDescription { primaries: 6, transfer: 1, matrix: 6, full_range: false });
    let pal = Converter::new(ColourDescription::UNSPECIFIED, 8, 720, 576).unwrap();
    assert_eq!((pal.resolved().primaries, pal.resolved().matrix), (5, 5));
    let hd = Converter::new(ColourDescription::UNSPECIFIED, 8, 1280, 720).unwrap();
    assert_eq!(hd.resolved(), ColourDescription::BT709);
    // reserved code points too
    let odd = Converter::new(ColourDescription { primaries: 3, transfer: 3, matrix: 3, full_range: false }, 8, 1920, 1080).unwrap();
    assert_eq!(odd.resolved(), ColourDescription::BT709);
}

#[test]
fn or_fills_only_unspecified_fields() {
    let vui = ColourDescription { primaries: 9, transfer: UNSPECIFIED, matrix: UNSPECIFIED, full_range: true };
    let colr = ColourDescription { primaries: 1, transfer: 18, matrix: 9, full_range: false };
    assert_eq!(vui.or(colr), ColourDescription { primaries: 9, transfer: 18, matrix: 9, full_range: true });
    assert!(ColourDescription::UNSPECIFIED.is_unspecified());
}

#[test]
fn ictcp_is_refused_with_a_reason() {
    let d = ColourDescription { primaries: 9, transfer: 16, matrix: 14, full_range: false };
    assert!(matches!(Converter::new(d, 10, 64, 64), Err(YuvError::Unsupported(_))));
}

#[test]
fn gbr_identity_matrix_reads_planes_as_g_b_r() {
    let d = ColourDescription { primaries: 1, transfer: 13, matrix: 0, full_range: true };
    let u = uniform(2, 2, 8, Chroma::C444, [10, 200, 90]);
    let mut out = vec![[0u8; 4]; 4];
    Converter::new(d, 8, 2, 2).unwrap().to_srgb8(&u.picture(), &mut out).unwrap();
    assert_eq!(out[0], [90, 10, 200, 255]);
}

#[test]
fn ycgco_round_trips_grey() {
    let d = ColourDescription { primaries: 1, transfer: 13, matrix: 8, full_range: true };
    assert!(near(srgb(d, 8, [100, 128, 128]), [100, 100, 100], 1));
}

/// A picture whose left half is one chroma value and right half another, 8-bit planes.
fn split(w: usize, h: usize, chroma: Chroma) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize) {
    let (cw, ch) = chroma.plane_size(w, h);
    let y = vec![126u8; w * h];
    let cb: Vec<u8> = (0..cw * ch).map(|i| if i % cw < cw / 2 { 90 } else { 166 }).collect();
    let cr: Vec<u8> = (0..cw * ch).map(|i| if i % cw < cw / 2 { 200 } else { 56 }).collect();
    (y, cb, cr, cw)
}

#[test]
fn chroma_is_upsampled_between_samples_for_420_and_422() {
    for chroma in [Chroma::C420, Chroma::C422] {
        let (w, h) = (8, 4);
        let (y, cb, cr, cw) = split(w, h, chroma);
        let pic = Picture {
            width: w,
            height: h,
            bit_depth: 8,
            chroma,
            y: Plane { samples: Samples::U8(&y), stride: w },
            cb: Plane { samples: Samples::U8(&cb), stride: cw },
            cr: Plane { samples: Samples::U8(&cr), stride: cw },
        };
        let mut out = vec![[0u8; 4]; w * h];
        Converter::new(ColourDescription::BT709, 8, w, h).unwrap().to_srgb8(&pic, &mut out).unwrap();
        for row in out.chunks(w) {
            // x = 0..2 sit on the left chroma; x = 3 halfway; x = 4.. on the right
            assert_eq!(row[0], row[2], "{chroma:?}");
            assert_ne!(row[2], row[3]);
            assert_ne!(row[3], row[4]);
            assert_eq!(row[4], row[7]);
            assert!(row[0][0] > row[7][0], "left is red, right is blue: {row:?}");
        }
    }
}

#[test]
fn padded_strides_are_honoured() {
    let (w, h, stride) = (3, 2, 7);
    let mut y = vec![0u16; stride * h];
    for r in 0..h {
        for x in 0..w {
            y[r * stride + x] = 235;
        }
    }
    let c = vec![128u16; 7];
    let pic = Picture {
        width: w,
        height: h,
        bit_depth: 8,
        chroma: Chroma::C420,
        y: Plane { samples: Samples::U16(&y), stride },
        cb: Plane { samples: Samples::U16(&c), stride: 7 },
        cr: Plane { samples: Samples::U16(&c), stride: 7 },
    };
    let mut out = vec![[0u8; 4]; w * h];
    Converter::new(ColourDescription::BT709, 8, w, h).unwrap().to_srgb8(&pic, &mut out).unwrap();
    assert!(out.iter().all(|p| *p == [255, 255, 255, 255]));
}

#[test]
fn bad_layouts_are_errors_not_panics() {
    let conv = Converter::new(ColourDescription::BT709, 8, 4, 4).unwrap();
    let u = uniform(4, 4, 8, Chroma::C420, [16, 128, 128]);
    let mut out = vec![[0u8; 4]; 16];
    let mut pic = u.picture();
    pic.y.samples = Samples::U16(&u.y[..15]);
    assert!(matches!(conv.to_srgb8(&pic, &mut out), Err(YuvError::Layout(_))));
    let mut pic = u.picture();
    pic.cb.stride = 1;
    assert!(conv.to_srgb8(&pic, &mut out).is_err());
    let mut pic = u.picture();
    pic.width = 0;
    assert!(conv.to_srgb8(&pic, &mut out).is_err());
    assert!(conv.to_srgb8(&u.picture(), &mut out[..15]).is_err(), "output too small");
    let mut pic = u.picture();
    pic.bit_depth = 10;
    assert!(conv.to_srgb8(&pic, &mut out).is_err(), "bit depth differs from the converter's");
    let bytes = [0u8; 16];
    let mut pic = u.picture();
    pic.y.samples = Samples::U8(&bytes);
    pic.bit_depth = 10;
    assert!(Converter::new(ColourDescription::BT709, 10, 4, 4).unwrap().to_srgb8(&pic, &mut out).is_err(), "8-bit samples for 10 bits");
    assert!(Converter::new(ColourDescription::BT709, 7, 4, 4).is_err());
    assert!(Converter::new(ColourDescription::BT709, 17, 4, 4).is_err());
    // a stride whose product overflows is a layout error
    let mut pic = u.picture();
    pic.y.stride = usize::MAX;
    assert!(conv.to_srgb8(&pic, &mut out).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Any samples (also above the bit depth's range), any code points: finite output, no panic.
    #[test]
    fn hostile_samples_and_code_points_never_panic(
        w in 1usize..9, h in 1usize..9, bits in 8u8..=16, c in 0usize..3,
        prim in any::<u8>(), tf in any::<u8>(), mx in any::<u8>(), full in any::<bool>(),
        seed in any::<u64>(),
    ) {
        let chroma = [Chroma::C420, Chroma::C422, Chroma::C444][c];
        let (cw, ch) = chroma.plane_size(w, h);
        let mut s = seed;
        let mut next = || { s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); (s >> 48) as u16 };
        let y: Vec<u16> = (0..w * h).map(|_| next()).collect();
        let cb: Vec<u16> = (0..cw * ch).map(|_| next()).collect();
        let cr: Vec<u16> = (0..cw * ch).map(|_| next()).collect();
        let d = ColourDescription { primaries: prim, transfer: tf, matrix: mx, full_range: full };
        let Ok(conv) = Converter::new(d, bits, w, h) else { return Ok(()) };
        let pic = Picture { width: w, height: h, bit_depth: bits, chroma,
            y: Plane { samples: Samples::U16(&y), stride: w },
            cb: Plane { samples: Samples::U16(&cb), stride: cw },
            cr: Plane { samples: Samples::U16(&cr), stride: cw } };
        let mut lin = vec![[0f32; 3]; w * h];
        conv.to_linear_rec2020(&pic, &mut lin).unwrap();
        prop_assert!(lin.iter().flatten().all(|v| v.is_finite() && *v >= 0.0));
        let mut rgba = vec![[0u8; 4]; w * h];
        conv.to_srgb8(&pic, &mut rgba).unwrap();
        prop_assert!(rgba.iter().all(|p| p[3] == 255));
    }
}

/// Sony XAVC S cameras tag their clips xvYCC (transfer 11, IEC 61966-2-4): the BT.709 curve, with
/// codes beyond nominal black and white allowed. It reads as BT.709 and keeps its own code.
#[test]
fn extended_gamut_bt709_curves_read_as_bt709() {
    for code in [11, 12] {
        let desc = ColourDescription { transfer: code, ..ColourDescription::BT709 };
        let conv = Converter::new(desc, 8, 1920, 1080).unwrap();
        assert_eq!(conv.resolved().transfer, code);
        assert!(!conv.is_hdr());
        // the fast path is picked from the curve read, not from the code point (Sony XAVC writes 11)
        assert!(conv.srgb_passthrough, "transfer {code}");
        for ycc in [[16, 128, 128], [126, 128, 128], [235, 128, 128], [81, 90, 240]] {
            assert_eq!(srgb(desc, 8, ycc), srgb(ColourDescription::BT709, 8, ycc), "transfer {code}: {ycc:?}");
        }
    }
}

/// HLG is shown as its scene light, peak at SDR white (as macOS renders HLG video for an SDR
/// screen, measured on a 16-step HLG chart): reference white (75 % signal) is a light grey, the
/// highlights above it keep their steps instead of piling into white.
#[test]
fn hlg_shows_its_scene_light_with_highlights_kept() {
    let hlg = ColourDescription { primaries: 9, transfer: 18, matrix: 9, full_range: false };
    let code = |signal: f32| (64.0 + 876.0 * signal).round() as u16;
    let at = |signal: f32| linear(hlg, 10, [code(signal), 512, 512])[1];
    // macOS (Quick Look): 0.258 at 73 % signal, 0.048 at 33 %, 0.716 at 93 %, white at peak
    let reference = at(0.75);
    assert!((0.22..=0.31).contains(&reference), "reference white {reference}");
    assert!((0.03..=0.06).contains(&at(1.0 / 3.0)), "{}", at(1.0 / 3.0));
    assert!((0.6..=0.8).contains(&at(14.0 / 15.0)), "{}", at(14.0 / 15.0));
    assert!(at(1.0) > 0.97, "peak {}", at(1.0));
    // each step above reference white still brightens
    let steps: Vec<f32> = (11..=15).map(|k| at(k as f32 / 15.0)).collect();
    assert!(steps.windows(2).all(|w| w[1] > w[0] + 0.05), "{steps:?}");
}

/// The per-pixel converter as it was before the row-at-a-time rewrite, kept as the reference the
/// fast path must match bit for bit.
fn plane16(s: &[u16], stride: usize) -> Plane<'_> {
    Plane { samples: Samples::U16(s), stride }
}

fn plane8(s: &[u8], stride: usize) -> Plane<'_> {
    Plane { samples: Samples::U8(s), stride }
}

mod reference {
    use super::super::*;

    pub fn srgb8(c: &Converter, pic: &Picture<'_>) -> Vec<[u8; 4]> {
        let enc = srgb_encode_lut();
        let mut out = vec![[0u8; 4]; pic.width * pic.height];
        for (y, row) in out.chunks_mut(pic.width).enumerate() {
            pixels(c, pic, y, Output::Srgb8, |x, rgb| {
                row[x] = if c.srgb_passthrough {
                    [unit_u8(rgb[0]), unit_u8(rgb[1]), unit_u8(rgb[2]), 255]
                } else {
                    [enc_u8(enc, rgb[0]), enc_u8(enc, rgb[1]), enc_u8(enc, rgb[2]), 255]
                };
            });
        }
        out
    }

    pub fn linear(c: &Converter, pic: &Picture<'_>) -> Vec<[f32; 3]> {
        let mut out = vec![[0f32; 3]; pic.width * pic.height];
        for (y, row) in out.chunks_mut(pic.width).enumerate() {
            pixels(c, pic, y, Output::Linear2020, |x, rgb| row[x] = rgb);
        }
        out
    }

    fn pixels(c: &Converter, pic: &Picture<'_>, y: usize, output: Output, mut put: impl FnMut(usize, [f32; 3])) {
        let w = pic.width;
        let (mut cb, mut cr) = (vec![0f32; w], vec![0f32; w]);
        chroma_row(c, pic, &pic.cb, y, &mut cb);
        chroma_row(c, pic, &pic.cr, y, &mut cr);
        let ymax = c.y_lut.len() - 1;
        for x in 0..w {
            let luma = c.y_lut[pic.y.samples.at(y * pic.y.stride + x).min(ymax)];
            let s = rgb_signal(c, luma, cb[x], cr[x]);
            put(x, finish(c, s, output));
        }
    }

    fn chroma_row(c: &Converter, pic: &Picture<'_>, plane: &Plane<'_>, y: usize, out: &mut [f32]) {
        let (sx, sy) = pic.chroma.shifts();
        let (cw, ch) = pic.chroma.plane_size(pic.width, pic.height);
        let lut = if c.matrix == Matrix::Identity { &c.y_lut } else { &c.c_lut };
        let cmax = lut.len() - 1;
        let at = |cx: usize, cy: usize| lut[plane.samples.at(cy.min(ch - 1) * plane.stride + cx.min(cw - 1)).min(cmax)];
        let (r0, r1, w1) = if sy == 0 {
            (y, y, 0.0)
        } else {
            let i = y >> 1;
            if y & 1 == 0 { (i, i.saturating_sub(1), 0.25) } else { (i, i + 1, 0.25) }
        };
        let sample = |cx: usize| at(cx, r0) * (1.0 - w1) + at(cx, r1) * w1;
        for (x, o) in out.iter_mut().enumerate() {
            *o = if sx == 0 {
                sample(x)
            } else if x & 1 == 0 {
                sample(x >> 1)
            } else {
                (sample(x >> 1) + sample((x >> 1) + 1)) * 0.5
            };
        }
    }

    fn rgb_signal(c: &Converter, y: f32, cb: f32, cr: f32) -> [f32; 3] {
        match c.matrix {
            Matrix::Kr { kr, kb } => {
                let kg = 1.0 - kr - kb;
                let r = y + 2.0 * (1.0 - kr) * cr;
                let b = y + 2.0 * (1.0 - kb) * cb;
                let g = if kg.abs() > 1e-6 { (y - kr * r - kb * b) / kg } else { y };
                [r, g, b]
            }
            Matrix::Identity => [cr, y, cb],
            Matrix::YCgCo => {
                let t = y - cb;
                [t + cr, y + cb, t - cr]
            }
        }
    }

    fn finish(c: &Converter, s: [f32; 3], output: Output) -> [f32; 3] {
        if output == Output::Srgb8 && c.srgb_passthrough {
            return s;
        }
        let lin = c.linearise(s);
        match output {
            Output::Srgb8 => apply(&c.to_srgb, lin),
            Output::Linear2020 => apply(&c.to_2020, lin).map(|v| v.max(0.0)),
        }
    }
}

/// Noise pictures (8 to 16 bits, every chroma layout, padded strides, odd sizes, samples above the
/// bit depth's range) convert exactly as the per-pixel reference converts them, for every matrix,
/// transfer and range.
#[test]
fn the_row_converter_matches_the_per_pixel_reference_exactly() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut checked = 0;
    for &(w, h) in &[(1, 1), (2, 3), (37, 9), (64, 17)] {
        for chroma in [Chroma::C420, Chroma::C422, Chroma::C444] {
            for bits in [8u8, 10, 12, 16] {
                let (cw, ch) = chroma.plane_size(w, h);
                let (ys, cs) = (w + 3, cw + 2);
                let max = (1u64 << bits) - 1;
                // mostly in range, some above it (hostile streams)
                let mut sample = |_| {
                    let v = next();
                    (if v % 17 == 0 { v >> 40 } else { (v >> 20) % (max + 1) }) as u16
                };
                let y16: Vec<u16> = (0..ys * h).map(&mut sample).collect();
                let cb16: Vec<u16> = (0..cs * ch).map(&mut sample).collect();
                let cr16: Vec<u16> = (0..cs * ch).map(&mut sample).collect();
                let narrow = |v: &[u16]| v.iter().map(|&s| s as u8).collect::<Vec<u8>>();
                let (y8, cb8, cr8) = (narrow(&y16), narrow(&cb16), narrow(&cr16));
                for eight in [false, true] {
                    if eight && bits != 8 {
                        continue;
                    }
                    let pic = if eight {
                        Picture { width: w, height: h, bit_depth: bits, chroma, y: plane8(&y8, ys), cb: plane8(&cb8, cs), cr: plane8(&cr8, cs) }
                    } else {
                        Picture { width: w, height: h, bit_depth: bits, chroma, y: plane16(&y16, ys), cb: plane16(&cb16, cs), cr: plane16(&cr16, cs) }
                    };
                    for matrix in [0u8, 1, 5, 8, 9, 12] {
                        for (primaries, transfer) in [(1u8, 1u8), (1, 4), (1, 8), (9, 1), (9, 16), (9, 18), (5, 6)] {
                            for full_range in [false, true] {
                                let d = ColourDescription { primaries, transfer, matrix, full_range };
                                let conv = Converter::new(d, bits, w, h).unwrap();
                                let mut fast = vec![[0u8; 4]; w * h];
                                conv.to_srgb8(&pic, &mut fast).unwrap();
                                assert_eq!(fast, reference::srgb8(&conv, &pic), "sRGB {d:?} {bits}-bit {chroma:?} {w}x{h} u8 {eight}");
                                let mut lin = vec![[0f32; 3]; w * h];
                                conv.to_linear_rec2020(&pic, &mut lin).unwrap();
                                let bits_of = |v: &[[f32; 3]]| v.iter().flatten().map(|f| f.to_bits()).collect::<Vec<_>>();
                                assert_eq!(bits_of(&lin), bits_of(&reference::linear(&conv, &pic)), "linear {d:?} {bits}-bit {chroma:?} {w}x{h}");
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(checked > 1000, "{checked}");
}
