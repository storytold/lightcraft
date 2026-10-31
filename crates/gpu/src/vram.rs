//! How much memory the graphics card has: wgpu doesn't say, so it is asked of the system (no
//! native calls: a registry query on Windows, sysfs or `nvidia-smi` on Linux). It sizes how much
//! export work may be on the GPU at once ([`crate::gate`]). `None` when the system doesn't say
//! (macOS and other shared-memory GPUs, an unknown driver): callers then assume a safe size.

/// `LIGHTCRAFT_GPU_MEMORY_MB`, else what the system reports for this adapter.
pub(crate) fn adapter_memory(info: &wgpu::AdapterInfo) -> Option<u64> {
    if let Some(mb) = std::env::var("LIGHTCRAFT_GPU_MEMORY_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|m| *m >= 64) {
        return Some(mb << 20);
    }
    if info.device_type != wgpu::DeviceType::DiscreteGpu {
        return None;
    }
    let found = probe(&info.name);
    if let Some(b) = found {
        log::info!("gpu: {} has {} MiB of memory", info.name, b >> 20);
    }
    found
}

/// What to assume when the system doesn't say: integrated and unknown adapters share the
/// computer's memory (2 GiB for our work), a discrete card has at least 4 GiB nowadays.
pub(crate) fn assumed(info: &wgpu::AdapterInfo) -> u64 {
    match info.device_type {
        wgpu::DeviceType::DiscreteGpu => 4 << 30,
        _ => 2 << 30,
    }
}

#[cfg(windows)]
fn probe(name: &str) -> Option<u64> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up for the query (the app has none).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // the display adapters' class key: each adapter has a numbered subkey with its name and memory
    let out = std::process::Command::new("reg")
        .args(["query", r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}", "/s"])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()?;
    parse_registry(&String::from_utf8_lossy(&out.stdout), name)
}

#[cfg(target_os = "linux")]
fn probe(name: &str) -> Option<u64> {
    // amdgpu (and others) publish it in sysfs
    let sysfs = (0..8)
        .filter_map(|i| std::fs::read_to_string(format!("/sys/class/drm/card{i}/device/mem_info_vram_total")).ok())
        .filter_map(|s| s.trim().parse::<u64>().ok())
        .max();
    if sysfs.is_some() {
        return sysfs;
    }
    let out = std::process::Command::new("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).output().ok()?;
    parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout), name)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn probe(_name: &str) -> Option<u64> {
    None
}

/// The memory of adapter `name` in `reg query … /s` output: the subkey whose `DriverDesc` is the
/// name (else the largest any adapter reports). `HardwareInformation.qwMemorySize` is a QWORD;
/// older drivers write `HardwareInformation.MemorySize` (a DWORD, wrong past 4 GiB: used only
/// without the other).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_registry(text: &str, name: &str) -> Option<u64> {
    struct Key {
        desc: Option<String>,
        qword: Option<u64>,
        dword: Option<u64>,
    }
    let mut keys: Vec<Key> = Vec::new();
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            keys.push(Key { desc: None, qword: None, dword: None });
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(k) = keys.last_mut() else { continue };
        match parts.as_slice() {
            ["DriverDesc", "REG_SZ", rest @ ..] => k.desc = Some(rest.join(" ")),
            ["HardwareInformation.qwMemorySize", "REG_QWORD", v] => k.qword = parse_hex(v),
            ["HardwareInformation.MemorySize", "REG_DWORD" | "REG_BINARY", v] => k.dword = parse_hex(v),
            _ => {}
        }
    }
    let memory = |k: &Key| k.qword.or(k.dword).filter(|b| *b >= 64 << 20);
    let wanted = name.trim().to_ascii_lowercase();
    keys.iter()
        .filter(|k| k.desc.as_deref().is_some_and(|d| d.trim().to_ascii_lowercase() == wanted))
        .find_map(memory)
        .or_else(|| keys.iter().filter_map(memory).max())
}

#[cfg_attr(not(windows), allow(dead_code))]
fn parse_hex(v: &str) -> Option<u64> {
    let v = v.trim();
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

/// `nvidia-smi --query-gpu=name,memory.total --format=csv,noheader,nounits` lines (`name, MiB`):
/// the card named `name`, else the largest.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_nvidia_smi(text: &str, name: &str) -> Option<u64> {
    let cards: Vec<(String, u64)> = text
        .lines()
        .filter_map(|l| {
            let (n, mb) = l.rsplit_once(',')?;
            Some((n.trim().to_ascii_lowercase(), mb.trim().parse::<u64>().ok()? << 20))
        })
        .collect();
    let wanted = name.trim().to_ascii_lowercase();
    cards.iter().find(|(n, _)| *n == wanted || wanted.contains(n.as_str())).map(|c| c.1).or_else(|| cards.iter().map(|c| c.1).max())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REG: &str = r"
HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0000
    DriverDesc    REG_SZ    Microsoft Basic Display Adapter

HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0001
    DriverDesc    REG_SZ    NVIDIA GeForce RTX 5070
    HardwareInformation.MemorySize    REG_BINARY    0x80000000
    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000

HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0002
    DriverDesc    REG_SZ    AMD Radeon(TM) Graphics
    HardwareInformation.qwMemorySize    REG_QWORD    0x20000000
";

    #[test]
    fn the_registry_names_the_adapters_memory() {
        assert_eq!(parse_registry(REG, "NVIDIA GeForce RTX 5070"), Some(12 << 30));
        assert_eq!(parse_registry(REG, "AMD Radeon(TM) Graphics"), Some(512 << 20));
        // an adapter not listed: the largest
        assert_eq!(parse_registry(REG, "Something Else"), Some(12 << 30));
        assert_eq!(parse_registry("", "x"), None);
        assert_eq!(parse_registry("garbage\n    HardwareInformation.qwMemorySize    REG_QWORD    nothex", "x"), None);
        // a DWORD alone still counts
        let old = "HKEY_X\n    DriverDesc    REG_SZ    Old Card\n    HardwareInformation.MemorySize    REG_DWORD    0x40000000\n";
        assert_eq!(parse_registry(old, "Old Card"), Some(1 << 30));
    }

    #[test]
    fn nvidia_smi_lines_are_read() {
        let text = "NVIDIA GeForce RTX 5070, 12227\nNVIDIA GeForce GT 1030, 2001\n";
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce RTX 5070"), Some(12227 << 20));
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce GT 1030"), Some(2001 << 20));
        assert_eq!(parse_nvidia_smi(text, "nope"), Some(12227 << 20));
        assert_eq!(parse_nvidia_smi("No devices were found", "x"), None);
    }
}
