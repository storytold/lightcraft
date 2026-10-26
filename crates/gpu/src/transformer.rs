//! The content models' transformers (SigLIP 2, CLIP: `lightcraft-vision`) on the GPU: a [`TowerGpu`] runs a
//! [`TowerPlan`] (the plain description of an image or text tower) with WGSL compute kernels, behind the
//! [`TowerRunner`] interface the vision crate falls back from to candle on the CPU.
//!
//! - **Device:** one of its own ([`crate::compute`]), separate from the render device and from denoise's, with the same
//!   switches (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, `LIGHTCRAFT_GPU_ADAPTER`, the GPU preference), the crash
//!   sentinel around device creation, a 60 s limit per batch, and every failure an `Err`: a device that errs or is
//!   lost is not used again ([`broken_reason`]), and the vision crate runs the batch on the CPU.
//! - **Weights** stay on the device for the runner's life, in f32 (the layers' weights transposed to `[in, out]`, q/k/v
//!   packed into one product). Only the final embeddings are read back.
//! - **Kernels:** `wgsl/tf_matmul.wgsl` (tiled linear layers with fused bias, GELU and residual add), `tf_bmm.wgsl`
//!   (strided batched products for attention: heads are read out of the packed q/k/v rows and written back merged),
//!   `tf_rows.wgsl` (layer norm, softmax with the causal mask), `tf_embed.wgsl` (patch/token rows + class token +
//!   positions; picking one position per input). Tested against plain loops and, with the real models, against the
//!   candle CPU path (`tests/vision_towers.rs`).
//! - **Memory:** activations for up to [`MAX_IMAGES`] inputs are allocated once, sized to fit
//!   [`ACTIVATION_BUDGET`] and the adapter's buffer limits; bigger calls run in parts, and attention scores are made a
//!   few inputs at a time ([`SCORES_BUDGET`]). A tower that does not fit is refused (the CPU runs it).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use lightcraft_vision::accel::{Input, Linear, Norm, Pool, TensorSource, TowerPlan, TowerRunner};
use lightcraft_vision::config::Act;
use wgpu::util::DeviceExt;

use crate::compute::{Broken, CLOSING, Dev};

/// Longest wait for one batch's GPU work before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(60);
/// The largest single buffer: the kernels index with 32-bit element offsets.
const MAX_BUFFER: u64 = 2 << 30;
/// Device memory for the activations of one pass (all inputs in flight), besides the scores.
pub const ACTIVATION_BUDGET: u64 = 1 << 30;
/// Device memory for one round of attention scores (a few inputs' heads × positions²).
pub const SCORES_BUDGET: u64 = 256 << 20;
/// Most inputs in one pass (the vision crate's batch limit).
pub const MAX_IMAGES: usize = lightcraft_vision::MAX_BATCH;
/// Uniform slots are this far apart (the largest `min_uniform_buffer_offset_alignment` wgpu allows).
const SLOT: u64 = 256;
/// Dispatches per command buffer: work is handed to the driver in pieces, so other GPU work (the interactive
/// renders) gets its turns.
const PER_SUBMIT: usize = 48;

// ---------------------------------------------------------------------------------------------------------------------
// the device

static DEV: OnceLock<Result<Dev, String>> = OnceLock::new();
static BROKEN: Broken = Broken::new();
static FAIL_NEXT: AtomicBool = AtomicBool::new(false);

/// Why the GPU stopped being used for the content models (a device error), if it did.
pub fn broken_reason() -> Option<String> {
    BROKEN.reason()
}

/// Tests: the next pass fails as a lost device would (the device is then not used again in this process).
#[doc(hidden)]
pub fn inject_device_loss() {
    FAIL_NEXT.store(true, Ordering::SeqCst);
}

fn dev() -> Result<&'static Dev, String> {
    crate::compute::get(&DEV, &BROKEN, "content models", check_limits)
}

fn check_limits(info: &wgpu::AdapterInfo, l: &wgpu::Limits) -> Result<(), String> {
    if l.max_storage_buffers_per_shader_stage < 5
        || l.max_compute_invocations_per_workgroup < 256
        || l.max_compute_workgroup_size_x < 256
        || l.max_compute_workgroups_per_dimension < 65535
        || l.max_dynamic_uniform_buffers_per_pipeline_layout < 1
        || u64::from(l.min_uniform_buffer_offset_alignment) > SLOT
    {
        return Err(format!("{} is too limited for the content models' kernels", info.name));
    }
    if l.max_compute_workgroup_storage_size < 16 * 1024 {
        return Err(format!("{} has too little workgroup memory for the content models' kernels", info.name));
    }
    Ok(())
}

/// The adapter the content models would use ("NVIDIA GeForce RTX 4090 (Vulkan)"), or why they cannot use one. Creates
/// the device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(Dev::name)
}

/// The vision crate's accelerator: a [`TowerGpu`] for each tower, when the GPU can run it.
pub fn accelerator() -> Box<lightcraft_vision::accel::Accelerator> {
    Box::new(|plan: &TowerPlan, src: &dyn TensorSource| runner(plan, src).map(|r| Box::new(r) as Box<dyn TowerRunner>))
}

// ---------------------------------------------------------------------------------------------------------------------
// kernel parameters (each in its own 256-byte uniform slot)

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct MatP {
    m: u32,
    n: u32,
    k: u32,
    act: u32,
    acc: u32,
    pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct BmmP {
    m: u32,
    n: u32,
    k: u32,
    heads: u32,
    a_off: u32,
    a_sb: u32,
    a_sh: u32,
    a_si: u32,
    a_sk: u32,
    b_off: u32,
    b_sb: u32,
    b_sh: u32,
    b_sk: u32,
    b_sj: u32,
    c_off: u32,
    c_sb: u32,
    c_sh: u32,
    c_si: u32,
    c_sj: u32,
    alpha: f32,
    a_kfast: u32,
    b_kfast: u32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct RowsP {
    rows: u32,
    cols: u32,
    gx: u32,
    lq: u32,
    causal: u32,
    eps: f32,
    pad: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct EmbedP {
    total: u32,
    cols: u32,
    len: u32,
    cls: u32,
    gx: u32,
    pad: [u32; 3],
}

fn act_code(a: Act) -> u32 {
    match a {
        Act::GeluTanh => 1,
        Act::Gelu => 2,
        Act::QuickGelu => 3,
    }
}

/// Workgroups for one per row, or one per 256 numbers: `[gx, gy, 1]` with `gx ≤ 65535`.
fn spread(groups: usize) -> [u32; 3] {
    let gx = groups.clamp(1, 65535);
    [gx as u32, groups.div_ceil(gx).max(1) as u32, 1]
}

// ---------------------------------------------------------------------------------------------------------------------
// weights

struct Lin {
    w: wgpu::Buffer,
    b: wgpu::Buffer,
    inp: usize,
    out: usize,
}

struct Ln {
    w: wgpu::Buffer,
    b: wgpu::Buffer,
}

struct BlockW {
    ln1: Ln,
    qkv: Lin,
    o: Lin,
    ln2: Ln,
    fc1: Lin,
    fc2: Lin,
}

struct ProbeW {
    /// The probe's query, `probe · Wq + bq` (the same for every input), `[hidden]`.
    q: wgpu::Buffer,
    kv: Lin,
    o: Lin,
    ln: Ln,
    fc1: Lin,
    fc2: Lin,
}

struct Weights {
    patch: Option<Lin>,
    class: Option<wgpu::Buffer>,
    pos: wgpu::Buffer,
    pre_ln: Option<Ln>,
    blocks: Vec<BlockW>,
    final_ln: Ln,
    probe: Option<ProbeW>,
    proj: Option<Lin>,
    bytes: u64,
}

fn read(src: &dyn TensorSource, name: &str, len: usize) -> Result<Vec<f32>, String> {
    let (_, v) = src.read(name)?;
    if v.len() != len {
        return Err(format!("{name}: {} numbers, the model needs {len}", v.len()));
    }
    Ok(v)
}

/// A linear layer as `([in, out] weight, [out] bias)`: rows `l.rows` of the stored `[rows, in]` weight, transposed.
fn linear_parts(src: &dyn TensorSource, l: &Linear) -> Result<(Vec<f32>, Vec<f32>), String> {
    let (shape, w) = src.read(&l.weight)?;
    let (inp, out) = (l.inp, l.out);
    let rows = shape.first().copied().unwrap_or(0);
    if inp == 0 || rows.checked_mul(inp) != Some(w.len()) {
        return Err(format!("{}: shape {shape:?} is not [rows, {inp}]", l.weight));
    }
    let (first, count) = l.rows.unwrap_or((0, rows));
    if count != out || first.checked_add(count).is_none_or(|e| e > rows) {
        return Err(format!("{}: rows {first}..+{count} of {rows}, {out} wanted", l.weight));
    }
    let mut wt = vec![0f32; inp * out];
    for o in 0..out {
        let row = w.get((first + o) * inp..(first + o + 1) * inp).ok_or_else(|| format!("{}: short", l.weight))?;
        for (i, v) in row.iter().enumerate() {
            if let Some(d) = wt.get_mut(i * out + o) {
                *d = *v;
            }
        }
    }
    let bias = match &l.bias {
        Some(name) => {
            let b = read(src, name, rows)?;
            b.get(first..first + count).map(<[f32]>::to_vec).ok_or_else(|| format!("{name}: short"))?
        }
        None => vec![0f32; out],
    };
    Ok((wt, bias))
}

struct Uploader<'a> {
    d: &'a Dev,
    bytes: u64,
    max: u64,
}

impl Uploader<'_> {
    fn buffer(&mut self, label: &str, v: &[f32]) -> Result<wgpu::Buffer, String> {
        let bytes = (v.len() as u64).saturating_mul(4).max(16);
        if bytes > self.max {
            return Err(format!("{label}: {bytes} bytes, more than this GPU takes in one buffer"));
        }
        self.bytes += bytes;
        let mut data = bytemuck::cast_slice::<f32, u8>(v).to_vec();
        data.resize(bytes as usize, 0);
        Ok(self.d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(label),
            contents: &data,
            usage: wgpu::BufferUsages::STORAGE,
        }))
    }

    /// Several linear layers over the same input side by side: `[in, Σ out]`.
    fn linear(&mut self, src: &dyn TensorSource, parts: &[&Linear]) -> Result<Lin, String> {
        let inp = parts.first().map_or(0, |l| l.inp);
        let total: usize = parts.iter().map(|l| l.out).sum();
        if parts.iter().any(|l| l.inp != inp) || !inp.is_multiple_of(4) || !total.is_multiple_of(4) || inp == 0 || total == 0 {
            return Err(format!(
                "{}: sizes {inp} → {total} the kernels do not take (multiples of 4)",
                parts.first().map_or("", |l| l.weight.as_str())
            ));
        }
        let mut w = vec![0f32; inp * total];
        let mut b = Vec::with_capacity(total);
        let mut at = 0;
        for l in parts {
            let (pw, pb) = linear_parts(src, l)?;
            for i in 0..inp {
                if let (Some(dst), Some(s)) = (w.get_mut(i * total + at..i * total + at + l.out), pw.get(i * l.out..(i + 1) * l.out)) {
                    dst.copy_from_slice(s);
                }
            }
            b.extend_from_slice(&pb);
            at += l.out;
        }
        Ok(Lin { w: self.buffer("weights", &w)?, b: self.buffer("bias", &b)?, inp, out: total })
    }

    fn norm(&mut self, src: &dyn TensorSource, n: &Norm, d: usize) -> Result<Ln, String> {
        Ok(Ln { w: self.buffer("norm", &read(src, &n.weight, d)?)?, b: self.buffer("norm", &read(src, &n.bias, d)?)? })
    }
}

fn check_plan(plan: &TowerPlan) -> Result<(), String> {
    let d = plan.hidden;
    if d == 0
        || plan.heads == 0
        || !d.is_multiple_of(plan.heads)
        || !d.is_multiple_of(4)
        || !plan.intermediate.is_multiple_of(4)
        || !plan.out.is_multiple_of(4)
    {
        return Err(format!("{} tower: widths the kernels do not take ({d} wide, {} heads)", plan.name, plan.heads));
    }
    if plan.max_len == 0 || plan.max_len > 4096 || plan.blocks.is_empty() || plan.blocks.len() > 256 {
        return Err(format!("{} tower: {} positions, {} blocks", plan.name, plan.max_len, plan.blocks.len()));
    }
    if let Input::Patches { patches, proj } = &plan.input {
        let rows = *patches + usize::from(plan.class.is_some());
        if rows != plan.max_len || proj.out != d {
            return Err(format!("{} tower: {patches} patches do not fit {} positions", plan.name, plan.max_len));
        }
    }
    if matches!(plan.pool, Pool::Probe(_)) && plan.proj.is_none() && plan.out != d {
        return Err(format!("{} tower: the pooled width is not the output's", plan.name));
    }
    Ok(())
}

fn upload(d: &Dev, plan: &TowerPlan, src: &dyn TensorSource, max: u64) -> Result<Weights, String> {
    let dm = plan.hidden;
    let mut up = Uploader { d, bytes: 0, max };
    let patch = match &plan.input {
        Input::Patches { proj, .. } => Some(up.linear(src, &[proj])?),
        Input::Tokens => None,
    };
    let class = plan.class.as_ref().map(|c| read(src, c, dm).and_then(|v| up.buffer("class", &v))).transpose()?;
    let pos = up.buffer("positions", &read(src, &plan.positions, plan.max_len * dm)?)?;
    let pre_ln = plan.pre_ln.as_ref().map(|n| up.norm(src, n, dm)).transpose()?;
    let mut blocks = Vec::with_capacity(plan.blocks.len());
    for b in &plan.blocks {
        if [&b.q, &b.k, &b.v, &b.o].iter().any(|l| l.inp != dm || l.out != dm)
            || b.fc1.inp != dm
            || b.fc1.out != plan.intermediate
            || b.fc2.inp != plan.intermediate
            || b.fc2.out != dm
        {
            return Err(format!("{} tower: a block's sizes do not match its width", plan.name));
        }
        blocks.push(BlockW {
            ln1: up.norm(src, &b.ln1, dm)?,
            qkv: up.linear(src, &[&b.q, &b.k, &b.v])?,
            o: up.linear(src, &[&b.o])?,
            ln2: up.norm(src, &b.ln2, dm)?,
            fc1: up.linear(src, &[&b.fc1])?,
            fc2: up.linear(src, &[&b.fc2])?,
        });
    }
    let final_ln = up.norm(src, &plan.final_ln, dm)?;
    let probe = match &plan.pool {
        Pool::Pick => None,
        Pool::Probe(h) => {
            let probe = read(src, &h.probe, dm)?;
            let (wq, bq) = linear_parts(src, &h.q)?;
            if h.q.inp != dm || h.q.out != dm {
                return Err(format!("{} tower: the pooling head's sizes do not match its width", plan.name));
            }
            // the probe's query is the same for every input: worked out once, here
            let q: Vec<f32> = (0..dm)
                .map(|o| bq.get(o).copied().unwrap_or(0.0) + (0..dm).map(|i| probe[i] * wq.get(i * dm + o).copied().unwrap_or(0.0)).sum::<f32>())
                .collect();
            Some(ProbeW {
                q: up.buffer("probe", &q)?,
                kv: up.linear(src, &[&h.k, &h.v])?,
                o: up.linear(src, &[&h.o])?,
                ln: up.norm(src, &h.ln, dm)?,
                fc1: up.linear(src, &[&h.fc1])?,
                fc2: up.linear(src, &[&h.fc2])?,
            })
        }
    };
    let proj = plan.proj.as_ref().map(|p| up.linear(src, &[p])).transpose()?;
    if proj.as_ref().is_some_and(|p| p.inp != dm || p.out != plan.out) {
        return Err(format!("{} tower: the projection's sizes do not match", plan.name));
    }
    if probe.as_ref().is_some_and(|p| p.fc1.out != plan.intermediate || p.kv.out != 2 * dm) {
        return Err(format!("{} tower: the pooling head's sizes do not match", plan.name));
    }
    Ok(Weights { patch, class, pos, pre_ln, blocks, final_ln, probe, proj, bytes: up.bytes })
}

// ---------------------------------------------------------------------------------------------------------------------
// kernels

struct Pipes {
    mm_big: wgpu::ComputePipeline,
    mm_small: wgpu::ComputePipeline,
    mm_layout: wgpu::BindGroupLayout,
    bmm: wgpu::ComputePipeline,
    bmm_layout: wgpu::BindGroupLayout,
    norm: wgpu::ComputePipeline,
    softmax: wgpu::ComputePipeline,
    rows_layout: wgpu::BindGroupLayout,
    assemble: wgpu::ComputePipeline,
    gather: wgpu::ComputePipeline,
    embed_layout: wgpu::BindGroupLayout,
}

fn layout(d: &Dev, reads: u32) -> wgpu::BindGroupLayout {
    let mut entries = vec![wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: wgpu::BufferSize::new(SLOT),
        },
        count: None,
    }];
    for i in 1..=reads + 1 {
        entries.push(wgpu::BindGroupLayoutEntry {
            binding: i,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: i <= reads },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        });
    }
    d.device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("tf"), entries: &entries })
}

fn pipeline(d: &Dev, label: &str, src: &str, entry: &str, layout: &wgpu::BindGroupLayout) -> wgpu::ComputePipeline {
    let module = d.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(src.into()) });
    let pl = d.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(layout)], immediate_size: 0 });
    d.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(label),
        layout: Some(&pl),
        module: &module,
        entry_point: Some(entry),
        // every kernel writes its workgroup memory before reading it (see nn.rs: zeroing is slow to compile on DX12)
        compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
        cache: None,
    })
}

/// `wgsl/tf_matmul.wgsl` for `bm × bn` blocks (128 or 64), with each thread's `bm / 16` rows × `bn / 64` vec4
/// columns written out as separate variables.
fn matmul_source(bm: u32, bn: u32) -> String {
    use std::fmt::Write;
    let (tm, nv) = (bm / 16, bn / 64);
    let (mut decl, mut compute, mut store) = (String::new(), String::new(), String::new());
    for i in 0..tm {
        for j in 0..nv {
            let _ = writeln!(decl, "    var c{i}_{j} = vec4<f32>(0.0);");
            let _ = writeln!(store, "    put(mrow + {i}u, ncol + {}u, c{i}_{j});", 16 * j);
        }
    }
    for kk in 0..4 {
        for j in 0..nv {
            let _ = writeln!(compute, "            let b{kk}_{j} = Bs[(kv * 4u + {kk}u) * (BN / 4u) + tx + {}u];", 16 * j);
        }
    }
    for i in 0..tm {
        let _ = writeln!(compute, "            let a{i} = As[(arow + {i}u) * (BK / 4u) + kv];");
        for j in 0..nv {
            let _ = writeln!(
                compute,
                "            c{i}_{j} = fma(vec4<f32>(a{i}.w), b3_{j}, fma(vec4<f32>(a{i}.z), b2_{j}, fma(vec4<f32>(a{i}.y), b1_{j}, fma(vec4<f32>(a{i}.x), b0_{j}, c{i}_{j}))));"
            );
        }
    }
    include_str!("wgsl/tf_matmul.wgsl")
        .replace("{{BM}}", &bm.to_string())
        .replace("{{BN}}", &bn.to_string())
        .replace("{{DECL}}", &decl)
        .replace("{{COMPUTE}}", &compute)
        .replace("{{STORE}}", &store)
}

fn build_pipes(d: &Dev) -> Pipes {
    let mm_layout = layout(d, 3);
    let bmm_layout = layout(d, 2);
    let rows_layout = layout(d, 3);
    let embed_layout = layout(d, 4);
    let rows = include_str!("wgsl/tf_rows.wgsl");
    let embed = include_str!("wgsl/tf_embed.wgsl");
    Pipes {
        mm_big: pipeline(d, "tf_matmul 128", &matmul_source(128, 128), "main", &mm_layout),
        mm_small: pipeline(d, "tf_matmul 64", &matmul_source(64, 64), "main", &mm_layout),
        bmm: pipeline(d, "tf_bmm", include_str!("wgsl/tf_bmm.wgsl"), "main", &bmm_layout),
        norm: pipeline(d, "tf_norm", rows, "norm", &rows_layout),
        softmax: pipeline(d, "tf_softmax", rows, "softmax", &rows_layout),
        assemble: pipeline(d, "tf_assemble", embed, "assemble", &embed_layout),
        gather: pipeline(d, "tf_gather", embed, "gather", &embed_layout),
        mm_layout,
        bmm_layout,
        rows_layout,
        embed_layout,
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// activations and the bound steps

/// The sizes a workspace is made for.
#[derive(Clone, Copy, Debug)]
struct Sizes {
    /// Inputs per pass.
    cap: usize,
    /// Inputs per round of attention scores.
    chunk: usize,
    /// Positions per input.
    len: usize,
    hidden: usize,
    heads: usize,
    inter: usize,
    in_width: usize,
    out: usize,
}

impl Sizes {
    fn floats(&self) -> [(&'static str, usize); 7] {
        let rows = self.cap * self.len;
        let probe_scores = self.cap * self.heads * self.len;
        [
            ("input", rows * self.in_width),
            ("x", rows * self.hidden),
            ("h", rows * self.hidden),
            ("attn", rows * self.hidden),
            ("qkv", rows * 3 * self.hidden),
            ("mlp", rows * self.inter),
            ("scores", (self.chunk * self.heads * self.len * self.len).max(probe_scores)),
        ]
    }

    fn bytes(&self) -> u64 {
        self.floats().iter().map(|(_, f)| *f as u64 * 4).sum::<u64>() + (self.cap * self.out * 8) as u64
    }
}

/// How many inputs fit a pass and a round of scores, or why none do.
fn sizes(plan: &TowerPlan, max_binding: u64) -> Result<Sizes, String> {
    let (len, d, h) = (plan.max_len, plan.hidden, plan.heads);
    let one = Sizes { cap: 1, chunk: 1, len, hidden: d, heads: h, inter: plan.intermediate, in_width: plan.input_width(), out: plan.out };
    let per = one.bytes();
    let widest = one.floats().iter().map(|(_, f)| *f as u64 * 4).max().unwrap_or(0);
    let score_bytes = (h * len * len * 4) as u64;
    let mut cap = (ACTIVATION_BUDGET / per.max(1)).min(max_binding / widest.max(1)) as usize;
    cap = cap.min(MAX_IMAGES);
    let chunk = (SCORES_BUDGET.min(max_binding) / score_bytes.max(1)) as usize;
    if cap == 0 || chunk == 0 {
        return Err(format!("{} tower: one input's activations do not fit this GPU's buffers", plan.name));
    }
    let s = Sizes { cap, chunk: chunk.min(cap), ..one };
    if s.floats().iter().any(|(_, f)| *f as u64 * 4 > max_binding) {
        return Err(format!("{} tower: its activations do not fit this GPU's buffers", plan.name));
    }
    Ok(s)
}

struct MmStep {
    bg: wgpu::BindGroup,
    n: usize,
    k: usize,
    act: u32,
    acc: bool,
}

struct BlockBg {
    ln1: wgpu::BindGroup,
    qkv: MmStep,
    o: MmStep,
    ln2: wgpu::BindGroup,
    fc1: MmStep,
    fc2: MmStep,
}

struct ProbeBg {
    kv: MmStep,
    qk: wgpu::BindGroup,
    pv: wgpu::BindGroup,
    o: MmStep,
    ln: wgpu::BindGroup,
    fc1: MmStep,
    fc2: MmStep,
}

/// Which buffer holds the answer.
#[derive(Clone, Copy)]
enum Answer {
    X,
    Attn,
    Out,
}

struct Workspace {
    input: wgpu::Buffer,
    pick: wgpu::Buffer,
    out: wgpu::Buffer,
    x: wgpu::Buffer,
    attn: wgpu::Buffer,
    staging: wgpu::Buffer,
    params: wgpu::Buffer,
    slots: usize,
    patch: Option<MmStep>,
    assemble: wgpu::BindGroup,
    pre_ln: Option<wgpu::BindGroup>,
    blocks: Vec<BlockBg>,
    qk: wgpu::BindGroup,
    pv: wgpu::BindGroup,
    softmax: wgpu::BindGroup,
    final_ln: wgpu::BindGroup,
    gather: wgpu::BindGroup,
    probe: Option<ProbeBg>,
    proj: Option<MmStep>,
    answer: Answer,
    bytes: u64,
    _keep: Vec<wgpu::Buffer>,
}

fn storage(d: &Dev, label: &str, floats: usize, extra: wgpu::BufferUsages) -> wgpu::Buffer {
    d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (floats as u64 * 4).max(16),
        usage: wgpu::BufferUsages::STORAGE | extra,
        mapped_at_creation: false,
    })
}

fn bind(d: &Dev, layout: &wgpu::BindGroupLayout, params: &wgpu::Buffer, buffers: &[&wgpu::Buffer]) -> wgpu::BindGroup {
    let mut entries = vec![wgpu::BindGroupEntry {
        binding: 0,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: params, offset: 0, size: wgpu::BufferSize::new(SLOT) }),
    }];
    for (i, b) in buffers.iter().enumerate() {
        entries.push(wgpu::BindGroupEntry { binding: i as u32 + 1, resource: b.as_entire_binding() });
    }
    d.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout, entries: &entries })
}

/// The most dispatches one pass makes.
fn max_dispatches(plan: &TowerPlan, s: &Sizes) -> usize {
    let rounds = s.cap.div_ceil(s.chunk.max(1));
    4 + plan.blocks.len() * (6 + 3 * rounds) + 6 + 3 * rounds + 4
}

impl TowerGpu {
    fn workspace(&self) -> Workspace {
        let (d, w, s, p) = (self.dev, &self.weights, &self.sizes, &self.pipes);
        let rows = s.cap * s.len;
        let dm = s.hidden;
        let none = wgpu::BufferUsages::empty();
        let input = storage(d, "tf input", rows * s.in_width, wgpu::BufferUsages::COPY_DST);
        let x = storage(d, "tf x", rows * dm, wgpu::BufferUsages::COPY_SRC);
        let h = storage(d, "tf h", rows * dm, none);
        let attn = storage(d, "tf attn", rows * dm, wgpu::BufferUsages::COPY_SRC);
        let qkv = storage(d, "tf qkv", rows * 3 * dm, none);
        let mlp = storage(d, "tf mlp", rows * s.inter, none);
        let scores = storage(d, "tf scores", s.floats()[6].1, none);
        let pick = storage(d, "tf pick", s.cap, wgpu::BufferUsages::COPY_DST);
        let out = storage(d, "tf out", s.cap * s.out, wgpu::BufferUsages::COPY_SRC);
        let dummy = storage(d, "tf dummy", 4, none);
        let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tf readback"),
            size: (s.cap * s.out * 4).max(16) as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let slots = max_dispatches(&self.plan, s);
        let params = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tf params"),
            size: slots as u64 * SLOT,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mm = |a: &wgpu::Buffer, l: &Lin, y: &wgpu::Buffer, act: u32, acc: bool| MmStep {
            bg: bind(d, &p.mm_layout, &params, &[a, &l.w, &l.b, y]),
            n: l.out,
            k: l.inp,
            act,
            acc,
        };
        let ln = |src: &wgpu::Buffer, n: &Ln, dst: &wgpu::Buffer| bind(d, &p.rows_layout, &params, &[src, &n.w, &n.b, dst]);
        let act = act_code(self.plan.act);
        // embeddings: patches are projected into `attn` first; with a layer norm after them they go to `h`, normed into `x`
        let patch = w.patch.as_ref().map(|l| mm(&input, l, &attn, 0, false));
        let rows_in = if patch.is_some() { &attn } else { &input };
        let assembled = if w.pre_ln.is_some() { &h } else { &x };
        let assemble = bind(d, &p.embed_layout, &params, &[rows_in, w.class.as_ref().unwrap_or(&dummy), &w.pos, &dummy, assembled]);
        let pre_ln = w.pre_ln.as_ref().map(|n| ln(&h, n, &x));
        let blocks = w
            .blocks
            .iter()
            .map(|b| BlockBg {
                ln1: ln(&x, &b.ln1, &h),
                qkv: mm(&h, &b.qkv, &qkv, 0, false),
                o: mm(&attn, &b.o, &x, 0, true),
                ln2: ln(&x, &b.ln2, &h),
                fc1: mm(&h, &b.fc1, &mlp, act, false),
                fc2: mm(&mlp, &b.fc2, &x, 0, true),
            })
            .collect();
        let qk = bind(d, &p.bmm_layout, &params, &[&qkv, &qkv, &scores]);
        let pv = bind(d, &p.bmm_layout, &params, &[&scores, &qkv, &attn]);
        let softmax = bind(d, &p.rows_layout, &params, &[&dummy, &dummy, &dummy, &scores]);
        let final_ln = ln(&x, &w.final_ln, &h);
        let gather = bind(d, &p.embed_layout, &params, &[&h, &dummy, &dummy, &pick, &attn]);
        let probe = w.probe.as_ref().map(|pw| ProbeBg {
            kv: mm(&h, &pw.kv, &qkv, 0, false),
            qk: bind(d, &p.bmm_layout, &params, &[&pw.q, &qkv, &scores]),
            pv: bind(d, &p.bmm_layout, &params, &[&scores, &qkv, &attn]),
            o: mm(&attn, &pw.o, &x, 0, false),
            ln: ln(&x, &pw.ln, &h),
            fc1: mm(&h, &pw.fc1, &mlp, act, false),
            fc2: mm(&mlp, &pw.fc2, &x, 0, true),
        });
        let pooled = if probe.is_some() { &x } else { &attn };
        let proj = w.proj.as_ref().map(|l| mm(pooled, l, &out, 0, false));
        let answer = match (&proj, &probe) {
            (Some(_), _) => Answer::Out,
            (None, Some(_)) => Answer::X,
            (None, None) => Answer::Attn,
        };
        let bytes = s.bytes() + slots as u64 * SLOT;
        Workspace {
            input,
            pick,
            out,
            x,
            attn,
            staging,
            params,
            slots,
            patch,
            assemble,
            pre_ln,
            blocks,
            qk,
            pv,
            softmax,
            final_ln,
            gather,
            probe,
            proj,
            answer,
            bytes,
            _keep: vec![h, qkv, mlp, scores, dummy],
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// one pass

/// The dispatches of one pass, before their parameters go to the GPU.
struct Calls<'a> {
    list: Vec<(&'a wgpu::ComputePipeline, &'a wgpu::BindGroup, [u32; 3])>,
    params: Vec<u8>,
}

impl<'a> Calls<'a> {
    fn push<T: Pod>(&mut self, pipe: &'a wgpu::ComputePipeline, bg: &'a wgpu::BindGroup, params: &T, groups: [u32; 3]) {
        let mut slot = bytemuck::bytes_of(params).to_vec();
        slot.resize(SLOT as usize, 0);
        self.params.extend_from_slice(&slot);
        self.list.push((pipe, bg, groups));
    }

    fn matmul(&mut self, p: &'a Pipes, s: &'a MmStep, m: usize) {
        let (pipe, bm) = if m >= 1024 { (&p.mm_big, 128) } else { (&p.mm_small, 64) };
        let params = MatP { m: m as u32, n: s.n as u32, k: s.k as u32, act: s.act, acc: u32::from(s.acc), pad: [0; 3] };
        self.push(pipe, &s.bg, &params, [s.n.div_ceil(bm) as u32, m.div_ceil(bm) as u32, 1]);
    }

    fn norm(&mut self, p: &'a Pipes, bg: &'a wgpu::BindGroup, rows: usize, cols: usize, eps: f32) {
        let g = spread(rows);
        let params = RowsP { rows: rows as u32, cols: cols as u32, gx: g[0], lq: 1, causal: 0, eps, pad: [0; 2] };
        self.push(&p.norm, bg, &params, g);
    }

    /// Attention of `lq` queries over `lk` keys for `n` inputs, a round of `chunk` inputs at a time: `q`, `k`, `v` are
    /// read from their buffers by strides (`*_row`: numbers per position; `q_shared`: one query row for all inputs),
    /// the result is written to `[n · lq, hidden]` rows.
    #[allow(clippy::too_many_arguments)]
    fn attention(
        &mut self,
        p: &'a Pipes,
        ws: &'a Workspace,
        qk: &'a wgpu::BindGroup,
        pv: &'a wgpu::BindGroup,
        s: &Sizes,
        n: usize,
        lq: usize,
        lk: usize,
        causal: bool,
        q: (usize, usize, bool),
        kv: (usize, usize, usize),
    ) {
        self.attention_lite(p, &ws.softmax, qk, pv, s, n, lq, lk, causal, q, kv);
    }

    #[allow(clippy::too_many_arguments)]
    fn attention_lite(
        &mut self,
        p: &'a Pipes,
        softmax: &'a wgpu::BindGroup,
        qk: &'a wgpu::BindGroup,
        pv: &'a wgpu::BindGroup,
        s: &Sizes,
        n: usize,
        lq: usize,
        lk: usize,
        causal: bool,
        q: (usize, usize, bool),
        kv: (usize, usize, usize),
    ) {
        let (dm, heads) = (s.hidden, s.heads);
        let hd = dm / heads.max(1);
        let (q_off, q_row, q_shared) = q;
        let (k_off, v_off, kv_row) = kv;
        let mut b0 = 0;
        while b0 < n {
            let c = s.chunk.min(n - b0);
            let z = (c * heads) as u32;
            let scores = BmmP {
                m: lq as u32,
                n: lk as u32,
                k: hd as u32,
                heads: heads as u32,
                a_off: if q_shared { q_off } else { q_off + b0 * lq * q_row } as u32,
                a_sb: if q_shared { 0 } else { lq * q_row } as u32,
                a_sh: hd as u32,
                a_si: q_row as u32,
                a_sk: 1,
                b_off: (k_off + b0 * lk * kv_row) as u32,
                b_sb: (lk * kv_row) as u32,
                b_sh: hd as u32,
                b_sk: 1,
                b_sj: kv_row as u32,
                c_off: 0,
                c_sb: (heads * lq * lk) as u32,
                c_sh: (lq * lk) as u32,
                c_si: lk as u32,
                c_sj: 1,
                alpha: (hd as f32).powf(-0.5),
                a_kfast: 1,
                b_kfast: 1,
                pad: [0; 2],
            };
            self.push(&p.bmm, qk, &scores, [lk.div_ceil(64) as u32, lq.div_ceil(64) as u32, z]);
            let rows = c * heads * lq;
            let g = spread(rows);
            let sm = RowsP { rows: rows as u32, cols: lk as u32, gx: g[0], lq: lq as u32, causal: u32::from(causal), eps: 0.0, pad: [0; 2] };
            self.push(&p.softmax, softmax, &sm, g);
            let out = BmmP {
                m: lq as u32,
                n: hd as u32,
                k: lk as u32,
                heads: heads as u32,
                a_off: 0,
                a_sb: (heads * lq * lk) as u32,
                a_sh: (lq * lk) as u32,
                a_si: lk as u32,
                a_sk: 1,
                b_off: (v_off + b0 * lk * kv_row) as u32,
                b_sb: (lk * kv_row) as u32,
                b_sh: hd as u32,
                b_sk: kv_row as u32,
                b_sj: 1,
                c_off: (b0 * lq * dm) as u32,
                c_sb: (lq * dm) as u32,
                c_sh: hd as u32,
                c_si: dm as u32,
                c_sj: 1,
                alpha: 1.0,
                a_kfast: 1,
                b_kfast: 0,
                pad: [0; 2],
            };
            self.push(&p.bmm, pv, &out, [hd.div_ceil(64) as u32, lq.div_ceil(64) as u32, z]);
            b0 += c;
        }
    }
}

/// A tower on the GPU. One pass runs at a time; calls from other threads wait.
pub struct TowerGpu {
    dev: &'static Dev,
    plan: TowerPlan,
    sizes: Sizes,
    pipes: Pipes,
    weights: Weights,
    ws: Mutex<Option<Workspace>>,
    ws_bytes: u64,
}

/// Put `plan`'s tower on the GPU, reading its weights from `src`, or say why it cannot be (no GPU, a size the kernels
/// do not take, too little memory). Creates the device on first use and compiles the kernels.
pub fn runner(plan: &TowerPlan, src: &dyn TensorSource) -> Result<TowerGpu, String> {
    let _work = crate::exit::enter().ok_or(CLOSING)?;
    let d = dev()?;
    check_plan(plan)?;
    let max_binding = d.limits.max_storage_buffer_binding_size.min(d.limits.max_buffer_size).min(MAX_BUFFER);
    let sizes = sizes(plan, max_binding)?;
    let scopes = [
        d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
        d.device.push_error_scope(wgpu::ErrorFilter::Validation),
        d.device.push_error_scope(wgpu::ErrorFilter::Internal),
    ];
    // the driver compiles the kernels here: a crash in it is reported at the next launch (issue #250)
    let pipes = crate::backend::with_marker("content model kernel compilation", || build_pipes(d));
    let weights = upload(d, plan, src, max_binding);
    let mut g = weights.map(|weights| TowerGpu { dev: d, plan: plan.clone(), sizes, pipes, weights, ws: Mutex::new(None), ws_bytes: 0 });
    if let Ok(t) = &mut g {
        let ws = t.workspace();
        t.ws_bytes = ws.bytes;
        *t.ws.lock().unwrap_or_else(|e| e.into_inner()) = Some(ws);
    }
    let [oom, validation, internal] = scopes;
    let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
    if let Some(e) = errors.into_iter().flatten().next() {
        return Err(format!("{}: the {} tower could not be set up on the GPU ({e})", d.info.name, plan.name));
    }
    let t = g?;
    log::info!(
        "gpu (content models): {} tower on {}: {:.0} MB of weights, {:.0} MB for {} inputs at a time",
        plan.name,
        d.name(),
        t.weights.bytes as f64 / 1e6,
        t.ws_bytes as f64 / 1e6,
        t.sizes.cap
    );
    Ok(t)
}

impl TowerGpu {
    /// Device memory this tower holds: weights and activations (bytes).
    pub fn bytes(&self) -> u64 {
        self.weights.bytes + self.ws_bytes
    }

    /// Inputs per pass.
    pub fn batch(&self) -> usize {
        self.sizes.cap
    }

    /// One pass of `n ≤ cap` inputs of `len` positions.
    fn pass(&self, ws: &Workspace, input: &[f32], n: usize, len: usize, pick: &[u32]) -> Result<Vec<f32>, String> {
        let (d, p, s, w) = (self.dev, &self.pipes, &self.sizes, &self.weights);
        let dm = s.hidden;
        let rows = n * len;
        let mut calls = Calls { list: Vec::new(), params: Vec::new() };
        // embeddings
        let tokens_in = match (&ws.patch, &self.plan.input) {
            (Some(step), Input::Patches { patches, .. }) => {
                calls.matmul(p, step, n * patches);
                *patches
            }
            _ => len,
        };
        let cls = usize::from(w.class.is_some());
        let total = rows * dm;
        let g = spread(total.div_ceil(256));
        let ep = EmbedP { total: total as u32, cols: dm as u32, len: len as u32, cls: cls as u32, gx: g[0], pad: [0; 3] };
        if tokens_in + cls != len {
            return Err("the input does not fit the tower".into());
        }
        calls.push(&p.assemble, &ws.assemble, &ep, g);
        let eps = self.plan.eps;
        if let Some(bg) = &ws.pre_ln {
            calls.norm(p, bg, rows, dm, eps);
        }
        for b in &ws.blocks {
            calls.norm(p, &b.ln1, rows, dm, eps);
            calls.matmul(p, &b.qkv, rows);
            calls.attention(p, ws, &ws.qk, &ws.pv, s, n, len, len, self.plan.causal, (0, 3 * dm, false), (dm, 2 * dm, 3 * dm));
            calls.matmul(p, &b.o, rows);
            calls.norm(p, &b.ln2, rows, dm, eps);
            calls.matmul(p, &b.fc1, rows);
            calls.matmul(p, &b.fc2, rows);
        }
        calls.norm(p, &ws.final_ln, rows, dm, eps);
        match &ws.probe {
            Some(pb) => {
                calls.matmul(p, &pb.kv, rows);
                calls.attention(p, ws, &pb.qk, &pb.pv, s, n, 1, len, false, (0, dm, true), (0, dm, 2 * dm));
                calls.matmul(p, &pb.o, n);
                calls.norm(p, &pb.ln, n, dm, eps);
                calls.matmul(p, &pb.fc1, n);
                calls.matmul(p, &pb.fc2, n);
            }
            None => {
                let total = n * dm;
                let g = spread(total.div_ceil(256));
                let gp = EmbedP { total: total as u32, cols: dm as u32, len: len as u32, cls: 0, gx: g[0], pad: [0; 3] };
                calls.push(&p.gather, &ws.gather, &gp, g);
            }
        }
        if let Some(step) = &ws.proj {
            calls.matmul(p, step, n);
        }
        if calls.list.len() > ws.slots {
            return Err(format!("{} dispatches, room for {}", calls.list.len(), ws.slots));
        }
        let out_bytes = (n * s.out * 4) as u64;
        let answer = match ws.answer {
            Answer::Out => &ws.out,
            Answer::X => &ws.x,
            Answer::Attn => &ws.attn,
        };
        let scopes = [
            d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory),
            d.device.push_error_scope(wgpu::ErrorFilter::Validation),
            d.device.push_error_scope(wgpu::ErrorFilter::Internal),
        ];
        d.queue.write_buffer(&ws.params, 0, &calls.params);
        d.queue.write_buffer(&ws.input, 0, bytemuck::cast_slice(input));
        if !pick.is_empty() {
            d.queue.write_buffer(&ws.pick, 0, bytemuck::cast_slice(pick));
        }
        let chunks: Vec<_> = calls.list.chunks(PER_SUBMIT).collect();
        let last = chunks.len().saturating_sub(1);
        for (i, part) in chunks.into_iter().enumerate() {
            let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("content model") });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("content model"), timestamp_writes: None });
                for (j, (pipe, bg, groups)) in part.iter().enumerate() {
                    let slot = ((i * PER_SUBMIT + j) as u64 * SLOT) as u32;
                    pass.set_pipeline(pipe);
                    pass.set_bind_group(0, *bg, &[slot]);
                    pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
                }
            }
            if i == last {
                enc.copy_buffer_to_buffer(answer, 0, &ws.staging, 0, out_bytes);
            }
            d.queue.submit([enc.finish()]);
        }
        let slice = ws.staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
        let [oom, validation, internal] = scopes;
        let errors = [pollster::block_on(internal.pop()), pollster::block_on(validation.pop()), pollster::block_on(oom.pop())];
        let [internal, validation, oom] = errors;
        if let Some(e) = oom {
            return Err(format!("the GPU ran out of memory: {e}"));
        }
        if let Some(e) = internal.or(validation) {
            BROKEN.mark(&format!("device error: {e}"));
            return Err(format!("the GPU reported an error: {e}"));
        }
        if let Err(e) = waited {
            BROKEN.mark(&format!("the GPU did not finish a batch: {e}"));
            return Err(format!("the GPU did not finish: {e}"));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("reading the result back failed: {e}")),
            Err(e) => return Err(format!("reading the result back failed: {e}")),
        }
        let out = {
            let data = slice.get_mapped_range().map_err(|e| format!("reading the result back failed: {e:?}"))?;
            data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>()
        };
        ws.staging.unmap();
        Ok(out)
    }

    fn run_all(&self, input: &[f32], n: usize, pick: &[u32]) -> Result<Vec<f32>, String> {
        let (s, plan) = (&self.sizes, &self.plan);
        let in_rows = input.len().checked_div(s.in_width).unwrap_or(0);
        if n == 0 || in_rows * s.in_width != input.len() || !in_rows.is_multiple_of(n) {
            return Err(format!("{} numbers for {n} inputs of width {}", input.len(), s.in_width));
        }
        let rows_each = in_rows / n;
        let len = match &plan.input {
            Input::Patches { patches, .. } if rows_each == *patches => s.len,
            Input::Patches { .. } => return Err(format!("{rows_each} patches per image, the tower takes {}", s.len)),
            Input::Tokens if (1..=s.len).contains(&rows_each) => rows_each,
            Input::Tokens => return Err(format!("{rows_each} tokens per text, the tower takes at most {}", s.len)),
        };
        let picking = matches!(plan.pool, Pool::Pick);
        if picking && (pick.len() != n || pick.iter().any(|&p| p as usize >= len)) {
            return Err("a position to pool is missing or outside the input".into());
        }
        let mut guard = self.ws.lock().unwrap_or_else(|e| e.into_inner());
        let ws = match guard.take() {
            Some(ws) => ws,
            None => {
                let d = self.dev;
                let scopes = [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];
                let ws = self.workspace();
                let [oom, validation] = scopes;
                let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
                if let Some(e) = oom.or(validation) {
                    return Err(format!("the GPU could not make room for the model's work: {e}"));
                }
                ws
            }
        };
        let mut out = Vec::with_capacity(n * s.out);
        let per = rows_each * s.in_width;
        let mut done = 0;
        while done < n {
            let c = s.cap.min(n - done);
            let part = input.get(done * per..(done + c) * per).ok_or("input short")?;
            let picks = if picking { pick.get(done..done + c).ok_or("pick short")? } else { &[][..] };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.pass(&ws, part, c, len, picks)));
            match r {
                Ok(Ok(v)) => out.extend(v),
                Ok(Err(e)) => {
                    // its readback buffer may be left half-mapped: a fresh workspace is made when one is next needed
                    return Err(e);
                }
                Err(_) => {
                    BROKEN.mark("a content model pass panicked");
                    return Err("the GPU runner gave up".into());
                }
            }
            done += c;
        }
        *guard = Some(ws);
        Ok(out)
    }
}

impl TowerRunner for TowerGpu {
    fn run(&self, input: &[f32], n: usize, pick: &[u32]) -> Result<Vec<f32>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if FAIL_NEXT.swap(false, Ordering::SeqCst) {
            BROKEN.mark("device lost (simulated)");
            return Err("device lost (simulated)".into());
        }
        // one batch is one piece of GPU work: quitting waits for it
        let Some(_work) = crate::exit::enter() else { return Err(CLOSING.into()) };
        self.run_all(input, n, pick)
    }

    fn usable(&self) -> Result<(), String> {
        if let Some(why) = broken_reason() {
            return Err(why);
        }
        if let Some(why) = crate::switched_off() {
            return Err(why);
        }
        if crate::shutting_down() {
            return Err(CLOSING.into());
        }
        Ok(())
    }

    fn device(&self) -> String {
        self.dev.info.name.clone()
    }
}

#[cfg(test)]
mod tests;
