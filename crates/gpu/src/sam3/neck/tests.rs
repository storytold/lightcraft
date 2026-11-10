//! The neck kernels against plain loops in f64 (odd sizes, every border of the 3 × 3 convolution, tiles cut off), and
//! a whole small neck against a direct transcription of `Neck::forward`. The real necks against candle:
//! `tests/it/sam3_backbone.rs`.

use super::super::tests::{gelu, gpu, read, values, worst};
use super::*;

/// The neck's kernels on the test device; a kernel that doesn't compile is a failure.
fn neck_kernels(d: &'static Dev) -> &'static Kernels {
    kernels(d).unwrap()
}

/// PyTorch's `ConvTranspose2d(cin, cout, 2, stride 2)` on pixel rows `x` (`[side², cin]`), weight `[cin, cout, 2, 2]`:
/// `[(2 · side)², cout]`, row-major.
fn conv_transpose_ref(x: &[f64], side: usize, (cin, cout): (usize, usize), w: &[f32], b: &[f32]) -> Vec<f64> {
    let out_side = 2 * side;
    let mut y = vec![0f64; out_side * out_side * cout];
    for i in 0..side {
        for j in 0..side {
            for ky in 0..2 {
                for kx in 0..2 {
                    for co in 0..cout {
                        let mut v = b[co] as f64;
                        for ci in 0..cin {
                            v += x[(i * side + j) * cin + ci] * w[((ci * cout + co) * 2 + ky) * 2 + kx] as f64;
                        }
                        y[((2 * i + ky) * out_side + 2 * j + kx) * cout + co] = v;
                    }
                }
            }
        }
    }
    y
}

/// PyTorch's `Conv2d(cin, cout, k, padding k / 2)` on pixel rows `x` (`[side², cin]`), weight `[cout, cin, k, k]`:
/// `[side², cout]`.
pub(in crate::sam3) fn conv_ref(x: &[f64], side: usize, (cin, cout, k): (usize, usize, usize), w: &[f32], b: &[f32]) -> Vec<f64> {
    let pad = (k / 2) as isize;
    let mut y = vec![0f64; side * side * cout];
    for r in 0..side {
        for c in 0..side {
            for co in 0..cout {
                let mut v = b[co] as f64;
                for ky in 0..k {
                    for kx in 0..k {
                        let (rr, cc) = (r as isize + ky as isize - pad, c as isize + kx as isize - pad);
                        if rr < 0 || cc < 0 || rr >= side as isize || cc >= side as isize {
                            continue;
                        }
                        let at = (rr as usize * side + cc as usize) * cin;
                        for ci in 0..cin {
                            v += x[at + ci] * w[((co * cin + ci) * k + ky) * k + kx] as f64;
                        }
                    }
                }
                y[(r * side + c) * cout + co] = v;
            }
        }
    }
    y
}

pub(in crate::sam3) fn f32s(v: &[f64]) -> Vec<f32> {
    v.iter().map(|x| *x as f32).collect()
}

pub(in crate::sam3) fn f64s(v: &[f32]) -> Vec<f64> {
    v.iter().map(|x| *x as f64).collect()
}

#[test]
fn weights_are_laid_out_as_the_products_read_them() {
    // ConvTranspose2d [cin 2, cout 3, 2, 2]: value = 1000 ci + 100 co + 10 ky + kx; rows [(ky, kx, co), ci]
    let w: Vec<f32> =
        (0..2).flat_map(|ci| (0..3).flat_map(move |co| (0..4).map(move |q| (1000 * ci + 100 * co + 10 * (q / 2) + q % 2) as f32))).collect();
    let rows = transposed_rows(&w, 2, 3);
    assert_eq!(rows[(3 * 3 + 2) * 2 + 1], 1211.0);
    assert_eq!(rows[3 * 2], 1.0);
    // Conv2d [cout 2, cin 3, 3, 3]: value = 1000 co + 100 ci + 10 ky + kx; rows [cout, ky, kx, cin]
    let w: Vec<f32> =
        (0..2).flat_map(|co| (0..3).flat_map(move |ci| (0..9).map(move |t| (1000 * co + 100 * ci + 10 * (t / 3) + t % 3) as f32))).collect();
    let rows = conv_rows(&w, 2, 3, 3);
    assert_eq!(rows[(9 + 2 * 3 + 1) * 3 + 2], 1221.0);
    assert_eq!(rows[4 * 3], 11.0);
}

#[test]
fn transposed_convolutions_and_the_reorder_give_the_definition() {
    let Some(d) = gpu() else { return };
    let k = neck_kernels(d);
    // one up-sampling (odd side, cut-off tiles), and two with GELU between as in level 0
    for (case, (side, chans)) in [(5usize, vec![8usize, 6]), (3, vec![12, 8, 4]), (9, vec![4, 68])].into_iter().enumerate() {
        let x = values(side * side * chans[0], 10 + case as u64, 1.0);
        let mut want = f64s(&x);
        let mut s = side;
        let mut g = Graph::new(d);
        let mut cur = upload(d, "x", &x);
        for (j, pair) in chans.windows(2).enumerate() {
            let (cin, cout) = (pair[0], pair[1]);
            let w = values(cin * cout * 4, 20 + j as u64, 0.3);
            let b = values(cout, 30 + j as u64, 0.5);
            let gelu_after = j + 2 < chans.len();
            want = conv_transpose_ref(&want, s, (cin, cout), &w, &b);
            if gelu_after {
                want.iter_mut().for_each(|v| *v = gelu(*v));
            }
            let [bw, bb] = transposed_weights(d, "w", &w, &b, cin, cout);
            let y = buffer(d, "y", s * s * 4 * cout);
            g.matmul([&cur, &bw, &bb, &y], (s * s, 4 * cout, cin), 1, gelu_after, false, Precision::F32).unwrap();
            cur = y;
            s *= 2;
        }
        let cout = *chans.last().unwrap();
        let out = buffer(d, "out", s * s * cout);
        g.unshuffle(k, &cur, &out, cout, s, chans.len() - 1).unwrap();
        let got = read(d, g, &out, s * s * cout);
        let err = worst(&got, &f32s(&want));
        assert!(err < 1e-4, "case {case}: off by {err}");
    }
}

#[test]
fn the_3x3_convolution_gives_the_definition_at_every_border() {
    let Some(d) = gpu() else { return };
    let k = neck_kernels(d);
    // odd sides, a single pixel (all border), N not a multiple of the tile, more pixels than one tile
    for (case, (side, cin, cout)) in [(7usize, 12usize, 5usize), (1, 8, 3), (9, 4, 70), (17, 16, 64), (2, 256, 9)].into_iter().enumerate() {
        let x = values(side * side * cin, 40 + case as u64, 1.0);
        let w = values(cout * cin * 9, 50 + case as u64, 0.2);
        let b = values(cout, 60 + case as u64, 0.5);
        let [bw, bb] = conv_weights(d, "w", &w, &b, (cin, cout, 3));
        let (bx, by) = (upload(d, "x", &x), buffer(d, "y", side * side * cout));
        let mut g = Graph::new(d);
        g.conv3(k, [&bx, &bw, &bb, &by], side, (cin, cout)).unwrap();
        let got = read(d, g, &by, side * side * cout);
        let want = f32s(&conv_ref(&f64s(&x), side, (cin, cout, 3), &w, &b));
        let err = worst(&got, &want);
        assert!(err < 2e-5 * ((9 * cin) as f32).sqrt(), "case {case} ({side}² {cin} → {cout}): off by {err}");
    }
}

#[test]
fn the_neck_kernels_refuse_what_they_cannot_do() {
    let Some(d) = gpu() else { return };
    let k = neck_kernels(d);
    let b = buffer(d, "b", 16);
    let mut g = Graph::new(d);
    assert!(g.conv3(k, [&b, &b, &b, &b], 4, (6, 8)).is_err(), "channels not a multiple of 4");
    assert!(g.conv3(k, [&b, &b, &b, &b], 0, (8, 8)).is_err(), "an empty image");
    assert!(g.unshuffle(k, &b, &b, 4, 6, 2).is_err(), "a side that is not a multiple of 2^ups");
}

/// A neck level's weights in the checkpoint's layouts: the transposed convolutions (weight, bias, cin, cout),
/// `proj1` (weight, bias, cin) and `proj2`.
struct RefLevel {
    up: Vec<(Vec<f32>, Vec<f32>, usize, usize)>,
    proj1: (Vec<f32>, Vec<f32>, usize),
    proj2: (Vec<f32>, Vec<f32>),
}

/// `Neck::forward` for one level on the channels-first backbone `x`, in f64: `[fpn, side, side]`.
fn level_ref(x: &[f32], sizes: Sizes, l: &RefLevel) -> Vec<f32> {
    let Sizes { grid, hidden, fpn } = sizes;
    let mut rows: Vec<f64> = (0..grid * grid).flat_map(|p| (0..hidden).map(move |c| x[c * grid * grid + p] as f64)).collect();
    let mut side = grid;
    for (j, (w, b, cin, cout)) in l.up.iter().enumerate() {
        rows = conv_transpose_ref(&rows, side, (*cin, *cout), w, b);
        if j + 1 < l.up.len() {
            rows.iter_mut().for_each(|v| *v = gelu(*v));
        }
        side *= 2;
    }
    let (w1, b1, inter) = &l.proj1;
    let rows = conv_ref(&rows, side, (*inter, fpn, 1), w1, b1);
    let rows = conv_ref(&rows, side, (fpn, fpn, 3), &l.proj2.0, &l.proj2.1);
    (0..fpn).flat_map(|c| (0..side * side).map(move |p| (c, p))).map(|(c, p)| rows[p * fpn + c] as f32).collect()
}

#[test]
fn a_small_neck_gives_what_the_reference_neck_gives() {
    let Some(d) = gpu() else { return };
    let k = neck_kernels(d);
    let sizes = Sizes { grid: 6, hidden: 32, fpn: 8 };
    let Sizes { grid, hidden, fpn } = sizes;
    let x = values(hidden * grid * grid, 70, 1.0);
    let work = Work::new(d, sizes);
    d.queue.write_buffer(&work.a, 0, bytemuck::cast_slice(&x));
    let mut seed = 80;
    let mut next = |n: usize, scale: f32| {
        seed += 1;
        values(n, seed, scale)
    };
    // in order: level 0 turns the upload into the pixel rows the others read
    for i in 0..LEVELS {
        let chans: Vec<usize> = match i {
            0 => vec![hidden, hidden / 2, hidden / 4],
            1 => vec![hidden, hidden / 2],
            _ => vec![hidden],
        };
        let up: Vec<_> = chans.windows(2).map(|p| (next(p[0] * p[1] * 4, 0.3), next(p[1], 0.3), p[0], p[1])).collect();
        let inter = *chans.last().unwrap();
        let r = RefLevel { up, proj1: (next(fpn * inter, 0.3), next(fpn, 0.3), inter), proj2: (next(fpn * fpn * 9, 0.2), next(fpn, 0.3)) };
        let w = LevelWeights {
            up: r.up.iter().map(|(w, b, cin, cout)| transposed_weights(d, "up", w, b, *cin, *cout)).collect(),
            proj1: conv_weights(d, "proj1", &r.proj1.0, &r.proj1.1, (inter, fpn, 1)),
            proj2: conv_weights(d, "proj2", &r.proj2.0, &r.proj2.1, (fpn, fpn, 3)),
        };
        let level = record_level(d, k, &work, &w, i, sizes).unwrap();
        let got = level.recorded.run("a test", d, &level.out, &work.staging, level.len).unwrap();
        let want = level_ref(&x, sizes, &r);
        let err = worst(&got, &want);
        assert!(err < 1e-4, "level {i}: off by {err}");
    }
}
