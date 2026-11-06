//! How much device memory is free right now, where that can be asked without `unsafe`: the NVIDIA
//! driver's `nvidia-smi` (Linux and Windows) and the amdgpu driver's sysfs counters (Linux). Other
//! devices (AMD/Intel on Windows needs DXGI; integrated GPUs share system memory) report `None`, and the
//! caller keeps its fixed budget. The answer is cached for a few seconds: renders ask often, and the
//! query starts a process on NVIDIA. Only the first question for an adapter waits for the query; later
//! ones get the last answer while a background thread asks again, so no render (and no lock it holds)
//! waits for `nvidia-smi`. A card is found by its PCI address where the backend reports it, else by its
//! device id; when that matches several cards (two of the same model), the answer is `None`.

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How long an answer is fresh; an older one is still returned while a newer one is asked for.
const TTL: Duration = Duration::from_secs(3);
/// How long `nvidia-smi` may take before it is given up on.
const QUERY_TIMEOUT: Duration = Duration::from_secs(2);

const NVIDIA: u32 = 0x10de;
const AMD: u32 = 0x1002;

static CACHE: Cache = Cache::new(TTL);

/// Free device memory of the adapter `info` describes, in bytes, or `None` when it cannot be asked.
pub fn free_bytes(info: &wgpu::AdapterInfo) -> Option<u64> {
    let adapter = Adapter { vendor: info.vendor, device: info.device, bus: pci_address(&info.device_pci_bus_id) };
    CACHE.get(&adapter, query)
}

/// Which card: vendor and device id, and the PCI address where the backend reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Adapter {
    vendor: u32,
    device: u32,
    bus: Option<Pci>,
}

/// A PCI address: domain, bus, device, function.
type Pci = [u32; 4];

/// `0000:01:00.0` (Vulkan, sysfs) or `00000000:01:00.0` (`nvidia-smi`).
fn pci_address(s: &str) -> Option<Pci> {
    let mut parts = s.trim().split(':');
    let (domain, bus, rest) = (parts.next()?, parts.next()?, parts.next()?);
    let (device, function) = rest.split_once('.')?;
    if parts.next().is_some() {
        return None;
    }
    let hex = |x: &str| u32::from_str_radix(x.trim(), 16).ok();
    Some([hex(domain)?, hex(bus)?, hex(device)?, hex(function)?])
}

/// One card a driver lists, with its free memory in bytes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Card {
    bus: Option<Pci>,
    device: u32,
    free: u64,
}

/// The free memory of `want` among the listed `cards`: the card at its PCI address when both sides
/// report one, else the only card with its device id, else the only card listed. `None` when that is
/// ambiguous or the address is listed by no card.
fn pick(cards: &[Card], want: &Adapter) -> Option<u64> {
    if let Some(bus) = want.bus
        && cards.iter().any(|c| c.bus.is_some())
    {
        return cards.iter().find(|c| c.bus == Some(bus)).map(|c| c.free);
    }
    let mut same = cards.iter().filter(|c| c.device == want.device);
    match (same.next(), same.next()) {
        (Some(c), None) => Some(c.free),
        (None, _) if cards.len() == 1 => cards.first().map(|c| c.free),
        _ => None,
    }
}

/// The last answer for one adapter: when it was asked, the answer, and since when a newer one is
/// being asked for.
struct Answer {
    adapter: Adapter,
    at: Instant,
    free: Option<u64>,
    asking: Option<Instant>,
}

type Ask = fn(&Adapter) -> Option<u64>;

/// Adapters remembered (a renderer, a face finder and a model may each use another device).
const ADAPTERS: usize = 4;

/// A background query not done after this is taken as lost, and the next question asks again.
const LOST: Duration = Duration::from_secs(10);

struct Cache {
    ttl: Duration,
    answers: Mutex<Vec<Answer>>,
}

impl Cache {
    const fn new(ttl: Duration) -> Self {
        Cache { ttl, answers: Mutex::new(Vec::new()) }
    }

    /// The answer of `ask` for `adapter`: cached, refreshed in the background once it is older than the
    /// TTL (in the caller where no thread can be started), and asked for directly the first time.
    fn get(&'static self, adapter: &Adapter, ask: Ask) -> Option<u64> {
        {
            let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(a) = answers.iter_mut().find(|a| a.adapter == *adapter) {
                let busy = a.asking.is_some_and(|t| t.elapsed() < LOST);
                if a.at.elapsed() >= self.ttl && !busy {
                    let adapter = *adapter;
                    let refresh = std::thread::Builder::new().name("vram-query".into()).spawn(move || self.store(adapter, ask(&adapter)));
                    if refresh.is_err() {
                        drop(answers);
                        return self.ask_now(&adapter, ask);
                    }
                    a.asking = Some(Instant::now());
                }
                return a.free;
            }
        }
        self.ask_now(adapter, ask)
    }

    fn ask_now(&self, adapter: &Adapter, ask: Ask) -> Option<u64> {
        let free = ask(adapter);
        self.store(*adapter, free);
        free
    }

    /// Keep `adapter`'s answer (each adapter has its own; the oldest one goes when the list is full).
    fn store(&self, adapter: Adapter, free: Option<u64>) {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        answers.retain(|a| a.adapter != adapter);
        if answers.len() >= ADAPTERS {
            answers.remove(0);
        }
        answers.push(Answer { adapter, at: Instant::now(), free, asking: None });
    }

    /// Make `adapter`'s answer as old as the TTL (tests: no waiting, so no answer goes stale by itself
    /// on a slow machine). `false` when there is no answer, or the clock does not reach that far back.
    #[cfg(test)]
    fn expire(&self, adapter: &Adapter) -> bool {
        let mut answers = self.answers.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(a) = answers.iter_mut().find(|a| a.adapter == *adapter) else { return false };
        let Some(at) = a.at.checked_sub(self.ttl) else { return false };
        a.at = at;
        true
    }

    #[cfg(test)]
    fn asking(&self, adapter: &Adapter) -> bool {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|a| a.adapter == *adapter && a.asking.is_some())
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn query(adapter: &Adapter) -> Option<u64> {
    match adapter.vendor {
        NVIDIA => nvidia(adapter),
        AMD => amdgpu(adapter),
        _ => None,
    }
}

/// `nvidia-smi --query-gpu=pci.bus_id,pci.device_id,memory.free`: one line per GPU,
/// `DDDDDDDD:BB:DD.F, 0xDDDDVVVV, <MiB>`.
#[cfg(not(target_arch = "wasm32"))]
fn nvidia(adapter: &Adapter) -> Option<u64> {
    let out = run_with_timeout("nvidia-smi", &["--query-gpu=pci.bus_id,pci.device_id,memory.free", "--format=csv,noheader,nounits"])?;
    pick(&parse_nvidia(&out), adapter)
}

/// The cards `nvidia-smi` lists, free memory in bytes.
fn parse_nvidia(out: &str) -> Vec<Card> {
    out.lines()
        .filter_map(|l| {
            let mut f = l.split(',');
            let (bus, id, free) = (f.next()?, f.next()?, f.next()?);
            let id = u32::from_str_radix(id.trim().trim_start_matches("0x").trim_start_matches("0X"), 16).ok()?;
            let free = free.trim().parse::<u64>().ok()?.checked_mul(1 << 20)?;
            Some(Card { bus: pci_address(bus), device: id >> 16, free })
        })
        .collect()
}

/// `mem_info_vram_total - mem_info_vram_used` of the amdgpu card (its sysfs device directory is named
/// by its PCI address).
#[cfg(not(target_arch = "wasm32"))]
fn amdgpu(adapter: &Adapter) -> Option<u64> {
    let read_hex = |p: &std::path::Path| -> Option<u32> {
        let s = std::fs::read_to_string(p).ok()?;
        u32::from_str_radix(s.trim().trim_start_matches("0x"), 16).ok()
    };
    let read_u64 = |p: &std::path::Path| -> Option<u64> { std::fs::read_to_string(p).ok()?.trim().parse().ok() };
    let mut cards: Vec<Card> = Vec::new();
    let mut seen = Vec::new();
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        // card0 and renderD128 are the same device
        let Ok(dev) = std::fs::canonicalize(entry.path().join("device")) else { continue };
        if seen.contains(&dev) || read_hex(&dev.join("vendor")) != Some(adapter.vendor) {
            continue;
        }
        seen.push(dev.clone());
        let (Some(device), Some(total), Some(used)) =
            (read_hex(&dev.join("device")), read_u64(&dev.join("mem_info_vram_total")), read_u64(&dev.join("mem_info_vram_used")))
        else {
            continue;
        };
        let bus = dev.file_name().and_then(|n| pci_address(&n.to_string_lossy()));
        cards.push(Card { bus, device, free: total.saturating_sub(used) });
    }
    pick(&cards, adapter)
}

/// Run a program and return its standard output, or `None` if it is missing, fails or takes too long.
#[cfg(not(target_arch = "wasm32"))]
fn run_with_timeout(program: &str, args: &[&str]) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(Stdio::null());
    // the release app has no console: without this, Windows opens one for each query
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().ok()?;
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

    const fn adapter(vendor: u32, device: u32) -> Adapter {
        Adapter { vendor, device, bus: None }
    }

    /// Wait until `flag` is set (a fake query that answers when the test says so), at most 10 s.
    fn wait_for(flag: &std::sync::atomic::AtomicBool) {
        let start = Instant::now();
        while !flag.load(std::sync::atomic::Ordering::SeqCst) && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Wait until `cache` has no background query for `a` running, at most 10 s.
    fn settled(cache: &Cache, a: &Adapter) {
        let start = Instant::now();
        while cache.asking(a) && start.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!cache.asking(a), "the background query finished");
    }

    #[test]
    fn a_stale_answer_is_returned_while_a_new_one_is_asked_for() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        static CACHE: Cache = Cache::new(Duration::from_secs(30));
        static ASKED: AtomicU64 = AtomicU64::new(0);
        static ANSWER: AtomicBool = AtomicBool::new(true);
        fn blocked(_: &Adapter) -> Option<u64> {
            wait_for(&ANSWER);
            Some(ASKED.fetch_add(1, Ordering::SeqCst) + 1)
        }
        let a = adapter(NVIDIA, 1);
        // the first question waits for the answer
        assert_eq!(CACHE.get(&a, blocked), Some(1));
        assert!(CACHE.expire(&a));
        // stale, and the driver does not answer: the old answer at once, a new one in the background
        ANSWER.store(false, Ordering::SeqCst);
        let t = Instant::now();
        assert_eq!(CACHE.get(&a, blocked), Some(1));
        assert_eq!(CACHE.get(&a, blocked), Some(1));
        assert!(t.elapsed() < Duration::from_secs(5), "waited {:?} for the query", t.elapsed());
        assert!(CACHE.asking(&a));
        ANSWER.store(true, Ordering::SeqCst);
        settled(&CACHE, &a);
        assert_eq!(CACHE.get(&a, blocked), Some(2), "the background answer is kept");
        assert_eq!(ASKED.load(Ordering::SeqCst), 2, "one query in the background, not one per question");
        // another adapter is asked for directly
        assert_eq!(CACHE.get(&adapter(AMD, 2), blocked), Some(3));
    }

    #[test]
    fn each_adapter_keeps_its_own_answer() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        static CACHE: Cache = Cache::new(Duration::from_secs(30));
        static ASKED: AtomicU64 = AtomicU64::new(0);
        static ANSWER: AtomicBool = AtomicBool::new(true);
        // card 1 answers when the test says so, card 2 at once; 1000s and 2000s
        fn ask(a: &Adapter) -> Option<u64> {
            if a.device == 1 {
                wait_for(&ANSWER);
            }
            Some(a.device as u64 * 1000 + ASKED.fetch_add(1, Ordering::SeqCst))
        }
        let (one, two) = (adapter(NVIDIA, 1), adapter(NVIDIA, 2));
        assert_eq!(CACHE.get(&one, ask), Some(1000));
        assert_eq!(CACHE.get(&two, ask), Some(2001));
        assert!(CACHE.expire(&one) && CACHE.expire(&two));
        // card 1 refreshes in the background while card 2 is asked for: neither waits, nor replaces the other
        ANSWER.store(false, Ordering::SeqCst);
        assert_eq!(CACHE.get(&one, ask), Some(1000));
        assert_eq!(CACHE.get(&two, ask), Some(2001));
        settled(&CACHE, &two);
        ANSWER.store(true, Ordering::SeqCst);
        settled(&CACHE, &one);
        let asked = ASKED.load(Ordering::SeqCst);
        assert_eq!(asked, 4, "one background query per card");
        let (a1, a2) = (CACHE.get(&one, ask), CACHE.get(&two, ask));
        assert!(a1.is_some_and(|v| v / 1000 == 1) && a2.is_some_and(|v| v / 1000 == 2), "{a1:?} {a2:?}");
        assert_eq!(ASKED.load(Ordering::SeqCst), asked, "fresh answers are not asked for again");
    }

    #[test]
    fn pci_addresses_parse_in_both_forms() {
        assert_eq!(pci_address("0000:01:00.0"), Some([0, 1, 0, 0]));
        assert_eq!(pci_address("00000000:0A:00.1"), Some([0, 10, 0, 1]));
        assert_eq!(pci_address(""), None);
        assert_eq!(pci_address("01:00.0"), None);
        assert_eq!(pci_address("0000:01:00.0:9"), None);
    }

    #[test]
    fn nvidia_output_is_matched_by_address_then_device_id() {
        let two = "00000000:01:00.0, 0x179B10DE, 3990\n00000000:02:00.0, 0x1F0810DE, 5120\n";
        let cards = parse_nvidia(two);
        assert_eq!(pick(&cards, &adapter(NVIDIA, 0x1f08)), Some(5120 << 20));
        assert_eq!(pick(&cards, &adapter(NVIDIA, 0x179b)), Some(3990 << 20));
        // several GPUs and none matches: unknown
        assert_eq!(pick(&cards, &adapter(NVIDIA, 0x2204)), None);
        // a single GPU is taken even if its id is reported differently
        assert_eq!(pick(&parse_nvidia("00000000:01:00.0, 0x179B10DE, 3990\n"), &adapter(NVIDIA, 0x1234)), Some(3990 << 20));
        // garbage is not a number
        assert!(parse_nvidia("No devices were found\n").is_empty());
        assert_eq!(pick(&parse_nvidia(""), &adapter(NVIDIA, 0x179b)), None);
    }

    #[test]
    fn two_cards_of_one_model_are_told_apart_by_address_or_not_at_all() {
        let twins = parse_nvidia("00000000:01:00.0, 0x1F0810DE, 1000\n00000000:02:00.0, 0x1F0810DE, 6000\n");
        let at = |bus| Adapter { bus: pci_address(bus), ..adapter(NVIDIA, 0x1f08) };
        assert_eq!(pick(&twins, &at("0000:02:00.0")), Some(6000 << 20));
        assert_eq!(pick(&twins, &at("0000:01:00.0")), Some(1000 << 20));
        // without an address the model alone is ambiguous: no answer, the caller keeps its fixed budget
        assert_eq!(pick(&twins, &adapter(NVIDIA, 0x1f08)), None);
        assert_eq!(pick(&twins, &at("")), None);
        // an address no card has is no card: the model is not looked up instead
        let mixed = parse_nvidia("00000000:01:00.0, 0x1F0810DE, 1000\n00000000:02:00.0, 0x220410DE, 6000\n");
        assert_eq!(pick(&mixed, &at("0000:03:00.0")), None);
    }
}
