//! Devices of their own for GPU compute that is not rendering (AI Denoise's network, the content models'
//! transformers): one wgpu device per user, separate from the render device, so a job and the interactive renders
//! are separate contexts the driver time-slices, and a failure of one never takes the other down.
//!
//! The same switches as GPU rendering apply (`LIGHTCRAFT_GPU`, `LIGHTCRAFT_GPU_BACKEND`, the GPU preference:
//! [`crate::switched_off`]) and the same crash sentinel around device creation; `LIGHTCRAFT_GPU_ADAPTER` picks an
//! adapter by (part of) its name. A device that reports an error or is lost marks its user [`Broken`]: the user stops
//! using the GPU for the rest of the process.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) struct Dev {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
    pub limits: wgpu::Limits,
}

impl Dev {
    /// "NVIDIA GeForce RTX 4090 (Vulkan)".
    pub fn name(&self) -> String {
        format!("{} ({:?})", self.info.name, self.info.backend)
    }
}

/// Why a compute user stopped using its device (the first failure), once it did.
pub(crate) struct Broken {
    flag: AtomicBool,
    why: Mutex<Option<String>>,
}

impl Broken {
    pub const fn new() -> Self {
        Self { flag: AtomicBool::new(false), why: Mutex::new(None) }
    }

    pub fn mark(&self, why: &str) {
        let mut w = self.why.lock().unwrap_or_else(|e| e.into_inner());
        if w.is_none() {
            *w = Some(why.to_string());
        }
        self.flag.store(true, Ordering::Relaxed);
    }

    pub fn reason(&self) -> Option<String> {
        self.flag.load(Ordering::Relaxed).then(|| self.why.lock().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_else(|| "device error".into()))
    }

    /// Tests: use the device again.
    #[cfg(test)]
    pub fn clear(&self) {
        *self.why.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.flag.store(false, Ordering::Relaxed);
    }
}

/// Why nothing runs on the GPU once [`crate::begin_shutdown`] was called.
pub(crate) const CLOSING: &str = "LightCraft is closing";

/// The device in `cell`, made on first use by [`create`]; an error once `broken` is set, the GPU is switched off or the
/// process is closing.
pub(crate) fn get(
    cell: &'static std::sync::OnceLock<Result<Dev, String>>,
    broken: &'static Broken,
    label: &'static str,
    check: fn(&wgpu::AdapterInfo, &wgpu::Limits) -> Result<(), String>,
) -> Result<&'static Dev, String> {
    if let Some(why) = broken.reason() {
        return Err(format!("stopped after a GPU failure: {why}"));
    }
    if let Some(why) = crate::switched_off() {
        return Err(why);
    }
    // creating the device is GPU work too: not once the process is ending (issue #620)
    let _work = match cell.get() {
        Some(_) => None,
        None => Some(crate::exit::enter().ok_or(CLOSING)?),
    };
    cell.get_or_init(|| create(label, broken, check)).as_ref().map_err(|e| e.clone())
}

fn create(label: &'static str, broken: &'static Broken, check: fn(&wgpu::AdapterInfo, &wgpu::Limits) -> Result<(), String>) -> Result<Dev, String> {
    let Some(backends) = crate::backend::compute_backends() else { return Err("disabled by LIGHTCRAFT_GPU_BACKEND=off".into()) };
    crate::backend::with_init_marker(backends, || {
        std::panic::catch_unwind(|| make(backends, label, broken, check)).unwrap_or_else(|_| Err("device creation panicked".into()))
    })
}

fn make(
    backends: wgpu::Backends,
    label: &'static str,
    broken: &'static Broken,
    check: fn(&wgpu::AdapterInfo, &wgpu::Limits) -> Result<(), String>,
) -> Result<Dev, String> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    // DX12 shaders compile with FXC (issue #471)
    desc.backend_options = crate::backend::backend_options();
    // no indirect-call validation pipeline: Intel Metal fails to compile it (#250)
    desc.flags = crate::backend::instance_flags_from_env(desc.flags);
    let instance = wgpu::Instance::new(desc);
    let wanted = std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty());
    let adapter = match &wanted {
        Some(w) => {
            let all = pollster::block_on(instance.enumerate_adapters(backends));
            let names: Vec<String> = all.iter().map(|a| a.get_info().name).collect();
            all.into_iter()
                .find(|a| {
                    let i = a.get_info();
                    i.device_type != wgpu::DeviceType::Cpu && i.name.to_lowercase().contains(w.as_str())
                })
                .ok_or_else(|| format!("no GPU adapter named like `{w}` (found: {})", names.join(", ")))?
        }
        None => pollster::block_on(
            instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
        )
        .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?,
    };
    let info = adapter.get_info();
    if info.device_type == wgpu::DeviceType::Cpu {
        return Err(format!("software adapter ({}) skipped: the CPU is faster", info.name));
    }
    let limits = adapter.limits();
    check(&info, &limits)?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some(label),
        required_limits: limits.clone(),
        ..Default::default()
    }))
    .map_err(|e| format!("{}: device creation failed ({e})", info.name))?;
    device.on_uncaptured_error(std::sync::Arc::new(move |e| {
        log::error!("gpu ({label}): {e}");
        broken.mark(&format!("device error: {e}"));
    }));
    device.set_device_lost_callback(move |reason, msg| {
        log::error!("gpu ({label}): device lost ({reason:?}): {msg}");
        broken.mark(&format!("device lost ({reason:?}): {msg}"));
    });
    log::info!("gpu ({label}): {} ({:?})", info.name, info.backend);
    Ok(Dev { device, queue, info, limits })
}
