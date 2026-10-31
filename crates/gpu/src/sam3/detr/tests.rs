//! The DETR encoder's kernels against plain loops: attention with heads of 32 (self- and cross-attention, the padding
//! bias, queries not filling the last workgroup), the sum, ReLU and the layer norm's eps. The whole encoder against
//! candle: `tests/it/sam3_detr.rs`.

use super::super::dev;
use super::*;

fn gpu() -> Option<(&'static Dev, &'static Kernels)> {
    let d = match dev() {
        Ok(d) => d,
        // a kernel that doesn't compile is a failure, not a missing GPU
        Err(e) if e.contains("did not compile") => panic!("{e}"),
        Err(e) => {
            lightcraft_testkit::skip!(Gpu, "no GPU for the SAM 3 kernels ({e})");
            return None;
        }
    };
    Some((d, kernels(d).unwrap()))
}

/// Deterministic values in −scale..scale.
fn values(n: usize, seed: u64, scale: f32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (0..n)
        .map(|_| {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * scale
        })
        .collect()
}

fn read(d: &'static Dev, g: Graph, out: &wgpu::Buffer, len: usize) -> Vec<f32> {
    let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: (len * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    g.record().run("a test", d, out, &staging, len).unwrap()
}

fn worst(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len());
    got.iter().zip(want).fold(0f32, |m, (a, b)| m.max((a - b).abs()))
}

/// Where a test attention's operand is in its buffer.
struct At<'a> {
    data: &'a [f32],
    stride: usize,
    offset: usize,
}

/// Softmax attention of every query over all keys, `heads` heads of 32, as candle computes it: the scores
/// `q · k / sqrt(32) + bias` in f32 (so f32::MIN swallows a padded key's score), the rest in f64.
fn attention_ref(q: At, k: At, v: At, bias: Option<&[f32]>, (queries, keys, heads): (usize, usize, usize)) -> Vec<f32> {
    let scale = (HEAD_DIM as f32).powf(-0.5);
    let mut out = vec![0f32; queries * heads * HEAD_DIM];
    for t in 0..queries {
        for h in 0..heads {
            let at = |o: &At, row: usize, c: usize| o.data[row * o.stride + o.offset + h * HEAD_DIM + c];
            let scores: Vec<f32> = (0..keys)
                .map(|j| {
                    let dot = (0..HEAD_DIM).map(|c| f64::from(at(&q, t, c)) * f64::from(at(&k, j, c))).sum::<f64>() as f32;
                    dot * scale + bias.map_or(0.0, |b| b[j])
                })
                .collect();
            let top = scores.iter().fold(f32::MIN, |m, s| m.max(*s));
            let w: Vec<f64> = scores.iter().map(|s| f64::from(s - top).exp()).collect();
            let sum: f64 = w.iter().sum();
            for c in 0..HEAD_DIM {
                let o = (0..keys).map(|j| w[j] * f64::from(at(&v, j, c))).sum::<f64>() / sum;
                out[(t * heads + h) * HEAD_DIM + c] = o as f32;
            }
        }
    }
    out
}

#[test]
fn the_kernel_source_is_written_out() {
    let src = attention_source(KEYS);
    assert!(!src.contains("{{"), "a mark is left");
    assert!(src.contains("let s31 = ") && !src.contains("let s32 = "));
    assert!(src.contains("let q7 = ") && !src.contains("let q8 = "));
}

#[test]
fn self_attention_over_8_heads_of_32_gives_the_softmax_of_the_definition() {
    let Some((d, k)) = gpu() else { return };
    let heads = 8;
    let w = heads * HEAD_DIM;
    // q | k in one buffer (k at column 256), v in another; 200 queries leave the last workgroup part empty
    for (queries, keys) in [(200, 96), (64, 32), (130, 320)] {
        let rows = queries.max(keys);
        let qk = values(rows * 2 * w, queries as u64, 2.0);
        let v = values(keys * w, 7 + keys as u64, 1.0);
        let (bqk, bv) = (upload(d, "qk", &qk), upload(d, "v", &v));
        let out = buffer(d, "out", queries * w);
        let mut g = Graph::new(d);
        let ops = [
            Operand { buffer: &bqk, stride: 2 * w, offset: 0 },
            Operand { buffer: &bqk, stride: 2 * w, offset: w },
            Operand { buffer: &bv, stride: w, offset: 0 },
        ];
        g.attention_detr(k, ops, None, &out, (queries, keys, heads)).unwrap();
        let got = read(d, g, &out, queries * w);
        let want = attention_ref(
            At { data: &qk, stride: 2 * w, offset: 0 },
            At { data: &qk, stride: 2 * w, offset: w },
            At { data: &v, stride: w, offset: 0 },
            None,
            (queries, keys, heads),
        );
        let err = worst(&got, &want);
        assert!(err < 2e-5, "{queries} queries × {keys} keys: off by {err}");
    }
}

#[test]
fn cross_attention_to_32_prompt_tokens_leaves_out_the_padding() {
    let Some((d, k)) = gpu() else { return };
    let (heads, queries) = (8, 130);
    let w = heads * HEAD_DIM;
    let q = values(queries * w, 1, 2.0);
    // the prompt's k | v ([32, 512]), then its bias: 0 for the real tokens, f32::MIN for padding
    for real in [5, 32, 1, 0] {
        let mut kv = values(TEXT * 2 * w, 2 + real as u64, 2.0);
        let bias: Vec<f32> = (0..TEXT).map(|j| if j < real { 0.0 } else { f32::MIN }).collect();
        kv.extend(&bias);
        let (bq, bkv) = (upload(d, "q", &q), upload(d, "kv", &kv));
        let out = buffer(d, "out", queries * w);
        let mut g = Graph::new(d);
        let ops = [
            Operand { buffer: &bq, stride: w, offset: 0 },
            Operand { buffer: &bkv, stride: 2 * w, offset: 0 },
            Operand { buffer: &bkv, stride: 2 * w, offset: w },
        ];
        g.attention_detr(k, ops, Some(TEXT * 2 * w), &out, (queries, TEXT, heads)).unwrap();
        let got = read(d, g, &out, queries * w);
        let want = attention_ref(
            At { data: &q, stride: w, offset: 0 },
            At { data: &kv, stride: 2 * w, offset: 0 },
            At { data: &kv, stride: 2 * w, offset: w },
            Some(&bias),
            (queries, TEXT, heads),
        );
        let err = worst(&got, &want);
        assert!(err < 2e-5, "{real} real tokens: off by {err}");
        if real == 1 {
            // one real token: every query gets its v
            let first = &kv[w..2 * w];
            assert!(worst(&got[..w], first) < 1e-6);
        }
    }
}

#[test]
fn attention_the_kernel_cannot_do_is_an_error() {
    let Some((d, k)) = gpu() else { return };
    let b = buffer(d, "b", 64 * 256);
    let op = |stride, offset| Operand { buffer: &b, stride, offset };
    // keys not a multiple of 32, a stride not a multiple of 4, q not at column 0
    assert!(Graph::new(d).attention_detr(k, [op(256, 0), op(256, 0), op(256, 0)], None, &b, (64, 40, 8)).is_err());
    assert!(Graph::new(d).attention_detr(k, [op(256, 0), op(258, 0), op(256, 0)], None, &b, (64, 32, 8)).is_err());
    assert!(Graph::new(d).attention_detr(k, [op(256, 4), op(256, 0), op(256, 0)], None, &b, (64, 32, 8)).is_err());
    assert!(Graph::new(d).add(k, [&b, &b, &b], 6).is_err());
}

#[test]
fn sum_relu_and_torch_layer_norm_give_what_the_definitions_give() {
    let Some((d, k)) = gpu() else { return };
    // the sum, over more than one row of workgroups' worth
    let n = 4 * 256 * 70 + 12;
    let (a, b) = (values(n, 1, 1.0), values(n, 2, 1.0));
    let (ba, bb, out) = (upload(d, "a", &a), upload(d, "b", &b), buffer(d, "out", n));
    let mut g = Graph::new(d);
    g.add(k, [&ba, &bb, &out], n).unwrap();
    let want: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    assert_eq!(read(d, g, &out, n), want);

    // ReLU in the product's epilogue
    let (m, nn, kk) = (70, 90, 64);
    let (x, w, bias) = (values(m * kk, 3, 1.0), values(nn * kk, 4, 0.3), values(nn, 5, 0.5));
    let (bx, bw, bbias, by) = (upload(d, "x", &x), upload(d, "w", &w), upload(d, "bias", &bias), buffer(d, "y", m * nn));
    let mut g = Graph::new(d);
    g.matmul_relu([&bx, &bw, &bbias, &by], (m, nn, kk)).unwrap();
    let got = read(d, g, &by, m * nn);
    let want: Vec<f32> = (0..m * nn)
        .map(|o| {
            let (i, j) = (o / nn, o % nn);
            let v = f64::from(bias[j]) + (0..kk).map(|t| f64::from(x[i * kk + t]) * f64::from(w[j * kk + t])).sum::<f64>();
            v.max(0.0) as f32
        })
        .collect();
    assert!(worst(&got, &want) < 1e-5);
    assert!(got.contains(&0.0) && got.iter().any(|v| *v > 0.0));

    // rows of small spread, where eps 1e-5 and 1e-6 differ
    let (rows, c) = (5, WIDTH);
    let x = values(rows * c, 6, 3e-3);
    let (gamma, beta) = (values(c, 7, 1.0), values(c, 8, 0.1));
    let (bx, bg, bb, by) = (upload(d, "x", &x), upload(d, "g", &gamma), upload(d, "b", &beta), buffer(d, "y", rows * c));
    let mut g = Graph::new(d);
    g.layernorm_torch([&bx, &bg, &bb, &by], rows, c).unwrap();
    let got = read(d, g, &by, rows * c);
    let mut want = vec![0f32; rows * c];
    for r in 0..rows {
        let row = &x[r * c..(r + 1) * c];
        let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / c as f64;
        let var = row.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum::<f64>() / c as f64;
        for i in 0..c {
            want[r * c + i] = ((f64::from(row[i]) - mean) / (var + f64::from(EPS)).sqrt() * f64::from(gamma[i]) + f64::from(beta[i])) as f32;
        }
    }
    let err = worst(&got, &want);
    assert!(err < 1e-4, "layer norm off by {err}");
}

/// The encoder's steps at its shapes:
/// `cargo test --release -p lightcraft-gpu --features sam --lib sam3::detr::tests::timings -- --ignored --nocapture`.
#[test]
#[ignore = "a benchmark: needs a GPU"]
fn timings() {
    let Some((d, k)) = gpu() else { return };
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
    let w = buffer(d, "w", MLP * WIDTH);
    let b = buffer(d, "b", MLP);
    for (n, kk) in [(2 * WIDTH, WIDTH), (WIDTH, WIDTH), (MLP, WIDTH), (WIDTH, MLP)] {
        let flops = 2.0 * (TOKENS * n * kk) as f64;
        time(&format!("matmul {TOKENS}x{n}x{kk}"), flops, &|g| g.matmul([&x, &w, &b, &y], (TOKENS, n, kk), 1, false, false, Precision::F32).unwrap());
    }
    for keys in [TOKENS, TEXT] {
        let flops = 4.0 * (TOKENS * keys * WIDTH) as f64;
        time(&format!("attention {keys} keys"), flops, &|g| {
            let ops = [
                Operand { buffer: &x, stride: 2 * WIDTH, offset: 0 },
                Operand { buffer: &x, stride: 2 * WIDTH, offset: WIDTH },
                Operand { buffer: &x, stride: WIDTH, offset: 0 },
            ];
            g.attention_detr(k, ops, None, &y, (TOKENS, keys, WIDTH / HEAD_DIM)).unwrap()
        });
    }
    time("layernorm", 0.0, &|g| g.layernorm_torch([&x, &b, &b, &y], TOKENS, WIDTH).unwrap());
    time("add", 0.0, &|g| g.add(k, [&x, &x, &y], TOKENS * WIDTH).unwrap());
}
