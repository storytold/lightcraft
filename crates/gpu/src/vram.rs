//! How much device memory is free right now, where that can be asked without `unsafe`: the NVIDIA
//! driver's `nvidia-smi` (Linux and Windows) and the amdgpu driver's sysfs counters (Linux). Other
//! devices (AMD/Intel on Windows needs DXGI; integrated GPUs share system memory) report `None`, and the
//! caller keeps its fixed budget. The answer is cached for a few seconds: renders ask often, and the
//! query starts a process on NVIDIA.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long an answer is reused.
const TTL: Duration = Duration::from_secs(3);
/// How long `nvidia-smi` may take before it is given up on.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;

static CACHE: Mutex<Option<(Instant, u32, u32, Option<u64>)>> = Mutex::new(None);

/// Free device memory of the adapter `info` describes, in bytes, or `None` when it cannot be asked.
pub fn free_bytes(info: &wgpu::AdapterInfo) -> Option<u64> {
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, vendor, device, free)) = *cache
        && vendor == info.vendor
        && device == info.device
        && at.elapsed() < TTL
    {
        return free;
    }
    let free = query(info.vendor, info.device);
    *cache = Some((Instant::now(), info.vendor, info.device, free));
    free
}

#[cfg(not(target_arch = "wasm32"))]
fn query(vendor: u32, device: u32) -> Option<u64> {
    match vendor {
        NVIDIA => nvidia(device),
        AMD => amdgpu(vendor, device),
        _ => None,
    }
}

/// `nvidia-smi --query-gpu=pci.device_id,memory.free`: one line per GPU, `0xDDDDVVVV, <MiB>`.
#[cfg(not(target_arch = "wasm32"))]
fn nvidia(device: u32) -> Option<u64> {
    let out = run_with_timeout("nvidia-smi", &["--query-gpu=pci.device_id,memory.free", "--format=csv,noheader,nounits"])?;
    parse_nvidia(&out, device)
}

/// The free MiB of the GPU whose PCI id names `device`, or of the only GPU listed; in bytes.
fn parse_nvidia(out: &str, device: u32) -> Option<u64> {
    let gpus: Vec<(u32, u64)> = out
        .lines()
        .filter_map(|l| {
            let (id, free) = l.split_once(',')?;
            let id = u32::from_str_radix(id.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok()?;
            let free: u64 = free.trim().parse().ok()?;
            Some((id >> 16, free))
        })
        .collect();
    let free = match gpus.iter().find(|g| g.0 == device) {
        Some(g) => g.1,
        None if gpus.len() == 1 => gpus.first()?.1,
        None => return None,
    };
    free.checked_mul(1 << 20)
}

/// `mem_info_vram_total - mem_info_vram_used` of the amdgpu card with this vendor and device id.
#[cfg(not(target_arch = "wasm32"))]
fn amdgpu(vendor: u32, device: u32) -> Option<u64> {
    let read_hex = |p: &std::path::Path| -> Option<u32> {
        let s = std::fs::read_to_string(p).ok()?;
        u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
    };
    let read_u64 = |p: &std::path::Path| -> Option<u64> { std::fs::read_to_string(p).ok()?.trim().parse().ok() };
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dev = entry.path().join("device");
        if read_hex(&dev.join("vendor")) != Some(vendor) || read_hex(&dev.join("device")) != Some(device) {
            continue;
        }
        let (Some(total), Some(used)) = (read_u64(&dev.join("mem_info_vram_total")), read_u64(&dev.join("mem_info_vram_used"))) else { continue };
        return Some(total.saturating_sub(used));
    }
    None
}

/// Run a program and return its standard output, or `None` if it is missing, fails or takes too long.
#[cfg(not(target_arch = "wasm32"))]
fn run_with_timeout(program: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut child = Command::new(program).args(args).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null()).spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() > QUERY_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_output_is_matched_by_device_id() {
        let two = "0x179B10DE, 3990\n0x1F0810DE, 5120\n";
        assert_eq!(parse_nvidia(two, 0x1f08), Some(5120 << 20));
        assert_eq!(parse_nvidia(two, 0x179b), Some(3990 << 20));
        // several GPUs and none matches: unknown
        assert_eq!(parse_nvidia(two, 0x2204), None);
        // a single GPU is taken even if its id is reported differently
        assert_eq!(parse_nvidia("0x179B10DE, 3990\n", 0x1234), Some(3990 << 20));
        // garbage is not a number
        assert_eq!(parse_nvidia("No devices were found\n", 0x179b), None);
        assert_eq!(parse_nvidia("", 0x179b), None);
    }
}
