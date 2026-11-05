//! The mask decoder's kernels against plain loops in f64: the up-sampling with the channels-first level added (odd
//! sides, cut-off workgroups), GroupNorm + ReLU (chunks cut off, a large mean, group sizes), and one whole pixel
//! decoder stage (up-sampling → 3 × 3 convolution at every border → GroupNorm → ReLU) on small images. The real
//! decoder against candle: `tests/it/sam3_mask_decoder.rs`.

use super::super::neck::tests::{conv_ref, f32s, f64s};
use super::super::tests::{gpu, read, values, worst};
use super::*;

fn pixel_kernels(d: &'static Dev) -> &'static Kernels {
    kernels(d).unwrap()
}

/// Nearest up-sampling of the rows `x` (`[from², c]`) to `to²`, plus the channels-first `skip` (`[c, to, to]`).
fn upsample_add_ref(x: &[f64], skip: &[f32], c: usize, (from, to): (usize, usize)) -> Vec<f64> {
    let mut y = vec![0f64; to * to * c];
    for r in 0..to {
        for col in 0..to {
            // candle's upsample_nearest2d: floor(i · from / to)
            let (sr, sc) = ((r as f64 * from as f64 / to as f64) as usize, (col as f64 * from as f64 / to as f64) as usize);
            for ch in 0..c {
                y[(r * to + col) * c + ch] = x[(sr * from + sc) * c + ch] + skip[ch * to * to + r * to + col] as f64;
            }
        }
    }
    y
}

/// GroupNorm (centred two-pass variance, as candle's) and ReLU on the rows `x` (`[pixels, c]`).
fn group_norm_relu_ref(x: &[f64], c: usize, groups: usize, gamma: &[f32], beta: &[f32], eps: f64) -> Vec<f64> {
    let pixels = x.len() / c;
    let per = c / groups;
    let mut y = x.to_vec();
    for g in 0..groups {
        let at = |p: usize, i: usize| x[p * c + g * per + i];
        let n = (pixels * per) as f64;
        let mean = (0..pixels).flat_map(|p| (0..per).map(move |i| (p, i))).map(|(p, i)| at(p, i)).sum::<f64>() / n;
        let var = (0..pixels).flat_map(|p| (0..per).map(move |i| (p, i))).map(|(p, i)| (at(p, i) - mean).powi(2)).sum::<f64>() / n;
        for p in 0..pixels {
            for i in 0..per {
                let ch = g * per + i;
                let v = (x[p * c + ch] - mean) / (var + eps).sqrt() * gamma[ch] as f64 + beta[ch] as f64;
                y[p * c + ch] = v.max(0.0);
            }
        }
    }
    y
}

#[test]
fn up_sampling_adds_the_channels_first_level() {
    let Some(d) = gpu() else { return };
    let k = pixel_kernels(d);
    // the decoder's doublings, an odd one, and channels that leave the last workgroup part empty
    for (case, (from, to, c)) in [(3usize, 6usize, 4usize), (5, 10, 12), (4, 7, 3), (1, 2, 256)].into_iter().enumerate() {
        let x = values(from * from * c, 100 + case as u64, 1.0);
        let skip = values(c * to * to, 110 + case as u64, 1.0);
        let (bx, bs, by) = (upload(d, "x", &x), upload(d, "skip", &skip), buffer(d, "y", to * to * c));
        let mut g = Graph::new(d);
        g.upsample_add(k, [&bx, &bs, &by], c, (from, to)).unwrap();
        let got = read(d, g, &by, to * to * c);
        let err = worst(&got, &f32s(&upsample_add_ref(&f64s(&x), &skip, c, (from, to))));
        assert!(err < 1e-6, "case {case}: off by {err}");
    }
}

#[test]
fn group_norm_and_relu_give_the_definition() {
    let Some(d) = gpu() else { return };
    let k = pixel_kernels(d);
    // one chunk cut short; several with the last cut short; the decoder's 256 channels in 8 groups over chunks of
    // 256 pixels, around a mean far from 0 (where E[x²] − E[x]² in f32 would lose the variance)
    for (case, (pixels, c, groups, offset)) in
        [(37usize, 16usize, 4usize, 0f32), (700, 24, 3, 0.0), (1100, 256, 8, 0.0), (600, 256, 8, 50.0), (5, 8, 1, 0.0)].into_iter().enumerate()
    {
        let x: Vec<f32> = values(pixels * c, 120 + case as u64, 2.0).iter().map(|v| v + offset).collect();
        let gamma = values(c, 130 + case as u64, 1.5);
        let beta = values(c, 140 + case as u64, 0.5);
        // normed in place, then read back: a buffer that can be copied from
        let bx = buffer(d, "x", pixels * c);
        d.queue.write_buffer(&bx, 0, bytemuck::cast_slice(&x));
        let (bg, bb) = (upload(d, "gamma", &gamma), upload(d, "beta", &beta));
        let (partials, stats) = (buffer(d, "partials", partials_len(pixels, groups)), buffer(d, "stats", 2 * groups));
        let mut g = Graph::new(d);
        g.group_norm_relu(k, [&bx, &bg, &bb], [&partials, &stats], (pixels, c, groups), EPS).unwrap();
        let got = read(d, g, &bx, pixels * c);
        let want = f32s(&group_norm_relu_ref(&f64s(&x), c, groups, &gamma, &beta, f64::from(EPS)));
        let err = worst(&got, &want);
        assert!(err < 2e-5, "case {case} ({pixels} pixels, {c} channels, {groups} groups): off by {err}");
        assert!(got.iter().all(|v| *v >= 0.0), "case {case}: ReLU");
    }
}

#[test]
fn a_small_pixel_decoder_stage_gives_the_definition() {
    let Some(d) = gpu() else { return };
    let (k, kn) = (pixel_kernels(d), neck::kernels(d).unwrap());
    // two stages as in the decoder (3² → 6² → 12²), 16 channels in 4 groups
    let (c, groups) = (16usize, 4usize);
    let x = values(3 * 3 * c, 150, 1.0);
    let mut want = f64s(&x);
    let mut cur = upload(d, "x", &x);
    let mut g = Graph::new(d);
    let mut side = 3;
    for stage in 0..2u64 {
        let skip = values(c * 4 * side * side, 160 + stage, 1.0);
        let (w, b) = (values(c * c * 9, 170 + stage, 0.3), values(c, 180 + stage, 0.3));
        let (gamma, beta) = (values(c, 190 + stage, 1.0), values(c, 200 + stage, 0.3));
        want = upsample_add_ref(&want, &skip, c, (side, 2 * side));
        side *= 2;
        want = conv_ref(&want, side, (c, c, 3), &w, &b);
        want = group_norm_relu_ref(&want, c, groups, &gamma, &beta, f64::from(EPS));

        let up = buffer(d, "up", side * side * c);
        let out = buffer(d, "out", side * side * c);
        let [bw, bb] = conv_weights(d, "conv", &w, &b, (c, c, 3));
        let (bs, bg, bt) = (upload(d, "skip", &skip), upload(d, "gamma", &gamma), upload(d, "beta", &beta));
        let (partials, stats) = (buffer(d, "partials", partials_len(side * side, groups)), buffer(d, "stats", 2 * groups));
        g.upsample_add(k, [&cur, &bs, &up], c, (side / 2, side)).unwrap();
        g.conv3(kn, [&up, &bw, &bb, &out], side, (c, c)).unwrap();
        g.group_norm_relu(k, [&out, &bg, &bt], [&partials, &stats], (side * side, c, groups), EPS).unwrap();
        cur = out;
    }
    let got = read(d, g, &cur, side * side * c);
    let err = worst(&got, &f32s(&want));
    assert!(err < 1e-4, "off by {err}");
}

#[test]
fn the_mask_decoder_kernels_refuse_what_they_cannot_do() {
    let Some(d) = gpu() else { return };
    let k = pixel_kernels(d);
    let b = buffer(d, "b", 16);
    let mut g = Graph::new(d);
    assert!(g.upsample_add(k, [&b, &b, &b], 4, (0, 2)).is_err(), "an empty image");
    assert!(g.upsample_add(k, [&b, &b, &b], 4, (4, 2)).is_err(), "down-sampling");
    assert!(g.group_norm_relu(k, [&b, &b, &b], [&b, &b], (4, 12, 8), EPS).is_err(), "channels not a multiple of the groups");
    assert!(g.group_norm_relu(k, [&b, &b, &b], [&b, &b], (4, 512, 8), EPS).is_err(), "more channels than threads");
    assert!(g.group_norm_relu(k, [&b, &b, &b], [&b, &b], (0, 8, 8), EPS).is_err(), "no pixels");
}

#[test]
fn the_decoder_refuses_inputs_of_the_wrong_size() {
    let Some(d) = gpu() else { return };
    // checked before the checkpoint is read: an empty reader is never touched
    let dir = std::env::temp_dir().join(format!("lc-sam3-pixel-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut st = 2u64.to_le_bytes().to_vec();
    st.extend_from_slice(b"{}");
    std::fs::write(dir.join("model.safetensors"), st).unwrap();
    std::fs::write(dir.join("vocab.json"), "{}").unwrap();
    std::fs::write(dir.join("merges.txt"), "").unwrap();
    let m = MaskDecoder::new(WeightReader::open(&dir).unwrap());
    let (mem, text, valid) = (vec![0f32; GRID * GRID * WIDTH], vec![0f32; TEXT * WIDTH], vec![true; TEXT]);
    let (l1, l0) = (vec![0f32; WIDTH * 144 * 144], vec![0f32; WIDTH * SIDE * SIDE]);
    let rows = vec![0f32; 3 * WIDTH];
    assert!(m.run(d, &mem[1..], &text, &valid, [&l1, &l0], &rows, &[0.0; 3]).is_err_and(|e| e.contains("expected")));
    assert!(m.run(d, &mem, &text, &valid, [&l0, &l1], &rows, &[0.0; 3]).is_err_and(|e| e.contains("expected")));
    assert!(m.run(d, &mem, &text, &valid, [&l1, &l0], &rows, &[0.0; 2]).is_err_and(|e| e.contains("row values")));
    assert!(m.run(d, &mem, &text, &valid, [&l1, &l0], &[], &[]).is_err_and(|e| e.contains("row values")));
    // the right sizes reach the checkpoint, which has no decoder
    assert!(m.run(d, &mem, &text, &valid, [&l1, &l0], &rows, &[0.0; 3]).is_err_and(|e| e.contains("mask_decoder")));
    assert!(m.bytes().is_none());
    std::fs::remove_dir_all(dir).unwrap();
}
