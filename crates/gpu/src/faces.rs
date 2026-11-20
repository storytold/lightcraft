//! Face networks on the GPU: an [`lcc_faces::net::Accelerator`] that runs face detection (MediaPipe BlazeFace +
//! FaceMesh-V2 via `lcc-tflite`) and recognition (SFace, AuraFace, user ONNX via `lcc-faces`) with WGSL compute
//! kernels:
//! - Mixed-precision FP16 (`wgsl/faces_conv_f16.wgsl`, `wgsl/faces_ops_f16.wgsl`, `enable f16;`): stores weights,
//!   activations and workgroup tiles in IEEE-754 `f16` while accumulating in `f32`, halving VRAM footprint and memory
//!   bandwidth on Tensor/FP16-capable GPUs (`Features::SHADER_F16`).
//! - FP32 fallback (`wgsl/faces_conv.wgsl`, `wgsl/faces_ops.wgsl`): used automatically when the adapter lacks
//!   `SHADER_F16`, when a model exceeds `f16` range/tolerance, or when requested by settings.
//! - Batched `[B, C, H, W]` execution: runs all faces or detector windows in a batch through a single command pass per
//!   layer so weight tiles in workgroup memory are shared across the batch.
//! - Persistent on-disk plan cache: remembers verified adapter/driver/precision benchmarks across sessions to skip
//!   startup CPU-vs-GPU re-benchmarking, plus reusable scratch buffer pooling across batches.
//! - Dedicated NVIDIA (`0x10de`) / discrete GPU preference and explicit adapter selection (`AdapterKey`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use bytemuck::{Pod, Zeroable};
use half::f16;
use lcc_faces::net::program::{Arg, Binary, INPUT, Op, Program, Unary};
use lcc_faces::net::{Accelerator, ConvJob};

pub use crate::ai::Card;
use crate::cards::{AdapterEntry, AdapterKey, AdapterKind};

/// Longest wait for one convolution or batch before the device is given up on.
const TIMEOUT: Duration = Duration::from_secs(30);
/// Below this many multiply-adds a single convolution stays on the CPU: the upload, dispatch and read-back cost more.
const MIN_MACS: usize = 1 << 20;
/// Workgroups a convolution should make at least, cutting its K into slices when its tiles are too few.
const TARGET_WORKGROUPS: usize = 256;
/// Weights one model may keep on the device (AuraFace's are about 250 MB in FP32, ~125 MB in FP16).
const MAX_RESIDENT: u64 = 768 << 20;
/// Maximum pooled intermediate scratch bytes retained on one `FaceGpu` across batches.
const MAX_SCRATCH_POOL: u64 = 128 << 20;
/// PCI vendor ID for NVIDIA GPUs (preferred automatically when selecting dedicated Tensor/FP16 hardware).
const VENDOR_NVIDIA: u32 = 0x10de;
/// Magic header for persistent on-disk plan cache entries.
const PLAN_MAGIC: &[u8; 4] = b"LCFP";
const PLAN_VERSION: u8 = 1;

/// Precision mode for face network GPU execution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Precision {
    /// Mixed-precision FP16 when the adapter supports `SHADER_F16` (falling back to FP32 on tolerance mismatch),
    /// otherwise FP32.
    #[default]
    Auto,
    /// Mixed-precision FP16 storage with FP32 accumulation (`enable f16;`).
    Fp16,
    /// Full IEEE-754 FP32 storage and accumulation.
    Fp32,
}

impl Precision {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => Some(Self::Auto),
            "fp16" | "f16" | "half" => Some(Self::Fp16),
            "fp32" | "f32" | "float" => Some(Self::Fp32),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fp16 => "fp16",
            Self::Fp32 => "fp32",
        }
    }
}

/// Active precision of a [`FaceGpu`] instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivePrecision {
    Fp16,
    Fp32,
}

impl ActivePrecision {
    fn id(self) -> &'static str {
        match self {
            Self::Fp16 => "fp16",
            Self::Fp32 => "fp32",
        }
    }

    fn tag(self) -> u8 {
        match self {
            Self::Fp16 => 1,
            Self::Fp32 => 2,
        }
    }

    fn elem_bytes(self) -> u64 {
        match self {
            Self::Fp16 => 2,
            Self::Fp32 => 4,
        }
    }
}

struct PipelineSet {
    tiled: wgpu::ComputePipeline,
    direct: wgpu::ComputePipeline,
    reduce: wgpu::ComputePipeline,
    ops: Ops,
}

struct Dev {
    device: wgpu::Device,
    queue: wgpu::Queue,
    info: wgpu::AdapterInfo,
    adapter_key: AdapterKey,
    driver_sig: String,
    limits: wgpu::Limits,
    layout: wgpu::BindGroupLayout,
    fp32: PipelineSet,
    fp16: Option<PipelineSet>,
    /// Bound where a kernel has fewer operands than bindings.
    dummy: wgpu::Buffer,
    /// The card role it serves (`Second` or `Editing`).
    card: Card,
    broken: Arc<AtomicBool>,
    broken_why: Arc<Mutex<Option<String>>>,
}

/// The kernels of `wgsl/faces_ops.wgsl` / `wgsl/faces_ops_f16.wgsl`.
struct Ops {
    unary: wgpu::ComputePipeline,
    affine: wgpu::ComputePipeline,
    prelu: wgpu::ComputePipeline,
    binary: wgpu::ComputePipeline,
    transpose: wgpu::ComputePipeline,
    maxpool: wgpu::ComputePipeline,
    resize: wgpu::ComputePipeline,
    gavg: wgpu::ComputePipeline,
    gemv: wgpu::ComputePipeline,
}

/// The face device on each card ([`Card::index`] order).
static DEVS: [Mutex<Option<Result<&'static Dev, String>>>; 2] = [const { Mutex::new(None) }, const { Mutex::new(None) }];
/// Named/explicitly-bound face devices, keyed by [`AdapterKey`].
static NAMED_DEVS: OnceLock<Mutex<HashMap<AdapterKey, Result<&'static Dev, String>>>> = OnceLock::new();
/// Automatic face device preferring dedicated NVIDIA (`0x10de`) / Tensor-capable GPUs.
static AUTO_DEV: Mutex<Option<Result<&'static Dev, String>>> = Mutex::new(None);
static BROKEN: [AtomicBool; 2] = [const { AtomicBool::new(false) }, const { AtomicBool::new(false) }];
static BROKEN_WHY: [Mutex<Option<String>>; 2] = [const { Mutex::new(None) }, const { Mutex::new(None) }];

/// Persistent on-disk cache directory for verified GPU plans.
static CACHE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// In-memory plan cache mirror `(driver_sig, precision_tag, program_fp) -> faster`.
static PLAN_MEM: OnceLock<Mutex<HashMap<(String, u8, u64), bool>>> = OnceLock::new();

/// Configure the directory where persistent face GPU plan caches are saved across sessions.
pub fn set_cache_dir(dir: Option<PathBuf>) {
    *CACHE_DIR.lock().unwrap_or_else(PoisonError::into_inner) = dir;
}

fn cache_dir() -> Option<PathBuf> {
    CACHE_DIR.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

fn plan_mem() -> &'static Mutex<HashMap<(String, u8, u64), bool>> {
    PLAN_MEM.get_or_init(|| Mutex::new(HashMap::new()))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf29ce484222325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

fn plan_cache_path(dir: &Path, driver_sig: &str, precision: ActivePrecision, program_fp: u64) -> PathBuf {
    let sig_hash = fnv1a64(driver_sig.as_bytes());
    dir.join(format!("plan-{sig_hash:016x}-{program_fp:016x}-{}.bin", precision.id()))
}

fn parse_cached_plan(bytes: &[u8], driver_sig: &str, precision: ActivePrecision, program_fp: u64) -> Option<bool> {
    if bytes.len() < 41 || bytes.len() > 4096 || bytes.get(..4)? != PLAN_MAGIC || *bytes.get(4)? != PLAN_VERSION {
        return None;
    }
    let (payload, tag_bytes) = bytes.split_at(bytes.len() - 8);
    let expected = u64::from_le_bytes(tag_bytes.try_into().ok()?);
    if fnv1a64(payload) != expected {
        return None;
    }
    let faster_byte = *payload.get(5)?;
    if faster_byte > 1 {
        return None;
    }
    let prec_tag = *payload.get(6)?;
    let fp = u64::from_le_bytes(payload.get(7..15)?.try_into().ok()?);
    if prec_tag != precision.tag() || fp != program_fp {
        return None;
    }
    let fast_ms = f64::from_le_bytes(payload.get(15..23)?.try_into().ok()?);
    let cpu_ms = f64::from_le_bytes(payload.get(23..31)?.try_into().ok()?);
    if !fast_ms.is_finite() || !cpu_ms.is_finite() || fast_ms < 0.0 || cpu_ms < 0.0 {
        return None;
    }
    let sig_len = usize::from(u16::from_le_bytes(payload.get(31..33)?.try_into().ok()?));
    let end = 33usize.checked_add(sig_len)?;
    if end != payload.len() {
        return None;
    }
    let sig = std::str::from_utf8(payload.get(33..end)?).ok()?;
    if sig != driver_sig {
        return None;
    }
    Some(faster_byte != 0)
}

fn read_cached_plan(driver_sig: &str, precision: ActivePrecision, program_fp: u64) -> Option<bool> {
    if let Some(&faster) = plan_mem().lock().unwrap_or_else(PoisonError::into_inner).get(&(driver_sig.to_string(), precision.tag(), program_fp)) {
        return Some(faster);
    }
    let dir = cache_dir()?;
    let path = plan_cache_path(&dir, driver_sig, precision, program_fp);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > 4096 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let faster = parse_cached_plan(&bytes, driver_sig, precision, program_fp)?;
    plan_mem().lock().unwrap_or_else(PoisonError::into_inner).insert((driver_sig.to_string(), precision.tag(), program_fp), faster);
    Some(faster)
}

fn write_cached_plan(driver_sig: &str, precision: ActivePrecision, program_fp: u64, faster: bool, fast_ms: f64, cpu_ms: f64) {
    plan_mem().lock().unwrap_or_else(PoisonError::into_inner).insert((driver_sig.to_string(), precision.tag(), program_fp), faster);
    let Some(dir) = cache_dir() else { return };
    if driver_sig.len() > 1024 || std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = plan_cache_path(&dir, driver_sig, precision, program_fp);
    let sig_bytes = driver_sig.as_bytes();
    let mut buf = Vec::with_capacity(41 + sig_bytes.len());
    buf.extend_from_slice(PLAN_MAGIC);
    buf.push(PLAN_VERSION);
    buf.push(u8::from(faster));
    buf.push(precision.tag());
    buf.extend_from_slice(&program_fp.to_le_bytes());
    buf.extend_from_slice(&fast_ms.to_le_bytes());
    buf.extend_from_slice(&cpu_ms.to_le_bytes());
    buf.extend_from_slice(&(sig_bytes.len() as u16).to_le_bytes());
    buf.extend_from_slice(sig_bytes);
    let checksum = fnv1a64(&buf);
    buf.extend_from_slice(&checksum.to_le_bytes());
    let tmp = path.with_extension("part");
    if std::fs::write(&tmp, &buf).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Clear broken device state and any cached failure so that a retry (or settings change) can attempt GPU initialization again.
pub fn reset() {
    for (i, b) in BROKEN.iter().enumerate() {
        let was_broken = b.swap(false, Ordering::Relaxed);
        if let Some(w) = BROKEN_WHY.get(i) {
            *w.lock().unwrap_or_else(PoisonError::into_inner) = None;
        }
        if let Some(slot) = DEVS.get(i) {
            let mut dev = slot.lock().unwrap_or_else(PoisonError::into_inner);
            if was_broken || matches!(&*dev, Some(Err(_))) {
                *dev = None;
            }
        }
    }
    {
        let mut auto = AUTO_DEV.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(&*auto, Some(Err(_))) || matches!(&*auto, Some(Ok(d)) if d.broken.load(Ordering::Relaxed)) {
            *auto = None;
        }
    }
    if let Some(map) = NAMED_DEVS.get() {
        let mut map = map.lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, v| matches!(v, Ok(d) if !d.broken.load(Ordering::Relaxed)));
    }
}

/// Stop using `card`'s face device (the first reason is kept); the other card's is not touched.
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

/// Stop using `card`'s face device, as a device error does (tests of moving a model to the other card).
#[doc(hidden)]
pub fn give_up_on(card: Card, why: &str) {
    mark_broken(card, why);
}

/// Why the GPU stopped being used for faces on `card` (a device error), if it did.
pub fn broken_reason_on(card: Card) -> Option<String> {
    let i = card.index();
    BROKEN
        .get(i)
        .is_some_and(|b| b.load(Ordering::Relaxed))
        .then(|| BROKEN_WHY.get(i).and_then(|w| w.lock().unwrap_or_else(PoisonError::into_inner).clone()).unwrap_or_else(|| "device error".into()))
}

/// Why the GPU stopped being used for faces (a device error) on every card a new model could go to, if it did.
pub fn broken_reason() -> Option<String> {
    let second_failed = DEVS.first().is_some_and(|m| matches!(&*m.lock().unwrap_or_else(PoisonError::into_inner), Some(Err(_))));
    let second_usable = broken_reason_on(Card::Second).is_none() && !second_failed;
    if second_usable { None } else { broken_reason_on(Card::Editing) }
}

/// The graphics cards usable for face acceleration (populates the adapter list if not yet cached).
pub fn available_devices() -> Vec<AdapterEntry> {
    let cached = crate::adapters();
    if !cached.is_empty() {
        return cached;
    }
    crate::refresh_adapters()
}

/// Rank score for automatic adapter selection: prefers dedicated NVIDIA (`0x10de`) Tensor/FP16-capable GPUs
/// off-screen first, then NVIDIA on-screen, then other dedicated off-screen GPUs, then remaining hardware GPUs.
fn auto_adapter_score(entry: &AdapterEntry) -> (u8, u8, u8) {
    let nvidia = u8::from(entry.key.vendor == VENDOR_NVIDIA);
    let dedicated = u8::from(entry.kind == AdapterKind::Dedicated);
    let off_screen = u8::from(!entry.drives_screen);
    (nvidia, dedicated, off_screen)
}

/// The face device for automatic selection: prefers dedicated NVIDIA (`0x10de`) / discrete hardware when
/// available, while respecting `{ENV_PREFIX}_GPU_ADAPTER` and broken-card state.
fn dev() -> Result<&'static Dev, String> {
    if let Some(env_dev) = lcc_brand::env("FACE_DEVICE").map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto")) {
        return dev_for(Some(&env_dev));
    }
    if broken_reason_on(Card::Second).is_some() {
        return dev_on(Card::Editing);
    }
    if crate::cards::helper_choice() == crate::cards::HelperChoice::Same {
        return dev_on(Card::Second).or_else(|_| dev_on(Card::Editing));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let cached = {
        let mut slot = AUTO_DEV.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(cached) = &*slot {
            Some(cached.clone())
        } else {
            let _work = crate::exit::enter().ok_or("the app is closing")?;
            let res = create_auto_device().map(|d| &*Box::leak(Box::new(d)));
            *slot = Some(res.clone());
            Some(res)
        }
    };
    match cached {
        Some(Ok(d)) if dev_broken_reason(d).is_none() => Ok(d),
        Some(Ok(d)) if d.card == Card::Second => dev_on(Card::Editing),
        Some(Ok(d)) => Err(format!("stopped after a GPU failure: {}", dev_broken_reason(d).unwrap_or_default())),
        Some(Err(e)) => Err(e),
        None => Err("no face GPU device".into()),
    }
}

fn dev_for(choice: Option<&str>) -> Result<&'static Dev, String> {
    let Some(key_str) = choice.map(str::trim).filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto")) else {
        return dev();
    };
    if key_str.eq_ignore_ascii_case("cpu") {
        return Err("the CPU is chosen in Settings".into());
    }
    if key_str.eq_ignore_ascii_case("nvidia") {
        return dev_nvidia();
    }
    let parsed = AdapterKey::parse(key_str).ok_or_else(|| format!("unknown graphics card `{key_str}`"))?;
    dev_named(parsed)
}

fn dev_nvidia() -> Result<&'static Dev, String> {
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let entries = available_devices();
    let best = entries
        .iter()
        .filter(|e| e.usable && e.key.vendor == VENDOR_NVIDIA)
        .max_by_key(|e| (u8::from(e.kind == AdapterKind::Dedicated), u8::from(!e.drives_screen)))
        .ok_or_else(|| "no NVIDIA GPU adapter is available".to_string())?;
    dev_named(best.key.clone())
}

fn dev_named(key: AdapterKey) -> Result<&'static Dev, String> {
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let map = NAMED_DEVS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(existing) = map.lock().unwrap_or_else(PoisonError::into_inner).get(&key).cloned() {
        let d = existing?;
        if let Some(why) = dev_broken_reason(d) {
            return Err(format!("stopped after a GPU failure: {why}"));
        }
        return Ok(d);
    }
    let _work = crate::exit::enter().ok_or("the app is closing")?;
    let key_clone = key.clone();
    let res = create_named_device(key_clone).map(|d| &*Box::leak(Box::new(d)));
    map.lock().unwrap_or_else(PoisonError::into_inner).insert(key, res.clone());
    res
}

/// `card`'s face device, made on first use; `Err` when it can't be used (GPU switched off, no such card, it failed).
fn dev_on(card: Card) -> Result<&'static Dev, String> {
    if let Some(why) = broken_reason_on(card) {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    let slot = DEVS.get(card.index()).ok_or(crate::ai::NO_SECOND_CARD)?;
    let mut dev = slot.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(cached) = &*dev {
        return cached.clone();
    }
    let _work = crate::exit::enter().ok_or("the app is closing")?;
    let res = create_device(card).map(|d| &*Box::leak(Box::new(d)));
    *dev = Some(res.clone());
    res
}

fn create_device(card: Card) -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    crate::init::create(backends, move || make_device_on(backends, card))
}

fn create_auto_device() -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    crate::init::create(backends, move || make_auto_device(backends))
}

fn create_named_device(key: AdapterKey) -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err(format!("disabled by {}=off", lcc_brand::env_var("GPU_BACKEND"))) };
    crate::init::create(backends, move || make_named_device(backends, key))
}

fn entry(binding: u32, ty: wgpu::BufferBindingType) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

fn adapter_limits_ok(adapter: &wgpu::Adapter) -> Result<(), String> {
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
        return Err(format!("{} is too limited for the face kernels", info.name));
    }
    Ok(())
}

fn new_compute_instance(backends: wgpu::Backends) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.backend_options = crate::backend::backend_options();
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    crate::new_instance(desc)
}

fn make_device_on(backends: wgpu::Backends, card: Card) -> Result<Dev, String> {
    let instance = new_compute_instance(backends);
    let adapter = crate::ai_adapter_on(&instance, backends, card == Card::Second)?;
    let info = adapter.get_info();
    let entries = crate::adapters();
    let key = entries
        .iter()
        .find(|e| e.key.name == info.name.trim() && e.key.vendor == info.vendor && e.key.device == info.device)
        .map(|e| e.key.clone())
        .unwrap_or_else(|| AdapterKey::of(&info, 1));
    build_dev(adapter, card, key)
}

fn make_auto_device(backends: wgpu::Backends) -> Result<Dev, String> {
    let instance = new_compute_instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = crate::remember_adapters(&adapters);
    let wanted_hit = crate::backend::wanted_adapter().and_then(|wanted| {
        entries
            .iter()
            .enumerate()
            .find(|(_, e)| e.usable && crate::backend::adapter_name_matches(&wanted, &e.name))
            .map(|(i, e)| (i, e.key.clone(), e.drives_screen))
    });
    let chosen = wanted_hit.or_else(|| {
        entries
            .iter()
            .enumerate()
            .filter(|(i, e)| e.usable && adapters.get(*i).is_some_and(|a| adapter_limits_ok(a).is_ok()))
            .max_by_key(|(_, e)| auto_adapter_score(e))
            .map(|(i, e)| (i, e.key.clone(), e.drives_screen))
    });
    if let Some((idx, key, drives_screen)) = chosen
        && let Some(adapter) = adapters.into_iter().nth(idx)
    {
        let card = if drives_screen { Card::Editing } else { Card::Second };
        return build_dev(adapter, card, key);
    }
    // Fallback to the standard second-then-editing card resolution.
    make_device_on(backends, Card::Second).or_else(|_| make_device_on(backends, Card::Editing))
}

fn make_named_device(backends: wgpu::Backends, key: AdapterKey) -> Result<Dev, String> {
    let instance = new_compute_instance(backends);
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    let entries = crate::remember_adapters(&adapters);
    let found = entries.iter().position(|e| e.key == key).or_else(|| entries.iter().position(|e| e.key.same_model(&key)));
    let idx = found.ok_or_else(|| format!("graphics card `{}` is not available", key.name))?;
    if let Some(why) = entries.get(idx).and_then(|e| e.why_not.as_ref()) {
        return Err(format!("{} cannot run face models: {why}", key.name));
    }
    let drives_screen = entries.get(idx).is_some_and(|e| e.drives_screen);
    let resolved_key = entries.get(idx).map(|e| e.key.clone()).unwrap_or(key);
    let adapter = adapters.into_iter().nth(idx).ok_or_else(|| "graphics card disappeared".to_string())?;
    let card = if drives_screen { Card::Editing } else { Card::Second };
    build_dev(adapter, card, resolved_key)
}

fn build_pipeline_set(device: &wgpu::Device, pl: &wgpu::PipelineLayout, conv_src: &str, ops_src: &str, suffix: &str) -> PipelineSet {
    let conv_label = format!("faces conv {suffix}");
    let ops_label = format!("faces ops {suffix}");
    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&conv_label),
        source: wgpu::ShaderSource::Wgsl(conv_src.into()),
    });
    let ops_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&ops_label),
        source: wgpu::ShaderSource::Wgsl(ops_src.into()),
    });
    let pipe_in = |module: &wgpu::ShaderModule, entry: &str| {
        let name = format!("{entry}_{suffix}");
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(&name),
            layout: Some(pl),
            module,
            entry_point: Some(entry),
            // kernels write all of their workgroup memory before reading it
            compilation_options: wgpu::PipelineCompilationOptions { zero_initialize_workgroup_memory: false, ..Default::default() },
            cache: None,
        })
    };
    PipelineSet {
        tiled: pipe_in(&module, "tiled"),
        direct: pipe_in(&module, "direct"),
        reduce: pipe_in(&module, "reduce"),
        ops: Ops {
            unary: pipe_in(&ops_module, "unary"),
            affine: pipe_in(&ops_module, "affine"),
            prelu: pipe_in(&ops_module, "prelu"),
            binary: pipe_in(&ops_module, "binary"),
            transpose: pipe_in(&ops_module, "transpose"),
            maxpool: pipe_in(&ops_module, "maxpool"),
            resize: pipe_in(&ops_module, "resize"),
            gavg: pipe_in(&ops_module, "gavg"),
            gemv: pipe_in(&ops_module, "gemv"),
        },
    }
}

fn build_dev(adapter: wgpu::Adapter, card: Card, adapter_key: AdapterKey) -> Result<Dev, String> {
    adapter_limits_ok(&adapter)?;
    let info = adapter.get_info();
    let limits = adapter.limits();
    let has_f16 = adapter.features().contains(wgpu::Features::SHADER_F16);
    let required_features = if has_f16 { wgpu::Features::SHADER_F16 } else { wgpu::Features::empty() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(match card {
            Card::Second => "lcc-faces-second",
            Card::Editing => "lcc-faces",
        }),
        required_features,
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    let broken = Arc::new(AtomicBool::new(false));
    let broken_why = Arc::new(Mutex::new(None));
    let which = card.id();
    {
        let broken = broken.clone();
        let broken_why = broken_why.clone();
        device.on_uncaptured_error(Arc::new(move |e| {
            let msg = format!("device error: {e}");
            log::error!("gpu (faces, {which} card): {e}");
            {
                let mut w = broken_why.lock().unwrap_or_else(PoisonError::into_inner);
                if w.is_none() {
                    *w = Some(msg.clone());
                }
            }
            broken.store(true, Ordering::Relaxed);
            mark_broken(card, &msg);
        }));
    }
    {
        let broken = broken.clone();
        let broken_why = broken_why.clone();
        device.set_device_lost_callback(move |reason, msg| {
            let full = format!("device lost ({reason:?}): {msg}");
            log::error!("gpu (faces, {which} card): {full}");
            {
                let mut w = broken_why.lock().unwrap_or_else(PoisonError::into_inner);
                if w.is_none() {
                    *w = Some(full.clone());
                }
            }
            broken.store(true, Ordering::Relaxed);
            mark_broken(card, &full);
        });
    }
    use wgpu::BufferBindingType::{Storage, Uniform};
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("faces conv"),
        entries: &[
            entry(0, Uniform),
            entry(1, Storage { read_only: true }),
            entry(2, Storage { read_only: true }),
            entry(3, Storage { read_only: true }),
            entry(4, Storage { read_only: false }),
        ],
    });
    let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
    let fp32 = build_pipeline_set(&device, &pl, include_str!("wgsl/faces_conv.wgsl"), include_str!("wgsl/faces_ops.wgsl"), "fp32");
    let fp16 = has_f16
        .then(|| build_pipeline_set(&device, &pl, include_str!("wgsl/faces_conv_f16.wgsl"), include_str!("wgsl/faces_ops_f16.wgsl"), "fp16"));
    let dummy = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("faces unused"),
        size: 16,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let driver_sig = format!("{adapter_key}|{}|{:?}", info.driver_info, info.backend);
    log::info!("gpu (faces, {which} card): {} ({:?}, f16={has_f16}, nvidia={})", info.name, info.backend, info.vendor == VENDOR_NVIDIA);
    Ok(Dev {
        device,
        queue,
        info,
        adapter_key,
        driver_sig,
        limits,
        layout,
        fp32,
        fp16,
        dummy,
        card,
        broken,
        broken_why,
    })
}

/// The adapter face scans would use ("NVIDIA GeForce RTX 4070 (Vulkan)"), or why they cannot use one. Creates the
/// device on first use.
pub fn adapter() -> Result<String, String> {
    dev().map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

/// [`adapter`] on `card` (no other card instead).
pub fn adapter_on(card: Card) -> Result<String, String> {
    dev_on(card).map(|d| format!("{} ({:?})", d.info.name, d.info.backend))
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    m: u32,
    n: u32,
    k: u32,
    cg: u32,
    h: u32,
    w: u32,
    ow: u32,
    kh: u32,
    kw: u32,
    sh: u32,
    sw: u32,
    pt: u32,
    pl: u32,
    bias: u32,
    total: u32,
    row: u32,
    splits: u32,
    kchunk: u32,
    plane_out: u32,
    cout: u32,
    cin: u32,
    batch: u32,
    _a: u32,
    _b: u32,
}

/// A layer's weights and bias on the device.
struct Resident {
    weights: wgpu::Buffer,
    bias: wgpu::Buffer,
}

/// Reusable intermediate GPU buffer pool across batches.
#[derive(Default)]
struct BufferPool {
    buffers: Vec<(u64, wgpu::Buffer)>,
    bytes: u64,
}

impl BufferPool {
    fn take(&mut self, needed: u64) -> Option<(u64, wgpu::Buffer)> {
        let best = self
            .buffers
            .iter()
            .enumerate()
            .filter(|(_, (size, _))| *size >= needed && *size <= needed.saturating_mul(2))
            .min_by_key(|(_, (size, _))| *size)
            .map(|(i, _)| i)?;
        let item = self.buffers.swap_remove(best);
        self.bytes = self.bytes.saturating_sub(item.0);
        Some(item)
    }

    fn put(&mut self, size: u64, buffer: wgpu::Buffer) {
        if size > MAX_SCRATCH_POOL || self.bytes.saturating_add(size) > MAX_SCRATCH_POOL {
            return;
        }
        self.bytes = self.bytes.saturating_add(size);
        self.buffers.push((size, buffer));
    }

    fn clear(&mut self) {
        self.buffers.clear();
        self.bytes = 0;
    }
}

/// The GPU accelerator for one loaded face model: its weights stay on the device while it lives.
pub struct FaceGpu {
    dev: &'static Dev,
    precision: ActivePrecision,
    layers: Mutex<HashMap<usize, Arc<Resident>>>,
    consts: Mutex<HashMap<usize, wgpu::Buffer>>,
    pool: Mutex<BufferPool>,
    resident: AtomicU64,
    cache_hit: AtomicBool,
}

/// A GPU accelerator for one face model, or why there is none (GPU off, missing or broken): prefers dedicated NVIDIA
/// (`0x10de`) / discrete hardware when available, else the second graphics card, else the card for editing. Uses
/// mixed-precision FP16 automatically when supported.
pub fn accelerator() -> Result<Arc<FaceGpu>, String> {
    let env_prec = lcc_brand::env("FACE_PRECISION").and_then(|s| Precision::parse(&s)).unwrap_or(Precision::Auto);
    accelerator_with(dev()?, env_prec)
}

/// [`accelerator`] on `card` (no other card instead).
pub fn accelerator_on(card: Card) -> Result<Arc<FaceGpu>, String> {
    let env_prec = lcc_brand::env("FACE_PRECISION").and_then(|s| Precision::parse(&s)).unwrap_or(Precision::Auto);
    accelerator_with(dev_on(card)?, env_prec)
}

/// A GPU accelerator bound explicitly to `device` (`None`/`"auto"` = prefer NVIDIA/dedicated, `"cpu"` = refused,
/// `"nvidia"` = dedicated NVIDIA, or an `AdapterKey` string) and `precision`.
pub fn accelerator_for(device: Option<&str>, precision: Precision) -> Result<Arc<FaceGpu>, String> {
    let effective_prec = match precision {
        Precision::Auto => lcc_brand::env("FACE_PRECISION").and_then(|s| Precision::parse(&s)).unwrap_or(Precision::Auto),
        other => other,
    };
    accelerator_with(dev_for(device)?, effective_prec)
}

fn accelerator_with(dev: &'static Dev, requested: Precision) -> Result<Arc<FaceGpu>, String> {
    let precision = match requested {
        Precision::Auto => if dev.fp16.is_some() { ActivePrecision::Fp16 } else { ActivePrecision::Fp32 },
        Precision::Fp16 => {
            if dev.fp16.is_none() {
                return Err(format!("{} does not support WGSL shader-f16", dev.info.name));
            }
            ActivePrecision::Fp16
        }
        Precision::Fp32 => ActivePrecision::Fp32,
    };
    Ok(Arc::new(FaceGpu {
        dev,
        precision,
        layers: Mutex::new(HashMap::new()),
        consts: Mutex::new(HashMap::new()),
        pool: Mutex::new(BufferPool::default()),
        resident: AtomicU64::new(0),
        cache_hit: AtomicBool::new(false),
    }))
}

fn u32_of(v: usize) -> Result<u32, String> {
    u32::try_from(v).ok().filter(|v| *v < (1 << 31)).ok_or_else(|| "a layer too large for the GPU kernel".to_string())
}

/// How much of the GPU face work may take: at most `concurrent` runs at once, and busy for at most `share` of the time
/// (after a run of `t`, the next one waits `t · (1 − share) / share`); on the second graphics card, which draws no
/// window, `second_share`. Set by the background scan's pace.
struct Pace {
    concurrent: usize,
    share: f32,
    second_share: f32,
    running: usize,
}

impl Pace {
    /// The share of the time a model on `card` may keep it busy.
    fn share_on(&self, card: Card) -> f32 {
        match card {
            Card::Second => self.second_share,
            Card::Editing => self.share,
        }
    }
}

static PACE: Mutex<Pace> = Mutex::new(Pace { concurrent: usize::MAX, share: 1.0, second_share: 1.0, running: 0 });
static PACE_FREED: std::sync::Condvar = std::sync::Condvar::new();

fn share_of(share: f32) -> f32 {
    if share.is_finite() { share.clamp(0.05, 1.0) } else { 1.0 }
}

/// Limit the GPU work face models do: `concurrent` runs at once (at least one) and `share` of the time (0.05 to 1) on
/// the card for editing, `second_share` on the second graphics card.
pub fn set_pace(concurrent: usize, share: f32, second_share: f32) {
    let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
    p.concurrent = concurrent.max(1);
    p.share = share_of(share);
    p.second_share = share_of(second_share);
    PACE_FREED.notify_all();
}

/// A turn on the GPU under the pace; waits for one. Dropping it gives it back.
struct Turn {
    share: f32,
    started: std::time::Instant,
}

impl Turn {
    fn take(card: Card) -> Turn {
        let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
        while p.running >= p.concurrent {
            p = PACE_FREED.wait_timeout(p, Duration::from_millis(200)).map(|(g, _)| g).unwrap_or_else(|e| e.into_inner().0);
        }
        p.running += 1;
        Turn { share: p.share_on(card), started: std::time::Instant::now() }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        if self.share < 1.0 {
            let rest = self.started.elapsed().mul_f32((1.0 - self.share) / self.share).min(Duration::from_secs(2));
            std::thread::sleep(rest);
        }
        let mut p = PACE.lock().unwrap_or_else(PoisonError::into_inner);
        p.running = p.running.saturating_sub(1);
        PACE_FREED.notify_one();
    }
}

/// One dispatch to record: a pipeline, its parameters, its four buffers (three read, the last written) and its
/// workgroup counts.
struct Dispatch {
    pipeline: &'static wgpu::ComputePipeline,
    params: Vec<u8>,
    buffers: [wgpu::Buffer; 4],
    groups: [u32; 3],
}

/// Workgroups for `count` items of 256 (or `count` workgroups when `per_group`): x, y and the row length the kernel
/// reads back as `q(1)`.
fn flat(count: usize, limit: usize) -> Option<[u32; 3]> {
    let blocks = count.max(1);
    let row = blocks.clamp(1, limit.min(65_535));
    let rows = blocks.div_ceil(row);
    (rows <= limit).then_some([u32::try_from(row).ok()?, u32::try_from(rows).ok()?, u32::try_from(row).ok()?])
}

fn params24(values: &[u32]) -> Vec<u8> {
    let mut all = [0u32; 24];
    for (to, v) in all.iter_mut().zip(values) {
        *to = *v;
    }
    bytemuck::cast_slice(&all).to_vec()
}

fn encode_f16(data: &[f32]) -> Vec<u16> {
    data.iter().map(|&v| f16::from_f32(v).to_bits()).collect()
}

fn f16_overflows(data: &[f32]) -> bool {
    data.iter().any(|&v| v.is_finite() && v.abs() > 65_500.0)
}

impl FaceGpu {
    /// The card role its device serves (`Second` or `Editing`).
    pub fn card(&self) -> Card {
        self.dev.card
    }

    /// Active numeric precision (`"fp16"` or `"fp32"`).
    pub fn precision_name(&self) -> &'static str {
        self.precision.id()
    }

    /// Whether the adapter supports `SHADER_F16`.
    pub fn f16_supported(&self) -> bool {
        self.dev.fp16.is_some()
    }

    /// Whether the adapter is an NVIDIA GPU (`0x10de`).
    pub fn is_nvidia(&self) -> bool {
        self.dev.info.vendor == VENDOR_NVIDIA
    }

    /// Whether the last verification check hit the persistent on-disk plan cache.
    pub fn was_plan_cached(&self) -> bool {
        self.cache_hit.load(Ordering::Relaxed)
    }

    /// Clone this accelerator on the same `Dev` but forcing FP32 precision (for automatic FP16 -> FP32 fallback).
    pub fn as_fp32(&self) -> Arc<FaceGpu> {
        Arc::new(FaceGpu {
            dev: self.dev,
            precision: ActivePrecision::Fp32,
            layers: Mutex::new(HashMap::new()),
            consts: Mutex::new(HashMap::new()),
            pool: Mutex::new(BufferPool::default()),
            resident: AtomicU64::new(0),
            cache_hit: AtomicBool::new(false),
        })
    }

    /// Bytes of weights this model keeps on the device.
    pub fn resident_bytes(&self) -> u64 {
        self.resident.load(Ordering::Relaxed)
    }

    fn pipes(&self) -> &PipelineSet {
        match self.precision {
            ActivePrecision::Fp16 => self.dev.fp16.as_ref().unwrap_or(&self.dev.fp32),
            ActivePrecision::Fp32 => &self.dev.fp32,
        }
    }

    fn upload_values(&self, label: &str, data: &[f32]) -> wgpu::Buffer {
        match self.precision {
            ActivePrecision::Fp16 => {
                let zero = [0u16; 8];
                let packed = if data.is_empty() { None } else { Some(encode_f16(data)) };
                let slice = packed.as_deref().unwrap_or(&zero);
                crate::upload::buffer_init(
                    &self.dev.device,
                    &self.dev.queue,
                    Some(label),
                    bytemuck::cast_slice(slice),
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                )
            }
            ActivePrecision::Fp32 => {
                let zero = [0f32; 4];
                crate::upload::buffer_init(
                    &self.dev.device,
                    &self.dev.queue,
                    Some(label),
                    bytemuck::cast_slice(if data.is_empty() { &zero } else { data }),
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                )
            }
        }
    }

    fn upload_batch_inputs(&self, inputs: &[&[f32]]) -> wgpu::Buffer {
        match self.precision {
            ActivePrecision::Fp16 => {
                let total: usize = inputs.iter().map(|s| s.len()).sum();
                let mut packed = Vec::with_capacity(total);
                for s in inputs {
                    packed.extend(s.iter().map(|&v| f16::from_f32(v).to_bits()));
                }
                let zero = [0u16; 8];
                crate::upload::buffer_init(
                    &self.dev.device,
                    &self.dev.queue,
                    Some("faces batch input"),
                    bytemuck::cast_slice(if packed.is_empty() { &zero } else { packed.as_slice() }),
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                )
            }
            ActivePrecision::Fp32 => {
                let total: usize = inputs.iter().map(|s| s.len()).sum();
                let mut flat = Vec::with_capacity(total);
                for s in inputs {
                    flat.extend_from_slice(s);
                }
                let zero = [0f32; 4];
                crate::upload::buffer_init(
                    &self.dev.device,
                    &self.dev.queue,
                    Some("faces batch input"),
                    bytemuck::cast_slice(if flat.is_empty() { &zero } else { flat.as_slice() }),
                    wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                )
            }
        }
    }

    /// Whether `bytes` more may stay on the device; counts them if so.
    fn keep(&self, bytes: u64) -> bool {
        if self.resident_bytes().saturating_add(bytes) > MAX_RESIDENT {
            return false;
        }
        self.resident.fetch_add(bytes, Ordering::Relaxed);
        true
    }

    /// A convolution's weights and bias: kept on the device while the budget lasts, uploaded for this run past it.
    fn resident(&self, job: &ConvJob) -> Arc<Resident> {
        let mut layers = self.layers.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(r) = layers.get(&job.layer) {
            return r.clone();
        }
        let r = Arc::new(Resident {
            weights: self.upload_values("faces weights", job.weights),
            bias: self.upload_values("faces bias", job.bias.unwrap_or(&[])),
        });
        let elem = self.precision.elem_bytes();
        if self.keep((job.weights.len() as u64 + job.cout as u64) * elem) {
            layers.insert(job.layer, r.clone());
        }
        r
    }

    /// A program's constant, kept on the device like the weights.
    fn constant(&self, id: usize, data: &[f32]) -> wgpu::Buffer {
        let mut consts = self.consts.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(b) = consts.get(&id) {
            return b.clone();
        }
        let b = self.upload_values("faces constant", data);
        if self.keep(data.len() as u64 * self.precision.elem_bytes()) {
            consts.insert(id, b.clone());
        }
        b
    }

    fn largest_binding(&self) -> u64 {
        self.dev.limits.max_storage_buffer_binding_size.min(self.dev.limits.max_buffer_size)
    }

    fn take_scratch(&self, local_pool: &mut Vec<(u64, wgpu::Buffer)>, bytes: u64) -> (u64, wgpu::Buffer) {
        let aligned = bytes.max(4).div_ceil(4) * 4;
        if let Some(best) = local_pool
            .iter()
            .enumerate()
            .filter(|(_, (size, _))| *size >= aligned && *size <= aligned.saturating_mul(2))
            .min_by_key(|(_, (size, _))| *size)
            .map(|(i, _)| i)
        {
            return local_pool.swap_remove(best);
        }
        if let Some(hit) = self.pool.lock().unwrap_or_else(PoisonError::into_inner).take(aligned) {
            return hit;
        }
        (aligned, self.scratch(aligned))
    }

    /// The dispatches of one batched convolution (`batch` inputs packed as `[batch, cin, h, w]`) reading `input`
    /// and writing `output`, or `None` when it exceeds the device.
    fn conv_dispatches_batched(
        &self,
        job: &ConvJob,
        batch: usize,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        local_pool: &mut Vec<(u64, wgpu::Buffer)>,
        keep_alive: &mut Vec<(u64, wgpu::Buffer)>,
    ) -> Result<Option<Vec<Dispatch>>, String> {
        let d = self.dev;
        let pipes = self.pipes();
        let elem = self.precision.elem_bytes();
        let groups = job.groups.max(1);
        let (m, cg) = (job.cout / groups, job.cin / groups);
        if batch == 0 || m == 0 || cg == 0 || job.h == 0 || job.w == 0 || job.oh == 0 || job.ow == 0 || job.kh == 0 || job.kw == 0 {
            return Ok(None);
        }
        let plane_out = job.oh.checked_mul(job.ow).ok_or("a layer too large")?;
        let n = batch.checked_mul(plane_out).ok_or("a layer too large")?;
        let k = cg.checked_mul(job.kh).and_then(|v| v.checked_mul(job.kw)).ok_or("a layer too large")?;
        let total = batch.checked_mul(job.cout).and_then(|v| v.checked_mul(plane_out)).ok_or("a layer too large")?;
        let direct = m < 8;
        // Too few 64 x 64 tiles to keep the GPU busy: cut K into slices that run side by side, summed in a second pass.
        let tiles = n.div_ceil(64) * m.div_ceil(64) * groups;
        let splits = if direct { 1 } else { (TARGET_WORKGROUPS / tiles.max(1)).clamp(1, (k / 128).max(1)) };
        let kchunk = k.div_ceil(splits).div_ceil(16) * 16;
        let splits = k.div_ceil(kchunk.max(1)).max(1);
        let partial = total.checked_mul(splits).ok_or("a layer too large")?;
        let cin_len = batch.checked_mul(job.cin).and_then(|v| v.checked_mul(job.h)).and_then(|v| v.checked_mul(job.w)).ok_or("a layer too large")?;
        let largest = [cin_len, job.weights.len(), partial].into_iter().max().unwrap_or(0) as u64 * elem;
        let limit = d.limits.max_compute_workgroups_per_dimension as usize;
        let Some(sum) = flat(total.div_ceil(256), limit) else { return Ok(None) };
        let (wx, wy, wz) = if direct { (sum[0] as usize, sum[1] as usize, 1) } else { (n.div_ceil(64), m.div_ceil(64), groups * splits) };
        if largest > self.largest_binding() || [wx, wy, wz].into_iter().any(|v| v > limit) {
            return Ok(None);
        }
        let res = self.resident(job);
        let params = Params {
            m: u32_of(m)?,
            n: u32_of(n)?,
            k: u32_of(k)?,
            cg: u32_of(cg)?,
            h: u32_of(job.h)?,
            w: u32_of(job.w)?,
            ow: u32_of(job.ow)?,
            kh: u32_of(job.kh)?,
            kw: u32_of(job.kw)?,
            sh: u32_of(job.sh)?,
            sw: u32_of(job.sw)?,
            pt: u32_of(job.pt)?,
            pl: u32_of(job.pl)?,
            bias: u32::from(job.bias.is_some()),
            total: u32_of(total)?,
            row: sum[2],
            splits: u32_of(splits)?,
            kchunk: u32_of(kchunk)?,
            plane_out: u32_of(plane_out)?,
            cout: u32_of(job.cout)?,
            cin: u32_of(job.cin)?,
            batch: u32_of(batch)?,
            _a: 0,
            _b: 0,
        };
        let params = bytemuck::bytes_of(&params).to_vec();
        let main = |out: &wgpu::Buffer| Dispatch {
            pipeline: if direct { &pipes.direct } else { &pipes.tiled },
            params: params.clone(),
            buffers: [input.clone(), res.weights.clone(), res.bias.clone(), out.clone()],
            groups: [wx as u32, wy as u32, wz as u32],
        };
        if splits == 1 {
            return Ok(Some(vec![main(output)]));
        }
        let (p_size, partials) = self.take_scratch(local_pool, partial as u64 * elem);
        let partials_clone = partials.clone();
        keep_alive.push((p_size, partials));
        Ok(Some(vec![
            main(&partials_clone),
            Dispatch {
                pipeline: &pipes.reduce,
                params,
                buffers: [partials_clone, res.weights.clone(), res.bias.clone(), output.clone()],
                groups: [sum[0], sum[1], 1],
            },
        ]))
    }

    /// Record `list` into `enc` as one compute pass, with all their parameters in one uniform buffer.
    fn record(&self, enc: &mut wgpu::CommandEncoder, list: &[Dispatch]) -> Result<(), String> {
        let d = self.dev;
        if list.is_empty() {
            return Ok(());
        }
        let align = (d.limits.min_uniform_buffer_offset_alignment as usize).max(256);
        let mut blob = vec![0u8; list.len() * align];
        for (i, x) in list.iter().enumerate() {
            blob.get_mut(i * align..i * align + x.params.len()).ok_or("parameters too large")?.copy_from_slice(&x.params);
        }
        let uniform = crate::upload::buffer_init(&d.device, &d.queue, Some("faces params"), &blob, wgpu::BufferUsages::UNIFORM);
        let binds = list
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let [a, b, c, out] = &x.buffers;
                d.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &d.layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                buffer: &uniform,
                                offset: (i * align) as u64,
                                size: std::num::NonZeroU64::new(x.params.len() as u64),
                            }),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: a.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: b.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 4, resource: out.as_entire_binding() },
                    ],
                })
            })
            .collect::<Vec<_>>();
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("faces"), timestamp_writes: None });
        for (x, bind) in list.iter().zip(&binds) {
            pass.set_pipeline(x.pipeline);
            pass.set_bind_group(0, bind, &[]);
            let [gx, gy, gz] = x.groups;
            pass.dispatch_workgroups(gx, gy, gz);
        }
        Ok(())
    }

    /// Submit `enc`, which ends by copying `count` elements into `staging`, wait for it and read `staging` back as `f32`.
    fn finish(
        &self,
        enc: wgpu::CommandEncoder,
        staging: &wgpu::Buffer,
        count: usize,
        scopes: [wgpu::ErrorScopeGuard; 2],
    ) -> Result<Vec<f32>, String> {
        let d = self.dev;
        let index = d.queue.submit([enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let waited = d.device.poll(wgpu::PollType::Wait { submission_index: Some(index), timeout: Some(TIMEOUT) });
        let [oom, validation] = scopes;
        let (validation, oom) = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
        if let Some(e) = oom {
            self.pool.lock().unwrap_or_else(PoisonError::into_inner).clear();
            return Err(format!("the GPU ran out of memory: {e}"));
        }
        if let Some(e) = validation {
            self.pool.lock().unwrap_or_else(PoisonError::into_inner).clear();
            mark_dev_broken(d, &format!("device error: {e}"));
            return Err(format!("the GPU reported an error: {e}"));
        }
        if let Err(e) = waited {
            self.pool.lock().unwrap_or_else(PoisonError::into_inner).clear();
            mark_dev_broken(d, &format!("the GPU did not finish a face model: {e}"));
            return Err(format!("the GPU did not finish: {e}"));
        }
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(format!("reading the result back failed: {e}")),
            Err(e) => return Err(format!("reading the result back failed: {e}")),
        }
        let out = {
            let data = slice.get_mapped_range().map_err(|e| format!("reading the result back failed: {e:?}"))?;
            match self.precision {
                ActivePrecision::Fp16 => {
                    data.as_chunks::<2>().0.iter().take(count).map(|b| f16::from_bits(u16::from_le_bytes(*b)).to_f32()).collect::<Vec<f32>>()
                }
                ActivePrecision::Fp32 => data.as_chunks::<4>().0.iter().take(count).map(|b| f32::from_le_bytes(*b)).collect::<Vec<f32>>(),
            }
        };
        staging.unmap();
        Ok(out)
    }

    fn scopes(&self) -> [wgpu::ErrorScopeGuard; 2] {
        [self.dev.device.push_error_scope(wgpu::ErrorFilter::OutOfMemory), self.dev.device.push_error_scope(wgpu::ErrorFilter::Validation)]
    }

    fn staging(&self, bytes: u64) -> wgpu::Buffer {
        self.dev.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces read-back"),
            size: bytes.max(4).div_ceil(4) * 4,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn scratch(&self, bytes: u64) -> wgpu::Buffer {
        self.dev.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("faces values"),
            size: bytes.max(4).div_ceil(4) * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// One convolution, uploaded and read back on its own.
    fn run(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        let elem = self.precision.elem_bytes();
        let total = job.cout.checked_mul(job.oh).and_then(|v| v.checked_mul(job.ow)).ok_or("a layer too large")?;
        let bytes = total as u64 * elem;
        if bytes > self.largest_binding() || job.input.len() as u64 * elem > self.largest_binding() {
            return Ok(None);
        }
        if self.precision == ActivePrecision::Fp16
            && (f16_overflows(job.weights) || job.bias.is_some_and(f16_overflows) || f16_overflows(job.input))
        {
            return Err("convolution values exceed FP16 range".into());
        }
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let _turn = Turn::take(self.dev.card);
        let d = self.dev;
        let scopes = self.scopes();
        let input = self.upload_values("faces input", job.input);
        let output = self.scratch(bytes);
        let mut local_pool = Vec::new();
        let mut keep_alive = Vec::new();
        let Some(list) = self.conv_dispatches_batched(job, 1, &input, &output, &mut local_pool, &mut keep_alive)? else {
            let [oom, validation] = scopes;
            let _ = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
            return Ok(None);
        };
        let staging = self.staging(bytes);
        let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        self.record(&mut enc, &list)?;
        enc.copy_buffer_to_buffer(&output, 0, &staging, 0, bytes.max(4).div_ceil(4) * 4);
        let res = self.finish(enc, &staging, total, scopes)?;
        let mut gpool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
        for (sz, buf) in local_pool.into_iter().chain(keep_alive) {
            gpool.put(sz, buf);
        }
        gpool.put(bytes.max(4).div_ceil(4) * 4, output);
        Ok(Some(res))
    }

    /// The dispatch of one batched non-convolution step.
    #[allow(clippy::too_many_arguments)]
    fn op_dispatch_batched(
        &self,
        op: &Op,
        lens: &[usize],
        step_args: &[Arg],
        batch: usize,
        out_len_one: usize,
        args: [wgpu::Buffer; 3],
        out: &wgpu::Buffer,
    ) -> Result<Option<Dispatch>, String> {
        let d = self.dev;
        let limit = d.limits.max_compute_workgroups_per_dimension as usize;
        let u = u32_of;
        let total_elems = batch.checked_mul(out_len_one).ok_or("a layer too large")?;
        if total_elems == 0 {
            return Ok(None);
        }
        let n = u(total_elems)?;
        let per_value = |pipeline: &'static wgpu::ComputePipeline, rest: &[u32]| -> Option<Dispatch> {
            let groups = flat(total_elems.div_ceil(256), limit)?;
            let mut all = vec![n, groups[2]];
            all.extend_from_slice(rest);
            Some(Dispatch {
                pipeline,
                params: params24(&all),
                buffers: [args[0].clone(), args[1].clone(), args[2].clone(), out.clone()],
                groups: [groups[0], groups[1], 1],
            })
        };
        let per_group = |pipeline: &'static wgpu::ComputePipeline, rest: &[u32]| -> Option<Dispatch> {
            let groups = flat(total_elems, limit)?;
            let mut all = vec![n, groups[2]];
            all.extend_from_slice(rest);
            Some(Dispatch {
                pipeline,
                params: params24(&all),
                buffers: [args[0].clone(), args[1].clone(), args[2].clone(), out.clone()],
                groups: [groups[0], groups[1], 1],
            })
        };
        let four = |v: &[usize; 4]| -> Result<[u32; 4], String> { Ok([u(v[0])?, u(v[1])?, u(v[2])?, u(v[3])?]) };
        let k = &self.pipes().ops;
        Ok(match op {
            Op::Conv(_) => None,
            Op::Unary(f) => per_value(&k.unary, &[u32::from(*f == Unary::Sigmoid)]),
            Op::ChannelAffine { channels, plane } => per_value(&k.affine, &[u(*plane)?, u(*channels)?]),
            Op::ChannelPrelu { channels, plane } => per_value(&k.prelu, &[u(*plane)?, u(*channels)?]),
            Op::Binary { op, dims, a, b } => {
                let kind = match op {
                    Binary::Add => 0,
                    Binary::Sub => 1,
                    Binary::Mul => 2,
                };
                let mut dims_b = *dims;
                let mut a_b = *a;
                let mut b_b = *b;
                if batch > 1 {
                    dims_b[0] = batch;
                    a_b[0] = match step_args.first() {
                        Some(Arg::Value(v)) => lens.get(*v).copied().unwrap_or(0),
                        _ => 0,
                    };
                    b_b[0] = match step_args.get(1) {
                        Some(Arg::Value(v)) => lens.get(*v).copied().unwrap_or(0),
                        _ => 0,
                    };
                }
                let mut rest = vec![kind];
                rest.extend(four(&dims_b)?);
                rest.extend(four(&a_b)?);
                rest.extend(four(&b_b)?);
                per_value(&k.binary, &rest)
            }
            Op::Transpose { dims, strides } => {
                let mut dims_b = *dims;
                let mut strides_b = *strides;
                if batch > 1 {
                    dims_b[0] = batch;
                    strides_b[0] = match step_args.first() {
                        Some(Arg::Value(v)) => lens.get(*v).copied().unwrap_or(0),
                        _ => 0,
                    };
                }
                let mut rest = vec![0];
                rest.extend(four(&dims_b)?);
                rest.extend(four(&strides_b)?);
                per_value(&k.transpose, &rest)
            }
            Op::MaxPool { h, w, kh, kw, sh, sw, oh, ow, .. } => {
                per_value(&k.maxpool, &[u(*h)?, u(*w)?, u(*kh)?, u(*kw)?, u(*sh)?, u(*sw)?, u(*oh)?, u(*ow)?])
            }
            Op::Resize { h, w, sh, sw, .. } => per_value(&k.resize, &[u(*h)?, u(*w)?, u(*sh)?, u(*sw)?]),
            Op::GlobalAverage { plane, .. } => {
                if *plane == 0 {
                    return Ok(None);
                }
                per_group(&k.gavg, &[u(*plane)?])
            }
            Op::Gemv { k: kk, n: nn, trans_b, alpha } => per_group(&k.gemv, &[u(*kk)?, u(*nn)?, u32::from(*trans_b), alpha.to_bits()]),
        })
    }

    /// Whether `prog` can execute a batch `[B, ...]` across all layers in a single pass per layer.
    fn supports_batching(prog: &Program) -> bool {
        prog.steps.iter().all(|s| match &s.op {
            Op::Binary { dims, .. } | Op::Transpose { dims, .. } => dims[0] == 1,
            _ => true,
        })
    }

    /// Maximum batch chunk size that fits storage buffer bindings and total staging size.
    fn max_batch_chunk(&self, prog: &Program, count: usize) -> usize {
        if count <= 1 || !Self::supports_batching(prog) {
            return 1;
        }
        let elem = self.precision.elem_bytes();
        let largest_binding = self.largest_binding();
        let max_tensor = prog.lens.iter().copied().max().unwrap_or(1).max(1) as u64;
        let max_conv_partial = prog
            .steps
            .iter()
            .filter_map(|s| match &s.op {
                Op::Conv(j) => j.cout.checked_mul(j.oh).and_then(|v| v.checked_mul(j.ow)),
                _ => None,
            })
            .max()
            .unwrap_or(1)
            .max(1) as u64;
        let by_binding = (largest_binding / (max_tensor.max(max_conv_partial) * elem)).max(1) as usize;
        let out_elems: u64 = prog.outputs.iter().map(|(_, v, _)| prog.lens.get(*v).copied().unwrap_or(0) as u64).sum::<u64>().max(1);
        let by_staging = (self.dev.limits.max_buffer_size / (out_elems * elem)).max(1) as usize;
        count.min(by_binding).min(by_staging).max(1)
    }

    /// Run a whole program over `inputs`, batching `[B, ...]` so weight tiles are shared across faces/windows
    /// with one upload and one read-back per chunk.
    fn program(&self, prog: &Program, inputs: &[&[f32]]) -> Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        let d = self.dev;
        let elem = self.precision.elem_bytes();
        let largest = self.largest_binding();
        if prog.lens.iter().any(|l| *l as u64 * elem > largest) || inputs.iter().any(|i| Some(&i.len()) != prog.lens.get(INPUT)) {
            return Ok(None);
        }
        if inputs.is_empty() {
            return Ok(Some(Vec::new()));
        }
        if self.precision == ActivePrecision::Fp16 {
            let weights_overflow = prog.steps.iter().any(|s| {
                let op_ov = matches!(&s.op, Op::Conv(j) if f16_overflows(j.weights) || j.bias.is_some_and(f16_overflows));
                let arg_ov = s.args.iter().any(|a| matches!(a, Arg::Const { data, .. } if f16_overflows(data)));
                op_ov || arg_ov
            });
            if weights_overflow || inputs.iter().any(|i| f16_overflows(i)) {
                return Err("model values exceed FP16 range".into());
            }
        }
        let out_elems_one: usize = prog.outputs.iter().map(|(_, v, _)| prog.lens.get(*v).copied().unwrap_or(0)).sum();
        let Some(_work) = crate::exit::enter() else { return Err("the app is closing".into()) };
        let _turn = Turn::take(self.dev.card);

        let chunk_max = self.max_batch_chunk(prog, inputs.len());
        let mut all_per_input: Vec<Vec<Vec<f32>>> = Vec::with_capacity(inputs.len());
        for chunk in inputs.chunks(chunk_max) {
            let batch = chunk.len();
            let _ = out_elems_one.checked_mul(batch).ok_or("too many faces at once")?;
            let total_bytes: u64 = prog
                .outputs
                .iter()
                .map(|(_, v, _)| {
                    let raw = prog.lens.get(*v).copied().unwrap_or(0) as u64 * batch as u64 * elem;
                    raw.max(4).div_ceil(4) * 4
                })
                .sum::<u64>()
                .max(4);
            if total_bytes > d.limits.max_buffer_size {
                return Ok(None);
            }
            let scopes = self.scopes();
            let staging = self.staging(total_bytes);
            let mut enc = d.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("faces program") });
            let mut local_pool: Vec<(u64, wgpu::Buffer)> = Vec::new();
            let mut keep_alive: Vec<(u64, wgpu::Buffer)> = Vec::new();
            let mut values: Vec<Option<(u64, wgpu::Buffer)>> = vec![None; prog.lens.len()];

            let first_buf = self.upload_batch_inputs(chunk);
            let first_bytes = ((chunk[0].len() * batch) as u64 * elem).max(4).div_ceil(4) * 4;
            if let Some(v) = values.get_mut(INPUT) {
                *v = Some((first_bytes, first_buf));
            }
            let mut list = Vec::new();
            let mut fits = true;
            for step in &prog.steps {
                let len_one = prog.lens.get(step.out).copied().ok_or("a step writes a value the program does not have")?;
                let out_bytes = (len_one as u64 * batch as u64 * elem).max(4).div_ceil(4) * 4;
                let (out_cap, out) = self.take_scratch(&mut local_pool, out_bytes);
                let mut args = Vec::with_capacity(3);
                for a in &step.args {
                    args.push(match a {
                        Arg::Value(v) => {
                            values.get(*v).and_then(|v| v.as_ref()).map(|(_, b)| b.clone()).ok_or("a step reads a value before it is made")?
                        }
                        Arg::Const { id, data } => self.constant(*id, data),
                    });
                }
                let dispatches = match &step.op {
                    Op::Conv(job) => {
                        let input = args.first().ok_or("a convolution without input")?;
                        self.conv_dispatches_batched(job, batch, input, &out, &mut local_pool, &mut keep_alive)?
                    }
                    op => {
                        let arg = |i: usize| args.get(i).cloned().unwrap_or_else(|| d.dummy.clone());
                        self.op_dispatch_batched(op, &prog.lens, &step.args, batch, len_one, [arg(0), arg(1), arg(2)], &out)?
                            .map(|x| vec![x])
                    }
                };
                let Some(dispatches) = dispatches else {
                    fits = false;
                    break;
                };
                list.extend(dispatches);
                if let Some(v) = values.get_mut(step.out) {
                    *v = Some((out_cap, out));
                }
                for f in &step.free {
                    if let Some(b) = values.get_mut(*f).and_then(Option::take) {
                        local_pool.push(b);
                    }
                }
            }
            if !fits {
                let [oom, validation] = scopes;
                let _ = (pollster::block_on(validation.pop()), pollster::block_on(oom.pop()));
                return Ok(None);
            }
            self.record(&mut enc, &list)?;
            let mut copy_offsets = Vec::with_capacity(prog.outputs.len());
            let mut at = 0u64;
            for (_, v, _) in &prog.outputs {
                let (bytes, buffer) = values.get(*v).and_then(|v| v.as_ref()).ok_or("an output was never made")?;
                let raw_bytes = prog.lens.get(*v).copied().unwrap_or(0) as u64 * batch as u64 * elem;
                let aligned_bytes = raw_bytes.max(4).div_ceil(4) * 4;
                enc.copy_buffer_to_buffer(buffer, 0, &staging, at, aligned_bytes.min(*bytes));
                copy_offsets.push((at, prog.lens.get(*v).copied().unwrap_or(0)));
                at += aligned_bytes;
            }
            let total_read_elems = (at / elem) as usize;
            let flat_out = self.finish(enc, &staging, total_read_elems, scopes)?;
            {
                let mut gpool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
                for (sz, buf) in local_pool.into_iter().chain(keep_alive) {
                    gpool.put(sz, buf);
                }
                for (sz, buf) in values.into_iter().flatten() {
                    if sz > 0 {
                        gpool.put(sz, buf);
                    }
                }
            }
            // De-interleave `[output_k][batch][len_k]` -> `per_input[b][output_k]`.
            let mut chunk_per_input: Vec<Vec<Vec<f32>>> = vec![Vec::with_capacity(prog.outputs.len()); batch];
            for (byte_off, len_one) in copy_offsets {
                let elem_off = (byte_off / elem) as usize;
                for (b, slot) in chunk_per_input.iter_mut().enumerate() {
                    let start = elem_off + b * len_one;
                    let end = start + len_one;
                    let slice = flat_out.get(start..end).ok_or("the read-back is short")?;
                    slot.push(slice.to_vec());
                }
            }
            all_per_input.extend(chunk_per_input);
        }
        Ok(Some(all_per_input))
    }
}

/// Run `f`, turning a panic into a broken device (`card`'s only).
fn guarded<T>(dev: &Dev, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        mark_dev_broken(dev, "a face model on the GPU panicked");
        Err("the GPU runner gave up".into())
    })
}

impl Accelerator for FaceGpu {
    fn device(&self) -> String {
        format!("{} ({:?})", self.dev.info.name, self.dev.info.backend)
    }

    fn precision(&self) -> &'static str {
        self.precision.id()
    }

    fn adapter_key(&self) -> Option<String> {
        Some(self.dev.adapter_key.to_string())
    }

    fn plan_cached(&self, program_fingerprint: u64) -> bool {
        let hit = read_cached_plan(&self.dev.driver_sig, self.precision, program_fingerprint) == Some(true);
        self.cache_hit.store(hit, Ordering::Relaxed);
        hit
    }

    fn record_plan(&self, program_fingerprint: u64, faster: bool, fast_ms: f64, cpu_ms: f64) {
        write_cached_plan(&self.dev.driver_sig, self.precision, program_fingerprint, faster, fast_ms, cpu_ms);
    }

    fn conv(&self, job: &ConvJob) -> Result<Option<Vec<f32>>, String> {
        if let Some(why) = dev_broken_reason(self.dev) {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if crate::switched_off().is_some() || job.macs() < MIN_MACS {
            return Ok(None);
        }
        guarded(self.dev, || self.run(job))
    }

    fn run_program(&self, program: &Program, inputs: &[&[f32]]) -> Result<Option<Vec<Vec<Vec<f32>>>>, String> {
        if let Some(why) = dev_broken_reason(self.dev) {
            return Err(format!("the GPU is not used any more: {why}"));
        }
        if crate::switched_off().is_some() || PER_CONV.load(Ordering::Relaxed) {
            return Ok(None);
        }
        guarded(self.dev, || self.program(program, inputs))
    }
}

/// Tests and benchmarks only: run convolutions one by one (the first step's way) instead of whole programs.
static PER_CONV: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;

    /// The second graphics card draws no window: only the share set for it holds its face runs back.
    #[test]
    fn each_card_has_its_own_share_of_the_time() {
        let p = Pace { concurrent: 2, share: 0.75, second_share: 1.0, running: 0 };
        assert_eq!((p.share_on(Card::Editing), p.share_on(Card::Second)), (0.75, 1.0));
        assert_eq!((share_of(f32::NAN), share_of(0.0), share_of(3.0)), (1.0, 0.05, 1.0));
    }

    #[test]
    fn precision_and_auto_adapter_ranking_prefer_dedicated_nvidia() {
        assert_eq!(Precision::parse(""), Some(Precision::Auto));
        assert_eq!(Precision::parse("auto"), Some(Precision::Auto));
        assert_eq!(Precision::parse("fp16"), Some(Precision::Fp16));
        assert_eq!(Precision::parse("f16"), Some(Precision::Fp16));
        assert_eq!(Precision::parse("fp32"), Some(Precision::Fp32));
        assert_eq!(Precision::parse("bogus"), None);

        let mk = |name: &str, vendor: u32, kind: AdapterKind, drives_screen: bool| AdapterEntry {
            key: AdapterKey { backend: "vulkan".into(), vendor, device: 1, name: name.into(), index: 1 },
            name: name.into(),
            kind,
            memory: None,
            backend: "vulkan".into(),
            drives_screen,
            usable: true,
            why_not: None,
        };
        let intel_igpu = mk("Intel UHD", 0x8086, AdapterKind::BuiltIn, true);
        let amd_dgpu = mk("AMD RX 7900", 0x1002, AdapterKind::Dedicated, false);
        let nv_screen = mk("NVIDIA RTX 4070", VENDOR_NVIDIA, AdapterKind::Dedicated, true);
        let nv_offscreen = mk("NVIDIA RTX 4090", VENDOR_NVIDIA, AdapterKind::Dedicated, false);
        assert!(auto_adapter_score(&nv_offscreen) > auto_adapter_score(&nv_screen));
        assert!(auto_adapter_score(&nv_screen) > auto_adapter_score(&amd_dgpu));
        assert!(auto_adapter_score(&amd_dgpu) > auto_adapter_score(&intel_igpu));
    }

    #[test]
    fn persistent_plan_cache_roundtrips_and_rejects_mismatches() {
        let dir = std::env::temp_dir().join(format!("lc-face-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        set_cache_dir(Some(dir.clone()));
        let sig = "vulkan:10de:2783:NVIDIA RTX 4070#1|560.35|Vulkan";
        let fp = 0x1234_5678_9abc_def0;
        assert_eq!(read_cached_plan(sig, ActivePrecision::Fp16, fp), None);
        write_cached_plan(sig, ActivePrecision::Fp16, fp, true, 12.5, 48.0);
        assert_eq!(read_cached_plan(sig, ActivePrecision::Fp16, fp), Some(true));
        // Different precision or driver signature misses cleanly.
        assert_eq!(read_cached_plan(sig, ActivePrecision::Fp32, fp), None);
        assert_eq!(read_cached_plan("vulkan:10de:2783:NVIDIA RTX 4070#1|561.00|Vulkan", ActivePrecision::Fp16, fp), None);
        // Corrupting the file invalidates the integrity check without panicking.
        plan_mem().lock().unwrap().clear();
        let path = plan_cache_path(&dir, sig, ActivePrecision::Fp16, fp);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[10] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(read_cached_plan(sig, ActivePrecision::Fp16, fp), None);
        set_cache_dir(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_cache_rejects_hostile_bytes() {
        let sig = "vulkan:10de:2783:NVIDIA RTX 4070#1|560.35|Vulkan";
        let fp = 0xfeed_face_cafe_beef;
        let build = |faster: u8, prec: u8, fprint: u64, fast_ms: f64, cpu_ms: f64, sig_len: u16, body: &[u8]| {
            let mut buf = Vec::new();
            buf.extend_from_slice(PLAN_MAGIC);
            buf.push(PLAN_VERSION);
            buf.push(faster);
            buf.push(prec);
            buf.extend_from_slice(&fprint.to_le_bytes());
            buf.extend_from_slice(&fast_ms.to_le_bytes());
            buf.extend_from_slice(&cpu_ms.to_le_bytes());
            buf.extend_from_slice(&sig_len.to_le_bytes());
            buf.extend_from_slice(body);
            let sum = fnv1a64(&buf);
            buf.extend_from_slice(&sum.to_le_bytes());
            buf
        };

        let valid = build(1, ActivePrecision::Fp16.tag(), fp, 5.0, 20.0, sig.len() as u16, sig.as_bytes());
        assert_eq!(parse_cached_plan(&valid, sig, ActivePrecision::Fp16, fp), Some(true));

        // Truncations at every length and single-byte bit flips.
        for n in 0..valid.len() {
            assert_eq!(parse_cached_plan(&valid[..n], sig, ActivePrecision::Fp16, fp), None, "truncation at {n}");
        }
        for i in 0..valid.len() {
            let mut flipped = valid.clone();
            flipped[i] ^= 0x55;
            assert_eq!(parse_cached_plan(&flipped, sig, ActivePrecision::Fp16, fp), None, "bit flip at {i}");
        }

        // Hostile payloads with valid FNV checksums.
        assert_eq!(parse_cached_plan(&[], sig, ActivePrecision::Fp16, fp), None);
        assert_eq!(parse_cached_plan(&vec![0u8; 4097], sig, ActivePrecision::Fp16, fp), None);
        assert_eq!(parse_cached_plan(&build(2, 1, fp, 5.0, 20.0, sig.len() as u16, sig.as_bytes()), sig, ActivePrecision::Fp16, fp), None);
        assert_eq!(parse_cached_plan(&build(1, 1, fp, f64::NAN, 20.0, sig.len() as u16, sig.as_bytes()), sig, ActivePrecision::Fp16, fp), None);
        assert_eq!(parse_cached_plan(&build(1, 1, fp, -1.0, 20.0, sig.len() as u16, sig.as_bytes()), sig, ActivePrecision::Fp16, fp), None);
        assert_eq!(parse_cached_plan(&build(1, 1, fp, 5.0, 20.0, u16::MAX, sig.as_bytes()), sig, ActivePrecision::Fp16, fp), None);
        let mut padded = sig.as_bytes().to_vec();
        padded.extend_from_slice(b"extra");
        assert_eq!(parse_cached_plan(&build(1, 1, fp, 5.0, 20.0, sig.len() as u16, &padded), sig, ActivePrecision::Fp16, fp), None);
        let bad_utf8 = [0xffu8; 4];
        assert_eq!(parse_cached_plan(&build(1, 1, fp, 5.0, 20.0, 4, &bad_utf8), sig, ActivePrecision::Fp16, fp), None);
    }

    /// A failure on one card stops face models there only.
    #[test]
    fn a_broken_card_keeps_its_first_reason_and_leaves_the_other() {
        assert_eq!(broken_reason_on(Card::Second), None);
        give_up_on(Card::Second, "device lost");
        give_up_on(Card::Second, "a later error");
        assert_eq!(broken_reason_on(Card::Second).as_deref(), Some("device lost"));
        assert_eq!(broken_reason_on(Card::Editing), None, "the card for editing's device is separate");
        assert!(dev_on(Card::Second).err().unwrap_or_default().contains("device lost"));
        assert!(accelerator_on(Card::Second).is_err());
        match accelerator() {
            Ok(a) => assert_eq!(a.card(), Card::Editing),
            Err(e) => lcc_testkit::skip!(Gpu, "no GPU for the face kernels: {e}"),
        }
        reset();
        assert_eq!(broken_reason_on(Card::Second), None);
    }

    /// Both FP32 and FP16 face kernels validate with naga without a GPU.
    #[test]
    fn the_face_kernels_validate_without_a_gpu() {
        assert_eq!(crate::validate_wgsl("faces_conv", include_str!("wgsl/faces_conv.wgsl")), Ok(()));
        assert_eq!(crate::validate_wgsl("faces_ops", include_str!("wgsl/faces_ops.wgsl")), Ok(()));
        assert_eq!(crate::validate_wgsl("faces_conv_f16", include_str!("wgsl/faces_conv_f16.wgsl")), Ok(()));
        assert_eq!(crate::validate_wgsl("faces_ops_f16", include_str!("wgsl/faces_ops_f16.wgsl")), Ok(()));
    }

    fn gpu() -> Option<Arc<FaceGpu>> {
        match accelerator() {
            Ok(a) => Some(a),
            Err(e) => {
                lcc_testkit::skip!(Gpu, "no GPU for the face kernels: {e}");
                None
            }
        }
    }

    /// The plain definition of a convolution, to check the kernels against.
    fn reference(j: &ConvJob) -> Vec<f32> {
        let (m, cg) = (j.cout / j.groups, j.cin / j.groups);
        let mut out = vec![0f32; j.cout * j.oh * j.ow];
        for co in 0..j.cout {
            let g = co / m;
            for oy in 0..j.oh {
                for ox in 0..j.ow {
                    let mut v = j.bias.map_or(0.0, |b| b[co]);
                    for ci in 0..cg {
                        for ky in 0..j.kh {
                            for kx in 0..j.kw {
                                let (iy, ix) = ((oy * j.sh + ky) as isize - j.pt as isize, (ox * j.sw + kx) as isize - j.pl as isize);
                                if iy >= 0 && ix >= 0 && (iy as usize) < j.h && (ix as usize) < j.w {
                                    v += j.weights[((co * cg + ci) * j.kh + ky) * j.kw + kx]
                                        * j.input[((g * cg + ci) * j.h + iy as usize) * j.w + ix as usize];
                                }
                            }
                        }
                    }
                    out[(co * j.oh + oy) * j.ow + ox] = v;
                }
            }
        }
        out
    }

    #[test]
    fn the_gpu_convolution_gives_what_the_definition_gives() {
        let Some(gpu) = gpu() else { return };
        let tol = if gpu.precision() == "fp16" { 2e-2 } else { 1e-3 };
        // (cin, cout, groups, k, stride, pad, h, w): dense, strided, depthwise, grouped, pointwise, odd sizes
        for (layer, (cin, cout, groups, k, s, p, h, w)) in [
            (3, 16, 1, 3, 2, 1, 41, 37),
            (64, 64, 1, 3, 1, 1, 28, 28),
            (32, 32, 32, 3, 1, 1, 30, 31),
            (16, 32, 4, 3, 2, 1, 33, 20),
            (48, 72, 1, 1, 1, 0, 50, 45),
            (8, 130, 1, 5, 1, 2, 23, 19),
        ]
        .into_iter()
        .enumerate()
        {
            let cg = cin / groups;
            let weights: Vec<f32> = (0..cout * cg * k * k).map(|i| ((i * 37 % 23) as f32 - 11.0) * 0.03).collect();
            let bias: Vec<f32> = (0..cout).map(|i| i as f32 * 0.01 - 0.3).collect();
            let input: Vec<f32> = (0..cin * h * w).map(|i| ((i * 13 % 29) as f32 - 14.0) * 0.07).collect();
            let (oh, ow) = ((h + 2 * p - k) / s + 1, (w + 2 * p - k) / s + 1);
            let job = ConvJob {
                layer,
                cin,
                h,
                w,
                cout,
                groups,
                kh: k,
                kw: k,
                sh: s,
                sw: s,
                pt: p,
                pl: p,
                oh,
                ow,
                weights: &weights,
                bias: Some(&bias),
                input: &input,
            };
            let got = gpu.run(&job).unwrap().unwrap();
            let want = reference(&job);
            let worst = got.iter().zip(&want).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
            assert!(worst < tol, "case {layer} ({}): off by {worst}", gpu.precision());
        }
        assert!(gpu.resident_bytes() > 0, "the weights stay on the device");
    }

    #[test]
    fn every_operator_on_the_gpu_gives_what_the_cpu_gives() {
        let Some(gpu) = gpu() else { return };
        let net = lcc_faces::net::Net::new(lcc_faces::synthetic::every_operator_graph()).unwrap();
        let inputs: Vec<lcc_faces::net::Tensor> = (0..5)
            .map(|s| {
                lcc_faces::net::Tensor::new(vec![1, 4, 8, 8], (0..256).map(|i| (((i * 31 + s * 7) % 37) as f32 - 18.0) * 0.05).collect()).unwrap()
            })
            .collect();
        let cpu: Vec<_> = inputs.iter().map(|x| net.run(x.clone()).unwrap()).collect();
        let program = net.program(&[1, 4, 8, 8]).unwrap().unwrap();
        let slices: Vec<&[f32]> = inputs.iter().map(|t| t.data.as_slice()).collect();
        for g in [gpu.as_fp32(), gpu] {
            let tol = if g.precision() == "fp16" { 5e-3 } else { 1e-4 };
            let got = g.program(&program, &slices).unwrap().unwrap();
            assert_eq!(got.len(), cpu.len());
            for (want, got) in cpu.iter().zip(&got) {
                for ((name, want), got) in want.iter().zip(got) {
                    let worst = want.data.iter().zip(got).fold(0f32, |m, (a, b)| m.max((a - b).abs()));
                    assert!(worst < tol, "{name} ({}): off by {worst}", g.precision());
                }
            }
        }
    }

    /// The real models on the GPU agree with the CPU interpreter: `LC_FACE_MODELS=<folder>` holding
    /// `face_detection_yunet_2023mar.onnx`, `face_recognition_sface_2021dec.onnx` and (optionally) `glintr100.onnx`.
    #[test]
    fn real_face_models_on_the_gpu_agree_with_the_cpu() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else {
            lcc_testkit::skip!(Model, "LC_FACE_MODELS is not set");
            return;
        };
        if gpu().is_none() {
            return;
        }
        let det = lcc_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (900, 700);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i / 3 % w) as f32 * 0.25 + (i / 3 / w) as f32 * 0.3 + (i * 7919 % 41) as f32) as u8).collect();
        let opts = lcc_faces::yunet::Options { score: 0.0, ..Default::default() };
        let cpu = det.detect(&rgb, w, h, &opts).unwrap();
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let fast = det.detect(&rgb, w, h, &opts).unwrap();
        assert!(det.net().accelerator().is_some());
        assert_eq!(cpu.len(), fast.len());
        for (a, b) in cpu.iter().zip(&fast) {
            assert!((a.score - b.score).abs() < 1e-3, "{} vs {}", a.score, b.score);
        }
        for (file, sha) in
            [("face_recognition_sface_2021dec.onnx", lcc_faces::known::SFACE_SHA256), ("glintr100.onnx", lcc_faces::known::AURAFACE_SHA256)]
        {
            let path = dir.join(file);
            if !path.exists() {
                continue;
            }
            let e = lcc_faces::runtime::Embedder::load(&path, &lcc_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let face: Vec<u8> = (0..ew * eh * 3).map(|i| ((i * 13) % 251) as u8).collect();
            let cpu = e.embed(&face).unwrap();
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let fast = e.embed(&face).unwrap();
            let cosine: f32 = cpu.iter().zip(&fast).map(|(a, b)| a * b).sum();
            assert!(cosine > 0.9999, "{file}: cosine {cosine}");
        }
    }

    /// The MediaPipe Face Landmarker models (`lcc-tflite`, lowered to device programs) on the GPU agree with the CPU
    /// interpreter on a synthetic picture, alone and batched: `LC_MEDIAPIPE_FACE=<folder>` holding `face_landmarker.task`.
    /// With `--nocapture`, prints the timings.
    #[test]
    fn mediapipe_face_models_on_the_gpu_agree_with_the_cpu() {
        let Some(dir) = std::env::var_os("LC_MEDIAPIPE_FACE").map(std::path::PathBuf::from) else {
            lcc_testkit::skip!(Model, "LC_MEDIAPIPE_FACE is not set");
            return;
        };
        if gpu().is_none() {
            return;
        }
        let bundle = std::fs::read(dir.join("face_landmarker.task")).unwrap();
        for name in ["face_detector.tflite", "face_landmarks_detector.tflite"] {
            let model = lcc_tflite::Model::from_bytes(lcc_tflite::zip::entry(&bundle, name).unwrap()).unwrap();
            let n: usize = model.input_shapes()[0].iter().product();
            let inputs: Vec<Vec<f32>> =
                (0..8).map(|s| (0..n).map(|i| ((i + s * 977) as f32 * 0.0137).sin() * 0.9 + ((i / 3) % 7) as f32 * 0.02).collect()).collect();
            let refs: Vec<&[f32]> = inputs.iter().map(Vec::as_slice).collect();
            let cpu: Vec<_> = refs.iter().map(|x| model.run(&[x]).unwrap()).collect();
            let acc = accelerator().unwrap();
            let tol = if acc.precision() == "fp16" { 1e-2 } else { 1e-3 };
            let (on_device, steps) = model.device_steps();
            model.accelerate(acc, refs[0], tol, false).unwrap();
            let one = model.run(&[refs[1]]).unwrap();
            let batch = model.run_batch(&refs).unwrap();
            assert!(model.accelerator().is_some(), "{name}: the GPU was dropped");
            let close = |a: &[Vec<f32>], b: &[Vec<f32>]| {
                for (x, y) in a.iter().zip(b) {
                    let scale = x.iter().fold(1e-6f32, |m, v| m.max(v.abs()));
                    let worst = x.iter().zip(y).fold(0f32, |m, (p, q)| m.max((p - q).abs()));
                    assert!(worst <= tol * scale, "{name}: off by {} of {scale}", worst);
                }
            };
            close(&cpu[1], &one);
            for (c, g) in cpu.iter().zip(&batch) {
                close(c, g);
            }
            let time = |f: &mut dyn FnMut()| {
                f();
                let t = std::time::Instant::now();
                for _ in 0..5 {
                    f();
                }
                t.elapsed().as_secs_f64() * 1000.0 / 5.0
            };
            let cpu_ms = time(&mut || refs.iter().for_each(|x| drop(model.run_cpu(&[x]).unwrap())));
            let one_ms = time(&mut || refs.iter().for_each(|x| drop(model.run(&[x]).unwrap())));
            let batch_ms = time(&mut || drop(model.run_batch(&refs).unwrap()));
            eprintln!(
                "{name}: {on_device} of {steps} steps on the GPU; 8 runs: CPU {cpu_ms:.1} ms, GPU one by one {one_ms:.1} ms, \
                 GPU batched {batch_ms:.1} ms"
            );
        }
    }

    /// Timings, CPU against the GPU one convolution at a time and as whole programs (one face, and a batch of 16):
    /// `LC_FACE_MODELS=<folder> cargo test --release -p lcc-gpu faces::tests::timings -- --ignored --nocapture`.
    #[test]
    #[ignore = "a benchmark: needs LC_FACE_MODELS and a GPU"]
    fn timings() {
        let Some(dir) = std::env::var_os("LC_FACE_MODELS").map(std::path::PathBuf::from) else { return };
        let Some(_) = gpu() else { return };
        let time = |runs: usize, f: &mut dyn FnMut()| {
            f();
            let t = std::time::Instant::now();
            for _ in 0..runs {
                f();
            }
            t.elapsed().as_secs_f64() * 1000.0 / runs as f64
        };
        let det = lcc_faces::yunet::Detector::new(&std::fs::read(dir.join("face_detection_yunet_2023mar.onnx")).unwrap()).unwrap();
        let (w, h) = (1024, 768);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| (i * 7 % 251) as u8).collect();
        let opts = lcc_faces::yunet::Options::default();
        let cpu = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        PER_CONV.store(true, Ordering::Relaxed);
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let per_conv = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        PER_CONV.store(false, Ordering::Relaxed);
        det.accelerate(accelerator().unwrap(), false).unwrap();
        let whole = time(10, &mut || drop(det.detect(&rgb, w, h, &opts).unwrap()));
        assert!(det.net().accelerator().is_some());
        eprintln!("YuNet 640: CPU {cpu:.1} ms, GPU per convolution {per_conv:.1} ms, GPU whole network {whole:.1} ms");
        for (file, sha) in
            [("face_recognition_sface_2021dec.onnx", lcc_faces::known::SFACE_SHA256), ("glintr100.onnx", lcc_faces::known::AURAFACE_SHA256)]
        {
            let e = lcc_faces::runtime::Embedder::load(&dir.join(file), &lcc_faces::known::lookup(sha).unwrap()).unwrap();
            let (ew, eh) = e.input_size();
            let faces: Vec<Vec<u8>> = (0..16).map(|s| (0..ew * eh * 3).map(|i| ((i + s * 101) * 13 % 251) as u8).collect()).collect();
            let refs: Vec<&[u8]> = faces.iter().map(Vec::as_slice).collect();
            let cpu = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            PER_CONV.store(true, Ordering::Relaxed);
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let per_conv = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            PER_CONV.store(false, Ordering::Relaxed);
            e.accelerate(accelerator().unwrap(), false).unwrap();
            let whole = time(5, &mut || drop(e.embed(&faces[0]).unwrap()));
            let batch = time(3, &mut || drop(e.embed_batch(&refs))) / 16.0;
            assert!(e.net().accelerator().is_some());
            eprintln!(
                "{file}: CPU {cpu:.1} ms, GPU per convolution {per_conv:.1} ms, GPU whole network {whole:.1} ms, batch of 16 {batch:.1} ms a face"
            );
        }
    }
}
