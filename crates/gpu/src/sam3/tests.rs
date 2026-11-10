//! The SAM 3 kernels against plain loops on small shapes (tiles cut off at the edges, several windows). The whole
//! backbone against candle: `tests/it/sam3_backbone.rs`.

use super::*;

pub(super) fn gpu() -> Option<&'static Dev> {
    match dev() {
        Ok(d) => Some(d),
        // a kernel that doesn't compile is a failure, not a missing GPU
        Err(e) if e.contains("did not compile") => panic!("{e}"),
        Err(e) => {
            lightcraft_testkit::skip!(Gpu, "no GPU for the SAM 3 kernels ({e})");
            None
        }
    }
}

/// Deterministic values in −scale..scale.
pub(super) fn values(n: usize, seed: u64, scale: f32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * scale
        })
        .collect()
}

pub(super) fn read(d: &'static Dev, g: Graph, out: &wgpu::Buffer, len: usize) -> Vec<f32> {
    let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (len * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    g.record().run("a test", d, out, &staging, len).unwrap()
}

pub(super) fn worst(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len());
    got.iter().zip(want).fold(0f32, |m, (a, b)| m.max((a - b).abs()))
}

/// GELU, erf form (candle's `gelu_erf`), with a high-precision erf: the kernel's approximation is what is checked.
pub(super) fn gelu(v: f64) -> f64 {
    0.5 * v * (1.0 + erf(v / std::f64::consts::SQRT_2))
}

/// erf to ~1e-15 (Taylor series near 0, continued fraction of erfc further out).
fn erf(x: f64) -> f64 {
    let a = x.abs();
    let r = if a < 2.5 {
        let (mut term, mut sum, mut n) = (a, a, 0.0);
        while term.abs() > 1e-17 * sum.abs() {
            n += 1.0;
            term *= -a * a / n;
            sum += term / (2.0 * n + 1.0);
        }
        sum * 2.0 / std::f64::consts::PI.sqrt()
    } else {
        // erfc(a) = exp(−a²)/√π · 1/(a + 1/2/(a + 1/(a + 3/2/(a + 2/(a + …)))))
        let mut f = a;
        for k in (1..60).rev() {
            f = a + (k as f64 / 2.0) / f;
        }
        1.0 - (-a * a).exp() / std::f64::consts::PI.sqrt() / f
    };
    if x < 0.0 { -r } else { r }
}

#[test]
fn erf_reference_is_right() {
    for (x, want) in [(0.5, 0.520_499_877_813_046_5), (1.0, 0.842_700_792_949_714_9), (2.0, 0.995_322_265_018_952_7), (3.0, 0.999_977_909_503_001_4)]
    {
        assert!((erf(x) - want).abs() < 1e-13, "erf({x}) = {}", erf(x));
        assert!((erf(-x) + want).abs() < 1e-13);
    }
}

/// `y = act(x · wᵀ + bias[row % rows])` (+ y0) in f64.
#[allow(clippy::too_many_arguments)]
fn matmul_ref(x: &[f32], w: &[f32], bias: &[f32], y0: Option<&[f32]>, (m, n, k): (usize, usize, usize), rows: usize, act: bool) -> Vec<f32> {
    let mut out = vec![0f32; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut v = bias[(i % rows) * n + j] as f64;
            for t in 0..k {
                v += x[i * k + t] as f64 * w[j * k + t] as f64;
            }
            if act {
                v = gelu(v);
            }
            if let Some(y0) = y0 {
                v += y0[i * n + j] as f64;
            }
            out[i * n + j] = v as f32;
        }
    }
    out
}

#[test]
fn the_matrix_product_gives_what_the_definition_gives() {
    let Some(d) = gpu() else { return };
    // whole tiles, cut-off tiles in every direction, K not a multiple of the tile's 16, a position-style bias table
    for (case, (m, n, k, rows, act, residual)) in [
        (256, 128, 64, 1, false, false),
        (200, 136, 52, 1, false, false),
        (130, 300, 588, 7, false, false),
        (77, 260, 96, 1, true, false),
        (150, 129, 132, 1, false, true),
        (5, 3, 4, 1, true, true),
    ]
    .into_iter()
    .enumerate()
    {
        let x = values(m * k, case as u64, 1.0);
        let w = values(n * k, 100 + case as u64, 0.2);
        let bias = values(rows * n, 200 + case as u64, 0.5);
        let y0 = values(m * n, 300 + case as u64, 1.0);
        let (bx, bw, bb) = (upload(d, "x", &x), upload(d, "w", &w), upload(d, "b", &bias));
        let by = buffer(d, "y", m * n);
        d.queue.write_buffer(&by, 0, bytemuck::cast_slice(&y0));
        let mut g = Graph::new(d);
        g.matmul([&bx, &bw, &bb, &by], (m, n, k), rows, act, residual, Precision::F32).unwrap();
        let got = read(d, g, &by, m * n);
        let want = matmul_ref(&x, &w, &bias, residual.then_some(&y0[..]), (m, n, k), rows, act);
        let err = worst(&got, &want);
        assert!(err < 2e-5 * (k as f32).sqrt().max(1.0), "case {case} ({m}×{n}×{k}): off by {err}");
    }
}

#[test]
fn the_f16_matrix_product_uses_the_rounded_weights() {
    let Some(d) = gpu() else { return };
    let (m, n, k) = (140, 200, 96);
    let x = values(m * k, 1, 1.0);
    let w = values(n * k, 2, 0.2);
    let bias = values(n, 3, 0.5);
    let rounded: Vec<f32> = w.iter().map(|v| f16_value(f16_bits(*v))).collect();
    let (bx, bw, bb) = (upload(d, "x", &x), upload(d, "w", &pack_f16(&w)), upload(d, "b", &bias));
    let by = buffer(d, "y", m * n);
    let mut g = Graph::new(d);
    g.matmul([&bx, &bw, &bb, &by], (m, n, k), 1, true, false, Precision::F16).unwrap();
    let got = read(d, g, &by, m * n);
    let want = matmul_ref(&x, &rounded, &bias, None, (m, n, k), 1, true);
    let err = worst(&got, &want);
    assert!(err < 1e-4, "off by {err}");
    // K must be a multiple of the f16 tile
    assert!(Graph::new(d).matmul([&bx, &bw, &bb, &by], (m, n, 40), 1, false, false, Precision::F16).is_err());
}

/// The value of half `h` (test reference).
fn f16_value(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let (e, m) = (((h >> 10) & 0x1f) as i32, (h & 0x3ff) as f32);
    sign * match e {
        0 => m * 2f32.powi(-24),
        0x1f => {
            if m == 0.0 {
                f32::INFINITY
            } else {
                f32::NAN
            }
        }
        _ => (1.0 + m / 1024.0) * 2f32.powi(e - 15),
    }
}

#[test]
fn halves_round_to_nearest_even() {
    for (v, h) in [
        (0.0f32, 0x0000u16),
        (-0.0, 0x8000),
        (1.0, 0x3c00),
        (-2.0, 0xc000),
        (65504.0, 0x7bff),
        (65520.0, 0x7c00),
        (1e10, 0x7c00),
        (f32::INFINITY, 0x7c00),
        (6.103_515_6e-5, 0x0400),
        (5.960_464_5e-8, 0x0001),
        (2.0e-8, 0x0000),
        (0.333_333_34, 0x3555),
        // exactly between 1 and the next half up (1 + 2⁻¹¹): ties to even (down), just above: up
        (1.000_488_3, 0x3c00),
        (1.000_488_4, 0x3c01),
    ] {
        assert_eq!(f16_bits(v), h, "{v}");
    }
    assert!(f16_value(f16_bits(f32::NAN)).is_nan());
    // every half survives the round trip
    for h in (0u16..0x7c00).step_by(7) {
        assert_eq!(f16_bits(f16_value(h)), h);
    }
}

#[test]
fn layer_norm_gives_what_the_definition_gives() {
    let Some(d) = gpu() else { return };
    for (rows, c) in [(7, 1024), (3, 300), (2, 4)] {
        let x: Vec<f32> = values(rows * c, c as u64, 3.0).iter().enumerate().map(|(i, v)| v + (i / c) as f32).collect();
        let (gamma, beta) = (values(c, 5, 1.0), values(c, 6, 0.5));
        let (bx, bg, bb, by) = (upload(d, "x", &x), upload(d, "g", &gamma), upload(d, "b", &beta), buffer(d, "y", rows * c));
        let mut g = Graph::new(d);
        g.layernorm([&bx, &bg, &bb, &by], rows, c).unwrap();
        let got = read(d, g, &by, rows * c);
        let mut want = vec![0f32; rows * c];
        for r in 0..rows {
            let row = &x[r * c..(r + 1) * c];
            let mean = row.iter().map(|v| *v as f64).sum::<f64>() / c as f64;
            let var = row.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / c as f64;
            for i in 0..c {
                want[r * c + i] = ((row[i] as f64 - mean) / (var + EPS as f64).sqrt() * gamma[i] as f64 + beta[i] as f64) as f32;
            }
        }
        let err = worst(&got, &want);
        assert!(err < 2e-5, "{rows}×{c}: off by {err}");
    }
}

/// `vit::apply_rope` (x·cos + rotate_pairwise(x)·sin) with the tables of `vit::rope_tables` for token `t`.
fn rope_ref(x: &[f32], table: &[f32], t: usize) -> Vec<f32> {
    // the candle tables: [head_dim] cos and sin, each pair's value duplicated
    let (mut cos, mut sin) = (Vec::new(), Vec::new());
    for i in 0..32 {
        let (c, s) = (table[(t * 32 + i) * 2], table[(t * 32 + i) * 2 + 1]);
        cos.extend([c, c]);
        sin.extend([s, s]);
    }
    let mut out = vec![0f32; x.len()];
    for (h, head) in x.chunks(HEAD_DIM).enumerate() {
        for i in 0..HEAD_DIM {
            let rot = if i % 2 == 0 { -head[i + 1] } else { head[i - 1] };
            out[h * HEAD_DIM + i] = head[i] * cos[i] + rot * sin[i];
        }
    }
    out
}

#[test]
fn rope_tables_follow_the_candle_port() {
    // window table: plain row-major 24 × 24 positions; the global one lists the 72 × 72 grid in window order
    let win = rope_table(WINDOW, WINDOW, 1.0);
    let global = rope_table(GRID, WINDOW, WINDOW as f64 / GRID as f64);
    assert_eq!(win.len(), WINDOW * WINDOW * 64);
    assert_eq!(global.len(), TOKENS * 64);
    // token 25 of a window is at (1, 1): x frequency 0 rotates by 1, y frequency 0 by 1
    assert!((win[(25 * 32) * 2] - 1f32.cos()).abs() < 1e-7);
    assert!((win[(25 * 32 + 16) * 2 + 1] - 1f32.sin()).abs() < 1e-7);
    // window-order token 576 (the second window's first) is cell (0, 24): x = 24 · (24 / 72) = 8
    assert_eq!(window_cell(576, GRID, WINDOW), (0, 24));
    assert!((global[(576 * 32) * 2] - 8f32.cos()).abs() < 1e-6);
    assert!((global[(576 * 32 + 16) * 2] - 1.0).abs() < 1e-7, "y = 0");
    // every cell exactly once
    let mut seen = vec![false; TOKENS];
    for t in 0..TOKENS {
        let (r, c) = window_cell(t, GRID, WINDOW);
        assert!(!seen[r * GRID + c]);
        seen[r * GRID + c] = true;
    }
}

#[test]
fn rope_rotates_q_and_k_and_scales_q() {
    let Some(d) = gpu() else { return };
    let (tokens, hidden, rows) = (40, 128, 16);
    let stride = 3 * hidden;
    let qkv = values(tokens * stride, 9, 1.0);
    let table = rope_table(4, 4, 1.0);
    let (bq, bt) = (buffer(d, "qkv", tokens * stride), upload(d, "table", &table));
    d.queue.write_buffer(&bq, 0, bytemuck::cast_slice(&qkv));
    let mut g = Graph::new(d);
    g.rope(&bq, &bt, (tokens, stride, hidden), rows, 0.125).unwrap();
    let got = read(d, g, &bq, tokens * stride);
    let mut want = qkv.clone();
    for t in 0..tokens {
        let row = &qkv[t * stride..(t + 1) * stride];
        let q: Vec<f32> = rope_ref(&row[..hidden], &table, t % rows).iter().map(|v| v * 0.125).collect();
        let k = rope_ref(&row[hidden..2 * hidden], &table, t % rows);
        want[t * stride..t * stride + hidden].copy_from_slice(&q);
        want[t * stride + hidden..t * stride + 2 * hidden].copy_from_slice(&k);
    }
    let err = worst(&got, &want);
    assert!(err < 1e-6, "off by {err}");
}

/// Softmax attention of each query over its window's `span` keys (q already scaled), f64.
fn attention_ref(qkv: &[f32], (tokens, stride, hidden): (usize, usize, usize), span: usize) -> Vec<f32> {
    let mut out = vec![0f32; tokens * hidden];
    for h in 0..hidden / HEAD_DIM {
        for t in 0..tokens {
            let first = t / span * span;
            let q = &qkv[t * stride + h * HEAD_DIM..][..HEAD_DIM];
            let s: Vec<f64> = (first..first + span)
                .map(|j| q.iter().zip(&qkv[j * stride + hidden + h * HEAD_DIM..][..HEAD_DIM]).map(|(a, b)| *a as f64 * *b as f64).sum())
                .collect();
            let top = s.iter().fold(f64::MIN, |m, v| m.max(*v));
            let e: Vec<f64> = s.iter().map(|v| (v - top).exp()).collect();
            let sum: f64 = e.iter().sum();
            for c in 0..HEAD_DIM {
                let v: f64 = (first..first + span).zip(&e).map(|(j, w)| w * qkv[j * stride + 2 * hidden + h * HEAD_DIM + c] as f64).sum();
                out[t * hidden + h * HEAD_DIM + c] = (v / sum) as f32;
            }
        }
    }
    out
}

#[test]
fn windowed_and_global_attention_give_the_softmax_of_the_definition() {
    let Some(d) = gpu() else { return };
    let (tokens, hidden) = (384, 128);
    let stride = 3 * hidden;
    // large scores: the online maximum must keep exp() in range
    let qkv = values(tokens * stride, 11, 2.5);
    let bq = upload(d, "qkv", &qkv);
    for span in [64, 128, 384] {
        let out = buffer(d, "out", tokens * hidden);
        let mut g = Graph::new(d);
        g.attention(&bq, &out, (tokens, stride, hidden), span).unwrap();
        let got = read(d, g, &out, tokens * hidden);
        let want = attention_ref(&qkv, (tokens, stride, hidden), span);
        let err = worst(&got, &want);
        // relative to the outputs' size: Metal's exp() is a little less exact than Vulkan's
        // (span 128 is off by 2.04e-5 on an M4 Pro, 8e-6 of the largest output)
        let scale = want.iter().fold(1f32, |m, v| m.max(v.abs()));
        assert!(err < 2e-5 * scale, "span {span}: off by {err} (scale {scale})");
    }
    // windows must hold whole workgroups of queries
    assert!(Graph::new(d).attention(&bq, &bq, (tokens, stride, hidden), 96).is_err());
}

#[test]
fn patches_go_in_and_out_of_window_order() {
    let Some(d) = gpu() else { return };
    // 3 × 56 × 56 pixels, 14-pixel patches: a 4 × 4 grid of 2 × 2 windows
    let (side, patch, window) = (56, 14, 2);
    let grid = side / patch;
    let k = 3 * patch * patch;
    let pixels = values(3 * side * side, 12, 1.0);
    let bp = upload(d, "pixels", &pixels);
    let cols = buffer(d, "cols", grid * grid * k);
    let mut g = Graph::new(d);
    g.im2col(&bp, &cols, (side, patch, 3), window).unwrap();
    let got = read(d, g, &cols, grid * grid * k);
    for t in 0..grid * grid {
        let (row, col) = window_cell(t, grid, window);
        for ch in 0..3 {
            for ky in 0..patch {
                for kx in 0..patch {
                    let want = pixels[(ch * side + row * patch + ky) * side + col * patch + kx];
                    assert_eq!(got[t * k + (ch * patch + ky) * patch + kx], want, "token {t}");
                }
            }
        }
    }
    // and back: channels-first on the grid
    let channels = 8;
    let tokens = values(grid * grid * channels, 13, 1.0);
    let bt = upload(d, "tokens", &tokens);
    let out = buffer(d, "chw", grid * grid * channels);
    let mut g = Graph::new(d);
    g.channels_first(&bt, &out, channels, grid, window).unwrap();
    let got = read(d, g, &out, grid * grid * channels);
    for t in 0..grid * grid {
        let (row, col) = window_cell(t, grid, window);
        for ch in 0..channels {
            assert_eq!(got[(ch * grid + row) * grid + col], tokens[t * channels + ch]);
        }
    }
}

#[test]
fn the_kernel_source_is_written_out_for_both_weight_types() {
    for f16 in [false, true] {
        let src = matmul_source(TILE, f16);
        assert!(!src.contains("{{"), "a mark left");
        assert_eq!(src.contains("fn load_b16"), f16);
        assert_eq!(src.contains("fn load_b("), !f16);
        assert_eq!(src.matches("= fma(").count() as u32, 4 * TILE.rows * TILE.cols);
    }
}

/// Kernel timings at the backbone's shapes:
/// `cargo test --release -p lightcraft-gpu --features sam --lib sam3::tests::timings -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark: needs a GPU"]
fn timings() {
    let Some(d) = gpu() else { return };
    let reps = 20;
    let time = |name: &str, flops: f64, build: &dyn Fn(&mut Graph)| {
        let out = buffer(d, "out", 4);
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut g = Graph::new(d);
        for _ in 0..reps {
            build(&mut g);
        }
        let rec = g.record();
        rec.run("a test", d, &out, &staging, 4).unwrap();
        let t = std::time::Instant::now();
        rec.run("a test", d, &out, &staging, 4).unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3 / reps as f64;
        eprintln!("{name:>34}: {ms:7.3} ms  {:6.2} TFLOP/s", flops / ms / 1e9);
    };
    let x = buffer(d, "x", TOKENS * MLP);
    let y = buffer(d, "y", TOKENS * MLP);
    let w = buffer(d, "w", MLP * HIDDEN);
    let b = buffer(d, "b", MLP);
    let w16 = buffer(d, "w16", MLP * HIDDEN / 2);
    for (n, k) in [(3 * HIDDEN, HIDDEN), (HIDDEN, HIDDEN), (MLP, HIDDEN), (HIDDEN, MLP)] {
        let flops = 2.0 * (TOKENS * n * k) as f64;
        time(&format!("matmul {TOKENS}x{n}x{k}"), flops, &|g| g.matmul([&x, &w, &b, &y], (TOKENS, n, k), 1, false, false, Precision::F32).unwrap());
        time(&format!("matmul f16 {TOKENS}x{n}x{k}"), flops, &|g| {
            g.matmul([&x, &w16, &b, &y], (TOKENS, n, k), 1, false, false, Precision::F16).unwrap()
        });
    }
    // other blockings (`LC_SAM3_TILES=rows,cols,bk;…`)
    for t in std::env::var("LC_SAM3_TILES").unwrap_or_default().split(';').filter(|t| !t.is_empty()) {
        let v: Vec<u32> = t.split(',').map(|v| v.trim().parse().unwrap()).collect();
        let tile = Tile { rows: v[0], cols: v[1], bk: v[2] };
        let kernel: &'static Matmul =
            Box::leak(Box::new(Matmul { pipeline: pipeline(&d.device, &d.pipeline_layout, "variant", &matmul_source(tile, false), "main"), tile }));
        for (n, k) in [(3 * HIDDEN, HIDDEN), (MLP, HIDDEN), (HIDDEN, MLP)] {
            let flops = 2.0 * (TOKENS * n * k) as f64;
            time(&format!("{tile:?} {n}x{k}"), flops, &|g| {
                g.matmul_with(kernel, Precision::F32, [&x, &w, &b, &y], (TOKENS, n, k), 1, false, false).unwrap()
            });
        }
    }
    time("matmul 5184x4736x1024 gelu", 2.0 * (TOKENS * MLP * HIDDEN) as f64, &|g| {
        g.matmul([&x, &w, &b, &y], (TOKENS, MLP, HIDDEN), 1, true, false, Precision::F32).unwrap()
    });
    for span in [WINDOW * WINDOW, TOKENS] {
        let flops = 4.0 * (TOKENS * span * HIDDEN) as f64;
        time(&format!("attention span {span}"), flops, &|g| g.attention(&x, &y, (TOKENS, 3 * HIDDEN, HIDDEN), span).unwrap());
    }
    time("layernorm", 0.0, &|g| g.layernorm([&x, &b, &b, &y], TOKENS, HIDDEN).unwrap());
    time("rope", 0.0, &|g| g.rope(&x, &w, (TOKENS, 3 * HIDDEN, HIDDEN), TOKENS, 0.125).unwrap());
}

/// An adapter without the storage-buffer size or the workgroup memory the kernels need gets no device (the model then
/// runs in candle on the CPU); the error names what it lacks. No GPU needed.
#[test]
fn an_adapter_short_of_the_limits_is_refused_with_what_it_lacks() {
    assert!(short_limits(&needed_limits()).is_empty());
    let short = short_limits(&wgpu::Limits { max_storage_buffer_binding_size: 64 << 20, ..needed_limits() });
    assert_eq!(short.len(), 1, "{short:?}");
    assert!(short[0].starts_with("max_storage_buffer_binding_size 67108864 <"), "{short:?}");
    let short = short_limits(&wgpu::Limits { max_compute_workgroup_storage_size: 8 << 10, ..needed_limits() });
    assert_eq!(short.len(), 1, "{short:?}");
    assert!(short[0].starts_with("max_compute_workgroup_storage_size 8192 <"), "{short:?}");
}
