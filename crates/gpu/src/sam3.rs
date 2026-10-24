//! The SAM 3 vision backbone on the GPU: a [`lightcraft_segment::BackboneAccel`] that runs the 32-layer ViT of
//! `lightcraft-segment` (`vit.rs`, the reference) with WGSL compute kernels, so AI masks (Object, Subject, Describe)
//! need not wait ~40 s per photo for the CPU on machines without Metal.
//!
//! What runs (`docs/gpu-pipeline.md` → SAM 3 backbone): the patch embedding as im2col and a matrix product, the
//! 24 × 24 position table, `ln_pre`, then per layer layer norm → one q|k|v product → 2-D RoPE → attention (24 × 24
//! windows, or all 5184 tokens in layers 7, 15, 23 and 31) → output projection and residual → layer norm → MLP
//! (GELU, erf form) and residual. The tokens stay in *window order* the whole way (window-major, row-major inside a
//! window): a window's tokens are then a contiguous range for attention, the position table repeats per window, and
//! the global layers don't care about order (their RoPE table is stored in the same order). Only the last step puts
//! them back on the 72 × 72 grid, channels-first.
//!
//! Kernels: `wgsl/sam3_matmul.wgsl` (tiled, register-blocked; bias, GELU and residual in its epilogue),
//! `wgsl/sam3_attention.wgsl` (flash style: online softmax, the 16 × 5184² scores of a global layer never exist),
//! `wgsl/sam3_ops.wgsl` (layer norm, RoPE, im2col, the final reorder). All of the dispatches, with their parameters
//! and bind groups, are built once with the weights; a forward pass uploads the pixels, submits one command buffer
//! per layer and reads the result back once. No subgroups or shader-f16 needed. The two necks that follow the
//! backbone run on the same device, built when first asked for (`sam3/neck.rs`). The text detector's heads stay in
//! candle on the CPU.
//!
//! The device is its own (like denoise's and faces'): it respects the same switches as GPU rendering
//! (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, `LIGHTCRAFT_GPU_ADAPTER`, the GPU preference), the crash sentinel
//! around device creation and the shutdown gate. A device error or loss stops its use for the process; every
//! failure is an `Err` (the caller then runs the candle backbone), never a panic.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytemuck::Pod;
use lightcraft_segment::{BACKBONE_PREFIX, BackboneAccel, NeckKind, WeightReader};
use wgpu::util::DeviceExt;

mod neck;
#[cfg(test)]
mod tests;

/// Input side (pixels), patch size, patches per side.
const IMAGE: usize = 1008;
const PATCH: usize = 14;
const GRID: usize = IMAGE / PATCH;
const TOKENS: usize = GRID * GRID;
const HIDDEN: usize = 1024;
const HEADS: usize = 16;
const HEAD_DIM: usize = HIDDEN / HEADS;
const MLP: usize = 4736;
const LAYERS: usize = 32;
const WINDOW: usize = 24;
const GLOBAL: [usize; 4] = [7, 15, 23, 31];
/// The pretraining grid of the position table (336 / 14), tiled over the 72 × 72 grid.
const POS_SIDE: usize = 24;
const EPS: f32 = 1e-6;
/// Numbers per patch (3 × 14 × 14): the patch embedding's K.
const PATCH_K: usize = 3 * PATCH * PATCH;

/// Longest wait for one forward pass before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Workgroup memory the attention kernel needs: its key and value tiles (2 × [`ATTENTION_KEYS`] × 64 floats).
const ATTENTION_BYTES: u32 = 2 * ATTENTION_KEYS * HEAD_DIM as u32 * 4;
/// Keys per step of the attention kernel.
const ATTENTION_KEYS: u32 = 32;
/// The matrix product's blocking.
const TILE: Tile = Tile { rows: 4, cols: 4, bk: 16 };

/// How the matrix-product kernel blocks its work: each of its 16 × 16 threads makes `rows × cols` outputs (so a
/// workgroup makes 16 · rows × 16 · cols), walking k in steps of `bk` (a multiple of 8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Tile {
    rows: u32,
    cols: u32,
    bk: u32,
}

impl Tile {
    fn bm(self) -> usize {
        16 * self.rows as usize
    }

    fn bn(self) -> usize {
        16 * self.cols as usize
    }

    /// Workgroup memory: both tiles, k-contiguous, each row padded by one vector.
    fn workgroup_bytes(self) -> u32 {
        (self.bm() + self.bn()) as u32 * (self.bk / 4 + 1) * 16
    }
}

/// A matrix-product pipeline and its blocking.
struct Matmul {
    pipeline: wgpu::ComputePipeline,
    tile: Tile,
}

// ---------------------------------------------------------------------------------------------------------------------
// the device

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    k: Kernels,
    /// Bound where a kernel has fewer operands than bindings.
    dummy: wgpu::Buffer,
}

struct Kernels {
    matmul: Matmul,
    /// The same with f16 weights, compiled when first needed.
    matmul16: OnceLock<Matmul>,
    attention: wgpu::ComputePipeline,
    layernorm: wgpu::ComputePipeline,
    rope: wgpu::ComputePipeline,
    im2col: wgpu::ComputePipeline,
    to_chw: wgpu::ComputePipeline,
}

static DEV: OnceLock<Result<Dev, String>> = OnceLock::new();
static BROKEN: AtomicBool = AtomicBool::new(false);
static BROKEN_WHY: Mutex<Option<String>> = Mutex::new(None);

fn mark_broken(why: &str) {
    let mut w = BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner);
    if w.is_none() {
        *w = Some(why.to_string());
    }
    BROKEN.store(true, Ordering::Relaxed);
}

/// Why the GPU stopped being used for SAM 3 (a device error), if it did.
pub fn broken_reason() -> Option<String> {
    BROKEN.load(Ordering::Relaxed).then(|| BROKEN_WHY.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "device error".into()))
}

fn dev() -> Result<&'static Dev, String> {
    if let Some(why) = broken_reason() {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    // creating the device is GPU work too: not once the process is ending (issue #620)
    let _work = match DEV.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or("LightCraft is closing")?),
    };
    DEV.get_or_init(create_device).as_ref().map_err(|e| e.clone())
}

fn create_device() -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    crate::backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| make_device(backends)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

/// The largest buffer the backbone binds: the MLP's hidden activations (5184 × 4736 floats, 98 MB).
const LARGEST_BUFFER: u64 = (TOKENS * MLP * 4) as u64;

/// The limits the kernels need: the WebGPU downlevel defaults, with room for the largest buffer and the tiles.
fn needed_limits() -> wgpu::Limits {
    wgpu::Limits {
        max_storage_buffer_binding_size: LARGEST_BUFFER,
        max_buffer_size: LARGEST_BUFFER,
        max_compute_workgroup_storage_size: ATTENTION_BYTES.max(TILE.workgroup_bytes()),
        max_compute_invocations_per_workgroup: 256,
        max_compute_workgroup_size_x: 256,
        ..wgpu::Limits::downlevel_defaults()
    }
}

/// The limits an adapter with `have` falls short of ([`needed_limits`]), as "name have < want"; empty when it has them all.
/// Such an adapter gets no device: SAM 3 then runs in candle on the CPU, and Settings says why.
fn short_limits(have: &wgpu::Limits) -> Vec<String> {
    let mut short = Vec::new();
    needed_limits().check_limits_with_fail_fn(have, false, |name, want, have| short.push(format!("{name} {have} < {want}")));
    short
}

fn make_device(backends: wgpu::Backends) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = wgpu::Instance::new(desc);
    let wanted = std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let adapter = match &wanted {
        Some(w) => pollster::block_on(instance.enumerate_adapters(backends))
            .into_iter()
            .find(|a| {
                let i = a.get_info();
                i.device_type != wgpu::DeviceType::Cpu && i.name.to_lowercase().contains(w.as_str())
            })
            .ok_or_else(|| format!("no GPU adapter named like `{w}`"))?,
        None => pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?,
    };
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let need = needed_limits();
    let short = short_limits(&adapter.limits());
    if !short.is_empty() {
        return Err(format!("{} is too limited for the SAM 3 kernels ({})", info.name, short.join(", ")));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("lightcraft-sam3"),
        required_limits: need,
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    device.on_uncaptured_error(Arc::new(|e| {
        log::error!("gpu (sam3): {e}");
        mark_broken(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(|reason, msg| {
        log::error!("gpu (sam3): device lost ({reason:?}): {msg}");
        mark_broken(&format!("device lost ({reason:?}): {msg}"));
    });
    // a kernel the driver or naga rejects fails the device here, not the first forward pass
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    use wgpu::BufferBindingType::{Storage, Uniform};
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("sam3"),
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: false }),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let pipe = |label: &str, source: &str, entry: &str| pipeline(&device, &pl, label, source, entry);
    let ops = include_str!("wgsl/sam3_ops.wgsl");
    let k = Kernels {
        matmul: Matmul { pipeline: pipe("sam3 matmul", &matmul_source(TILE, false), "main"), tile: TILE },
        matmul16: OnceLock::new(),
        attention: pipe("sam3 attention", &attention_source(ATTENTION_KEYS), "main"),
        layernorm: pipe("sam3 ops", ops, "layernorm"),
        rope: pipe("sam3 ops", ops, "rope"),
        im2col: pipe("sam3 ops", ops, "im2col"),
        to_chw: pipe("sam3 ops", ops, "to_chw"),
    };
    let dummy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sam3 unused"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    if let Some(e) = pollster::block_on(validation.pop()) {
        return Err(format!("{}: the SAM 3 kernels did not compile ({e})", info.name));
    }
    let limits = device.limits();
    log::info!("gpu (sam3): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits, layout, pipeline_layout: pl, k, dummy })
}

impl Dev {
    fn matmul16(&self) -> &Matmul {
        self.k.matmul16.get_or_init(|| Matmul {
            pipeline: pipeline(&self.device, &self.pipeline_layout, "sam3 matmul f16", &matmul_source(TILE, true), "main"),
            tile: TILE,
        })
    }
}

fn pipeline(device: &wgpu::Device, layout: &wgpu::PipelineLayout, label: &str, source: &str, entry: &str) -> wgpu::ComputePipeline {
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(entry),
        layout: Some(layout),
        module: &module,
        entry_point: Some(entry),
        // every kernel writes its workgroup memory before reading it
        compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
        cache: None,
    })
}

/// The matrix-product kernel for `tile` (`bk` 16 with `f16` weights), written out from `wgsl/sam3_matmul.wgsl`: one
/// variable per accumulator and per prefetched vector.
fn matmul_source(tile: Tile, f16: bool) -> String {
    matmul_source_with(tile, f16, None)
}

/// [`matmul_source`] with `load_a` (the A-tile loader) from `a_loader` instead of the dense one (the neck's 3 × 3
/// convolution, `sam3/neck.rs`).
fn matmul_source_with(tile: Tile, f16: bool, a_loader: Option<&str>) -> String {
    let Tile { rows, cols, bk } = tile;
    use std::fmt::Write;
    let mut src = String::new();
    // keep the B-tile loader of this weight type, and the A-tile loader asked for
    let mut keep = true;
    for line in include_str!("wgsl/sam3_matmul.wgsl").lines() {
        match line.trim() {
            "// #if f32" => keep = !f16,
            "// #if f16" => keep = f16,
            "// #if dense" => keep = a_loader.is_none(),
            "// #endif" => keep = true,
            _ if keep => {
                src.push_str(line);
                src.push('\n');
            }
            _ => {}
        }
    }
    src.push_str(a_loader.unwrap_or_default());
    let (bm, bn) = (16 * rows, 16 * cols);
    let a_vecs = bm * bk / 4;
    let b_vecs = if f16 { bn * bk / 8 } else { bn * bk / 4 };
    let (mut decl, mut load, mut next, mut store_tile, mut compute, mut store) =
        (String::new(), String::new(), String::new(), String::new(), String::new(), String::new());
    let w = |s: &mut String, line: String| {
        s.push_str(&line);
        s.push('\n');
    };
    for i in 0..rows {
        let row: Vec<String> = (0..cols).map(|j| format!("var c{i}_{j} = 0.0;")).collect();
        w(&mut decl, format!("    {}", row.join(" ")));
    }
    let b_type = if f16 { "array<vec4<f32>, 2>" } else { "vec4<f32>" };
    let b_load = if f16 { "load_b16" } else { "load_b" };
    for (l, start) in (0..a_vecs).step_by(256).enumerate() {
        w(&mut decl, format!("    var pa{l}: vec4<f32>;"));
        let guard = if start + 256 > a_vecs { format!("if (tid + {start}u < {a_vecs}u) ") } else { String::new() };
        w(&mut load, format!("    {guard}{{ pa{l} = load_a(m0, 0u, tid + {start}u); }}"));
        w(&mut next, format!("            {guard}{{ pa{l} = load_a(m0, k0 + BK, tid + {start}u); }}"));
        w(&mut store_tile, format!("        {guard}{{ let e = tid + {start}u; As[e / KV * KS + e % KV] = pa{l}; }}"));
    }
    for (l, start) in (0..b_vecs).step_by(256).enumerate() {
        w(&mut decl, format!("    var pb{l}: {b_type};"));
        let guard = if start + 256 > b_vecs { format!("if (tid + {start}u < {b_vecs}u) ") } else { String::new() };
        w(&mut load, format!("    {guard}{{ pb{l} = {b_load}(n0, 0u, tid + {start}u); }}"));
        w(&mut next, format!("            {guard}{{ pb{l} = {b_load}(n0, k0 + BK, tid + {start}u); }}"));
        let put = if f16 {
            format!("let r = e / (BK / 8u) * KS + 2u * (e % (BK / 8u)); Bs[r] = pb{l}[0]; Bs[r + 1u] = pb{l}[1];")
        } else {
            format!("Bs[e / KV * KS + e % KV] = pb{l};")
        };
        w(&mut store_tile, format!("        {guard}{{ let e = tid + {start}u; {put} }}"));
    }
    for i in 0..rows {
        w(&mut compute, format!("            let a{i} = As[(ty + {}u) * KS + kv];", 16 * i));
    }
    for j in 0..cols {
        w(&mut compute, format!("            let b{j} = Bs[(tx + {}u) * KS + kv];", 16 * j));
    }
    for c in ["x", "y", "z", "w"] {
        for i in 0..rows {
            let mut line = String::from("           ");
            for j in 0..cols {
                // writing into a String cannot fail
                let _ = write!(line, " c{i}_{j} = fma(a{i}.{c}, b{j}.{c}, c{i}_{j});");
            }
            w(&mut compute, line);
        }
    }
    for i in 0..rows {
        w(&mut store, format!("    {{\n        let m = m0 + ty + {}u;\n        if (m < p.m) {{", 16 * i));
        for j in 0..cols {
            w(&mut store, format!("            put(m, n0 + tx + {}u, c{i}_{j});", 16 * j));
        }
        w(&mut store, "        }\n    }".into());
    }
    src.replace("{{WEIGHT}}", if f16 { "vec4<u32>" } else { "vec4<f32>" })
        .replace("{{BM}}", &bm.to_string())
        .replace("{{BN}}", &bn.to_string())
        .replace("{{BK}}", &bk.to_string())
        .replace("{{DECL}}", &decl)
        .replace("{{LOAD}}", &load)
        .replace("{{LOAD_NEXT}}", &next)
        .replace("{{STORE_TILE}}", &store_tile)
        .replace("{{COMPUTE}}", &compute)
        .replace("{{STORE}}", &store)
}

/// The attention kernel for `keys` keys per step, written out from `wgsl/sam3_attention.wgsl`: one variable per q
/// and output vector and per score.
fn attention_source(keys: u32) -> String {
    use std::fmt::Write;
    let d4 = (HEAD_DIM / 4) as u32;
    let (mut decl, mut scores, mut rescale, mut acc, mut store) = (String::new(), String::new(), String::new(), String::new(), String::new());
    // writing into a String cannot fail
    for c in 0..d4 {
        let _ = writeln!(decl, "    let q{c} = qkv[qa + {c}u];\n    var o{c} = vec4<f32>(0.0);");
        let _ = writeln!(rescale, "        o{c} *= fix;");
        let _ = writeln!(store, "    out[oa + {c}u] = o{c} * r;");
    }
    for j in 0..keys {
        let terms: Vec<String> = (0..d4).map(|c| format!("q{c} * Ks[{}u]", j * d4 + c)).collect();
        let _ = writeln!(
            scores,
            "        let d{j} = {};\n        let s{j} = d{j}.x + d{j}.y + d{j}.z + d{j}.w;\n        top = max(top, s{j});",
            terms.join(" + ")
        );
        let _ = writeln!(acc, "        {{\n            let w = exp(s{j} - top);\n            l += w;");
        for c in 0..d4 {
            let _ = writeln!(acc, "            o{c} = fma(vec4<f32>(w), Vs[{}u], o{c});", j * d4 + c);
        }
        let _ = writeln!(acc, "        }}");
    }
    include_str!("wgsl/sam3_attention.wgsl")
        .replace("{{KB}}", &keys.to_string())
        .replace("{{DECL}}", &decl)
        .replace("{{SCORES}}", &scores)
        .replace("{{RESCALE}}", &rescale)
        .replace("{{ACCUMULATE}}", &acc)
        .replace("{{STORE}}", &store)
}

/// The adapter the SAM 3 backbone would use ("NVIDIA GeForce RTX 5070 (Vulkan)"), or why it cannot use one. Creates
/// the device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

// ---------------------------------------------------------------------------------------------------------------------
// steps: one dispatch each, recorded once

/// One dispatch: a kernel, its parameters, its buffers (bindings 1–3 read, 4 written) and its workgroup counts.
struct Pending {
    pipeline: &'static wgpu::ComputePipeline,
    params: [u32; 8],
    buffers: [wgpu::Buffer; 4],
    groups: [u32; 3],
}

/// A recorded dispatch, ready to replay.
struct Step {
    pipeline: &'static wgpu::ComputePipeline,
    bind: wgpu::BindGroup,
    groups: [u32; 3],
}

fn u32_of(v: usize) -> Result<u32, String> {
    u32::try_from(v).ok().filter(|v| *v < (1 << 31)).ok_or_else(|| "a tensor too large for the SAM 3 kernels".to_string())
}

/// How the linear layers' weights are stored on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Precision {
    /// As in the checkpoint.
    F32,
    /// Halves, two per u32 (along k): half the memory and weight traffic; the products still run in f32.
    F16,
}

/// Builds a list of dispatches.
struct Graph {
    dev: &'static Dev,
    steps: Vec<Pending>,
    /// Where command buffers end (indices into `steps`): one submission each.
    cuts: Vec<usize>,
}

impl Graph {
    fn new(dev: &'static Dev) -> Graph {
        Graph { dev, steps: Vec::new(), cuts: Vec::new() }
    }

    /// Workgroups for `count` items of 256 (or `count` workgroups), and the row length the kernel reads back.
    fn flat(&self, count: usize) -> Result<([u32; 3], u32), String> {
        let limit = (self.dev.limits.max_compute_workgroups_per_dimension as usize).min(65_535);
        let blocks = count.max(1);
        let row = blocks.min(limit);
        let rows = blocks.div_ceil(row);
        if rows > limit {
            return Err("a step too large for the SAM 3 kernels".into());
        }
        Ok(([u32_of(row)?, u32_of(rows)?, 1], u32_of(row)?))
    }

    fn push(&mut self, pipeline: &'static wgpu::ComputePipeline, params: [u32; 8], buffers: [&wgpu::Buffer; 4], groups: [u32; 3]) {
        let buffers = buffers.map(Clone::clone);
        self.steps.push(Pending { pipeline, params, buffers, groups });
    }

    /// End a command buffer here.
    fn cut(&mut self) {
        if self.cuts.last() != Some(&self.steps.len()) {
            self.cuts.push(self.steps.len());
        }
    }

    /// `y = act(x · wᵀ + bias[row % bias_rows])` (or `y +=` with `residual`): x `[m, k]`, w `[n, k]` (f32, or f16
    /// pairs), bias `[bias_rows, n]`, y `[m, n]`.
    #[allow(clippy::too_many_arguments)]
    fn matmul(
        &mut self,
        [x, w, bias, y]: [&wgpu::Buffer; 4],
        (m, n, k): (usize, usize, usize),
        bias_rows: usize,
        gelu: bool,
        residual: bool,
        precision: Precision,
    ) -> Result<(), String> {
        let kernel = match precision {
            Precision::F32 => &self.dev.k.matmul,
            Precision::F16 => self.dev.matmul16(),
        };
        self.matmul_with(kernel, precision, [x, w, bias, y], (m, n, k), bias_rows, gelu, residual)
    }

    /// [`Graph::matmul`] with the pipeline `kernel` (benchmarks of other tiles).
    #[allow(clippy::too_many_arguments)]
    fn matmul_with(
        &mut self,
        kernel: &'static Matmul,
        precision: Precision,
        [x, w, bias, y]: [&wgpu::Buffer; 4],
        (m, n, k): (usize, usize, usize),
        bias_rows: usize,
        gelu: bool,
        residual: bool,
    ) -> Result<(), String> {
        let step = match precision {
            Precision::F32 => 4,
            Precision::F16 => kernel.tile.bk as usize,
        };
        if !k.is_multiple_of(step) || bias_rows == 0 {
            return Err(format!("a {m}×{k}·{k}×{n} product the SAM 3 kernel cannot do"));
        }
        let params = [u32_of(m)?, u32_of(n)?, u32_of(k)?, u32_of(bias_rows)?, u32::from(gelu), u32::from(residual), 0, 0];
        let groups = [u32_of(n.div_ceil(kernel.tile.bn()))?, u32_of(m.div_ceil(kernel.tile.bm()))?, 1];
        self.push(&kernel.pipeline, params, [x, w, bias, y], groups);
        Ok(())
    }

    /// Layer norm of `rows` rows of `c` numbers: `y = (x − mean) / sqrt(var + eps) · gamma + beta`.
    fn layernorm(&mut self, [x, gamma, beta, y]: [&wgpu::Buffer; 4], rows: usize, c: usize) -> Result<(), String> {
        let (g, row) = self.flat(rows)?;
        self.push(&self.dev.k.layernorm, [u32_of(rows)?, u32_of(c)?, 0, 0, EPS.to_bits(), 0, u32_of(rows)?, row], [x, gamma, beta, y], g);
        Ok(())
    }

    /// RoPE on the q and k of `qkv` (`[tokens, stride]`, q | k | v each `hidden` wide) in place, from a table of
    /// `table_rows` positions (token t uses row t % table_rows), and q scaled by `scale`.
    fn rope(
        &mut self,
        qkv: &wgpu::Buffer,
        table: &wgpu::Buffer,
        (tokens, stride, hidden): (usize, usize, usize),
        table_rows: usize,
        scale: f32,
    ) -> Result<(), String> {
        if hidden % HEAD_DIM != 0 || table_rows == 0 {
            return Err("a RoPE the SAM 3 kernel cannot do".into());
        }
        let count = tokens.checked_mul(hidden).ok_or("too many tokens")?;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let dummy = &self.dev.dummy;
        let params = [u32_of(tokens)?, u32_of(stride)?, u32_of(hidden)?, u32_of(table_rows)?, 0, scale.to_bits(), u32_of(count)?, row];
        self.push(&self.dev.k.rope, params, [dummy, table, dummy, qkv], g);
        Ok(())
    }

    /// Attention of every head over `qkv` (`[tokens, stride]`, q | k | v each `hidden` wide) into `out`
    /// (`[tokens, hidden]`), each query over the `span` tokens of its window.
    fn attention(
        &mut self,
        qkv: &wgpu::Buffer,
        out: &wgpu::Buffer,
        (tokens, stride, hidden): (usize, usize, usize),
        span: usize,
    ) -> Result<(), String> {
        if span == 0
            || !span.is_multiple_of(64)
            || !span.is_multiple_of(ATTENTION_KEYS as usize)
            || !tokens.is_multiple_of(span)
            || !hidden.is_multiple_of(HEAD_DIM)
            || !stride.is_multiple_of(4)
        {
            return Err("an attention the SAM 3 kernel cannot do".into());
        }
        let dummy = &self.dev.dummy;
        let params = [u32_of(tokens)?, u32_of(span)?, u32_of(stride)?, u32_of(hidden)?, 0, 0, 0, 0];
        let groups = [u32_of(tokens / 64)?, u32_of(hidden / HEAD_DIM)?, 1];
        if groups[0] > self.dev.limits.max_compute_workgroups_per_dimension {
            return Err("too many tokens for the SAM 3 attention kernel".into());
        }
        self.push(&self.dev.k.attention, params, [qkv, dummy, dummy, out], groups);
        Ok(())
    }

    /// The patches of `pixels` (`[channels, side, side]`) as rows of `[tokens, channels · patch²]`, in window order.
    fn im2col(
        &mut self,
        pixels: &wgpu::Buffer,
        cols: &wgpu::Buffer,
        (side, patch, channels): (usize, usize, usize),
        window: usize,
    ) -> Result<(), String> {
        let grid = side / patch.max(1);
        if patch == 0 || window == 0 || grid % window != 0 {
            return Err("a patch embedding the SAM 3 kernel cannot do".into());
        }
        let count = grid * grid * channels * patch * patch;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let dummy = &self.dev.dummy;
        let params = [u32_of(side)?, u32_of(patch)?, u32_of(grid)?, u32_of(window)?, u32_of(channels)?, 0, u32_of(count)?, row];
        self.push(&self.dev.k.im2col, params, [pixels, dummy, dummy, cols], g);
        Ok(())
    }

    /// Tokens `[grid², channels]` in window order to `[channels, grid, grid]`.
    fn channels_first(&mut self, x: &wgpu::Buffer, out: &wgpu::Buffer, channels: usize, grid: usize, window: usize) -> Result<(), String> {
        if window == 0 || !grid.is_multiple_of(window) {
            return Err("a reorder the SAM 3 kernel cannot do".into());
        }
        let count = channels * grid * grid;
        let (g, row) = self.flat(count.div_ceil(256))?;
        let dummy = &self.dev.dummy;
        self.push(&self.dev.k.to_chw, [u32_of(channels)?, u32_of(grid)?, u32_of(window)?, 0, 0, 0, u32_of(count)?, row], [x, dummy, dummy, out], g);
        Ok(())
    }

    /// The bind groups, with every step's parameters in one uniform buffer.
    fn record(self) -> Recorded {
        let d = self.dev;
        let align = (d.limits.min_uniform_buffer_offset_alignment as usize).max(32);
        let mut blob = vec![0u8; self.steps.len().max(1) * align];
        for (chunk, s) in blob.chunks_mut(align).zip(&self.steps) {
            if let Some(to) = chunk.get_mut(..32) {
                to.copy_from_slice(bytemuck::cast_slice(&s.params));
            }
        }
        let uniform = d.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sam3 params"),
            contents: &blob,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let steps = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let [a, b, c, out] = &s.buffers;
                let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &d.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &uniform,
                                offset: (i * align) as u64,
                                size: wgpu::BufferSize::new(32),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: a.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: b.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
                    ],
                });
                Step { pipeline: s.pipeline, bind, groups: s.groups }
            })
            .collect();
        let mut cuts = self.cuts;
        if cuts.last() != Some(&self.steps.len()) {
            cuts.push(self.steps.len());
        }
        Recorded { steps, cuts }
    }
}

/// Dispatches ready to replay, with where each command buffer ends.
struct Recorded {
    steps: Vec<Step>,
    cuts: Vec<usize>,
}

impl Recorded {
    /// Submit every step (one command buffer per cut), copy `len` floats of `src` into `staging` after the last,
    /// wait and read them back.
    /// Run the recorded dispatches of `stage` (its name for the reason a hung device is given up on) and read `len`
    /// floats of `src` back.
    fn run(&self, stage: &str, dev: &Dev, src: &wgpu::Buffer, staging: &wgpu::Buffer, len: usize) -> Result<Vec<f32>, String> {
        let bytes = len as u64 * 4;
        let oom = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut from = 0;
        let mut last = None;
        for (n, &to) in self.cuts.iter().enumerate() {
            let mut enc = dev.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("sam3") });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("sam3"), timestamp_writes: None });
                for s in self.steps.get(from..to).unwrap_or_default() {
                    pass.set_pipeline(s.pipeline);
                    pass.set_bind_group(0, &s.bind, &[]);
                    let [x, y, z] = s.groups;
                    pass.dispatch_workgroups(x, y, z);
                }
            }
            if n + 1 == self.cuts.len() {
                enc.copy_buffer_to_buffer(src, 0, staging, 0, bytes);
            }
            last = Some(dev.queue.submit([enc.finish()]));
            from = to;
        }
        let slice = staging.slice(..bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = dev.device.poll(wgpu::PollType::Wait { submission_index: last, timeout: Some(TIMEOUT) });
        let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
        // the read-back buffer must not stay mapped, whatever failed: the next pass copies into it
        let mapped = if waited.is_ok() { rx.recv_timeout(Duration::from_secs(10)).map_err(|e| e.to_string()) } else { Err("not finished".into()) };
        let out = (|| {
            if let Some(e) = oom {
                return Err(format!("the GPU ran out of memory: {e}"));
            }
            if let Some(e) = validation {
                mark_broken(&format!("device error: {e}"));
                return Err(format!("the GPU reported an error: {e}"));
            }
            if let Err(e) = &waited {
                mark_broken(&format!("the GPU did not finish {stage}: {e}"));
                return Err(format!("the GPU did not finish: {e}"));
            }
            match &mapped {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(format!("reading the result back failed: {e}")),
                Err(e) => return Err(format!("reading the result back failed: {e}")),
            }
            let data = slice.get_mapped_range().map_err(|e| format!("reading the result back failed: {e:?}"))?;
            Ok(data.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>())
        })();
        if matches!(mapped, Ok(Ok(()))) {
            staging.unmap();
        }
        let out = out?;
        if out.len() != len {
            return Err("the GPU returned a short result".into());
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// the backbone

/// Device buffers, each `len` floats (`STORAGE | COPY_DST | COPY_SRC`), or uploaded from `data`.
fn buffer(dev: &Dev, label: &str, len: usize) -> wgpu::Buffer {
    dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (len.max(4) * 4) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

/// A buffer holding `data`. Written through the queue rather than mapped at creation: a buffer the device has no
/// memory for then raises an out-of-memory error (and the caller tries something smaller), not a failed mapping.
fn upload<T: Pod>(dev: &Dev, label: &str, data: &[T]) -> wgpu::Buffer {
    let bytes: &[u8] = bytemuck::cast_slice(data);
    let b = dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (bytes.len() as u64).next_multiple_of(4).max(16),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    // write_buffer wants whole 4-byte words
    if bytes.len().is_multiple_of(4) && !bytes.is_empty() {
        dev.queue.write_buffer(&b, 0, bytes);
    }
    b
}

/// The nearest f16 to `v` (ties to even), as its bits.
fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    let round = |value: u32, shift: u32| -> u32 {
        let rest = value & ((1 << shift) - 1);
        let half = 1 << (shift - 1);
        let r = value >> shift;
        if rest > half || (rest == half && r & 1 == 1) { r + 1 } else { r }
    };
    if e <= 0 {
        // a subnormal half, or zero
        if e < -10 {
            return sign;
        }
        return sign | round(man | 0x80_0000, (14 - e) as u32) as u16;
    }
    // a carry out of the mantissa correctly bumps the exponent (up to infinity)
    sign | round(((e as u32) << 23) | man, 13) as u16
}

/// Weights `[n, k]` as f16 pairs (k even): u32 i holds k 2i (low half) and 2i + 1.
fn pack_f16(w: &[f32]) -> Vec<u32> {
    w.chunks(2)
        .map(|p| u32::from(f16_bits(p.first().copied().unwrap_or(0.0))) | (u32::from(f16_bits(p.get(1).copied().unwrap_or(0.0))) << 16))
        .collect()
}

/// The RoPE table of `Sam3ViTRotaryEmbedding` as `vit::rope_tables` makes it, for the tokens of a `side × side` grid
/// listed in window order (`window` per side; `side` itself: plain row-major), coordinates scaled by `scale`:
/// `[tokens, 32]` pairs (cos, sin), pair i of a head rotating by x · f_i (i < 16) or y · f_(i − 16).
fn rope_table(side: usize, window: usize, scale: f64) -> Vec<f32> {
    let spatial = HEAD_DIM / 2;
    let inv: Vec<f64> = (0..spatial).step_by(2).map(|i| 1.0 / 10000f64.powf(i as f64 / spatial as f64)).collect();
    let mut out = Vec::with_capacity(side * side * HEAD_DIM);
    for t in 0..side * side {
        let (row, col) = window_cell(t, side, window);
        let (x, y) = (col as f64 * scale, row as f64 * scale);
        for pos in [x, y] {
            for f in &inv {
                let a = (pos * f) as f32 as f64;
                out.extend([a.cos() as f32, a.sin() as f32]);
            }
        }
    }
    out
}

/// The grid cell (row, col) of window-order token `t` on a `side × side` grid of `window × window` windows.
fn window_cell(t: usize, side: usize, window: usize) -> (usize, usize) {
    let per_side = (side / window.max(1)).max(1);
    let (w, l) = (t / (window * window).max(1), t % (window * window).max(1));
    (w / per_side * window + l / window.max(1), w % per_side * window + l % window.max(1))
}

/// One layer's weights on the device.
struct LayerWeights {
    ln1: [wgpu::Buffer; 2],
    qkv: [wgpu::Buffer; 2],
    o: [wgpu::Buffer; 2],
    ln2: [wgpu::Buffer; 2],
    fc1: [wgpu::Buffer; 2],
    fc2: [wgpu::Buffer; 2],
}

/// Reads the backbone's tensors, checking their shapes.
struct Loader<'a> {
    weights: &'a WeightReader,
    dev: &'static Dev,
    precision: Precision,
    bytes: u64,
}

impl Loader<'_> {
    fn read(&self, name: &str, shape: &[usize]) -> Result<Vec<f32>, String> {
        let full = format!("{BACKBONE_PREFIX}.{name}");
        let (got, data) = self.weights.read(&full).map_err(|e| format!("{full}: {e}"))?;
        if got != shape {
            return Err(format!("{full}: shape {got:?}, expected {shape:?}"));
        }
        Ok(data)
    }

    /// A small tensor (a bias, a norm's scale…), always f32.
    fn small(&mut self, name: &str, shape: &[usize]) -> Result<wgpu::Buffer, String> {
        let data = self.read(name, shape)?;
        self.bytes += data.len() as u64 * 4;
        Ok(upload(self.dev, name, &data))
    }

    /// A linear layer's weight `[n, k]` (concatenated along n when there are several) in the backbone's precision.
    fn weight(&mut self, names: &[&str], n: usize, k: usize) -> Result<wgpu::Buffer, String> {
        let mut all = Vec::with_capacity(names.len() * n * k);
        for name in names {
            all.extend(self.read(name, &[n, k])?);
        }
        self.store(names.first().copied().unwrap_or("weight"), &all)
    }

    fn store(&mut self, label: &str, data: &[f32]) -> Result<wgpu::Buffer, String> {
        Ok(match self.precision {
            Precision::F32 => {
                self.bytes += data.len() as u64 * 4;
                upload(self.dev, label, data)
            }
            Precision::F16 => {
                let packed = pack_f16(data);
                self.bytes += packed.len() as u64 * 4;
                upload(self.dev, label, &packed)
            }
        })
    }

    fn norm(&mut self, name: &str) -> Result<[wgpu::Buffer; 2], String> {
        Ok([self.small(&format!("{name}.weight"), &[HIDDEN])?, self.small(&format!("{name}.bias"), &[HIDDEN])?])
    }

    fn layer(&mut self, i: usize) -> Result<LayerWeights, String> {
        let l = format!("layers.{i}");
        let a = format!("{l}.attention");
        let qkv_names = [format!("{a}.q_proj.weight"), format!("{a}.k_proj.weight"), format!("{a}.v_proj.weight")];
        let qkv_names: Vec<&str> = qkv_names.iter().map(String::as_str).collect();
        let mut qkv_bias = Vec::with_capacity(3 * HIDDEN);
        for p in ["q_proj", "k_proj", "v_proj"] {
            qkv_bias.extend(self.read(&format!("{a}.{p}.bias"), &[HIDDEN])?);
        }
        self.bytes += qkv_bias.len() as u64 * 4;
        Ok(LayerWeights {
            ln1: self.norm(&format!("{l}.layer_norm1"))?,
            qkv: [self.weight(&qkv_names, HIDDEN, HIDDEN)?, upload(self.dev, "qkv bias", &qkv_bias)],
            o: [self.weight(&[&format!("{a}.o_proj.weight")], HIDDEN, HIDDEN)?, self.small(&format!("{a}.o_proj.bias"), &[HIDDEN])?],
            ln2: self.norm(&format!("{l}.layer_norm2"))?,
            fc1: [self.weight(&[&format!("{l}.mlp.fc1.weight")], MLP, HIDDEN)?, self.small(&format!("{l}.mlp.fc1.bias"), &[MLP])?],
            fc2: [self.weight(&[&format!("{l}.mlp.fc2.weight")], HIDDEN, MLP)?, self.small(&format!("{l}.mlp.fc2.bias"), &[HIDDEN])?],
        })
    }
}

/// The buffers of a forward pass.
struct Work {
    pixels: wgpu::Buffer,
    out: wgpu::Buffer,
    staging: wgpu::Buffer,
}

/// The SAM 3 backbone on the GPU: weights, work buffers and every dispatch of a forward pass, made once.
pub struct Sam3Gpu {
    dev: &'static Dev,
    recorded: Recorded,
    /// One forward pass at a time (they share the work buffers).
    work: Mutex<Work>,
    /// Bytes of weights on the device.
    weight_bytes: u64,
    /// Bytes of work buffers on the device.
    work_bytes: u64,
    precision: Precision,
    /// The feature pyramids, built on first use.
    necks: neck::Necks,
}

/// The SAM 3 backbone on the GPU with the checkpoint's weights uploaded, or why it can't run there (GPU off, missing,
/// broken, a software adapter, too small, or out of memory). The weights take 1.8 GB of device memory, or 0.9 GB as f16
/// when the f32 ones do not fit.
pub fn backbone(weights: &WeightReader) -> Result<Box<dyn BackboneAccel>, String> {
    match load(weights, Precision::F32) {
        Ok(b) => Ok(Box::new(b)),
        // half the memory (0.9 GB of weights), a little less accurate (see `docs/gpu-pipeline.md`)
        Err(e) if e.starts_with(NO_MEMORY) => {
            log::info!("gpu (sam3): {e}; trying f16 weights");
            Ok(Box::new(load(weights, Precision::F16)?))
        }
        Err(e) => Err(e),
    }
}

/// How a build that ran out of device memory fails.
const NO_MEMORY: &str = "not enough GPU memory for the SAM 3 backbone";

/// [`backbone`] with the linear layers' weights stored in `precision`.
pub fn load(weights: &WeightReader, precision: Precision) -> Result<Sam3Gpu, String> {
    let dev = dev()?;
    let Some(_work) = crate::exit::enter() else { return Err("LightCraft is closing".into()) };
    guarded("the SAM 3 backbone", || build(dev, weights, precision))
}

fn build(dev: &'static Dev, weights: &WeightReader, precision: Precision) -> Result<Sam3Gpu, String> {
    let oom = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let validation = dev.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let made = build_inner(dev, weights, precision);
    // let the uploads land before the scopes close
    let _ = dev.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(TIMEOUT) });
    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
    if let Some(e) = oom {
        return Err(format!("{NO_MEMORY}: {e}"));
    }
    if let Some(e) = validation {
        return Err(format!("the GPU rejected the SAM 3 backbone: {e}"));
    }
    made
}

fn build_inner(dev: &'static Dev, weights: &WeightReader, precision: Precision) -> Result<Sam3Gpu, String> {
    let mut ld = Loader { weights, dev, precision: Precision::F32, bytes: 0 };
    // the patch embedding (K = 588, not a multiple of the f16 tile) and its position table stay f32
    let patch = ld.read("embeddings.patch_embeddings.projection.weight", &[HIDDEN, 3, PATCH, PATCH])?;
    let patch = ld.store("patch embedding", &patch)?;
    let pos = ld.small("embeddings.position_embeddings", &[1, POS_SIDE * POS_SIDE, HIDDEN])?;
    let ln_pre = ld.norm("layer_norm")?;
    ld.precision = precision;
    let mut layers = Vec::with_capacity(LAYERS);
    for i in 0..LAYERS {
        layers.push(ld.layer(i)?);
        // hand the queued uploads (~55 MB a layer) to the device, so their staging memory is freed as they land
        dev.queue.submit([]);
    }
    let rope_window = upload(dev, "rope window", &rope_table(WINDOW, WINDOW, 1.0));
    let rope_global = upload(dev, "rope global", &rope_table(GRID, WINDOW, WINDOW as f64 / GRID as f64));

    let pixels = buffer(dev, "sam3 pixels", 3 * IMAGE * IMAGE);
    let x = buffer(dev, "sam3 hidden", TOKENS * HIDDEN);
    let y = buffer(dev, "sam3 normed", TOKENS * HIDDEN);
    // im2col, then q|k|v, then the MLP's hidden activations
    let big = buffer(dev, "sam3 wide", TOKENS * MLP.max(3 * HIDDEN).max(PATCH_K));
    let staging = dev.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("sam3 read-back"),
        size: (TOKENS * HIDDEN * 4) as u64,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let work_bytes = ((3 * IMAGE * IMAGE + 3 * TOKENS * HIDDEN + TOKENS * MLP) * 4) as u64;

    let mut g = Graph::new(dev);
    g.im2col(&pixels, &big, (IMAGE, PATCH, 3), WINDOW)?;
    g.matmul([&big, &patch, &pos, &y], (TOKENS, HIDDEN, PATCH_K), POS_SIDE * POS_SIDE, false, false, Precision::F32)?;
    g.layernorm([&y, &ln_pre[0], &ln_pre[1], &x], TOKENS, HIDDEN)?;
    g.cut();
    for (i, l) in layers.iter().enumerate() {
        let global = GLOBAL.contains(&i);
        g.layernorm([&x, &l.ln1[0], &l.ln1[1], &y], TOKENS, HIDDEN)?;
        g.matmul([&y, &l.qkv[0], &l.qkv[1], &big], (TOKENS, 3 * HIDDEN, HIDDEN), 1, false, false, precision)?;
        let (table, rows, span) = if global { (&rope_global, TOKENS, TOKENS) } else { (&rope_window, WINDOW * WINDOW, WINDOW * WINDOW) };
        g.rope(&big, table, (TOKENS, 3 * HIDDEN, HIDDEN), rows, 1.0 / (HEAD_DIM as f32).sqrt())?;
        g.attention(&big, &y, (TOKENS, 3 * HIDDEN, HIDDEN), span)?;
        g.matmul([&y, &l.o[0], &l.o[1], &x], (TOKENS, HIDDEN, HIDDEN), 1, false, true, precision)?;
        g.layernorm([&x, &l.ln2[0], &l.ln2[1], &y], TOKENS, HIDDEN)?;
        g.matmul([&y, &l.fc1[0], &l.fc1[1], &big], (TOKENS, MLP, HIDDEN), 1, true, false, precision)?;
        g.matmul([&big, &l.fc2[0], &l.fc2[1], &x], (TOKENS, HIDDEN, MLP), 1, false, true, precision)?;
        g.cut();
    }
    g.channels_first(&x, &y, HIDDEN, GRID, WINDOW)?;
    let recorded = g.record();
    let necks = neck::Necks::new(weights.clone());
    Ok(Sam3Gpu { dev, recorded, work: Mutex::new(Work { pixels, out: y, staging }), weight_bytes: ld.bytes, work_bytes, precision, necks })
}

/// Run `f`, the work of `stage`, turning a panic into a broken device (with the stage's name).
fn guarded<T>(stage: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        mark_broken(&format!("{stage} on the GPU panicked"));
        Err("the GPU runner gave up".into())
    })
}

impl Sam3Gpu {
    /// Bytes of device memory the weights take.
    pub fn weight_bytes(&self) -> u64 {
        self.weight_bytes
    }

    /// Bytes of device memory the work buffers take (activations, input, read-back).
    pub fn work_bytes(&self) -> u64 {
        self.work_bytes + (TOKENS * HIDDEN * 4) as u64
    }

    fn run(&self, pixels: &[f32]) -> Result<Vec<f32>, String> {
        if pixels.len() != 3 * IMAGE * IMAGE {
            return Err(format!("{} pixel values, expected 3 × {IMAGE} × {IMAGE}", pixels.len()));
        }
        let Some(_work) = crate::exit::enter() else { return Err("LightCraft is closing".into()) };
        let work = self.work.lock().unwrap_or_else(PoisonError::into_inner);
        self.dev.queue.write_buffer(&work.pixels, 0, bytemuck::cast_slice(pixels));
        let out = self.recorded.run("the SAM 3 backbone", self.dev, &work.out, &work.staging, TOKENS * HIDDEN)?;
        if out.iter().any(|v| !v.is_finite()) {
            return Err("the GPU backbone produced non-finite values".into());
        }
        Ok(out)
    }
}

impl BackboneAccel for Sam3Gpu {
    fn forward(&self, pixels: &[f32]) -> Result<Vec<f32>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if let Some(why) = crate::switched_off() {
            return Err(why);
        }
        guarded("the SAM 3 backbone", || self.run(pixels))
    }

    fn neck(&self, which: NeckKind, backbone: &[f32], n: usize) -> Result<Vec<Vec<f32>>, String> {
        if let Some(why) = broken_reason() {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if let Some(why) = crate::switched_off() {
            return Err(why);
        }
        guarded("a SAM 3 neck", || self.necks.run(self.dev, which, backbone, n))
    }

    fn closing(&self) -> bool {
        crate::shutting_down()
    }

    fn runs_necks(&self) -> bool {
        true
    }

    fn device(&self) -> String {
        let weights = match self.precision {
            Precision::F32 => "",
            Precision::F16 => ", f16 weights",
        };
        format!("{} ({:?}{weights})", self.dev.info.name, self.dev.info.backend)
    }
}
