//! Full CLIP vision and text towers (AI semantic search) on the GPU through wgpu, so search runs on the graphics card
//! on every OS (Vulkan, DX12, Metal) with the same code. [`ClipVisionTower`] and [`ClipTextTower`] keep a tower's
//! embeddings, layer norms, transformer blocks and final projection on the device and run a whole batch in one
//! submission (`wgsl/clip.wgsl`: patch/token embeddings, layer norm, linear with GELU and residual add fused in,
//! multi-head attention, pooling gather), reading back only `[batch, embed_dim]` floats.
//!
//! The CPU encoder in `lcc-ml` is the reference: it checks a tower against itself on a test input before using it, and
//! falls back to the CPU on any error. The device is a separate one (like the face models'), on the second graphics card
//! when there is one, else the card for editing; it respects the same switches as GPU rendering and the crash sentinel
//! around device creation. Every failure is an `Err`, never a panic.
//!
//! Sources: own design, after the CLIP paper (Radford et al., "Learning Transferable Visual Models From Natural
//! Language Supervision", 2021) and the faces accelerator's device handling (`faces.rs`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};

pub use crate::ai::Card;
use crate::cards::AdapterKey;

/// Longest wait for one batch before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(120);
/// The longest sequence the attention kernel holds in workgroup memory (`MAX_SEQ` in the shader).
pub const MAX_SEQ: usize = 1024;
/// Most weights one tower may keep on the device.
const MAX_RESIDENT: u64 = 2 << 30;

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    limits: wgpu::Limits,
    layout: wgpu::BindGroupLayout,
    linear: wgpu::ComputePipeline,
    layernorm: wgpu::ComputePipeline,
    attention: wgpu::ComputePipeline,
    patch_embed: wgpu::ComputePipeline,
    text_embed: wgpu::ComputePipeline,
    gather: wgpu::ComputePipeline,
    card: Card,
    broken: Arc<AtomicBool>,
    broken_why: Arc<Mutex<Option<String>>>,
}

static DEVS: [OnceLock<Result<Arc<Dev>, String>>; 2] = [const { OnceLock::new() }, const { OnceLock::new() }];
static NAMED_DEVS: OnceLock<Mutex<HashMap<AdapterKey, Result<Arc<Dev>, String>>>> = OnceLock::new();
static BROKEN: [AtomicBool; 2] = [const { AtomicBool::new(false) }, const { AtomicBool::new(false) }];
static BROKEN_WHY: [Mutex<Option<String>>; 2] = [const { Mutex::new(None) }, const { Mutex::new(None) }];

fn mark_broken(card: Card, why: &str) {
    let i = card.index();
    if let Some(w) = BROKEN_WHY.get(i) {
        let mut w = w.lock().unwrap_or_else(PoisonError::into_inner);
        if w.is_none() {
            *w = Some(why.to_string());
        }
    }
    if let Some(b) = BROKEN.get(i) {
        b.store(true, Ordering::Relaxed);
    }
}

fn mark_dev_broken(dev: &Dev, why: &str) {
    {
        let mut w = dev.broken_why.lock().unwrap_or_else(PoisonError::into_inner);
        if w.is_none() {
            *w = Some(why.to_string());
        }
    }
    dev.broken.store(true, Ordering::Relaxed);
    mark_broken(dev.card, why);
}

fn dev_broken_reason(dev: &Dev) -> Option<String> {
    if dev.broken.load(Ordering::Relaxed) {
        return Some(dev.broken_why.lock().unwrap_or_else(PoisonError::into_inner).clone().unwrap_or_else(|| "device error".into()));
    }
    broken_reason_on(dev.card)
}

/// Why the GPU stopped being used for CLIP on `card` (a device error), if it did.
pub fn broken_reason_on(card: Card) -> Option<String> {
    let i = card.index();
    BROKEN
        .get(i)
        .is_some_and(|b| b.load(Ordering::Relaxed))
        .then(|| BROKEN_WHY.get(i).and_then(|w| w.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap_or_else(|| "device error".into()))
}

fn dev() -> Result<Arc<Dev>, String> {
    dev_on(Card::Second).or_else(|_| dev_on(Card::Editing))
}

fn dev_for(card_key: Option<&str>) -> Result<Arc<Dev>, String> {
    let Some(key_str) = card_key.map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto")) else {
        return dev();
    };
    if key_str.eq_ignore_ascii_case("cpu") {
        return Err("the AI device preference is the CPU".into());
    }
    let parsed = AdapterKey::parse(key_str).ok_or_else(|| format!("unknown graphics card `{key_str}`"))?;
    dev_named(parsed)
}

fn dev_on(card: Card) -> Result<Arc<Dev>, String> {
    if let Some(why) = broken_reason_on(card) {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let slot = DEVS.get(card.index()).ok_or(crate::ai::NO_SECOND_CARD)?;
    let _work = match slot.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or("the app is closing")?),
    };
    slot.get_or_init(|| create_device(card).map(Arc::new)).as_ref().map(Arc::clone).map_err(|e| e.clone())
}

fn dev_named(key: AdapterKey) -> Result<Arc<Dev>, String> {
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let map = NAMED_DEVS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(existing) = map.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned() {
        let d = existing?;
        if let Some(why) = dev_broken_reason(&d) {
            return Err(format!("stopped after a GPU failure: {why}"));
        }
        return Ok(d);
    }
    let _work = crate::exit::enter().ok_or("the app is closing")?;
    let created = create_named_device(key.clone()).map(Arc::new);
    let mut guard = map.lock().unwrap_or_else(PoisonError::into_inner);
    let res = guard.entry(key).or_insert(created).clone();
    let d = res?;
    if let Some(why) = dev_broken_reason(&d) {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    Ok(d)
}

fn create_device(card: Card) -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    crate::init::create(backends, move || make_device(backends, card))
}

fn create_named_device(key: AdapterKey) -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    crate::init::create(backends, move || make_named_device(backends, &key))
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn make_device(backends: wgpu::Backends, card: Card) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = crate::new_instance(desc);
    let adapter = crate::ai_adapter_on(&instance, backends, card == Card::Second)?;
    build_dev_from_adapter(adapter, card, None)
}

fn make_named_device(backends: wgpu::Backends, key: &AdapterKey) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = crate::new_instance(desc);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = crate::remember_adapters(&adapters);
    let idx = entries
        .iter()
        .position(|e| e.usable && e.key == *key)
        .or_else(|| entries.iter().position(|e| e.usable && e.key.same_model(key)))
        .ok_or_else(|| format!("selected graphics card ({}) is not available or not usable", key.name))?;
    let adapter = adapters.into_iter().nth(idx).ok_or_else(|| format!("selected graphics card ({}) is not available", key.name))?;
    build_dev_from_adapter(adapter, Card::Editing, Some(key))
}

fn build_dev_from_adapter(adapter: wgpu::Adapter, card: Card, named: Option<&AdapterKey>) -> Result<Dev, String> {
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let limits = adapter.limits();
    if limits.max_storage_buffers_per_shader_stage < 4
        || limits.max_compute_invocations_per_workgroup < 256
        || limits.max_compute_workgroup_size_x < 256
        || limits.max_compute_workgroup_storage_size < 8 * 1024
    {
        return Err(format!("{} is too limited for the CLIP kernels", info.name));
    }
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(match card {
            Card::Second => "lcc-clip-second",
            Card::Editing => "lcc-clip",
        }),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    let which = card.id();
    let is_named = named.is_some();
    let broken = Arc::new(AtomicBool::new(false));
    let broken_why = Arc::new(Mutex::new(None));
    let broken_err = Arc::clone(&broken);
    let broken_why_err = Arc::clone(&broken_why);
    device.on_uncaptured_error(Arc::new(move |e| {
        log::error!("gpu (clip, {which} card): {e}");
        let msg = format!("device error: {e}");
        {
            let mut w = broken_why_err.lock().unwrap_or_else(PoisonError::into_inner);
            if w.is_none() {
                *w = Some(msg.clone());
            }
        }
        broken_err.store(true, Ordering::Relaxed);
        if !is_named {
            mark_broken(card, &msg);
        }
    }));
    let broken_lost = Arc::clone(&broken);
    let broken_why_lost = Arc::clone(&broken_why);
    device.set_device_lost_callback(move |reason, msg| {
        log::error!("gpu (clip, {which} card): device lost ({reason:?}): {msg}");
        let why = format!("device lost ({reason:?}): {msg}");
        {
            let mut w = broken_why_lost.lock().unwrap_or_else(PoisonError::into_inner);
            if w.is_none() {
                *w = Some(why.clone());
            }
        }
        broken_lost.store(true, Ordering::Relaxed);
        if !is_named {
            mark_broken(card, &why);
        }
    });
    use wgpu::BufferBindingType::{Storage, Uniform};
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("clip"),
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: false }),
        ],
    });
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("clip"),
        source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/clip.wgsl").into()),
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let pipe = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            module: &module,
            entry_point: Some(entry),
            // every kernel writes its workgroup memory before reading it
            compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
            cache: None,
        })
    };
    let (linear, layernorm, attention, patch_embed, text_embed, gather) =
        (pipe("linear"), pipe("layernorm"), pipe("attention"), pipe("patch_embed"), pipe("text_embed"), pipe("gather"));
    log::info!("gpu (clip, {which} card): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits, layout, linear, layernorm, attention, patch_embed, text_embed, gather, card, broken, broken_why })
}

/// The adapter AI search would use ("AMD Radeon RX 7900 XTX (Vulkan)"), or why it cannot use one. Creates the device on
/// first use.
pub fn adapter() -> Result<String, String> {
    adapter_for(None)
}

/// [`adapter`] for a chosen graphics card key (`None` or `"auto"`: automatic selection).
pub fn adapter_for(card_key: Option<&str>) -> Result<String, String> {
    dev_for(card_key).map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

/// The shape of one tower's blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TowerSpec {
    pub hidden: usize,
    pub intermediate: usize,
    pub heads: usize,
    /// Each position attends to itself and earlier ones only (the text tower).
    pub causal: bool,
    pub eps: f32,
}

/// One block's weights, linear weights in `[out, in]` order.
#[derive(Clone, Copy, Debug)]
pub struct BlockRef<'a> {
    pub ln1_w: &'a [f32],
    pub ln1_b: &'a [f32],
    pub q_w: &'a [f32],
    pub q_b: &'a [f32],
    pub k_w: &'a [f32],
    pub k_b: &'a [f32],
    pub v_w: &'a [f32],
    pub v_b: &'a [f32],
    pub o_w: &'a [f32],
    pub o_b: &'a [f32],
    pub ln2_w: &'a [f32],
    pub ln2_b: &'a [f32],
    pub fc1_w: &'a [f32],
    pub fc1_b: &'a [f32],
    pub fc2_w: &'a [f32],
    pub fc2_b: &'a [f32],
}

struct Block {
    ln1: [wgpu::Buffer; 2],
    q: [wgpu::Buffer; 2],
    k: [wgpu::Buffer; 2],
    v: [wgpu::Buffer; 2],
    o: [wgpu::Buffer; 2],
    ln2: [wgpu::Buffer; 2],
    fc1: [wgpu::Buffer; 2],
    fc2: [wgpu::Buffer; 2],
}

/// One CLIP tower's transformer blocks on the GPU.
pub struct ClipTower {
    dev: Arc<Dev>,
    spec: TowerSpec,
    blocks: Vec<Block>,
}

/// Full weights and shape parameters for uploading a CLIP vision tower to the GPU.
#[derive(Clone, Copy, Debug)]
pub struct VisionTowerSpec<'a> {
    pub hidden: usize,
    pub intermediate: usize,
    pub heads: usize,
    pub eps: f32,
    pub embed_dim: usize,
    pub patch_size: usize,
    pub num_positions: usize,
    pub img_w: usize,
    pub img_h: usize,
    pub patch_weight: &'a [f32],
    pub class_embedding: &'a [f32],
    pub position_embedding: &'a [f32],
    pub pre_ln_w: &'a [f32],
    pub pre_ln_b: &'a [f32],
    pub blocks: &'a [BlockRef<'a>],
    pub post_ln_w: &'a [f32],
    pub post_ln_b: &'a [f32],
    pub projection: &'a [f32],
}

/// Full weights and shape parameters for uploading a CLIP text tower to the GPU.
#[derive(Clone, Copy, Debug)]
pub struct TextTowerSpec<'a> {
    pub hidden: usize,
    pub intermediate: usize,
    pub heads: usize,
    pub eps: f32,
    pub embed_dim: usize,
    pub vocab_size: usize,
    pub context_len: usize,
    pub token_embedding: &'a [f32],
    pub position_embedding: &'a [f32],
    pub blocks: &'a [BlockRef<'a>],
    pub final_ln_w: &'a [f32],
    pub final_ln_b: &'a [f32],
    pub projection: &'a [f32],
}

/// A full CLIP vision tower resident on the GPU.
pub struct ClipVisionTower {
    dev: Arc<Dev>,
    hidden: usize,
    intermediate: usize,
    heads: usize,
    eps: f32,
    embed_dim: usize,
    patch_size: usize,
    grid_w: usize,
    seq: usize,
    img_w: usize,
    img_h: usize,
    patch_weight: wgpu::Buffer,
    pos_with_cls: wgpu::Buffer,
    pre_ln: [wgpu::Buffer; 2],
    blocks: Vec<Block>,
    post_ln: [wgpu::Buffer; 2],
    proj_w: wgpu::Buffer,
    proj_zero_b: wgpu::Buffer,
}

/// A full CLIP text tower resident on the GPU.
pub struct ClipTextTower {
    dev: Arc<Dev>,
    hidden: usize,
    intermediate: usize,
    heads: usize,
    eps: f32,
    embed_dim: usize,
    vocab_size: usize,
    context_len: usize,
    token_emb: wgpu::Buffer,
    pos_emb: wgpu::Buffer,
    blocks: Vec<Block>,
    final_ln: [wgpu::Buffer; 2],
    proj_w: wgpu::Buffer,
    proj_zero_b: wgpu::Buffer,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    rows: u32,
    k: u32,
    n: u32,
    flags: u32,
    heads: u32,
    seq: u32,
    head_dim: u32,
    causal: u32,
    eps: f32,
    scale: f32,
    patch_size: u32,
    grid_w: u32,
}

const GELU: u32 = 1;
const ADD: u32 = 2;

fn u32_of(v: usize) -> Result<u32, String> {
    u32::try_from(v).map_err(|_| "a CLIP tower too large for the GPU".to_string())
}

fn validate_blocks(blocks: &[BlockRef<'_>], hidden: usize, intermediate: usize, heads: usize, eps: f32) -> Result<u64, String> {
    if hidden == 0 || intermediate == 0 || heads == 0 || !hidden.is_multiple_of(heads) || blocks.is_empty() || !eps.is_finite() {
        return Err("invalid CLIP tower shape".into());
    }
    let hh = hidden.checked_mul(hidden).ok_or("a CLIP tower too large")?;
    let hi = hidden.checked_mul(intermediate).ok_or("a CLIP tower too large")?;
    for b in blocks {
        let ok = [b.ln1_w, b.ln1_b, b.ln2_w, b.ln2_b, b.q_b, b.k_b, b.v_b, b.o_b, b.fc2_b].iter().all(|s| s.len() == hidden)
            && [b.q_w, b.k_w, b.v_w, b.o_w].iter().all(|s| s.len() == hh)
            && b.fc1_w.len() == hi
            && b.fc2_w.len() == hi
            && b.fc1_b.len() == intermediate;
        if !ok {
            return Err("CLIP block weights do not match the tower's shape".into());
        }
    }
    Ok((blocks.len() as u64).saturating_mul((4 * hh + 2 * hi + 9 * hidden + intermediate) as u64 * 4))
}

fn upload_f32(dev: &Dev, label: &str, data: &[f32]) -> wgpu::Buffer {
    crate::upload::buffer_init(&dev.device, &dev.queue, Some(label), bytemuck::cast_slice(data), wgpu::BufferUsages::STORAGE)
}

fn upload_blocks(dev: &Dev, blocks: &[BlockRef<'_>]) -> Vec<Block> {
    blocks
        .iter()
        .map(|b| Block {
            ln1: [upload_f32(dev, "clip ln1 w", b.ln1_w), upload_f32(dev, "clip ln1 b", b.ln1_b)],
            q: [upload_f32(dev, "clip q w", b.q_w), upload_f32(dev, "clip q b", b.q_b)],
            k: [upload_f32(dev, "clip k w", b.k_w), upload_f32(dev, "clip k b", b.k_b)],
            v: [upload_f32(dev, "clip v w", b.v_w), upload_f32(dev, "clip v b", b.v_b)],
            o: [upload_f32(dev, "clip o w", b.o_w), upload_f32(dev, "clip o b", b.o_b)],
            ln2: [upload_f32(dev, "clip ln2 w", b.ln2_w), upload_f32(dev, "clip ln2 b", b.ln2_b)],
            fc1: [upload_f32(dev, "clip fc1 w", b.fc1_w), upload_f32(dev, "clip fc1 b", b.fc1_b)],
            fc2: [upload_f32(dev, "clip fc2 w", b.fc2_w), upload_f32(dev, "clip fc2 b", b.fc2_b)],
        })
        .collect()
}

fn max_binding_bytes(dev: &Dev) -> u64 {
    dev.limits.max_storage_buffer_binding_size.min(dev.limits.max_buffer_size)
}

/// Upload a tower's blocks to the GPU, or say why it can't run there.
pub fn tower(spec: TowerSpec, blocks: &[BlockRef<'_>]) -> Result<ClipTower, String> {
    tower_for(None, spec, blocks)
}

/// [`tower`] on a chosen graphics card (`None` or `"auto"`: automatic selection).
pub fn tower_for(card_key: Option<&str>, spec: TowerSpec, blocks: &[BlockRef<'_>]) -> Result<ClipTower, String> {
    let total = validate_blocks(blocks, spec.hidden, spec.intermediate, spec.heads, spec.eps)?;
    if total > MAX_RESIDENT {
        return Err("the CLIP tower is too large for the GPU budget".into());
    }
    let hi = spec.hidden.checked_mul(spec.intermediate).ok_or("a CLIP tower too large")?;
    let dev = dev_for(card_key)?;
    if (hi as u64) * 4 > max_binding_bytes(&dev) {
        return Err(format!("{}: a CLIP weight is larger than a storage buffer may be", dev.info.name));
    }
    let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
    let scope = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let uploaded = upload_blocks(&dev, blocks);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("the GPU ran out of memory for the CLIP weights: {e}"));
    }
    Ok(ClipTower { dev, spec, blocks: uploaded })
}

/// Upload a full CLIP vision tower onto a chosen graphics card (`None` or `"auto"`: automatic selection).
pub fn vision_tower_for(card_key: Option<&str>, spec: &VisionTowerSpec<'_>) -> Result<ClipVisionTower, String> {
    let blocks_bytes = validate_blocks(spec.blocks, spec.hidden, spec.intermediate, spec.heads, spec.eps)?;
    let (h, i, embed_dim, ps) = (spec.hidden, spec.intermediate, spec.embed_dim, spec.patch_size);
    if embed_dim == 0 || ps == 0 || spec.img_w == 0 || spec.img_h == 0 || !spec.img_w.is_multiple_of(ps) || !spec.img_h.is_multiple_of(ps) {
        return Err("invalid CLIP vision shape".into());
    }
    let grid_w = spec.img_w / ps;
    let grid_h = spec.img_h / ps;
    let expected_pos = grid_w.checked_mul(grid_h).and_then(|v| v.checked_add(1)).ok_or("a CLIP vision tower too large")?;
    if spec.num_positions != expected_pos || spec.num_positions > MAX_SEQ {
        return Err("invalid CLIP vision sequence length".into());
    }
    let patch_len = h.checked_mul(3).and_then(|v| v.checked_mul(ps)).and_then(|v| v.checked_mul(ps)).ok_or("a CLIP vision tower too large")?;
    let pos_len = spec.num_positions.checked_mul(h).ok_or("a CLIP vision tower too large")?;
    let proj_len = embed_dim.checked_mul(h).ok_or("a CLIP vision tower too large")?;
    if spec.patch_weight.len() != patch_len
        || spec.class_embedding.len() != h
        || spec.position_embedding.len() != pos_len
        || spec.pre_ln_w.len() != h
        || spec.pre_ln_b.len() != h
        || spec.post_ln_w.len() != h
        || spec.post_ln_b.len() != h
        || spec.projection.len() != proj_len
    {
        return Err("CLIP vision weights do not match the tower's shape".into());
    }
    let hi = h.checked_mul(i).ok_or("a CLIP tower too large")?;
    let extra_floats = patch_len + pos_len + 4 * h + proj_len + embed_dim;
    let total = blocks_bytes.saturating_add((extra_floats as u64) * 4);
    if total > MAX_RESIDENT {
        return Err("the CLIP vision tower is too large for the GPU budget".into());
    }
    let dev = dev_for(card_key)?;
    let max_weight_bytes = (hi.max(patch_len).max(pos_len).max(proj_len) as u64) * 4;
    if max_weight_bytes > max_binding_bytes(&dev) {
        return Err(format!("{}: a CLIP vision weight is larger than a storage buffer may be", dev.info.name));
    }
    let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
    let mut pos_with_cls = spec.position_embedding.to_vec();
    if let Some(first_row) = pos_with_cls.get_mut(..h) {
        for (slot, &cls) in first_row.iter_mut().zip(spec.class_embedding) {
            *slot += cls;
        }
    }
    let zeros_proj_b = vec![0.0f32; embed_dim];
    let scope = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let patch_weight = upload_f32(&dev, "clip patch w", spec.patch_weight);
    let pos_with_cls = upload_f32(&dev, "clip pos+cls", &pos_with_cls);
    let pre_ln = [upload_f32(&dev, "clip pre_ln w", spec.pre_ln_w), upload_f32(&dev, "clip pre_ln b", spec.pre_ln_b)];
    let blocks = upload_blocks(&dev, spec.blocks);
    let post_ln = [upload_f32(&dev, "clip post_ln w", spec.post_ln_w), upload_f32(&dev, "clip post_ln b", spec.post_ln_b)];
    let proj_w = upload_f32(&dev, "clip vis proj w", spec.projection);
    let proj_zero_b = upload_f32(&dev, "clip vis proj b", &zeros_proj_b);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("the GPU ran out of memory for the CLIP vision weights: {e}"));
    }
    Ok(ClipVisionTower {
        dev,
        hidden: h,
        intermediate: i,
        heads: spec.heads,
        eps: spec.eps,
        embed_dim,
        patch_size: ps,
        grid_w,
        seq: spec.num_positions,
        img_w: spec.img_w,
        img_h: spec.img_h,
        patch_weight,
        pos_with_cls,
        pre_ln,
        blocks,
        post_ln,
        proj_w,
        proj_zero_b,
    })
}

/// Upload a full CLIP text tower onto a chosen graphics card (`None` or `"auto"`: automatic selection).
pub fn text_tower_for(card_key: Option<&str>, spec: &TextTowerSpec<'_>) -> Result<ClipTextTower, String> {
    let blocks_bytes = validate_blocks(spec.blocks, spec.hidden, spec.intermediate, spec.heads, spec.eps)?;
    let (h, i, embed_dim) = (spec.hidden, spec.intermediate, spec.embed_dim);
    if embed_dim == 0 || spec.vocab_size == 0 || spec.context_len == 0 || spec.context_len > MAX_SEQ {
        return Err("invalid CLIP text shape".into());
    }
    let tok_len = spec.vocab_size.checked_mul(h).ok_or("a CLIP text tower too large")?;
    let pos_len = spec.context_len.checked_mul(h).ok_or("a CLIP text tower too large")?;
    let proj_len = embed_dim.checked_mul(h).ok_or("a CLIP text tower too large")?;
    if spec.token_embedding.len() != tok_len
        || spec.position_embedding.len() != pos_len
        || spec.final_ln_w.len() != h
        || spec.final_ln_b.len() != h
        || spec.projection.len() != proj_len
    {
        return Err("CLIP text weights do not match the tower's shape".into());
    }
    let hi = h.checked_mul(i).ok_or("a CLIP tower too large")?;
    let extra_floats = tok_len + pos_len + 2 * h + proj_len + embed_dim;
    let total = blocks_bytes.saturating_add((extra_floats as u64) * 4);
    if total > MAX_RESIDENT {
        return Err("the CLIP text tower is too large for the GPU budget".into());
    }
    let dev = dev_for(card_key)?;
    let max_weight_bytes = (hi.max(tok_len).max(pos_len).max(proj_len) as u64) * 4;
    if max_weight_bytes > max_binding_bytes(&dev) {
        return Err(format!("{}: a CLIP text weight is larger than a storage buffer may be", dev.info.name));
    }
    let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
    let zeros_proj_b = vec![0.0f32; embed_dim];
    let scope = dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let token_emb = upload_f32(&dev, "clip tok emb", spec.token_embedding);
    let pos_emb = upload_f32(&dev, "clip text pos", spec.position_embedding);
    let blocks = upload_blocks(&dev, spec.blocks);
    let final_ln = [upload_f32(&dev, "clip final_ln w", spec.final_ln_w), upload_f32(&dev, "clip final_ln b", spec.final_ln_b)];
    let proj_w = upload_f32(&dev, "clip text proj w", spec.projection);
    let proj_zero_b = upload_f32(&dev, "clip text proj b", &zeros_proj_b);
    if let Some(e) = pollster::block_on(scope.pop()) {
        return Err(format!("the GPU ran out of memory for the CLIP text weights: {e}"));
    }
    Ok(ClipTextTower {
        dev,
        hidden: h,
        intermediate: i,
        heads: spec.heads,
        eps: spec.eps,
        embed_dim,
        vocab_size: spec.vocab_size,
        context_len: spec.context_len,
        token_emb,
        pos_emb,
        blocks,
        final_ln,
        proj_w,
        proj_zero_b,
    })
}

struct Pass<'a> {
    pipeline: &'a wgpu::ComputePipeline,
    params: Params,
    bufs: [&'a wgpu::Buffer; 4],
    groups: [u32; 3],
}

fn run_guarded(dev: &Dev, f: impl FnOnce() -> Result<Vec<f32>, String>) -> Result<Vec<f32>, String> {
    if let Some(why) = dev_broken_reason(dev) {
        return Err(format!("the GPU is not used any more: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        mark_dev_broken(dev, "CLIP on the GPU panicked");
        Err("the GPU runner gave up".into())
    })
}

fn make_scratch(d: &Dev, label: &str, bytes: u64) -> wgpu::Buffer {
    d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: bytes.max(4),
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn push_block_passes<'a>(
    passes: &mut Vec<Pass<'a>>,
    d: &'a Dev,
    blocks: &'a [Block],
    base: Params,
    h: u32,
    i: u32,
    r: u32,
    bh: u32,
    state: &'a wgpu::Buffer,
    normed: &'a wgpu::Buffer,
    q: &'a wgpu::Buffer,
    k: &'a wgpu::Buffer,
    v: &'a wgpu::Buffer,
    ctx: &'a wgpu::Buffer,
    mid: &'a wgpu::Buffer,
) {
    let lin_groups = |n: u32| [n.div_ceil(16), r.div_ceil(16), 1];
    for b in blocks {
        passes.push(Pass { pipeline: &d.layernorm, params: base, bufs: [state, &b.ln1[0], &b.ln1[1], normed], groups: [r, 1, 1] });
        for (wb, out) in [(&b.q, q), (&b.k, k), (&b.v, v)] {
            passes.push(Pass { pipeline: &d.linear, params: base, bufs: [normed, &wb[0], &wb[1], out], groups: lin_groups(h) });
        }
        passes.push(Pass { pipeline: &d.attention, params: base, bufs: [q, k, v, ctx], groups: [base.seq, bh, 1] });
        passes.push(Pass { pipeline: &d.linear, params: Params { flags: ADD, ..base }, bufs: [ctx, &b.o[0], &b.o[1], state], groups: lin_groups(h) });
        passes.push(Pass { pipeline: &d.layernorm, params: base, bufs: [state, &b.ln2[0], &b.ln2[1], normed], groups: [r, 1, 1] });
        passes.push(Pass {
            pipeline: &d.linear,
            params: Params { n: i, flags: GELU, ..base },
            bufs: [normed, &b.fc1[0], &b.fc1[1], mid],
            groups: lin_groups(i),
        });
        passes.push(Pass {
            pipeline: &d.linear,
            params: Params { k: i, flags: ADD, ..base },
            bufs: [mid, &b.fc2[0], &b.fc2[1], state],
            groups: lin_groups(h),
        });
    }
}

fn dispatch_and_readback(
    d: &Dev,
    label: &str,
    scopes: [wgpu::ErrorScopeGuard; 2],
    passes: &[Pass<'_>],
    src: &wgpu::Buffer,
    out_bytes: u64,
    expected_len: usize,
) -> Result<Vec<f32>, String> {
    let staging = d.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("clip read-back"),
        size: out_bytes.max(4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let binds: Vec<(wgpu::BindGroup, &wgpu::ComputePipeline, [u32; 3])> = passes
        .iter()
        .map(|p| {
            let u = crate::upload::buffer_init(&d.device, &d.queue, Some("clip params"), bytemuck::bytes_of(&p.params), wgpu::BufferUsages::UNIFORM);
            let [b1, b2, b3, b4] = p.bufs;
            let bind = d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &d.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: u.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: b1.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: b2.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: b3.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: b4.as_entire_binding() },
                ],
            });
            (bind, p.pipeline, p.groups)
        })
        .collect();
    let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(label), timestamp_writes: None });
        for (bind, pipeline, [gx, gy, gz]) in &binds {
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(*gx, *gy, *gz);
        }
    }
    enc.copy_buffer_to_buffer(src, 0, &staging, 0, out_bytes);
    let index = d.queue.submit([enc.finish()]);
    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });
    let waited = d.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(TIMEOUT) });
    let [oom, validation] = scopes;
    let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
    if let Some(e) = oom {
        return Err(format!("the GPU ran out of memory: {e}"));
    }
    if let Some(e) = validation {
        mark_dev_broken(d, &format!("device error: {e}"));
        return Err(format!("the GPU reported an error: {e}"));
    }
    if let Err(e) = waited {
        mark_dev_broken(d, &format!("the GPU did not finish a CLIP batch: {e}"));
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
    staging.unmap();
    if out.len() != expected_len {
        return Err("the GPU returned a result of the wrong size".into());
    }
    Ok(out)
}

impl ClipTower {
    /// The adapter it runs on ("AMD Radeon RX 7900 XTX (Vulkan)").
    pub fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    /// The card it runs on.
    pub fn card(&self) -> Card {
        self.dev.card
    }

    /// Run `x` (`[batch, seq, hidden]`, row-major) through every block; returns the result in the same shape.
    pub fn run(&self, x: &[f32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        run_guarded(&self.dev, || self.run_inner(x, batch, seq))
    }

    fn run_inner(&self, x: &[f32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        let s = self.spec;
        let rows = batch.checked_mul(seq).ok_or("a batch too large")?;
        if rows == 0 || seq > MAX_SEQ || x.len() != rows.checked_mul(s.hidden).ok_or("a batch too large")? {
            return Err("invalid CLIP batch shape".into());
        }
        let d = &self.dev;
        let max_groups = d.limits.max_compute_workgroups_per_dimension as usize;
        let bh = batch.checked_mul(s.heads).ok_or("a batch too large")?;
        if rows > max_groups || bh > max_groups || rows.div_ceil(16) > max_groups {
            return Err("a CLIP batch too large for one dispatch".into());
        }
        let big = rows.checked_mul(s.intermediate).ok_or("a batch too large")? as u64 * 4;
        if big > max_binding_bytes(d) {
            return Err("a CLIP batch too large for one buffer".into());
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let scopes = [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];
        let width_bytes = (rows * s.hidden) as u64 * 4;
        let state = crate::upload::buffer_init(
            &d.device,
            &d.queue,
            Some("clip state"),
            bytemuck::cast_slice(x),
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        );
        let (normed, q, k, v, ctx, mid) = (
            make_scratch(d, "clip normed", width_bytes),
            make_scratch(d, "clip q", width_bytes),
            make_scratch(d, "clip k", width_bytes),
            make_scratch(d, "clip v", width_bytes),
            make_scratch(d, "clip ctx", width_bytes),
            make_scratch(d, "clip mlp", big),
        );

        let (r, h, i) = (u32_of(rows)?, u32_of(s.hidden)?, u32_of(s.intermediate)?);
        let head_dim = s.hidden / s.heads;
        let base = Params {
            rows: r,
            k: h,
            n: h,
            flags: 0,
            heads: u32_of(s.heads)?,
            seq: u32_of(seq)?,
            head_dim: u32_of(head_dim)?,
            causal: u32::from(s.causal),
            eps: s.eps,
            scale: 1.0 / (head_dim as f32).sqrt(),
            patch_size: 0,
            grid_w: 0,
        };
        let mut passes: Vec<Pass<'_>> = Vec::with_capacity(self.blocks.len() * 9);
        push_block_passes(&mut passes, d, &self.blocks, base, h, i, r, u32_of(bh)?, &state, &normed, &q, &k, &v, &ctx, &mid);
        dispatch_and_readback(d, "clip blocks", scopes, &passes, &state, width_bytes, x.len())
    }
}

impl ClipVisionTower {
    /// The adapter it runs on ("AMD Radeon RX 7900 XTX (Vulkan)").
    pub fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    /// The card it runs on.
    pub fn card(&self) -> Card {
        self.dev.card
    }

    /// Run preprocessed CHW `images` (`[batch, 3, h, w]`) through the full vision tower; returns `[batch, embed_dim]`.
    pub fn run(&self, images: &[f32], batch: usize, h: usize, w: usize) -> Result<Vec<f32>, String> {
        run_guarded(&self.dev, || self.run_inner(images, batch, h, w))
    }

    fn run_inner(&self, images: &[f32], batch: usize, h: usize, w: usize) -> Result<Vec<f32>, String> {
        if batch == 0 || h != self.img_h || w != self.img_w {
            return Err("invalid CLIP vision batch shape".into());
        }
        let expected_in = batch.checked_mul(3).and_then(|v| v.checked_mul(h)).and_then(|v| v.checked_mul(w)).ok_or("a batch too large")?;
        if images.len() != expected_in {
            return Err("invalid CLIP vision image length".into());
        }
        let rows = batch.checked_mul(self.seq).ok_or("a batch too large")?;
        let d = &self.dev;
        let max_groups = d.limits.max_compute_workgroups_per_dimension as usize;
        let bh = batch.checked_mul(self.heads).ok_or("a batch too large")?;
        let max_lin_n = self.hidden.max(self.intermediate).max(self.embed_dim);
        if rows > max_groups || bh > max_groups || rows.div_ceil(16) > max_groups || max_lin_n.div_ceil(16) > max_groups {
            return Err("a CLIP vision batch too large for one dispatch".into());
        }
        let big = rows.checked_mul(self.intermediate).ok_or("a batch too large")? as u64 * 4;
        let in_bytes = (expected_in as u64) * 4;
        if big.max(in_bytes) > max_binding_bytes(d) {
            return Err("a CLIP vision batch too large for one buffer".into());
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let scopes = [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];

        let width_bytes = (rows * self.hidden) as u64 * 4;
        let batch_hidden_bytes = (batch * self.hidden) as u64 * 4;
        let expected_out = batch.checked_mul(self.embed_dim).ok_or("a batch too large")?;
        let out_bytes = (expected_out as u64) * 4;

        let images_buf = upload_f32(d, "clip images", images);
        let raw_state = make_scratch(d, "clip raw state", width_bytes);
        let state = make_scratch(d, "clip state", width_bytes);
        let (normed, q, k, v, ctx, mid) = (
            make_scratch(d, "clip normed", width_bytes),
            make_scratch(d, "clip q", width_bytes),
            make_scratch(d, "clip k", width_bytes),
            make_scratch(d, "clip v", width_bytes),
            make_scratch(d, "clip ctx", width_bytes),
            make_scratch(d, "clip mlp", big),
        );
        let pooled_a = make_scratch(d, "clip pooled a", batch_hidden_bytes);
        let pooled_b = make_scratch(d, "clip pooled b", batch_hidden_bytes);
        let out_buf = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clip vision out"),
            size: out_bytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let (r, b_u32, hidden_u32, inter_u32, embed_u32) =
            (u32_of(rows)?, u32_of(batch)?, u32_of(self.hidden)?, u32_of(self.intermediate)?, u32_of(self.embed_dim)?);
        let head_dim = self.hidden / self.heads;
        let base = Params {
            rows: r,
            k: hidden_u32,
            n: hidden_u32,
            flags: 0,
            heads: u32_of(self.heads)?,
            seq: u32_of(self.seq)?,
            head_dim: u32_of(head_dim)?,
            causal: 0,
            eps: self.eps,
            scale: 1.0 / (head_dim as f32).sqrt(),
            patch_size: u32_of(self.patch_size)?,
            grid_w: u32_of(self.grid_w)?,
        };
        let mut passes: Vec<Pass<'_>> = Vec::with_capacity(5 + self.blocks.len() * 9);
        passes.push(Pass {
            pipeline: &d.patch_embed,
            params: base,
            bufs: [&images_buf, &self.patch_weight, &self.pos_with_cls, &raw_state],
            groups: [r, 1, 1],
        });
        passes.push(Pass { pipeline: &d.layernorm, params: base, bufs: [&raw_state, &self.pre_ln[0], &self.pre_ln[1], &state], groups: [r, 1, 1] });
        push_block_passes(&mut passes, d, &self.blocks, base, hidden_u32, inter_u32, r, u32_of(bh)?, &state, &normed, &q, &k, &v, &ctx, &mid);
        passes.push(Pass {
            pipeline: &d.gather,
            params: Params { rows: b_u32, flags: 0, ..base },
            bufs: [&state, &self.proj_zero_b, &self.proj_zero_b, &pooled_a],
            groups: [b_u32, 1, 1],
        });
        passes.push(Pass {
            pipeline: &d.layernorm,
            params: Params { rows: b_u32, ..base },
            bufs: [&pooled_a, &self.post_ln[0], &self.post_ln[1], &pooled_b],
            groups: [b_u32, 1, 1],
        });
        passes.push(Pass {
            pipeline: &d.linear,
            params: Params { rows: b_u32, n: embed_u32, flags: 0, ..base },
            bufs: [&pooled_b, &self.proj_w, &self.proj_zero_b, &out_buf],
            groups: [embed_u32.div_ceil(16), b_u32.div_ceil(16), 1],
        });

        dispatch_and_readback(d, "clip vision", scopes, &passes, &out_buf, out_bytes, expected_out)
    }
}

impl ClipTextTower {
    /// The adapter it runs on ("AMD Radeon RX 7900 XTX (Vulkan)").
    pub fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    /// The card it runs on.
    pub fn card(&self) -> Card {
        self.dev.card
    }

    /// Run `token_ids` (`[batch, seq]`) and `eot_indices` (`[batch]`) through the full text tower; returns `[batch, embed_dim]`.
    pub fn run(&self, token_ids: &[u32], eot_indices: &[u32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        run_guarded(&self.dev, || self.run_inner(token_ids, eot_indices, batch, seq))
    }

    fn run_inner(&self, token_ids: &[u32], eot_indices: &[u32], batch: usize, seq: usize) -> Result<Vec<f32>, String> {
        if batch == 0 || seq != self.context_len {
            return Err("invalid CLIP text batch shape".into());
        }
        let rows = batch.checked_mul(seq).ok_or("a batch too large")?;
        if token_ids.len() != rows || eot_indices.len() != batch {
            return Err("invalid CLIP text token length".into());
        }
        let d = &self.dev;
        let max_groups = d.limits.max_compute_workgroups_per_dimension as usize;
        let bh = batch.checked_mul(self.heads).ok_or("a batch too large")?;
        let max_lin_n = self.hidden.max(self.intermediate).max(self.embed_dim);
        if rows > max_groups || bh > max_groups || rows.div_ceil(16) > max_groups || max_lin_n.div_ceil(16) > max_groups {
            return Err("a CLIP text batch too large for one dispatch".into());
        }
        let big = rows.checked_mul(self.intermediate).ok_or("a batch too large")? as u64 * 4;
        if big > max_binding_bytes(d) {
            return Err("a CLIP text batch too large for one buffer".into());
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let scopes = [d.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), d.device.push_error_scope(wgpu::ErrorFilter::Validation)];

        let width_bytes = (rows * self.hidden) as u64 * 4;
        let batch_hidden_bytes = (batch * self.hidden) as u64 * 4;
        let expected_out = batch.checked_mul(self.embed_dim).ok_or("a batch too large")?;
        let out_bytes = (expected_out as u64) * 4;

        let ids_buf =
            crate::upload::buffer_init(&d.device, &d.queue, Some("clip token ids"), bytemuck::cast_slice(token_ids), wgpu::BufferUsages::STORAGE);
        let eot_buf =
            crate::upload::buffer_init(&d.device, &d.queue, Some("clip eot"), bytemuck::cast_slice(eot_indices), wgpu::BufferUsages::STORAGE);
        let state = make_scratch(d, "clip state", width_bytes);
        let (normed, q, k, v, ctx, mid) = (
            make_scratch(d, "clip normed", width_bytes),
            make_scratch(d, "clip q", width_bytes),
            make_scratch(d, "clip k", width_bytes),
            make_scratch(d, "clip v", width_bytes),
            make_scratch(d, "clip ctx", width_bytes),
            make_scratch(d, "clip mlp", big),
        );
        let pooled_a = make_scratch(d, "clip pooled a", batch_hidden_bytes);
        let out_buf = d.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clip text out"),
            size: out_bytes.max(4),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let (r, b_u32, hidden_u32, inter_u32, embed_u32, vocab_u32) =
            (u32_of(rows)?, u32_of(batch)?, u32_of(self.hidden)?, u32_of(self.intermediate)?, u32_of(self.embed_dim)?, u32_of(self.vocab_size)?);
        let head_dim = self.hidden / self.heads;
        let base = Params {
            rows: r,
            k: hidden_u32,
            n: hidden_u32,
            flags: 0,
            heads: u32_of(self.heads)?,
            seq: u32_of(seq)?,
            head_dim: u32_of(head_dim)?,
            causal: 1,
            eps: self.eps,
            scale: 1.0 / (head_dim as f32).sqrt(),
            patch_size: 0,
            grid_w: 0,
        };
        let mut passes: Vec<Pass<'_>> = Vec::with_capacity(4 + self.blocks.len() * 9);
        passes.push(Pass {
            pipeline: &d.text_embed,
            params: Params { n: vocab_u32, ..base },
            bufs: [&ids_buf, &self.token_emb, &self.pos_emb, &state],
            groups: [r, 1, 1],
        });
        push_block_passes(&mut passes, d, &self.blocks, base, hidden_u32, inter_u32, r, u32_of(bh)?, &state, &normed, &q, &k, &v, &ctx, &mid);
        passes.push(Pass { pipeline: &d.layernorm, params: base, bufs: [&state, &self.final_ln[0], &self.final_ln[1], &normed], groups: [r, 1, 1] });
        passes.push(Pass {
            pipeline: &d.gather,
            params: Params { rows: b_u32, flags: 1, ..base },
            bufs: [&normed, &eot_buf, &self.proj_zero_b, &pooled_a],
            groups: [b_u32, 1, 1],
        });
        passes.push(Pass {
            pipeline: &d.linear,
            params: Params { rows: b_u32, n: embed_u32, flags: 0, ..base },
            bufs: [&pooled_a, &self.proj_w, &self.proj_zero_b, &out_buf],
            groups: [embed_u32.div_ceil(16), b_u32.div_ceil(16), 1],
        });

        dispatch_and_readback(d, "clip text", scopes, &passes, &out_buf, out_bytes, expected_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clip_kernels_validate_without_a_gpu() {
        assert_eq!(crate::validate_wgsl("clip", include_str!("wgsl/clip.wgsl")), Ok(()));
    }

    fn values(n: usize, seed: f32) -> Vec<f32> {
        (0..n).map(|i| (i as f32 * 0.37 + seed).sin() * 0.5).collect()
    }

    fn layernorm(x: &[f32], w: &[f32], b: &[f32], eps: f32) -> Vec<f32> {
        let n = x.len() as f32;
        let mean = x.iter().sum::<f32>() / n;
        let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n;
        let inv = 1.0 / (var + eps).sqrt();
        x.iter().zip(w).zip(b).map(|((v, w), b)| (v - mean) * inv * w + b).collect()
    }

    fn linear(x: &[f32], rows: usize, k: usize, w: &[f32], b: &[f32]) -> Vec<f32> {
        let n = b.len();
        let mut out = vec![0f32; rows * n];
        for r in 0..rows {
            for j in 0..n {
                out[r * n + j] = b[j] + (0..k).map(|i| x[r * k + i] * w[j * k + i]).sum::<f32>();
            }
        }
        out
    }

    fn gelu(x: f32) -> f32 {
        // erf by series (reference, independent of the shader's approximation)
        let z = x / std::f32::consts::SQRT_2;
        let mut sum = 0f64;
        let mut term = z as f64;
        for n in 0..60 {
            sum += term / (2 * n + 1) as f64;
            term *= -(z as f64) * (z as f64) / (n + 1) as f64;
        }
        0.5 * x * (1.0 + (sum * 2.0 / std::f64::consts::PI.sqrt()) as f32)
    }

    /// The pre-norm transformer block by its definition (the CLIP paper), on the CPU.
    fn reference(spec: TowerSpec, blocks: &[BlockRef<'_>], x: &[f32], batch: usize, seq: usize) -> Vec<f32> {
        let (h, heads) = (spec.hidden, spec.heads);
        let hd = h / heads;
        let rows = batch * seq;
        let mut x = x.to_vec();
        for b in blocks {
            let normed: Vec<f32> = x.chunks(h).flat_map(|r| layernorm(r, b.ln1_w, b.ln1_b, spec.eps)).collect();
            let (q, k, v) = (linear(&normed, rows, h, b.q_w, b.q_b), linear(&normed, rows, h, b.k_w, b.k_b), linear(&normed, rows, h, b.v_w, b.v_b));
            let mut ctx = vec![0f32; rows * h];
            for item in 0..batch {
                for head in 0..heads {
                    for qi in 0..seq {
                        let qrow = (item * seq + qi) * h + head * hd;
                        let scores: Vec<f32> = (0..seq)
                            .map(|j| {
                                if spec.causal && j > qi {
                                    f32::NEG_INFINITY
                                } else {
                                    let krow = (item * seq + j) * h + head * hd;
                                    (0..hd).map(|d| q[qrow + d] * k[krow + d]).sum::<f32>() / (hd as f32).sqrt()
                                }
                            })
                            .collect();
                        let m = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                        let e: Vec<f32> = scores.iter().map(|s| (s - m).exp()).collect();
                        let total: f32 = e.iter().sum();
                        for d in 0..hd {
                            ctx[qrow + d] = (0..seq).map(|j| e[j] * v[(item * seq + j) * h + head * hd + d]).sum::<f32>() / total;
                        }
                    }
                }
            }
            let o = linear(&ctx, rows, h, b.o_w, b.o_b);
            for (x, o) in x.iter_mut().zip(&o) {
                *x += o;
            }
            let normed: Vec<f32> = x.chunks(h).flat_map(|r| layernorm(r, b.ln2_w, b.ln2_b, spec.eps)).collect();
            let mid: Vec<f32> = linear(&normed, rows, h, b.fc1_w, b.fc1_b).into_iter().map(gelu).collect();
            let out = linear(&mid, rows, spec.intermediate, b.fc2_w, b.fc2_b);
            for (x, o) in x.iter_mut().zip(&out) {
                *x += o;
            }
        }
        x
    }

    fn make_owned_blocks(n_layers: usize, h: usize, i: usize) -> Vec<Vec<Vec<f32>>> {
        (0..n_layers)
            .map(|l| {
                let s = l as f32 * 10.0;
                vec![
                    values(h, s + 1.0).iter().map(|v| v + 1.0).collect(),
                    values(h, s + 2.0),
                    values(h * h, s + 3.0),
                    values(h, s + 4.0),
                    values(h * h, s + 5.0),
                    values(h, s + 6.0),
                    values(h * h, s + 7.0),
                    values(h, s + 8.0),
                    values(h * h, s + 9.0),
                    values(h, s + 10.0),
                    values(h, s + 11.0).iter().map(|v| v + 1.0).collect(),
                    values(h, s + 12.0),
                    values(h * i, s + 13.0),
                    values(i, s + 14.0),
                    values(h * i, s + 15.0),
                    values(h, s + 16.0),
                ]
            })
            .collect()
    }

    fn as_block_refs(owned: &[Vec<Vec<f32>>]) -> Vec<BlockRef<'_>> {
        owned
            .iter()
            .map(|w| BlockRef {
                ln1_w: &w[0],
                ln1_b: &w[1],
                q_w: &w[2],
                q_b: &w[3],
                k_w: &w[4],
                k_b: &w[5],
                v_w: &w[6],
                v_b: &w[7],
                o_w: &w[8],
                o_b: &w[9],
                ln2_w: &w[10],
                ln2_b: &w[11],
                fc1_w: &w[12],
                fc1_b: &w[13],
                fc2_w: &w[14],
                fc2_b: &w[15],
            })
            .collect()
    }

    /// Blocks on the GPU give what the definition gives, causal or not, with sizes that are not multiples of a tile.
    #[test]
    fn clip_blocks_on_the_gpu_match_the_definition() {
        if let Err(e) = adapter() {
            lcc_testkit::skip!(Gpu, "no GPU for the CLIP kernels: {e}");
            return;
        }
        for causal in [false, true] {
            let spec = TowerSpec { hidden: 24, intermediate: 40, heads: 3, causal, eps: 1e-5 };
            let owned = make_owned_blocks(2, spec.hidden, spec.intermediate);
            let blocks = as_block_refs(&owned);
            let (batch, seq) = (2, 7);
            let x = values(batch * seq * spec.hidden, 0.5);
            let t = tower(spec, &blocks).unwrap();
            let got = t.run(&x, batch, seq).unwrap();
            let want = reference(spec, &blocks, &x, batch, seq);
            let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0f32, f32::max);
            assert!(worst < 1e-3, "causal {causal}: GPU differs from the definition by {worst}");
        }
    }

    #[test]
    fn clip_vision_tower_on_gpu_matches_reference() {
        if let Err(e) = adapter() {
            lcc_testkit::skip!(Gpu, "no GPU for the CLIP vision tower: {e}");
            return;
        }
        let (h, i, heads, embed_dim, ps, img_w, img_h) = (24, 40, 3, 12, 4, 12, 8);
        let (grid_w, grid_h) = (img_w / ps, img_h / ps);
        let num_pos = 1 + grid_w * grid_h;
        let eps = 1e-5;
        let owned = make_owned_blocks(2, h, i);
        let blocks = as_block_refs(&owned);
        let patch_w = values(h * 3 * ps * ps, 1.0);
        let cls = values(h, 2.0);
        let pos = values(num_pos * h, 3.0);
        let pre_ln_w: Vec<f32> = values(h, 4.0).iter().map(|v| v + 1.0).collect();
        let pre_ln_b = values(h, 5.0);
        let post_ln_w: Vec<f32> = values(h, 6.0).iter().map(|v| v + 1.0).collect();
        let post_ln_b = values(h, 7.0);
        let proj = values(embed_dim * h, 8.0);
        let spec = VisionTowerSpec {
            hidden: h,
            intermediate: i,
            heads,
            eps,
            embed_dim,
            patch_size: ps,
            num_positions: num_pos,
            img_w,
            img_h,
            patch_weight: &patch_w,
            class_embedding: &cls,
            position_embedding: &pos,
            pre_ln_w: &pre_ln_w,
            pre_ln_b: &pre_ln_b,
            blocks: &blocks,
            post_ln_w: &post_ln_w,
            post_ln_b: &post_ln_b,
            projection: &proj,
        };
        let batch = 2;
        let images = values(batch * 3 * img_h * img_w, 0.25);
        let gpu_tower = vision_tower_for(None, &spec).unwrap();
        let got = gpu_tower.run(&images, batch, img_h, img_w).unwrap();

        let mut seq_in = vec![0.0f32; batch * num_pos * h];
        for b in 0..batch {
            for d in 0..h {
                seq_in[b * num_pos * h + d] = cls[d] + pos[d];
            }
            for p_idx in 0..(grid_w * grid_h) {
                let (py, px) = (p_idx / grid_w, p_idx % grid_w);
                let (y0, x0) = (py * ps, px * ps);
                let row_off = (b * num_pos + 1 + p_idx) * h;
                for d in 0..h {
                    let mut acc = 0.0f32;
                    for ch in 0..3 {
                        for u in 0..ps {
                            for v in 0..ps {
                                let img_idx = ((b * 3 + ch) * img_h + y0 + u) * img_w + x0 + v;
                                let w_idx = ((d * 3 + ch) * ps + u) * ps + v;
                                acc += images[img_idx] * patch_w[w_idx];
                            }
                        }
                    }
                    seq_in[row_off + d] = acc + pos[(1 + p_idx) * h + d];
                }
            }
        }
        let pre_normed: Vec<f32> = seq_in.chunks(h).flat_map(|r| layernorm(r, &pre_ln_w, &pre_ln_b, eps)).collect();
        let after = reference(TowerSpec { hidden: h, intermediate: i, heads, causal: false, eps }, &blocks, &pre_normed, batch, num_pos);
        let mut want = Vec::with_capacity(batch * embed_dim);
        for b in 0..batch {
            let cls_tok = &after[b * num_pos * h..b * num_pos * h + h];
            let post = layernorm(cls_tok, &post_ln_w, &post_ln_b, eps);
            let zero_b = vec![0.0f32; embed_dim];
            want.extend(linear(&post, 1, h, &proj, &zero_b));
        }
        let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let tol = 2e-3 * (1.0 + scale);
        let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0f32, f32::max);
        assert!(worst <= tol, "vision tower GPU differs from CPU reference by {worst} (tol {tol})");
    }

    #[test]
    fn clip_text_tower_on_gpu_matches_reference() {
        if let Err(e) = adapter() {
            lcc_testkit::skip!(Gpu, "no GPU for the CLIP text tower: {e}");
            return;
        }
        let (h, i, heads, embed_dim, vocab_size, context_len) = (24, 40, 3, 12, 32, 9);
        let eps = 1e-5;
        let owned = make_owned_blocks(2, h, i);
        let blocks = as_block_refs(&owned);
        let tok_emb = values(vocab_size * h, 1.0);
        let pos_emb = values(context_len * h, 2.0);
        let final_ln_w: Vec<f32> = values(h, 3.0).iter().map(|v| v + 1.0).collect();
        let final_ln_b = values(h, 4.0);
        let proj = values(embed_dim * h, 5.0);
        let spec = TextTowerSpec {
            hidden: h,
            intermediate: i,
            heads,
            eps,
            embed_dim,
            vocab_size,
            context_len,
            token_embedding: &tok_emb,
            position_embedding: &pos_emb,
            blocks: &blocks,
            final_ln_w: &final_ln_w,
            final_ln_b: &final_ln_b,
            projection: &proj,
        };
        let batch = 2;
        let ids: Vec<u32> = (0..(batch * context_len)).map(|idx| ((idx * 5 + 1) % vocab_size) as u32).collect();
        let eot = vec![2u32, 7u32];
        let gpu_tower = text_tower_for(None, &spec).unwrap();
        let got = gpu_tower.run(&ids, &eot, batch, context_len).unwrap();

        let mut seq_in = vec![0.0f32; batch * context_len * h];
        for b in 0..batch {
            for s in 0..context_len {
                let tok = ids[b * context_len + s] as usize;
                for d in 0..h {
                    seq_in[(b * context_len + s) * h + d] = tok_emb[tok * h + d] + pos_emb[s * h + d];
                }
            }
        }
        let after = reference(TowerSpec { hidden: h, intermediate: i, heads, causal: true, eps }, &blocks, &seq_in, batch, context_len);
        let mut want = Vec::with_capacity(batch * embed_dim);
        for b in 0..batch {
            let eot_idx = eot[b] as usize;
            let row = &after[(b * context_len + eot_idx) * h..(b * context_len + eot_idx + 1) * h];
            let post = layernorm(row, &final_ln_w, &final_ln_b, eps);
            let zero_b = vec![0.0f32; embed_dim];
            want.extend(linear(&post, 1, h, &proj, &zero_b));
        }
        let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let tol = 2e-3 * (1.0 + scale);
        let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0f32, f32::max);
        assert!(worst <= tol, "text tower GPU differs from CPU reference by {worst} (tol {tol})");
    }

    #[test]
    fn mismatched_weights_and_shapes_are_errors() {
        let spec = TowerSpec { hidden: 8, intermediate: 16, heads: 3, causal: false, eps: 1e-5 };
        assert!(tower(spec, &[]).is_err());
    }
}
