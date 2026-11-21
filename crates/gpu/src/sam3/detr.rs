//! The DETR encoder of SAM 3's text detector on the GPU (`lightcraft-segment`'s `detector.rs` is the reference): six
//! layers over the 72 × 72 image tokens of 256, each self-attention (q and k from the normed tokens plus their sine
//! position encoding, v from the normed tokens) → cross-attention to the prompt's 32 tokens (with the key-padding bias)
//! → MLP 256 → 2048 → 256 with ReLU, each after a layer norm (eps 1e-5) and added to the tokens. ~4 s per prompt on
//! the CPU through candle.
//!
//! Kernels: the backbone's matrix product (ReLU in its epilogue) and layer norm, and `wgsl/sam3_detr.wgsl`: attention
//! with heads of 32 whose q, k and v come from separate buffers (the cross-attention's k and v from the prompt), with
//! an optional per-key bias, and a sum. The weights (38 MB) and the kernels load on the first prompt, not with the
//! backbone; then every dispatch is recorded once, and a prompt writes the tokens, their position encoding, the
//! prompt's tokens and its padding bias, submits one command buffer per layer and reads the memory back.

use std::sync::{Mutex, OnceLock, PoisonError};

use lightcraft_segment::WeightReader;

use super::{Dev, Graph, Precision, Recorded, TIMEOUT, buffer, pipeline, u32_of, upload};

#[cfg(test)]
mod tests;

/// Where the encoder's weights are in the checkpoint.
const PREFIX: &str = "detector_model.detr_encoder";
/// Image tokens (72 × 72), their width, the prompt's tokens.
const TOKENS: usize = 72 * 72;
const WIDTH: usize = 256;
const TEXT: usize = 32;
const HEAD_DIM: usize = 32;
const MLP: usize = 2048;
const LAYERS: usize = 6;
/// PyTorch's `nn.LayerNorm` default (`TORCH_LN_EPS`).
const EPS: f32 = 1e-5;
/// Keys per step of the attention kernel (the prompt's 32 tokens are one step).
const KEYS: u32 = 32;
/// `p.gelu` of the matrix product: ReLU.
const RELU: u32 = 2;
/// No bias in the attention kernel.
const NONE: u32 = u32::MAX;

pub(super) struct Kernels {
    attention: wgpu::ComputePipeline,
    add: wgpu::ComputePipeline,
}

static KERNELS: OnceLock<Result<Kernels, String>> = OnceLock::new();

/// The encoder's kernels, compiled on first use (the mask decoder's prompt attention uses them too).
pub(super) fn kernels(dev: &'static Dev) -> Result<&'static Kernels, String> {
    KERNELS
        .get_or_init(|| {
            let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
            let source = attention_source(KEYS);
            let pipe = |entry: &str| pipeline(&dev.device, &dev.pipeline_layout, "sam3 detr", &source, entry);
            let k = Kernels { attention: pipe("attention"), add: pipe("add") };
            match pollster::block_on(validation.pop()) {
                Some(e) => Err(format!("{}: the SAM 3 DETR kernels did not compile ({e})", dev.info.name)),
                None => Ok(k),
            }
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The attention kernel for `keys` keys per step, written out from `wgsl/sam3_detr.wgsl` (as the backbone's).
fn attention_source(keys: u32) -> String {
    use std::fmt::Write;
    let d4 = (HEAD_DIM / 4) as u32;
    let (mut decl, mut scores, mut rescale, mut acc, mut store) = (String::new(), String::new(), String::new(), String::new(), String::new());
    // writing into a String cannot fail
    for c in 0..d4 {
        let _ = writeln!(decl, "    let q{c} = qs[qa + {c}u];\n    var o{c} = vec4<f32>(0.0);");
        let _ = writeln!(rescale, "        o{c} *= fix;");
        let _ = writeln!(store, "        out[oa + {c}u] = o{c} * r;");
    }
    for j in 0..keys {
        let terms: Vec<String> = (0..d4).map(|c| format!("q{c} * Ks[{}u]", j * d4 + c)).collect();
        let _ = writeln!(
            scores,
            "        let d{j} = {};\n        let s{j} = (d{j}.x + d{j}.y + d{j}.z + d{j}.w) * SCALE + Bs[{j}u];\n        top = max(top, s{j});",
            terms.join(" + ")
        );
        let _ = writeln!(acc, "        {{\n            let w = exp(s{j} - top);\n            l += w;");
        for c in 0..d4 {
            let _ = writeln!(acc, "            o{c} = fma(vec4<f32>(w), Vs[{}u], o{c});", j * d4 + c);
        }
        let _ = writeln!(acc, "        }}");
    }
    include_str!("../wgsl/sam3_detr.wgsl")
        .replace("{{KB}}", &keys.to_string())
        .replace("{{DECL}}", &decl)
        .replace("{{SCORES}}", &scores)
        .replace("{{RESCALE}}", &rescale)
        .replace("{{ACCUMULATE}}", &acc)
        .replace("{{STORE}}", &store)
}

/// Where an attention's q, k and v are: a buffer, its row stride and the column of head 0 (in floats).
pub(super) struct Operand<'a> {
    pub(super) buffer: &'a wgpu::Buffer,
    pub(super) stride: usize,
    pub(super) offset: usize,
}

impl Graph {
    /// Layer norm with PyTorch's eps (the backbone's is 1e-6).
    pub(super) fn layernorm_torch(&mut self, [x, gamma, beta, y]: [&wgpu::Buffer; 4], rows: usize, c: usize) -> Result<(), String> {
        let (g, row) = self.flat(rows)?;
        self.push(&self.dev.k.layernorm, [u32_of(rows)?, u32_of(c)?, 0, 0, EPS.to_bits(), 0, u32_of(rows)?, row], [x, gamma, beta, y], g);
        Ok(())
    }

    /// `y = relu(x · wᵀ + bias)`.
    fn matmul_relu(&mut self, buffers: [&wgpu::Buffer; 4], dims: (usize, usize, usize)) -> Result<(), String> {
        self.matmul(buffers, dims, 1, false, false, Precision::F32)?;
        if let Some(step) = self.steps.last_mut() {
            step.params[4] = RELU;
        }
        Ok(())
    }

    /// `out = a + b`, `len` floats (a multiple of 4).
    fn add(&mut self, k: &'static Kernels, [a, b, out]: [&wgpu::Buffer; 3], len: usize) -> Result<(), String> {
        if !len.is_multiple_of(4) {
            return Err("a sum the SAM 3 kernel cannot do".into());
        }
        let (g, row) = self.flat((len / 4).div_ceil(256))?;
        self.push(&k.add, [u32_of(len / 4)?, row, 0, 0, 0, 0, 0, 0], [a, b, &self.dev.dummy, out], g);
        Ok(())
    }

    /// Attention of `queries` queries over `keys` keys, `heads` heads of 32, into `out` (`[queries, heads · 32]`);
    /// with `bias`, key j's scores get `k.buffer[bias + j]` added.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn attention_detr(
        &mut self,
        kernels: &'static Kernels,
        [q, k, v]: [Operand; 3],
        bias: Option<usize>,
        out: &wgpu::Buffer,
        (queries, keys, heads): (usize, usize, usize),
    ) -> Result<(), String> {
        let aligned = [q.stride, q.offset, k.stride, k.offset, v.stride, v.offset].iter().all(|n| n.is_multiple_of(4));
        if keys == 0 || !keys.is_multiple_of(KEYS as usize) || !aligned || q.offset != 0 {
            return Err("an attention the SAM 3 DETR kernel cannot do".into());
        }
        let bias = match bias {
            Some(b) => u32_of(b)?,
            None => NONE,
        };
        let params =
            [u32_of(queries)?, u32_of(keys)?, u32_of(q.stride)?, u32_of(k.stride)?, u32_of(v.stride)?, u32_of(k.offset)?, u32_of(v.offset)?, bias];
        let groups = [u32_of(queries.div_ceil(64))?, u32_of(heads)?, 1];
        if groups[0] > self.dev.limits.max_compute_workgroups_per_dimension {
            return Err("too many queries for the SAM 3 DETR attention kernel".into());
        }
        self.push(&kernels.attention, params, [q.buffer, k.buffer, v.buffer, out], groups);
        Ok(())
    }
}

/// One layer's weights on the device: (weight, bias) pairs, the q | k and k | v ones concatenated.
struct Layer {
    ln1: [wgpu::Buffer; 2],
    self_qk: [wgpu::Buffer; 2],
    self_v: [wgpu::Buffer; 2],
    self_o: [wgpu::Buffer; 2],
    ln2: [wgpu::Buffer; 2],
    cross_q: [wgpu::Buffer; 2],
    cross_kv: [wgpu::Buffer; 2],
    cross_o: [wgpu::Buffer; 2],
    ln3: [wgpu::Buffer; 2],
    fc1: [wgpu::Buffer; 2],
    fc2: [wgpu::Buffer; 2],
}

/// Reads the encoder's tensors, checking their shapes.
struct Loader<'a> {
    weights: &'a WeightReader,
    dev: &'static Dev,
    bytes: u64,
}

impl Loader<'_> {
    /// The tensors `names` (each of `shape`) concatenated along the first axis.
    fn upload(&mut self, names: &[String], shape: &[usize]) -> Result<wgpu::Buffer, String> {
        let mut all = Vec::with_capacity(names.len() * shape.iter().product::<usize>());
        for name in names {
            let full = format!("{PREFIX}.{name}");
            let (got, data) = self.weights.read(&full).map_err(|e| format!("{full}: {e}"))?;
            if got != shape {
                return Err(format!("{full}: shape {got:?}, expected {shape:?}"));
            }
            all.extend(data);
        }
        self.bytes += all.len() as u64 * 4;
        Ok(upload(self.dev, names.first().map_or("weight", String::as_str), &all))
    }

    /// A linear layer (or several along n): `[n, k]` weights and their bias.
    fn linear(&mut self, names: &[String], n: usize, k: usize) -> Result<[wgpu::Buffer; 2], String> {
        let w: Vec<String> = names.iter().map(|n| format!("{n}.weight")).collect();
        let b: Vec<String> = names.iter().map(|n| format!("{n}.bias")).collect();
        Ok([self.upload(&w, &[n, k])?, self.upload(&b, &[n])?])
    }

    fn norm(&mut self, name: &str) -> Result<[wgpu::Buffer; 2], String> {
        Ok([self.upload(&[format!("{name}.weight")], &[WIDTH])?, self.upload(&[format!("{name}.bias")], &[WIDTH])?])
    }

    fn layer(&mut self, i: usize) -> Result<Layer, String> {
        let l = format!("layers.{i}");
        let (s, c) = (format!("{l}.self_attn"), format!("{l}.cross_attn"));
        let proj = |a: &str, ps: &[&str]| -> Vec<String> { ps.iter().map(|p| format!("{a}.{p}_proj")).collect() };
        Ok(Layer {
            ln1: self.norm(&format!("{l}.layer_norm1"))?,
            self_qk: self.linear(&proj(&s, &["q", "k"]), WIDTH, WIDTH)?,
            self_v: self.linear(&proj(&s, &["v"]), WIDTH, WIDTH)?,
            self_o: self.linear(&proj(&s, &["o"]), WIDTH, WIDTH)?,
            ln2: self.norm(&format!("{l}.layer_norm2"))?,
            cross_q: self.linear(&proj(&c, &["q"]), WIDTH, WIDTH)?,
            cross_kv: self.linear(&proj(&c, &["k", "v"]), WIDTH, WIDTH)?,
            cross_o: self.linear(&proj(&c, &["o"]), WIDTH, WIDTH)?,
            ln3: self.norm(&format!("{l}.layer_norm3"))?,
            fc1: self.linear(&[format!("{l}.mlp.fc1")], MLP, WIDTH)?,
            fc2: self.linear(&[format!("{l}.mlp.fc2")], WIDTH, MLP)?,
        })
    }
}

/// The buffers a prompt writes and reads.
struct Work {
    /// The image tokens in, the memory out (the layers add to it in place).
    x: wgpu::Buffer,
    pos: wgpu::Buffer,
    text: wgpu::Buffer,
    /// The cross-attention's k | v of the prompt (`[32, 512]`), then its padding bias (32 floats).
    text_kv: wgpu::Buffer,
    staging: wgpu::Buffer,
}

/// The encoder with its weights on the device and its dispatches recorded.
struct Built {
    recorded: Recorded,
    /// One prompt at a time (they share the work buffers).
    work: Mutex<Work>,
    /// Bytes of device memory: weights, work buffers.
    weight_bytes: u64,
    work_bytes: u64,
}

/// The DETR encoder, built on first use from the checkpoint the backbone came from.
pub(super) struct Encoder {
    weights: WeightReader,
    built: OnceLock<Result<Built, String>>,
}

impl Encoder {
    pub(super) fn new(weights: WeightReader) -> Self {
        Self { weights, built: OnceLock::new() }
    }

    /// Bytes of device memory the weights and the work buffers take, once built.
    pub(super) fn bytes(&self) -> Option<(u64, u64)> {
        self.built.get().and_then(|b| b.as_ref().ok()).map(|b| (b.weight_bytes, b.work_bytes))
    }

    /// The memory `[5184, 256]` for the image tokens and their position encoding (`[5184, 256]` each) and the prompt's
    /// tokens `[32, 256]` (`valid`: its real ones).
    pub(super) fn run(&self, dev: &'static Dev, image: &[f32], pos: &[f32], text: &[f32], valid: &[bool]) -> Result<Vec<f32>, String> {
        if image.len() != TOKENS * WIDTH || pos.len() != TOKENS * WIDTH || text.len() != TEXT * WIDTH || valid.len() != TEXT {
            return Err(format!(
                "{} image, {} position, {} prompt values and {} prompt tokens, expected {TOKENS} × {WIDTH}, {TOKENS} × {WIDTH}, {TEXT} × {WIDTH} and {TEXT}",
                image.len(),
                pos.len(),
                text.len(),
                valid.len()
            ));
        }
        let b = self.built.get_or_init(|| build(dev, &self.weights)).as_ref().map_err(Clone::clone)?;
        let bias: Vec<f32> = valid.iter().map(|&ok| if ok { 0.0 } else { f32::MIN }).collect();
        let work = b.work.lock().unwrap_or_else(PoisonError::into_inner);
        dev.queue.write_buffer(&work.x, 0, bytemuck::cast_slice(image));
        dev.queue.write_buffer(&work.pos, 0, bytemuck::cast_slice(pos));
        dev.queue.write_buffer(&work.text, 0, bytemuck::cast_slice(text));
        dev.queue.write_buffer(&work.text_kv, (TEXT * 2 * WIDTH * 4) as u64, bytemuck::cast_slice(&bias));
        let out = b.recorded.run("the SAM 3 DETR encoder", dev, &work.x, &work.staging, TOKENS * WIDTH)?;
        if out.iter().any(|v| !v.is_finite()) {
            return Err("the GPU DETR encoder produced non-finite values".into());
        }
        Ok(out)
    }
}

fn build(dev: &'static Dev, weights: &WeightReader) -> Result<Built, String> {
    let k = kernels(dev)?;
    let oom = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let made = build_inner(dev, k, weights);
    // let the uploads land before the scopes close
    let _ = dev.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
    if let Some(e) = oom {
        return Err(format!("not enough GPU memory for the SAM 3 DETR encoder: {e}"));
    }
    if let Some(e) = validation {
        return Err(format!("the GPU rejected the SAM 3 DETR encoder: {e}"));
    }
    made
}

fn build_inner(dev: &'static Dev, k: &'static Kernels, weights: &WeightReader) -> Result<Built, String> {
    let mut ld = Loader { weights, dev, bytes: 0 };
    let layers = (0..LAYERS).map(|i| ld.layer(i)).collect::<Result<Vec<_>, _>>()?;

    let tokens = |label: &str| buffer(dev, label, TOKENS * WIDTH);
    let (x, y, hp, v, att, pos) =
        (tokens("detr hidden"), tokens("detr normed"), tokens("detr normed + pos"), tokens("detr v"), tokens("detr attention"), tokens("detr pos"));
    // the self-attention's q | k, then the MLP's hidden activations
    let wide = buffer(dev, "detr wide", TOKENS * MLP.max(2 * WIDTH));
    let text = buffer(dev, "detr prompt", TEXT * WIDTH);
    let text_kv = buffer(dev, "detr prompt k|v", TEXT * 2 * WIDTH + TEXT);
    let staging = dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("detr read-back"),
        size: (TOKENS * WIDTH * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let work_bytes = ((7 * TOKENS * WIDTH + TOKENS * MLP + TEXT * (3 * WIDTH + 1)) * 4) as u64;

    let mut g = Graph::new(dev);
    let heads = WIDTH / HEAD_DIM;
    for l in &layers {
        // self-attention: q and k from the normed tokens plus their position, v from the normed tokens
        g.layernorm_torch([&x, &l.ln1[0], &l.ln1[1], &y], TOKENS, WIDTH)?;
        g.add(k, [&y, &pos, &hp], TOKENS * WIDTH)?;
        g.matmul([&hp, &l.self_qk[0], &l.self_qk[1], &wide], (TOKENS, 2 * WIDTH, WIDTH), 1, false, false, Precision::F32)?;
        g.matmul([&y, &l.self_v[0], &l.self_v[1], &v], (TOKENS, WIDTH, WIDTH), 1, false, false, Precision::F32)?;
        let (q, kk) = (Operand { buffer: &wide, stride: 2 * WIDTH, offset: 0 }, Operand { buffer: &wide, stride: 2 * WIDTH, offset: WIDTH });
        g.attention_detr(k, [q, kk, Operand { buffer: &v, stride: WIDTH, offset: 0 }], None, &att, (TOKENS, TOKENS, heads))?;
        g.matmul([&att, &l.self_o[0], &l.self_o[1], &x], (TOKENS, WIDTH, WIDTH), 1, false, true, Precision::F32)?;
        // cross-attention to the prompt, its padding masked
        g.layernorm_torch([&x, &l.ln2[0], &l.ln2[1], &y], TOKENS, WIDTH)?;
        g.matmul([&y, &l.cross_q[0], &l.cross_q[1], &v], (TOKENS, WIDTH, WIDTH), 1, false, false, Precision::F32)?;
        g.matmul([&text, &l.cross_kv[0], &l.cross_kv[1], &text_kv], (TEXT, 2 * WIDTH, WIDTH), 1, false, false, Precision::F32)?;
        let (tk, tv) = (Operand { buffer: &text_kv, stride: 2 * WIDTH, offset: 0 }, Operand { buffer: &text_kv, stride: 2 * WIDTH, offset: WIDTH });
        g.attention_detr(k, [Operand { buffer: &v, stride: WIDTH, offset: 0 }, tk, tv], Some(TEXT * 2 * WIDTH), &att, (TOKENS, TEXT, heads))?;
        g.matmul([&att, &l.cross_o[0], &l.cross_o[1], &x], (TOKENS, WIDTH, WIDTH), 1, false, true, Precision::F32)?;
        // MLP
        g.layernorm_torch([&x, &l.ln3[0], &l.ln3[1], &y], TOKENS, WIDTH)?;
        g.matmul_relu([&y, &l.fc1[0], &l.fc1[1], &wide], (TOKENS, MLP, WIDTH))?;
        g.matmul([&wide, &l.fc2[0], &l.fc2[1], &x], (TOKENS, WIDTH, MLP), 1, false, true, Precision::F32)?;
        g.cut();
    }
    let recorded = g.record();
    Ok(Built { recorded, work: Mutex::new(Work { x, pos, text, text_kv, staging }), weight_bytes: ld.bytes, work_bytes })
}
