//! How much memory the graphics card has: wgpu doesn't say, so it is asked of the system (no
//! native calls: a registry query on Windows, sysfs or `nvidia-smi` on Linux). It sizes how much
//! export work may be on the GPU at once ([`crate::gate`]). `None` when the system doesn't say
//! (macOS and other shared-memory GPUs, an unknown driver): callers then assume a safe size.

/// `LIGHTCRAFT_GPU_MEMORY_MB`, else what the system reports for this adapter.
pub(crate) fn adapter_memory(info: &wgpu::AdapterInfo) -> Option<u64> {
    if let Some(mb) = std::env::var("LIGHTCRAFT_GPU_MEMORY_MB").ok().and_then(|v| v.trim().parse::<u64>().ok()).filter(|m| *m >= 64) {
        // (a size too large for bytes falls back to the system's answer: review of PR #3544)
        if let Some(b) = mb.checked_mul(1 << 20) {
            return Some(b);
        }
    }
    if info.device_type != wgpu::DeviceType::DiscreteGpu {
        return None;
    }
    let found = probe(&info.name, info.vendor, info.device);
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
fn probe(name: &str, _vendor: u32, _device: u32) -> Option<u64> {
    use std::os::windows::process::CommandExt;
    /// No console window flashes up for the query (the app has none).
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // the display adapters' class key: each adapter has a numbered subkey with its name and memory.
    // Only the three values are asked for: listing every value of every subkey (`/s` alone) took
    // over a minute on a machine with many virtual displays, and the first GPU render waited for it.
    const KEY: &str = r"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
    let answers = ["DriverDesc", "HardwareInformation.qwMemorySize", "HardwareInformation.MemorySize"].map(|value| {
        let mut cmd = std::process::Command::new("reg");
        cmd.args(["query", KEY, "/s", "/v", value]).creation_flags(CREATE_NO_WINDOW);
        output_within(cmd, QUERY_TIME)
    });
    parse_registry(&registry_text(answers)?, name)
}

/// The three registry answers (the adapters' names, then their two memory sizes) as one listing;
/// `None` without the names: a size alone could be another adapter's (review of PR #3532).
#[cfg_attr(not(windows), allow(dead_code))]
fn registry_text(answers: [Option<Vec<u8>>; 3]) -> Option<String> {
    let [names, qword, dword] = answers;
    let mut text = String::from_utf8_lossy(&names?).into_owned();
    for out in [qword, dword].into_iter().flatten() {
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&out));
    }
    Some(text)
}

#[cfg(target_os = "linux")]
fn probe(name: &str, vendor: u32, device: u32) -> Option<u64> {
    let nvidia = || {
        let mut cmd = std::process::Command::new("nvidia-smi");
        cmd.args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"]);
        let out = output_within(cmd, QUERY_TIME)?;
        parse_nvidia_smi(&String::from_utf8_lossy(&out), name)
    };
    // NVIDIA's driver publishes no sysfs size: ask it first (an integrated AMD GPU's small
    // carve-out in sysfs is not this card's memory)
    if vendor == NVIDIA
        && let Some(b) = nvidia()
    {
        return Some(b);
    }
    // amdgpu (and others) publish it in sysfs: the card with this adapter's PCI ids (every
    // `cardN` entry, not only the first few: review of PR #3532)
    let entries =
        std::fs::read_dir("/sys/class/drm").map(|d| d.filter_map(|e| e.ok()?.file_name().into_string().ok()).collect::<Vec<_>>()).unwrap_or_default();
    let cards = entries.into_iter().filter(|n| is_card_entry(n)).take(64).map(|n| {
        let read = |file: &str| std::fs::read_to_string(format!("/sys/class/drm/{n}/device/{file}")).ok();
        let id = |file: &str| read(file).and_then(|v| parse_hex(v.trim())).and_then(|v| u32::try_from(v).ok());
        SysfsCard { vendor: id("vendor"), device: id("device"), memory: read("mem_info_vram_total").and_then(|s| s.trim().parse::<u64>().ok()) }
    });
    sysfs_memory(cards, vendor, device)
}

/// How long a memory query (`reg`, `nvidia-smi`) may take before it is given up on: a driver
/// that doesn't answer can't hold the first GPU render (review of PR #3532).
#[cfg(any(windows, target_os = "linux"))]
const QUERY_TIME: std::time::Duration = std::time::Duration::from_secs(3);

/// Bytes of a memory query's output kept (its answer is a few lines; review of PR #3499).
#[cfg(any(windows, target_os = "linux"))]
const MAX_QUERY_OUTPUT: u64 = 1 << 20;

/// `cmd`'s standard output, when it exits successfully within `limit`; else it is killed and
/// `None` (the caller falls back to another source or a safe size).
#[cfg(any(windows, target_os = "linux"))]
fn output_within(mut cmd: std::process::Command, limit: std::time::Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut child = cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    // read on a thread of its own, so a full pipe can't stop the query from exiting; its result
    // is waited for within the same deadline (a grandchild holding the pipe open can't hold us:
    // review of PR #3499). Output longer than MAX_QUERY_OUTPUT is drained and the answer is
    // `None`: a cut listing could name another card's size (review of PR #3499/#3506).
    let (otx, orx) = std::sync::mpsc::channel();
    let reader = std::thread::Builder::new().name("lc-gpu-query".into()).spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut stdout).take(MAX_QUERY_OUTPUT.saturating_add(1)).read_to_end(&mut b);
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        let _ = otx.send(b);
    });
    if reader.is_err() {
        // (no reader: the query is not left running; review of PR #3506)
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(20)),
            _ => break None,
        }
    };
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        log::warn!("gpu: a graphics memory query took longer than {} s; using a safe size", limit.as_secs());
        return None;
    };
    let left = deadline.saturating_duration_since(std::time::Instant::now()).max(std::time::Duration::from_millis(200));
    let out = orx.recv_timeout(left).ok()?;
    (status.success() && out.len() as u64 <= MAX_QUERY_OUTPUT).then_some(out)
}

/// Whether a `/sys/class/drm` entry is a card (`card0`, `card12`), not a connector (`card0-DP-1`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_card_entry(name: &str) -> bool {
    name.strip_prefix("card").is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
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
/// told apart by name; `None` rather than another card's size); when none has the name, the size
/// only when the listing has a single adapter (never another card's on a computer with several,
/// even when that card is the only one reporting a size). `HardwareInformation.qwMemorySize` is a QWORD;
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
            ["HardwareInformation.MemorySize", "REG_DWORD", v] => k.dword = parse_hex(v),
            // `reg` prints a binary value as its bytes in order (little-endian), without `0x`
            ["HardwareInformation.MemorySize", "REG_BINARY", v] => k.dword = parse_le_bytes(v),
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
        // no adapter of that name: a size counts only when the listing has one adapter (another
        // adapter's size is not this one's: review of PR #3506 / #3511; `assumed` applies instead)
        None if keys.iter().filter(|k| k.desc.is_some()).count() <= 1 => only(keys.iter().filter_map(memory)),
        None => None,
    }
}

/// A `REG_BINARY` value as `reg` prints it (`00000080`: bytes in order, least significant first)
/// as a number; `None` for an empty, odd-length, longer than 8 bytes or not hex value (review of
/// PR #3565).
#[cfg_attr(not(windows), allow(dead_code))]
fn parse_le_bytes(v: &str) -> Option<u64> {
    let v = v.trim();
    if v.is_empty() || !v.len().is_multiple_of(2) || v.len() > 16 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut n = 0u64;
    for (i, pair) in v.as_bytes().chunks(2).enumerate() {
        let byte = u64::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        n |= byte << (8 * i);
    }
    Some(n)
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
/// the cards named exactly `name` (else those whose name is part of it, `NVIDIA GeForce RTX 4070`
/// in `… 4070 Ti SUPER` only when no card has the exact name), only when they all report one size
/// (`None` rather than another card's); when none has the name, the only card, if there is one.
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
    let exact: Vec<u64> = cards.iter().filter(|(n, _)| *n == wanted).map(|c| c.1).collect();
    let named = if exact.is_empty() { cards.iter().filter(|(n, _)| wanted.contains(n.as_str())).map(|c| c.1).collect() } else { exact };
    match named.first() {
        Some(&b) => named.iter().all(|m| *m == b).then_some(b),
        None => only(cards.iter().map(|c| c.1)),
    }
}

/// The one size in `sizes`, `None` when there are none or several (another card's size is no
/// guess for this one).
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
fn only(mut sizes: impl Iterator<Item = u64>) -> Option<u64> {
    let first = sizes.next()?;
    sizes.next().is_none().then_some(first)
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
    HardwareInformation.MemorySize    REG_BINARY    00000080
    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000

HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0002
    DriverDesc    REG_SZ    AMD Radeon(TM) Graphics
    HardwareInformation.qwMemorySize    REG_QWORD    0x20000000
";

    #[test]
    fn the_registry_names_the_adapters_memory() {
        assert_eq!(parse_registry(REG, "NVIDIA GeForce RTX 5070"), Some(12 << 30));
        assert_eq!(parse_registry(REG, "AMD Radeon(TM) Graphics"), Some(512 << 20));
        // an adapter not listed, among several that report a size: not another card's (review of PR #3506)
        assert_eq!(parse_registry(REG, "Something Else"), None);
        assert_eq!(parse_registry("", "x"), None);
        assert_eq!(parse_registry("garbage\n    HardwareInformation.qwMemorySize    REG_QWORD    nothex", "x"), None);
        // a DWORD alone still counts
        let old = "HKEY_X\n    DriverDesc    REG_SZ    Old Card\n    HardwareInformation.MemorySize    REG_DWORD    0x40000000\n";
        assert_eq!(parse_registry(old, "Old Card"), Some(1 << 30));
        // a binary value is read as `reg` prints it: its bytes, least significant first (review of PR #3565)
        let bin = "HKEY_X\n    DriverDesc    REG_SZ    Old Card\n    HardwareInformation.MemorySize    REG_BINARY    00000080\n";
        assert_eq!(parse_registry(bin, "Old Card"), Some(2 << 30));
        assert_eq!(parse_le_bytes("00000080"), Some(0x8000_0000));
        for bad in ["", "0", "0x80000000", "zz000000", "000000000000000000"] {
            assert_eq!(parse_le_bytes(bad), None, "{bad:?}");
        }
    }

    /// Review of PR #3506 / #3511: an adapter whose name isn't listed, on a machine where another
    /// adapter reports the only size, isn't given that adapter's size.
    #[test]
    fn an_unlisted_adapter_is_not_given_the_only_other_size() {
        let two = "HKEY_X\\0000\n    DriverDesc    REG_SZ    Card A\n\nHKEY_X\\0001\n    DriverDesc    REG_SZ    Card B\n    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000\n";
        assert_eq!(parse_registry(two, "Card B"), Some(12 << 30));
        assert_eq!(parse_registry(two, "Card A"), None, "Card A reports no size");
        assert_eq!(parse_registry(two, "Card C"), None, "an adapter not listed");
        // one adapter in the listing, under another name: its size is the card's
        let one = "HKEY_X\\0000\n    DriverDesc    REG_SZ    Card B\n    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000\n";
        assert_eq!(parse_registry(one, "Card B (driver name)"), Some(12 << 30));
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
    fn drm_card_entries_are_cards_only() {
        assert!(is_card_entry("card0") && is_card_entry("card12"));
        assert!(!is_card_entry("card0-DP-1") && !is_card_entry("card") && !is_card_entry("renderD128") && !is_card_entry("version"));
    }

    /// Review of PR #3532: a memory query that doesn't answer is given up on, not waited for.
    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn a_query_that_does_not_answer_is_given_up_on() {
        let cmd = |args: &[&str]| {
            #[cfg(windows)]
            let mut c = std::process::Command::new("cmd");
            #[cfg(windows)]
            c.arg("/C");
            #[cfg(not(windows))]
            let mut c = std::process::Command::new("sh");
            #[cfg(not(windows))]
            c.arg("-c");
            c.args(args);
            c
        };
        let t = std::time::Instant::now();
        #[cfg(windows)]
        let slow = cmd(&["ping -n 20 127.0.0.1 >NUL"]);
        #[cfg(not(windows))]
        let slow = cmd(&["sleep 20"]);
        assert_eq!(output_within(slow, std::time::Duration::from_millis(300)), None);
        assert!(t.elapsed() < std::time::Duration::from_secs(5), "{:?}", t.elapsed());
        let out = output_within(cmd(&["echo 8192"]), std::time::Duration::from_secs(10)).unwrap();
        assert!(String::from_utf8_lossy(&out).contains("8192"));
        // a flood of output past MAX_QUERY_OUTPUT is no answer, and the query still finishes
        #[cfg(windows)]
        let flood = cmd(&["for /L %i in (1,1,40000) do @echo 0123456789012345678901234567890123456789"]);
        #[cfg(not(windows))]
        let flood = cmd(&["yes 0123456789012345678901234567890123456789 | head -n 40000"]);
        assert_eq!(output_within(flood, std::time::Duration::from_secs(60)), None);
    }

    /// Review of PR #3532: without the adapters' names, a memory size alone is no answer.
    #[test]
    fn a_registry_listing_without_names_is_no_answer() {
        let size = b"HKEY_LOCAL_MACHINE\\X\\0000\n    HardwareInformation.qwMemorySize    REG_QWORD    0x300000000\n".to_vec();
        assert_eq!(registry_text([None, Some(size.clone()), None]), None);
        let names = b"HKEY_LOCAL_MACHINE\\X\\0000\n    DriverDesc    REG_SZ    Card A\n".to_vec();
        let text = registry_text([Some(names), Some(size), None]).unwrap();
        assert_eq!(parse_registry(&text, "Card A"), Some(0x3_0000_0000));
    }

    #[test]
    fn nvidia_smi_lines_are_read() {
        let text = "NVIDIA GeForce RTX 5070, 12227\nNVIDIA GeForce GT 1030, 2001\n";
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce RTX 5070"), Some(12227 << 20));
        assert_eq!(parse_nvidia_smi(text, "NVIDIA GeForce GT 1030"), Some(2001 << 20));
        assert_eq!(parse_nvidia_smi(text, "nope"), None, "not another card's size");
        assert_eq!(parse_nvidia_smi("NVIDIA GeForce RTX 5070, 12227", "nope"), Some(12227 << 20), "the only card");
        // (review of PR #3499) the exact name wins over a shorter name inside it
        let family = "NVIDIA GeForce RTX 4070, 12282\nNVIDIA GeForce RTX 4070 Ti SUPER, 16376\n";
        assert_eq!(parse_nvidia_smi(family, "NVIDIA GeForce RTX 4070 Ti SUPER"), Some(16376 << 20));
        assert_eq!(parse_nvidia_smi(family, "NVIDIA GeForce RTX 4070"), Some(12282 << 20));
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
