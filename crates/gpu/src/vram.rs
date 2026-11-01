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
    // asked once per card (name and PCI ids): the listing of the cards and each device on them ask
    // again; two cards of one name and ids get one answer, which the probe gives only when they agree
    type Seen = Vec<((String, u32, u32), Option<u64>)>;
    static SEEN: std::sync::Mutex<Seen> = std::sync::Mutex::new(Vec::new());
    let key = (info.name.clone(), info.vendor, info.device);
    if let Some((_, b)) = SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().find(|(k, _)| *k == key) {
        return *b;
    }
    let found = probe(&info.name, info.vendor, info.device);
    if let Some(b) = found {
        log::info!("gpu: {} has {} MiB of memory", info.name, b >> 20);
    }
    SEEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push((key, found));
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
fn probe(name: &str, _vendor: u32, _device: u32) -> Option<u64> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up for the query (the app has none).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // the display adapters' class key: each adapter has a numbered subkey with its name and memory.
    // Only the three values are asked for: listing every value of every subkey (`/s` alone) took
    // over a minute on a machine with many virtual displays, and the first GPU render waited for it.
    const KEY: &str = r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let mut text = String::new();
    for value in ["DriverDesc", "HardwareInformation.qwMemorySize", "HardwareInformation.MemorySize"] {
        let Ok(out) = std::process::Command::new("reg").args(["query", KEY, "/s", "/v", value]).creation_flags(CREATE_NO_WINDOW).output() else {
            continue;
        };
        text.push_str(&String::from_utf8_lossy(&out.stdout));
        text.push('\n');
    }
    parse_registry(&text, name)
}

#[cfg(target_os = "linux")]
fn probe(name: &str, vendor: u32, device: u32) -> Option<u64> {
    let nvidia = || {
        let out = std::process::Command::new("nvidia-smi").args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]).output().ok()?;
        parse_nvidia_smi(&String::from_utf8_lossy(&out.stdout), name)
    };
    // NVIDIA's driver publishes no sysfs size: ask it first (an integrated AMD GPU's small
    // carve-out in sysfs is not this card's memory)
    if vendor == NVIDIA
        && let Some(b) = nvidia()
    {
        return Some(b);
    }
    // amdgpu (and others) publish it in sysfs: the card with this adapter's PCI ids
    let cards = (0..8).map(|i| {
        let read = |file: &str| std::fs::read_to_string(format!("/sys/class/drm/card{i}/device/{file}")).ok();
        let id = |file: &str| read(file).and_then(|v| parse_hex(v.trim())).and_then(|v| u32::try_from(v).ok());
        SysfsCard { vendor: id("vendor"), device: id("device"), memory: read("mem_info_vram_total").and_then(|s| s.trim().parse::<u64>().ok()) }
    });
    sysfs_memory(cards, vendor, device)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn probe(_name: &str, _vendor: u32, _device: u32) -> Option<u64> {
    None
}

/// PCI vendor id of NVIDIA.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const NVIDIA: u32 = 0x10de;

/// A DRM card in sysfs: its PCI vendor and device ids and its memory, when known.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy)]
struct SysfsCard {
    vendor: Option<u32>,
    device: Option<u32>,
    memory: Option<u64>,
}

/// The memory sysfs reports for the adapter with PCI ids `vendor`:`device` among `cards`: the
/// cards with both ids, else, when the device id matches none, the vendor's cards; either way
/// only when they all report one size. Cards that can't be told apart and differ (or a matching
/// card that reports nothing) give `None`, so the caller assumes a safe size rather than another
/// card's; another vendor's GPU (an integrated GPU beside a discrete card) never sizes this one.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn sysfs_memory(cards: impl Iterator<Item = SysfsCard>, vendor: u32, device: u32) -> Option<u64> {
    let mine: Vec<SysfsCard> = cards.filter(|c| c.vendor == Some(vendor)).collect();
    let exact: Vec<SysfsCard> = mine.iter().copied().filter(|c| device != 0 && c.device == Some(device)).collect();
    // (matched cards only, when any match: never another model's size)
    let candidates = if exact.is_empty() { mine } else { exact };
    let first = candidates.first()?.memory?;
    candidates.iter().all(|c| c.memory == Some(first)).then_some(first)
}

/// The memory of adapter `name` in `reg query … /s` output (several queries' outputs one after
/// another: a subkey's values are gathered under its path): the subkey whose `DriverDesc` is the
/// name, only when every subkey with that name reports one size (two cards of one model can't be
/// told apart by name; `None` rather than another card's size), else, when none has the name, the
/// largest any adapter reports. `HardwareInformation.qwMemorySize` is a QWORD;
/// older drivers write `HardwareInformation.MemorySize` (a DWORD, wrong past 4 GiB: used only
/// without the other).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_registry(text: &str, name: &str) -> Option<u64> {
    struct Key {
        path: String,
        desc: Option<String>,
        qword: Option<u64>,
        dword: Option<u64>,
    }
    let mut keys: Vec<Key> = Vec::new();
    let mut at: Option<usize> = None;
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            let path = line.trim().to_ascii_lowercase();
            at = Some(match keys.iter().position(|k| k.path == path) {
                Some(i) => i,
                None => {
                    keys.push(Key { path, desc: None, qword: None, dword: None });
                    keys.len() - 1
                }
            });
            continue;
        }
        if line.trim().starts_with("End of search") {
            at = None;
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some(k) = at.and_then(|i| keys.get_mut(i)) else { continue };
        match parts.as_slice() {
            ["DriverDesc", "REG_SZ", rest @ ..] => k.desc = Some(rest.join(" ")),
            ["HardwareInformation.qwMemorySize", "REG_QWORD", v] => k.qword = parse_hex(v),
            ["HardwareInformation.MemorySize", "REG_DWORD" | "REG_BINARY", v] => k.dword = parse_hex(v),
            _ => {}
        }
    }
    let memory = |k: &Key| k.qword.or(k.dword).filter(|b| *b >= 64 << 20);
    let wanted = name.trim().to_ascii_lowercase();
    let named: Vec<&Key> = keys.iter().filter(|k| k.desc.as_deref().is_some_and(|d| d.trim().to_ascii_lowercase() == wanted)).collect();
    match named.first() {
        Some(first) => {
            let b = memory(first)?;
            named.iter().all(|k| memory(k) == Some(b)).then_some(b)
        }
        None => keys.iter().filter_map(memory).max(),
    }
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
/// the cards named `name`, only when they all report one size (`None` rather than another card's),
/// else, when none has the name, the largest.
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
    let named: Vec<u64> = cards.iter().filter(|(n, _)| *n == wanted || wanted.contains(n.as_str())).map(|c| c.1).collect();
    match named.first() {
        Some(&b) => named.iter().all(|m| *m == b).then_some(b),
        None => cards.iter().map(|c| c.1).max(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review of PR #3499: an NVIDIA card beside an integrated AMD GPU is not sized by the AMD
    /// GPU's sysfs carve-out; cards of the adapter's own vendor are.
    #[test]
    fn sysfs_memory_counts_only_the_adapters_vendor() {
        const AMD: u32 = 0x1002;
        let card = |vendor, device, memory| SysfsCard { vendor, device, memory };
        let cards = || {
            [card(Some(AMD), Some(0x164e), Some(512u64 << 20)), card(Some(NVIDIA), Some(0x2783), None), card(None, None, Some(1 << 30))].into_iter()
        };
        assert_eq!(sysfs_memory(cards(), NVIDIA, 0x2783), None, "the integrated AMD GPU's memory is not the NVIDIA card's");
        assert_eq!(sysfs_memory(cards(), AMD, 0x164e), Some(512 << 20));
        assert_eq!(sysfs_memory(cards(), AMD, 0), Some(512 << 20), "the vendor's one card, without a device id");
    }

    /// Review of PR #3499 (9e2ff593e): two cards of one vendor of different sizes; each adapter is
    /// sized by its own card, never the larger one.
    #[test]
    fn sysfs_memory_is_the_adapters_own_card() {
        const AMD: u32 = 0x1002;
        let card = |device, memory| SysfsCard { vendor: Some(AMD), device: Some(device), memory: Some(memory) };
        let two = || [card(0x744c, 16u64 << 30), card(0x164e, 512 << 20)].into_iter();
        assert_eq!(sysfs_memory(two(), AMD, 0x164e), Some(512 << 20), "the small card is not sized by the large one");
        assert_eq!(sysfs_memory(two(), AMD, 0x744c), Some(16 << 30));
        assert_eq!(sysfs_memory(two(), AMD, 0x1234), None, "unmatched among different sizes: the safe assumed size");
        assert_eq!(sysfs_memory(two(), AMD, 0), None);
        let same = [card(0x744c, 16u64 << 30), card(0x744c, 16 << 30)].into_iter();
        assert_eq!(sysfs_memory(same, AMD, 0), Some(16 << 30), "cards of one size, unmatched: that size");
        // (review of 5af259201) a matching card that reports nothing is not sized by another card
        let silent = [SysfsCard { vendor: Some(AMD), device: Some(0x164e), memory: None }, card(0x744c, 16 << 30)].into_iter();
        assert_eq!(sysfs_memory(silent, AMD, 0x164e), None);
        // (review of 9c77aa7b3) two cards with the same ids that report different sizes: neither
        let differ = [card(0x744c, 16u64 << 30), card(0x744c, 8 << 30)].into_iter();
        assert_eq!(sysfs_memory(differ, AMD, 0x744c), None);
        assert_eq!(parse_hex("0x1002\n"), Some(0x1002), "sysfs writes the vendor id as hex");
    }

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

    /// One query a value (`reg query … /s /v …`, fast) instead of every value of every adapter
    /// (`/s`, over a minute on a machine with many virtual displays): the outputs one after another
    /// are gathered per adapter.
    #[test]
    fn the_registry_values_of_separate_queries_are_gathered_per_adapter() {
        let k = r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
        let text = [
            format!("{k}\\0000\n    DriverDesc    REG_SZ    NVIDIA GeForce RTX 5070\n\n"),
            format!("{k}\\0001\n    DriverDesc    REG_SZ    Meta Virtual Monitor\n\nEnd of search: 2 match(es) found.\n\n"),
            format!("{k}\\0000\n    HardwareInformation.qwMemorySize    REG_QWORD    0x2fc300000\n\nEnd of search: 1 match(es) found.\n\n"),
            format!("{k}\\0001\n    HardwareInformation.MemorySize    REG_DWORD    0x8000000\n\nEnd of search: 1 match(es) found.\n"),
        ]
        .concat();
        assert_eq!(parse_registry(&text, "NVIDIA GeForce RTX 5070"), Some(0x2fc300000));
        assert_eq!(parse_registry(&text, "Meta Virtual Monitor"), Some(128 << 20));
    }

    #[test]
    fn nvidia_smi_lines_are_read() {
        let text = "NVIDIA GeForce RTX 5070, 12227\nNVIDIA GeForce GT 1030, 2001\n";
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce RTX 5070"), Some(12227 << 20));
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce GT 1030"), Some(2001 << 20));
        assert_eq!(parse_nvidia_smi(text, "nope"), Some(12227 << 20));
        assert_eq!(parse_nvidia_smi("No devices were found", "x"), None);
    }

    /// Review of PR #3506: two cards of one name are sized only when they agree, never by the
    /// other's memory.
    #[test]
    fn same_named_cards_are_sized_only_when_they_agree() {
        let two = "NVIDIA GeForce RTX 4070, 12282\nNVIDIA GeForce RTX 4070, 8188\n";
        assert_eq!(parse_nvidia_smi(two, "NVIDIA GeForce RTX 4070"), None);
        let same = "NVIDIA GeForce RTX 4070, 12282\nNVIDIA GeForce RTX 4070, 12282\n";
        assert_eq!(parse_nvidia_smi(same, "NVIDIA GeForce RTX 4070"), Some(12282 << 20));
        let key = |i: u32, name: &str, mem: Option<u64>| {
            let m = mem.map(|b| format!("    HardwareInformation.qwMemorySize    REG_QWORD    {b:#x}\n")).unwrap_or_default();
            format!("HKEY_X\\{i:04}\n    DriverDesc    REG_SZ    {name}\n{m}\n")
        };
        let differ = [key(0, "Card", Some(12 << 30)), key(1, "Card", Some(8 << 30))].concat();
        assert_eq!(parse_registry(&differ, "Card"), None);
        let one_silent = [key(0, "Card", Some(12 << 30)), key(1, "Card", None), key(2, "Other", Some(16 << 30))].concat();
        assert_eq!(parse_registry(&one_silent, "Card"), None, "not the other card's either");
        let agree = [key(0, "Card", Some(12 << 30)), key(1, "Card", Some(12 << 30))].concat();
        assert_eq!(parse_registry(&agree, "Card"), Some(12 << 30));
    }
}
