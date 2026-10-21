//! The kernels one at a time and whole towers against plain loops (f64 sums), on odd sizes that are not multiples of
//! the tiles. Skipped (with a note) on a machine without a usable GPU.

use std::collections::HashMap;

use lightcraft_vision::accel::{Block, Input, Linear, Norm, Pool, ProbeHead, TensorSource, TowerPlan, TowerRunner};
use lightcraft_vision::config::Act;

use super::*;

fn noise(len: usize, seed: u64, scale: f32) -> Vec<f32> {
    let mut s = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            ((s.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 2.0 * scale
        })
        .collect()
}

/// The device-loss test breaks the device for a moment: it runs alone.
static ALONE: std::sync::RwLock<()> = std::sync::RwLock::new(());

fn shared() -> std::sync::RwLockReadGuard<'static, ()> {
    ALONE.read().unwrap_or_else(|e| e.into_inner())
}

fn gpu() -> Option<&'static Dev> {
    match dev() {
        Ok(d) => Some(d),
        Err(e) => {
            eprintln!("skipped: no GPU here ({e})");
            None
        }
    }
}

/// Largest difference relative to the reference's largest value.
fn worst(got: &[f32], want: &[f32]) -> f32 {
    assert_eq!(got.len(), want.len(), "sizes differ");
    assert!(got.iter().all(|v| v.is_finite()), "non-finite GPU output");
    let scale = want.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
    got.iter().zip(want).fold(0f32, |m, (a, b)| m.max((a - b).abs())) / scale
}

fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| f64::from(*x) * f64::from(*y)).sum();
    let na: f64 = a.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    let nb: f64 = b.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
    dot / (na * nb).max(1e-30)
}

// ---------------------------------------------------------------------------------------------------------------------
// one kernel at a time

struct Bench {
    d: &'static Dev,
    p: Pipes,
    params: wgpu::Buffer,
}

impl Bench {
    fn new() -> Option<Self> {
        let d = gpu()?;
        let params = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: 4 * SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(Self { d, p: build_pipes(d), params })
    }

    fn buf(&self, v: &[f32]) -> wgpu::Buffer {
        let mut data = bytemuck::cast_slice::<f32, u8>(v).to_vec();
        data.resize(data.len().max(16), 0);
        self.d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: &data,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        })
    }

    fn ubuf(&self, v: &[u32]) -> wgpu::Buffer {
        let mut data = bytemuck::cast_slice::<u32, u8>(v).to_vec();
        data.resize(data.len().max(16), 0);
        self.d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: &data, usage: wgpu::BufferUsages::STORAGE })
    }

    /// Run the calls (each params in its slot), then read `out` (`floats` numbers).
    fn run(&self, calls: Calls<'_>, out: &wgpu::Buffer, floats: usize) -> Vec<f32> {
        let d = self.d;
        let scope = d.device.push_error_scope(wgpu::ErrorFilter::Validation);
        d.queue.write_buffer(&self.params, 0, &calls.params);
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (floats * 4) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            for (i, (pipe, bg, g)) in calls.list.iter().enumerate() {
                pass.set_pipeline(pipe);
                pass.set_bind_group(0, *bg, &[(i as u64 * SLOT) as u32]);
                pass.dispatch_workgroups(g[0], g[1], g[2]);
            }
        }
        enc.copy_buffer_to_buffer(out, 0, &staging, 0, (floats * 4) as u64);
        d.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        d.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) }).unwrap();
        assert!(pollster::block_on(scope.pop()).is_none(), "validation error");
        let v = slice.get_mapped_range().unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect();
        staging.unmap();
        v
    }
}

fn act_ref(a: u32, x: f64) -> f64 {
    match a {
        1 => 0.5 * x * (1.0 + ((2.0 / std::f64::consts::PI).sqrt() * (x + 0.044715 * x * x * x)).tanh()),
        2 => 0.5 * x * (1.0 + erf(x / std::f64::consts::SQRT_2)),
        3 => x / (1.0 + (-1.702 * x).exp()),
        _ => x,
    }
}

/// erf by its series / continued fraction (to well below f32 precision).
fn erf(x: f64) -> f64 {
    if x.abs() > 4.0 {
        return x.signum();
    }
    // Taylor series: 2/√π Σ (−1)^n x^(2n+1) / (n! (2n+1))
    let mut sum = 0.0;
    let mut term = x;
    for n in 0..80 {
        sum += term / (2 * n + 1) as f64;
        term *= -x * x / (n + 1) as f64;
    }
    sum * 2.0 / std::f64::consts::PI.sqrt()
}

#[test]
fn the_linear_layer_kernel_matches_plain_loops() {
    let _shared = shared();
    let Some(b) = Bench::new() else { return };
    // (m, k, n, act, accumulate): small, odd and larger than one 128 block, n not a multiple of 64 or 16
    for (i, &(m, k, n, act, acc)) in [
        (1, 4, 4, 0, false),
        (7, 12, 20, 1, false),
        (70, 72, 100, 2, true),
        (130, 36, 132, 3, false),
        (1100, 64, 196, 1, true),
        (300, 4304 / 4, 8, 0, false),
    ]
    .iter()
    .enumerate()
    {
        let x = noise(m * k, 1 + i as u64, 1.0);
        let w = noise(k * n, 100 + i as u64, 0.2);
        let bias = noise(n, 200 + i as u64, 0.5);
        let y0 = noise(m * n, 300 + i as u64, 1.0);
        let mut want = vec![0f32; m * n];
        for r in 0..m {
            for c in 0..n {
                let s: f64 = (0..k).map(|j| f64::from(x[r * k + j]) * f64::from(w[j * n + c])).sum::<f64>() + f64::from(bias[c]);
                let v = act_ref(act, s);
                want[r * n + c] = (v + if acc { f64::from(y0[r * n + c]) } else { 0.0 }) as f32;
            }
        }
        let (xb, wb, bb, yb) = (b.buf(&x), b.buf(&w), b.buf(&bias), b.buf(&y0));
        let step = MmStep { bg: bind(b.d, &b.p.mm_layout, &b.params, &[&xb, &wb, &bb, &yb]), n, k, act, acc };
        let mut calls = Calls { list: Vec::new(), params: Vec::new() };
        calls.matmul(&b.p, &step, m);
        let got = b.run(calls, &yb, m * n);
        let e = worst(&got, &want);
        assert!(e < 2e-5, "matmul {m}×{k}×{n} act {act} acc {acc}: off by {e:e}");
    }
}

#[test]
fn the_batched_kernel_reads_and_writes_by_strides() {
    let _shared = shared();
    let Some(b) = Bench::new() else { return };
    // attention shapes: 3 inputs × 2 heads of 12 over 70 positions, packed q|k|v rows of 72
    let (n, heads, hd, len) = (3usize, 2usize, 12usize, 70usize);
    let dm = heads * hd;
    let qkv = noise(n * len * 3 * dm, 7, 1.0);
    let at = |bi: usize, t: usize, part: usize, h: usize, j: usize| qkv[(bi * len + t) * 3 * dm + part * dm + h * hd + j];
    // scores = Q · Kᵀ / √d
    let mut want = vec![0f32; n * heads * len * len];
    for bi in 0..n {
        for h in 0..heads {
            for i in 0..len {
                for j in 0..len {
                    let s: f64 = (0..hd).map(|q| f64::from(at(bi, i, 0, h, q)) * f64::from(at(bi, j, 1, h, q))).sum();
                    want[((bi * heads + h) * len + i) * len + j] = (s / (hd as f64).sqrt()) as f32;
                }
            }
        }
    }
    let qb = b.buf(&qkv);
    let sb = b.buf(&vec![0f32; n * heads * len * len]);
    let bg = bind(b.d, &b.p.bmm_layout, &b.params, &[&qb, &qb, &sb]);
    let mut calls = Calls { list: Vec::new(), params: Vec::new() };
    let p = BmmP {
        m: len as u32,
        n: len as u32,
        k: hd as u32,
        heads: heads as u32,
        a_sb: (len * 3 * dm) as u32,
        a_sh: hd as u32,
        a_si: (3 * dm) as u32,
        a_sk: 1,
        b_off: dm as u32,
        b_sb: (len * 3 * dm) as u32,
        b_sh: hd as u32,
        b_sk: 1,
        b_sj: (3 * dm) as u32,
        c_sb: (heads * len * len) as u32,
        c_sh: (len * len) as u32,
        c_si: len as u32,
        c_sj: 1,
        alpha: (hd as f32).powf(-0.5),
        a_kfast: 1,
        b_kfast: 1,
        ..Default::default()
    };
    calls.push(&b.p.bmm, &bg, &p, [2, 2, (n * heads) as u32]);
    let got = b.run(calls, &sb, want.len());
    let e = worst(&got, &want);
    assert!(e < 1e-5, "scores off by {e:e}");
}

#[test]
fn layer_norm_and_softmax_rows_match_plain_loops() {
    let _shared = shared();
    let Some(b) = Bench::new() else { return };
    let (rows, cols) = (37usize, 300usize);
    let x: Vec<f32> = noise(rows * cols, 3, 2.0).iter().enumerate().map(|(i, v)| v + (i / cols) as f32).collect();
    let w = noise(cols, 4, 1.0);
    let bias = noise(cols, 5, 1.0);
    let mut want = vec![0f32; rows * cols];
    for r in 0..rows {
        let row = &x[r * cols..(r + 1) * cols];
        let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / cols as f64;
        let var = row.iter().map(|v| (f64::from(*v) - mean).powi(2)).sum::<f64>() / cols as f64;
        for c in 0..cols {
            want[r * cols + c] = ((f64::from(row[c]) - mean) / (var + 1e-6).sqrt() * f64::from(w[c]) + f64::from(bias[c])) as f32;
        }
    }
    let (xb, wb, bb, yb) = (b.buf(&x), b.buf(&w), b.buf(&bias), b.buf(&vec![0f32; rows * cols]));
    let bg = bind(b.d, &b.p.rows_layout, &b.params, &[&xb, &wb, &bb, &yb]);
    let mut calls = Calls { list: Vec::new(), params: Vec::new() };
    calls.norm(&b.p, &bg, rows, cols, 1e-6);
    let e = worst(&b.run(calls, &yb, rows * cols), &want);
    assert!(e < 1e-5, "layer norm off by {e:e}");

    // softmax, causal: row r is query r % lq
    for causal in [false, true] {
        let (rows, lq, cols) = (2 * 9usize, 9usize, 9usize);
        let s = noise(rows * cols, 6, 8.0);
        let mut want = vec![0f32; rows * cols];
        for r in 0..rows {
            let seen = if causal { r % lq + 1 } else { cols };
            let m = s[r * cols..r * cols + seen].iter().fold(f64::NEG_INFINITY, |m, v| m.max(f64::from(*v)));
            let sum: f64 = (0..seen).map(|c| (f64::from(s[r * cols + c]) - m).exp()).sum();
            for c in 0..seen {
                want[r * cols + c] = ((f64::from(s[r * cols + c]) - m).exp() / sum) as f32;
            }
        }
        let (dummy, sb) = (b.buf(&[0.0]), b.buf(&s));
        let bg = bind(b.d, &b.p.rows_layout, &b.params, &[&dummy, &dummy, &dummy, &sb]);
        let mut calls = Calls { list: Vec::new(), params: Vec::new() };
        let g = spread(rows);
        calls.push(
            &b.p.softmax,
            &bg,
            &RowsP { rows: rows as u32, cols: cols as u32, gx: g[0], lq: lq as u32, causal: u32::from(causal), eps: 0.0, pad: [0; 2] },
            g,
        );
        let e = worst(&b.run(calls, &sb, rows * cols), &want);
        assert!(e < 1e-6, "softmax (causal {causal}) off by {e:e}");
    }
}

#[test]
fn embeddings_are_assembled_and_positions_picked() {
    let _shared = shared();
    let Some(b) = Bench::new() else { return };
    let (n, len, cols) = (3usize, 5usize, 8usize);
    let src = noise(n * (len - 1) * cols, 8, 1.0);
    let class = noise(cols, 9, 1.0);
    let pos = noise(len * cols, 10, 1.0);
    let mut want = vec![0f32; n * len * cols];
    for bi in 0..n {
        for t in 0..len {
            for c in 0..cols {
                let v = if t == 0 { class[c] } else { src[(bi * (len - 1) + t - 1) * cols + c] };
                want[(bi * len + t) * cols + c] = v + pos[t * cols + c];
            }
        }
    }
    let (sb, cb, pb, kb, xb) = (b.buf(&src), b.buf(&class), b.buf(&pos), b.ubuf(&[0]), b.buf(&vec![0f32; n * len * cols]));
    let bg = bind(b.d, &b.p.embed_layout, &b.params, &[&sb, &cb, &pb, &kb, &xb]);
    let total = n * len * cols;
    let g = spread(total.div_ceil(256));
    let mut calls = Calls { list: Vec::new(), params: Vec::new() };
    calls.push(&b.p.assemble, &bg, &EmbedP { total: total as u32, cols: cols as u32, len: len as u32, cls: 1, gx: g[0], pad: [0; 3] }, g);
    assert_eq!(b.run(calls, &xb, total), want);

    let picks = [4u32, 0, 2];
    let (pk, ob) = (b.ubuf(&picks), b.buf(&vec![0f32; n * cols]));
    let bg = bind(b.d, &b.p.embed_layout, &b.params, &[&xb, &cb, &pb, &pk, &ob]);
    let mut calls = Calls { list: Vec::new(), params: Vec::new() };
    let g = spread((n * cols).div_ceil(256));
    calls.push(&b.p.gather, &bg, &EmbedP { total: (n * cols) as u32, cols: cols as u32, len: len as u32, cls: 0, gx: g[0], pad: [0; 3] }, g);
    let got = b.run(calls, &ob, n * cols);
    for (i, &p) in picks.iter().enumerate() {
        let row = (i * len + p as usize) * cols;
        assert_eq!(&got[i * cols..(i + 1) * cols], &want[row..row + cols]);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// whole towers

/// Random tensors for a plan, by name.
struct Fake(HashMap<String, (Vec<usize>, Vec<f32>)>);

impl TensorSource for Fake {
    fn read(&self, name: &str) -> Result<(Vec<usize>, Vec<f32>), String> {
        self.0.get(name).cloned().ok_or_else(|| format!("missing {name}"))
    }
}

impl Fake {
    fn get(&self, name: &str) -> &[f32] {
        &self.0[name].1
    }
}

fn lin(name: &str, inp: usize, out: usize, bias: bool) -> Linear {
    Linear { weight: format!("{name}.w"), bias: bias.then(|| format!("{name}.b")), rows: None, inp, out }
}

fn norm(name: &str) -> Norm {
    Norm { weight: format!("{name}.w"), bias: format!("{name}.b") }
}

/// A tower plan of the given kind with small odd sizes, and its tensors.
fn tower(kind: &str, hidden: usize, heads: usize, inter: usize, len: usize, layers: usize, act: Act, seed: u64) -> (TowerPlan, Fake) {
    let mut t = HashMap::new();
    let mut add = |name: String, shape: Vec<usize>, scale: f32| {
        let n = shape.iter().product();
        let s = seed.wrapping_add(t.len() as u64 * 31);
        t.insert(name, (shape, noise(n, s, scale)));
    };
    let add_lin = |l: &Linear, rows: usize, add: &mut dyn FnMut(String, Vec<usize>, f32)| {
        add(l.weight.clone(), vec![rows, l.inp], 1.0 / (l.inp as f32).sqrt());
        if let Some(b) = &l.bias {
            add(b.clone(), vec![rows], 0.1);
        }
    };
    let add_norm = |n: &Norm, add: &mut dyn FnMut(String, Vec<usize>, f32)| {
        add(n.weight.clone(), vec![hidden], 0.3);
        add(n.bias.clone(), vec![hidden], 0.1);
    };
    let blocks: Vec<Block> = (0..layers)
        .map(|i| Block {
            ln1: norm(&format!("l{i}.ln1")),
            q: lin(&format!("l{i}.q"), hidden, hidden, true),
            k: lin(&format!("l{i}.k"), hidden, hidden, true),
            v: lin(&format!("l{i}.v"), hidden, hidden, true),
            o: lin(&format!("l{i}.o"), hidden, hidden, true),
            ln2: norm(&format!("l{i}.ln2")),
            fc1: lin(&format!("l{i}.fc1"), hidden, inter, true),
            fc2: lin(&format!("l{i}.fc2"), inter, hidden, true),
        })
        .collect();
    for b in &blocks {
        add_norm(&b.ln1, &mut add);
        add_norm(&b.ln2, &mut add);
        for l in [&b.q, &b.k, &b.v, &b.o, &b.fc1, &b.fc2] {
            add_lin(l, l.out, &mut add);
        }
    }
    let final_ln = norm("final");
    add_norm(&final_ln, &mut add);
    add("pos".into(), vec![len, hidden], 0.5);
    let (input, class, pre_ln, pool, proj, causal, out) = match kind {
        // SigLIP-like: patches with bias, probe head, no projection
        "siglip-image" => {
            let proj = lin("patch", 48, hidden, true);
            add_lin(&proj, hidden, &mut add);
            let packed = |i: usize| Linear {
                weight: "head.in.w".into(),
                bias: Some("head.in.b".into()),
                rows: Some((i * hidden, hidden)),
                inp: hidden,
                out: hidden,
            };
            add("head.in.w".into(), vec![3 * hidden, hidden], 1.0 / (hidden as f32).sqrt());
            add("head.in.b".into(), vec![3 * hidden], 0.1);
            add("probe".into(), vec![1, 1, hidden], 1.0);
            let h = ProbeHead {
                probe: "probe".into(),
                q: packed(0),
                k: packed(1),
                v: packed(2),
                o: lin("head.o", hidden, hidden, true),
                ln: norm("head.ln"),
                fc1: lin("head.fc1", hidden, inter, true),
                fc2: lin("head.fc2", inter, hidden, true),
            };
            add_norm(&h.ln, &mut add);
            for l in [&h.o, &h.fc1, &h.fc2] {
                add_lin(l, l.out, &mut add);
            }
            (Input::Patches { patches: len, proj }, None, None, Pool::Probe(Box::new(h)), None, false, hidden)
        }
        // CLIP-like image: patches without bias, a class token, a layer norm in front, picked and projected
        "clip-image" => {
            let proj = lin("patch", 48, hidden, false);
            add_lin(&proj, hidden, &mut add);
            add("class".into(), vec![hidden], 1.0);
            let pre = norm("pre");
            add_norm(&pre, &mut add);
            let p = lin("proj", hidden, 20, false);
            add_lin(&p, 20, &mut add);
            (Input::Patches { patches: len - 1, proj }, Some("class".to_string()), Some(pre), Pool::Pick, Some(p), false, 20)
        }
        // text: token rows in, picked and projected (causal for CLIP)
        _ => {
            let p = lin("proj", hidden, 24, kind == "siglip-text");
            add_lin(&p, 24, &mut add);
            (Input::Tokens, None, None, Pool::Pick, Some(p), kind == "clip-text", 24)
        }
    };
    let plan = TowerPlan {
        name: "test",
        hidden,
        heads,
        intermediate: inter,
        eps: 1e-5,
        act,
        causal,
        input,
        class,
        positions: "pos".into(),
        max_len: len,
        pre_ln,
        blocks,
        final_ln,
        pool,
        proj,
        out,
    };
    (plan, Fake(t))
}

/// The tower in plain loops (f64 sums).
fn reference(plan: &TowerPlan, f: &Fake, input: &[f32], n: usize, pick: &[u32]) -> Vec<f32> {
    let d = plan.hidden;
    let linear = |x: &[f64], rows: usize, l: &Linear| -> Vec<f64> {
        let w = f.get(&l.weight);
        let (first, _) = l.rows.unwrap_or((0, l.out));
        let b = l.bias.as_ref().map(|b| f.get(b));
        let mut y = vec![0f64; rows * l.out];
        for r in 0..rows {
            for o in 0..l.out {
                let mut s = b.map_or(0.0, |b| f64::from(b[first + o]));
                for i in 0..l.inp {
                    s += x[r * l.inp + i] * f64::from(w[(first + o) * l.inp + i]);
                }
                y[r * l.out + o] = s;
            }
        }
        y
    };
    let layer_norm = |x: &[f64], n: &Norm| -> Vec<f64> {
        let (w, b) = (f.get(&n.weight), f.get(&n.bias));
        x.chunks(d)
            .flat_map(|row| {
                let mean = row.iter().sum::<f64>() / d as f64;
                let var = row.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / d as f64;
                row.iter()
                    .enumerate()
                    .map(move |(i, v)| (v - mean) / (var + f64::from(plan.eps)).sqrt() * f64::from(w[i]) + f64::from(b[i]))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let act = act_code(plan.act);
    let hd = d / plan.heads;
    // attention of q rows (lq per input, or one shared) over k/v rows (lk per input)
    let attend = |q: &[f64], lq: usize, shared: bool, k: &[f64], v: &[f64], lk: usize, causal: bool| -> Vec<f64> {
        let mut out = vec![0f64; n * lq * d];
        for bi in 0..n {
            for h in 0..plan.heads {
                for i in 0..lq {
                    let qrow = if shared { i } else { bi * lq + i };
                    let seen = if causal { i + 1 } else { lk };
                    let s: Vec<f64> = (0..seen)
                        .map(|j| (0..hd).map(|c| q[qrow * d + h * hd + c] * k[(bi * lk + j) * d + h * hd + c]).sum::<f64>() / (hd as f64).sqrt())
                        .collect();
                    let m = s.iter().fold(f64::NEG_INFINITY, |m, v| m.max(*v));
                    let e: Vec<f64> = s.iter().map(|v| (v - m).exp()).collect();
                    let sum: f64 = e.iter().sum();
                    for c in 0..hd {
                        out[(bi * lq + i) * d + h * hd + c] = (0..seen).map(|j| e[j] / sum * v[(bi * lk + j) * d + h * hd + c]).sum();
                    }
                }
            }
        }
        out
    };
    let x64: Vec<f64> = input.iter().map(|v| f64::from(*v)).collect();
    let rows_in = input.len() / plan.input_width() / n;
    let (emb, len) = match &plan.input {
        Input::Patches { proj, .. } => (linear(&x64, n * rows_in, proj), plan.max_len),
        Input::Tokens => (x64, rows_in),
    };
    let cls = usize::from(plan.class.is_some());
    let pos = f.get(&plan.positions);
    let mut x = vec![0f64; n * len * d];
    for bi in 0..n {
        for t in 0..len {
            for c in 0..d {
                let v =
                    if cls == 1 && t == 0 { f64::from(f.get(plan.class.as_deref().unwrap())[c]) } else { emb[(bi * (len - cls) + t - cls) * d + c] };
                x[(bi * len + t) * d + c] = v + f64::from(pos[t * d + c]);
            }
        }
    }
    if let Some(n) = &plan.pre_ln {
        x = layer_norm(&x, n);
    }
    let rows = n * len;
    for b in &plan.blocks {
        let h = layer_norm(&x, &b.ln1);
        let (q, k, v) = (linear(&h, rows, &b.q), linear(&h, rows, &b.k), linear(&h, rows, &b.v));
        let a = attend(&q, len, false, &k, &v, len, plan.causal);
        let o = linear(&a, rows, &b.o);
        x.iter_mut().zip(o).for_each(|(x, o)| *x += o);
        let h = layer_norm(&x, &b.ln2);
        let m: Vec<f64> = linear(&h, rows, &b.fc1).into_iter().map(|v| act_ref(act, v)).collect();
        let o = linear(&m, rows, &b.fc2);
        x.iter_mut().zip(o).for_each(|(x, o)| *x += o);
    }
    let h = layer_norm(&x, &plan.final_ln);
    let pooled = match &plan.pool {
        Pool::Pick => (0..n).flat_map(|i| h[(i * len + pick[i] as usize) * d..(i * len + pick[i] as usize + 1) * d].to_vec()).collect::<Vec<_>>(),
        Pool::Probe(head) => {
            let probe: Vec<f64> = f.get(&head.probe).iter().map(|v| f64::from(*v)).collect();
            let q = linear(&probe, 1, &head.q);
            let (k, v) = (linear(&h, rows, &head.k), linear(&h, rows, &head.v));
            let a = attend(&q, 1, true, &k, &v, len, false);
            let mut p = linear(&a, n, &head.o);
            let m: Vec<f64> = linear(&layer_norm(&p, &head.ln), n, &head.fc1).into_iter().map(|v| act_ref(act, v)).collect();
            p.iter_mut().zip(linear(&m, n, &head.fc2)).for_each(|(p, o)| *p += o);
            p
        }
    };
    let out = match &plan.proj {
        Some(p) => linear(&pooled, n, p),
        None => pooled,
    };
    out.into_iter().map(|v| v as f32).collect()
}

fn check_tower(kind: &str, plan: &TowerPlan, f: &Fake, g: &TowerGpu, n: usize, len: usize, seed: u64) {
    let input = noise(n * len * plan.input_width(), seed, 1.0);
    let pick: Vec<u32> = match plan.pool {
        Pool::Pick if plan.class.is_some() => vec![0; n],
        Pool::Pick => (0..n).map(|i| ((i * 7 + 3) % len) as u32).collect(),
        Pool::Probe(_) => Vec::new(),
    };
    let want = reference(plan, f, &input, n, &pick);
    let got = g.run(&input, n, &pick).unwrap();
    let e = worst(&got, &want);
    let cos = got.chunks(plan.out).zip(want.chunks(plan.out)).map(|(a, b)| cosine(a, b)).fold(1f64, f64::min);
    eprintln!("{kind}: {n} inputs of {len}: off by {e:e} of the largest value, cosine ≥ {cos:.7}");
    assert!(e < 1e-4 && cos > 0.99999, "{kind}: the GPU is off by {e:e} (cosine {cos})");
}

#[test]
fn whole_towers_match_plain_loops() {
    let _shared = shared();
    if gpu().is_none() {
        return;
    }
    // odd widths (72 = 6 heads of 12), an MLP of 100, positions not a multiple of 64
    for (kind, len, act) in
        [("siglip-image", 70, Act::GeluTanh), ("clip-image", 50, Act::QuickGelu), ("siglip-text", 64, Act::GeluTanh), ("clip-text", 77, Act::Gelu)]
    {
        let (plan, f) = tower(kind, 72, 6, 100, len, 2, act, 11);
        let g = runner(&plan, &f).unwrap();
        for (n, seed) in [(1usize, 1u64), (5, 2)] {
            check_tower(kind, &plan, &f, &g, n, if plan.input_width() == 48 { plan.input.patches() } else { len }, seed);
        }
        // a text shorter than the context (CLIP runs only as long as its longest text)
        if matches!(plan.input, Input::Tokens) {
            check_tower(kind, &plan, &f, &g, 3, 9, 3);
        }
    }
}

#[test]
fn passes_in_parts_and_scores_in_rounds_give_the_same_answer() {
    let _shared = shared();
    if gpu().is_none() {
        return;
    }
    let (plan, f) = tower("siglip-image", 72, 6, 100, 70, 1, Act::GeluTanh, 12);
    let d = dev().unwrap();
    let max = d.limits.max_storage_buffer_binding_size.min(MAX_BUFFER);
    let mut g = runner(&plan, &f).unwrap();
    // three inputs per pass, two per round of scores
    g.sizes = Sizes { cap: 3, chunk: 2, ..g.sizes };
    *g.ws.lock().unwrap() = None;
    assert!(sizes(&plan, max).unwrap().cap > 3);
    check_tower("siglip-image in parts", &plan, &f, &g, 8, 70, 4);
    let (plan, f) = tower("clip-text", 72, 6, 100, 77, 1, Act::QuickGelu, 13);
    let mut g = runner(&plan, &f).unwrap();
    g.sizes = Sizes { cap: 2, chunk: 1, ..g.sizes };
    *g.ws.lock().unwrap() = None;
    check_tower("clip-text in parts", &plan, &f, &g, 5, 20, 5);
}

#[test]
fn bad_inputs_and_plans_are_errors_not_crashes() {
    let _shared = shared();
    if gpu().is_none() {
        return;
    }
    let (plan, f) = tower("clip-text", 72, 6, 100, 77, 1, Act::QuickGelu, 14);
    let g = runner(&plan, &f).unwrap();
    let row = plan.input_width();
    assert!(g.run(&[], 1, &[0]).is_err());
    assert!(g.run(&vec![0.0; row * 3], 0, &[]).is_err(), "no inputs");
    assert!(g.run(&vec![0.0; row * 3 + 1], 1, &[0]).is_err(), "a ragged input");
    assert!(g.run(&vec![0.0; row * 78], 1, &[0]).is_err(), "longer than the context");
    assert!(g.run(&vec![0.0; row * 4], 2, &[0]).is_err(), "a pick missing");
    assert!(g.run(&vec![0.0; row * 4], 2, &[0, 2]).is_err(), "a pick outside the text");
    assert!(g.run(&vec![0.0; row * 4], 2, &[0, 1]).is_ok());
    // a tensor missing or of the wrong size
    let mut broken = Fake(f.0.clone());
    broken.0.remove("l0.fc1.w");
    assert!(runner(&plan, &broken).is_err());
    let mut broken = Fake(f.0.clone());
    broken.0.insert("l0.fc2.b".into(), (vec![3], vec![0.0; 3]));
    assert!(runner(&plan, &broken).is_err());
    // sizes the kernels do not take
    let mut odd = plan.clone();
    odd.heads = 7;
    assert!(runner(&odd, &f).is_err());
    let (mut odd, f2) = tower("siglip-text", 72, 6, 100, 77, 1, Act::GeluTanh, 15);
    odd.blocks[0].fc1.out = 98;
    assert!(runner(&odd, &f2).is_err());
    // one input's activations larger than the buffers allow
    assert!(sizes(&plan, 1024).is_err());
}

#[test]
fn a_lost_device_is_an_error_and_is_not_used_again() {
    let _alone = ALONE.write().unwrap_or_else(|e| e.into_inner());
    if gpu().is_none() {
        return;
    }
    let (plan, f) = tower("siglip-text", 72, 6, 100, 16, 1, Act::GeluTanh, 16);
    let g = runner(&plan, &f).unwrap();
    let input = noise(16 * 72, 1, 1.0);
    assert!(g.run(&input, 1, &[15]).is_ok());
    inject_device_loss();
    let e = g.run(&input, 1, &[15]).unwrap_err();
    assert!(e.contains("device lost"), "{e}");
    assert!(g.usable().is_err() && broken_reason().is_some());
    assert!(g.run(&input, 1, &[15]).is_err(), "not used again");
    assert!(runner(&plan, &f).is_err(), "no new runner either");
    BROKEN.clear();
    assert!(g.run(&input, 1, &[15]).is_ok());
}

/// (tests: the patches per image of an image tower)
trait Patches {
    fn patches(&self) -> usize;
}

impl Patches for Input {
    fn patches(&self) -> usize {
        match self {
            Input::Patches { patches, .. } => *patches,
            Input::Tokens => 0,
        }
    }
}

/// Throughput of the kernels at So400m's sizes: `cargo test -p lightcraft-gpu --features vision --lib kernel_speed -- --ignored --nocapture`.
#[test]
#[ignore = "a probe for kernel speed"]
fn kernel_speed() {
    let _shared = shared();
    let Some(b) = Bench::new() else { return };
    let (m, k, n) = (8 * 576, 1152, 4304);
    let (xb, wb, bb, yb) = (b.buf(&noise(m * k, 1, 1.0)), b.buf(&noise(k * n, 2, 0.1)), b.buf(&noise(n, 3, 0.1)), b.buf(&vec![0f32; m * n]));
    let step = MmStep { bg: bind(b.d, &b.p.mm_layout, &b.params, &[&xb, &wb, &bb, &yb]), n, k, act: 1, acc: false };
    time(
        &b,
        "matmul 4608×1152×4304",
        2.0 * (m * k * n) as f64,
        &|| {
            let mut c = Calls { list: Vec::new(), params: Vec::new() };
            c.matmul(&b.p, &step, m);
            c
        },
        &yb,
    );
    let (heads, hd, len, imgs) = (16usize, 72usize, 576usize, 8usize);
    let dm = heads * hd;
    let qkv = b.buf(&noise(imgs * len * 3 * dm, 4, 1.0));
    let sb = b.buf(&vec![0f32; imgs * heads * len * len]);
    let ab = b.buf(&vec![0f32; imgs * len * dm]);
    let qk = bind(b.d, &b.p.bmm_layout, &b.params, &[&qkv, &qkv, &sb]);
    let pv = bind(b.d, &b.p.bmm_layout, &b.params, &[&sb, &qkv, &ab]);
    let sm = bind(b.d, &b.p.rows_layout, &b.params, &[&ab, &ab, &ab, &sb]);
    let s = Sizes { cap: imgs, chunk: imgs, len, hidden: dm, heads, inter: 4304, in_width: 768, out: dm };
    let ws_soft = sm;
    // attention alone: scores, softmax, output
    let fl = 2.0 * 2.0 * (imgs * heads * len * len * hd) as f64;
    let ws = WsLite { softmax: ws_soft };
    time(
        &b,
        "attention 8 × 16 heads × 576²",
        fl,
        &|| {
            let mut c = Calls { list: Vec::new(), params: Vec::new() };
            c.attention_lite(&b.p, &ws.softmax, &qk, &pv, &s, imgs, len, len, false, (0, 3 * dm, false), (dm, 2 * dm, 3 * dm));
            c
        },
        &ab,
    );
}

fn time<'a>(b: &Bench, label: &str, flops: f64, make: &dyn Fn() -> Calls<'a>, out: &wgpu::Buffer) {
    b.run(make(), out, 4);
    let runs = 5;
    let t = std::time::Instant::now();
    for _ in 0..runs {
        b.run(make(), out, 4);
    }
    let s = t.elapsed().as_secs_f64() / runs as f64;
    println!("{label}: {:.2} ms, {:.1} TFLOP/s", s * 1e3, flops / s / 1e12);
}

struct WsLite {
    softmax: wgpu::BindGroup,
}
