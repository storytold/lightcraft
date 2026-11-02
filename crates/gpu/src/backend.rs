//! Which wgpu backends LightCraft lets wgpu use — for its compute device and for the desktop
//! window (eframe) — and the crash sentinel around device creation (issue #136).
//!
//! wgpu loads the system driver of *every* backend in the instance's set while it enumerates
//! adapters, even when it then picks another one: with Vulkan in the set, a broken Vulkan driver
//! can crash the process (an access violation inside the driver DLL, which nothing can catch)
//! although DX12 would have been used. So Windows defaults to DX12 alone. macOS uses Metal, Linux
//! Vulkan (plus GL for the window when the build has it).
//!
//! Overrides, read once per process:
//! - `LIGHTCRAFT_GPU_BACKEND` = `dx12` | `vulkan` | `metal` | `gl` (comma lists allowed), `auto`
//!   (the platform default), or `off` (no GPU compute; the window still needs one backend and
//!   keeps the platform default);
//! - else `WGPU_BACKEND` (wgpu's own variable, same names) for both the window and compute.
//! - `LIGHTCRAFT_GPU_ADAPTER` = (part of) a graphics card's name, any case (`4070`): the card for
//!   the compute devices — the render device, the denoise network's, the face detector's
//!   ([`pick_adapter`]). The window stays on the card that drives the screen.
//!
//! Backends this build doesn't contain (e.g. `gl`: the GL backend isn't compiled in) are ignored
//! with a warning and the platform default is used.

use std::path::PathBuf;
use std::sync::Mutex;

pub use wgpu::Backends;

/// What the environment asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendChoice {
    /// The platform default.
    Auto,
    /// No GPU compute.
    Off,
    /// These backends.
    Use(Backends),
}

/// Parse a backend list (`"dx12"`, `"vulkan,gl"`, `"auto"`, `"off"`…). `None`: nothing usable in
/// it (empty or unknown names).
pub fn parse_backends(s: &str) -> Option<BackendChoice> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "" => None,
        "auto" | "default" => Some(BackendChoice::Auto),
        "off" | "none" | "cpu" | "0" | "false" | "no" => Some(BackendChoice::Off),
        // the window's OpenGL renderer (see `WindowRenderer`): compute keeps its default
        "glow" => Some(BackendChoice::Auto),
        _ => {
            let b = Backends::from_comma_list(&s);
            (!b.is_empty()).then_some(BackendChoice::Use(b))
        }
    }
}

/// The choice from `LIGHTCRAFT_GPU_BACKEND` (`lightcraft`) or else `WGPU_BACKEND` (`wgpu`): the
/// first one that parses wins; an unknown value is skipped (with a warning).
pub fn choose(lightcraft: Option<&str>, wgpu: Option<&str>) -> BackendChoice {
    for (name, v) in [("LIGHTCRAFT_GPU_BACKEND", lightcraft), ("WGPU_BACKEND", wgpu)] {
        let Some(v) = v else { continue };
        match parse_backends(v) {
            // WGPU_BACKEND is wgpu's variable: it selects backends, it never turns the GPU off
            Some(BackendChoice::Off) if name == "WGPU_BACKEND" => log::warn!("gpu: WGPU_BACKEND={v} ignored (use LIGHTCRAFT_GPU_BACKEND=off)"),
            Some(c) => return c,
            None => log::warn!("gpu: {name}={v} names no known backend; ignored"),
        }
    }
    BackendChoice::Auto
}

/// The backends compute devices use when nothing is asked for on `os` (`std::env::consts::OS`).
pub fn default_compute_backends(os: &str) -> Backends {
    match os {
        // never touch the Vulkan driver unless asked: see the module docs (issue #136)
        "windows" => Backends::DX12,
        "macos" | "ios" => Backends::METAL,
        _ => Backends::PRIMARY,
    }
}

/// The backends the window uses when nothing is asked for on `os`.
pub fn default_window_backends(os: &str) -> Backends {
    match os {
        "windows" => Backends::DX12,
        "macos" | "ios" => Backends::METAL,
        // eframe's default (GL only when the build has it)
        _ => Backends::PRIMARY | Backends::GL,
    }
}

/// Resolve `choice` against the backends this build contains (`compiled`): `None` = GPU off.
/// Asked-for backends that aren't compiled in fall back to `default`.
pub fn resolve(choice: BackendChoice, default: Backends, compiled: Backends) -> Option<Backends> {
    match choice {
        BackendChoice::Off => None,
        BackendChoice::Auto => Some(default),
        BackendChoice::Use(b) if b.intersects(compiled) => Some(b & compiled),
        BackendChoice::Use(b) => {
            log::warn!("gpu: backend {b:?} is not in this build ({compiled:?}); using {default:?}");
            Some(default)
        }
    }
}

fn env_choice() -> BackendChoice {
    static C: std::sync::OnceLock<BackendChoice> = std::sync::OnceLock::new();
    *C.get_or_init(|| {
        let lc = std::env::var("LIGHTCRAFT_GPU_BACKEND").ok();
        let wg = std::env::var("WGPU_BACKEND").ok();
        choose(lc.as_deref(), wg.as_deref())
    })
}

/// The backends of the compute device (`None`: `LIGHTCRAFT_GPU_BACKEND=off`).
pub fn compute_backends() -> Option<Backends> {
    resolve(env_choice(), default_compute_backends(std::env::consts::OS), wgpu::Instance::enabled_backend_features())
}

/// The backends for the desktop window's renderer (eframe/egui-wgpu). The window always needs
/// one: `off` keeps the platform default.
pub fn window_backends() -> Backends {
    let default = default_window_backends(std::env::consts::OS);
    let choice = match env_choice() {
        BackendChoice::Off => BackendChoice::Auto,
        c => c,
    };
    resolve(choice, default, wgpu::Instance::enabled_backend_features()).unwrap_or(default)
}

/// The DX12 shader compiler: FXC (`d3dcompiler_47.dll`, part of Windows) unless
/// `WGPU_DX12_COMPILER` (`wgpu`, wgpu's own variable: `fxc`, `dxc`, `auto`…) names another.
///
/// Issue #471: wgpu's default (`Auto`) loads whichever `dxcompiler.dll` the DLL search path finds
/// first — LightCraft ships none, so it is some other program's (an SDK's, a folder on `PATH`). A
/// copy without its `dxil.dll` beside it warns that the DXIL is unsigned; wgpu takes the warning for
/// a compile error, its own validation pipelines fail, the device is lost and the window never
/// opens. FXC is always there and compiles everything LightCraft's shaders need.
pub fn dx12_compiler(env: Option<&str>) -> wgpu::Dx12Compiler {
    let Some(v) = env else { return wgpu::Dx12Compiler::Fxc };
    v.parse().unwrap_or_else(|e| {
        log::warn!("gpu: WGPU_DX12_COMPILER={v} ignored: {e}");
        wgpu::Dx12Compiler::Fxc
    })
}

/// wgpu's backend options for every instance LightCraft creates — the window's and the compute
/// devices': wgpu's environment variables, with the DX12 compiler from [`dx12_compiler`].
pub fn backend_options() -> wgpu::BackendOptions {
    let mut o = wgpu::BackendOptions::from_env_or_default();
    o.dx12.shader_compiler = dx12_compiler(std::env::var("WGPU_DX12_COMPILER").ok().as_deref());
    o
}

/// wgpu's instance flags for every instance LightCraft creates, from `flags` (wgpu's defaults with its
/// environment variables applied) without `VALIDATION_INDIRECT_CALL`, unless
/// `WGPU_VALIDATION_INDIRECT_CALL` asks for it (any value but `0`). That validation builds an internal
/// compute pipeline when each device is created; Intel Macs' Metal compiler fails on it ("Compiler
/// encountered an internal error"), the device is lost and the window never opens (#250). It only
/// guards indirect draws and dispatches, which neither LightCraft nor egui issue.
pub fn instance_flags(flags: wgpu::InstanceFlags, env: Option<&str>) -> wgpu::InstanceFlags {
    if env.is_some_and(|v| v != "0") {
        flags | wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    } else {
        flags - wgpu::InstanceFlags::VALIDATION_INDIRECT_CALL
    }
}

/// [`instance_flags`] with `WGPU_VALIDATION_INDIRECT_CALL` read from the environment.
pub fn instance_flags_from_env(flags: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    instance_flags(flags, std::env::var("WGPU_VALIDATION_INDIRECT_CALL").ok().as_deref())
}

/// The graphics card `LIGHTCRAFT_GPU_ADAPTER` asks for: (part of) its name, trimmed and in lower
/// case; `None` when it is unset or blank.
pub fn wanted_adapter() -> Option<String> {
    wanted_adapter_from(std::env::var("LIGHTCRAFT_GPU_ADAPTER").ok().as_deref())
}

/// [`wanted_adapter`] from the variable's value `env`.
pub fn wanted_adapter_from(env: Option<&str>) -> Option<String> {
    env.map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty())
}

/// Whether the adapter called `name` is the one `wanted` asks for: `wanted` is part of its name,
/// ignoring case (`4070` matches "NVIDIA GeForce RTX 4070 SUPER").
pub fn adapter_name_matches(wanted: &str, name: &str) -> bool {
    let wanted = wanted.trim().to_lowercase();
    !wanted.is_empty() && name.to_lowercase().contains(&wanted)
}

/// One adapter as [`choose_named_adapter`] sees it, in wgpu's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate<'a> {
    pub name: &'a str,
    /// Order among several matching cards, lower first ([`kind_rank`]).
    pub rank: u8,
    /// A software renderer (WARP, llvmpipe): never picked, the CPU pipeline is faster.
    pub software: bool,
}

impl<'a> Candidate<'a> {
    /// A candidate from an adapter's name and kind.
    pub fn of(name: &'a str, kind: wgpu::DeviceType) -> Candidate<'a> {
        Candidate { name, rank: kind_rank(kind), software: kind == wgpu::DeviceType::Cpu }
    }
}

/// The order among cards that all match: dedicated, built-in, virtual, other, software.
pub fn kind_rank(kind: wgpu::DeviceType) -> u8 {
    match kind {
        wgpu::DeviceType::DiscreteGpu => 0,
        wgpu::DeviceType::IntegratedGpu => 1,
        wgpu::DeviceType::VirtualGpu => 2,
        wgpu::DeviceType::Other => 3,
        wgpu::DeviceType::Cpu => 4,
    }
}

/// What a `LIGHTCRAFT_GPU_ADAPTER` name picks among the candidates ([`choose_named_adapter`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NamedChoice {
    /// The card used (the first of `matches`), `None`: the default choice.
    pub index: Option<usize>,
    /// Every hardware card whose name matches, in the defined order.
    pub matches: Vec<usize>,
    /// Software renderers whose name matches (never used).
    pub software: Vec<usize>,
}

/// Which of `candidates` (in wgpu's order) `wanted` picks: of the hardware cards whose name
/// contains it (any case), the one whose whole name it is, then dedicated before built-in before
/// virtual cards ([`kind_rank`]), then wgpu's order. Software renderers are never picked.
pub fn choose_named_adapter(wanted: &str, candidates: &[Candidate<'_>]) -> NamedChoice {
    let w = wanted.trim().to_lowercase();
    let hits = || candidates.iter().enumerate().filter(|(_, c)| adapter_name_matches(&w, c.name));
    let software: Vec<usize> = hits().filter(|(_, c)| c.software).map(|(i, _)| i).collect();
    let mut ranked: Vec<(bool, u8, usize)> =
        hits().filter(|(_, c)| !c.software).map(|(i, c)| (c.name.trim().to_lowercase() != w, c.rank, i)).collect();
    // (the index last: equal ones keep wgpu's order)
    ranked.sort_unstable();
    let matches: Vec<usize> = ranked.into_iter().map(|(_, _, i)| i).collect();
    NamedChoice { index: matches.first().copied(), matches, software }
}

/// The log line for `choice` (warning: `true`): which card the name picked and why, or why none.
pub fn named_choice_note(wanted: &str, choice: &NamedChoice, candidates: &[Candidate<'_>]) -> (bool, String) {
    let name = |i: &usize| candidates.get(*i).map_or("?", |c| c.name);
    let list = |v: &[usize]| v.iter().map(name).collect::<Vec<_>>().join(", ");
    match (choice.index, choice.matches.len()) {
        (Some(i), 1) => (false, format!("gpu: LIGHTCRAFT_GPU_ADAPTER={wanted}: using {}", name(&i))),
        (Some(i), n) => (
            false,
            format!(
                "gpu: LIGHTCRAFT_GPU_ADAPTER={wanted} matches {n} graphics cards ({}): using {} (a card's whole name first, then dedicated before built-in cards, then the system's order); give more of the name to pick another",
                list(&choice.matches),
                name(&i)
            ),
        ),
        (None, _) if !choice.software.is_empty() => (
            true,
            format!(
                "gpu: LIGHTCRAFT_GPU_ADAPTER={wanted} names only a software renderer ({}), never used: the processor is faster; using the default choice",
                list(&choice.software)
            ),
        ),
        (None, _) => {
            let all: Vec<&str> = candidates.iter().map(|c| c.name).collect();
            (true, format!("gpu: LIGHTCRAFT_GPU_ADAPTER={wanted} names none of the graphics cards ({}); using the default choice", all.join(", ")))
        }
    }
}

/// Lines already logged by [`log_once`] (each device that picks a card asks again).
static LOGGED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Log `line` (as a warning when `warn`) the first time only; whether it was logged now.
pub fn log_once(warn: bool, line: &str) -> bool {
    let mut seen = LOGGED.lock().unwrap_or_else(|e| e.into_inner());
    if seen.iter().any(|l| l == line) {
        return false;
    }
    // (a handful of distinct lines in a run; the cap only bounds a pathological caller)
    if seen.len() < 64 {
        seen.push(line.to_string());
    }
    if warn {
        log::warn!("{line}");
    } else {
        log::info!("{line}");
    }
    true
}

/// Which of `adapters` (name and kind, in wgpu's order) `wanted` picks ([`choose_named_adapter`]).
pub fn find_named_adapter<'a>(wanted: &str, adapters: impl IntoIterator<Item = (&'a str, wgpu::DeviceType)>) -> Option<usize> {
    let candidates: Vec<Candidate<'a>> = adapters.into_iter().map(|(n, k)| Candidate::of(n, k)).collect();
    choose_named_adapter(wanted, &candidates).index
}

/// A compute instance on `backends`, with LightCraft's backend options and flags.
pub(crate) fn compute_instance(backends: Backends) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    // only these drivers are loaded (never Vulkan on Windows unless asked: see the module docs)
    desc.backends = backends;
    // DX12 shaders compile with FXC (issue #471)
    desc.backend_options = backend_options();
    // no indirect-call validation pipeline: Intel Metal fails to compile it (#250)
    desc.flags = instance_flags_from_env(desc.flags);
    wgpu::Instance::new(desc)
}

/// The adapters on `instance` in wgpu's order, and the settings' list of them
/// ([`crate::cards::entries_from`]; also kept for [`crate::adapters`]).
pub(crate) fn enumerate(instance: &wgpu::Instance, backends: Backends) -> (Vec<wgpu::Adapter>, Vec<crate::cards::AdapterEntry>) {
    let all = pollster::block_on(instance.enumerate_adapters(backends));
    let described: Vec<(wgpu::AdapterInfo, u32, Option<u64>)> = all
        .iter()
        .map(|a| {
            let info = a.get_info();
            let memory = crate::vram::adapter_memory(&info);
            (info, a.limits().max_storage_buffers_per_shader_stage, memory)
        })
        .collect();
    let entries = crate::cards::entries_from(&described, crate::cards::screen().as_ref());
    crate::cards::set_listed(entries.clone());
    (all, entries)
}

/// An adapter picked for a compute device, with what the settings know of it.
pub(crate) struct Picked {
    pub adapter: wgpu::Adapter,
    /// Its key (`None`: wgpu picked one the listing doesn't show).
    pub key: Option<crate::cards::AdapterKey>,
    pub resolution: crate::cards::Resolution,
    pub entries: Vec<crate::cards::AdapterEntry>,
}

/// The editing card's adapter on `instance` ([`crate::cards::resolve_card`]: `LIGHTCRAFT_GPU_ADAPTER`,
/// else the card saved in the settings unless `automatic`, else the screen's, else the
/// high-performance one). Never a panic; `Err` only when there is no adapter at all.
pub(crate) fn pick_editing(instance: &wgpu::Instance, backends: Backends, automatic: bool) -> Result<Picked, String> {
    let (all, entries) = enumerate(instance, backends);
    let saved = if automatic { None } else { crate::cards::card() };
    let resolution = crate::cards::resolve_card(&entries, wanted_adapter().as_deref(), saved.as_ref());
    picked_at(instance, backends, all, entries, resolution)
}

/// The adapter of entry `resolution.index` (none: wgpu's high-performance pick).
pub(crate) fn picked_at(
    instance: &wgpu::Instance,
    backends: Backends,
    all: Vec<wgpu::Adapter>,
    entries: Vec<crate::cards::AdapterEntry>,
    resolution: crate::cards::Resolution,
) -> Result<Picked, String> {
    let mut all = all;
    if let Some(i) = resolution.index.filter(|i| *i < all.len()) {
        let adapter = all.swap_remove(i);
        let key = entries.get(i).map(|e| e.key.clone());
        return Ok(Picked { adapter, key, resolution, entries });
    }
    let adapter = pollster::block_on(
        instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() }),
    )
    .map_err(|e| format!("no GPU adapter among {backends:?} ({e})"))?;
    let k = crate::cards::AdapterKey::of(&adapter.get_info(), 1);
    let wgpu_pick = entries.iter().position(|e| e.key.same_model(&k));
    // wgpu's pick can't render here but a listed card can: that one (review of PR #3544)
    if let Some(u) = usable_instead(&entries, wgpu_pick).filter(|u| *u < all.len()) {
        let adapter = all.swap_remove(u);
        let key = entries.get(u).map(|e| e.key.clone());
        return Ok(Picked { adapter, key, resolution, entries });
    }
    let key = wgpu_pick.and_then(|i| entries.get(i)).map(|e| e.key.clone());
    Ok(Picked { adapter, key, resolution, entries })
}

/// When wgpu's own pick (entry `wgpu_pick`) is listed as unusable: the first usable entry.
fn usable_instead(entries: &[crate::cards::AdapterEntry], wgpu_pick: Option<usize>) -> Option<usize> {
    let pick = entries.get(wgpu_pick?)?;
    if pick.usable { None } else { entries.iter().position(|e| e.usable) }
}

/// The adapter for a compute device of its own (the denoise network's, the face detector's, the
/// mask models'): the editing card ([`pick_editing`]). Never a panic.
pub fn pick_adapter(instance: &wgpu::Instance, backends: Backends) -> Result<wgpu::Adapter, String> {
    pick_editing(instance, backends, false).map(|p| p.adapter)
}

/// `LIGHTCRAFT_GPU_BACKEND=off`.
pub(crate) fn env_off() -> bool {
    env_choice() == BackendChoice::Off
}

static MARKER: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Scopes of driver work (device creation, shader and pipeline compilation) running now, each
/// with what it does, so that overlapping ones (the window-side warm-up and a first denoise, the
/// editing card and the second card) share one marker file that names them all.
static ACTIVE: Mutex<Scopes> = Mutex::new(Scopes::new());

/// Driver-work scopes running now: (number, line) of each.
pub(crate) struct Scopes {
    next: u64,
    running: Vec<(u64, String)>,
}

impl Scopes {
    pub(crate) const fn new() -> Scopes {
        Scopes { next: 1, running: Vec::new() }
    }
}

/// Write this file before the compute device is created or any shader / pipeline is compiled
/// (the kernels, the denoise network's), and remove it once that returned (successfully or not).
/// A file still there at the next launch means the process died inside the driver — e.g. Apple's
/// Metal compiler (`MTLCompilerService`) aborting on a kernel, issue #250: see
/// [`take_init_marker`]. `None` (the default) writes nothing.
pub fn set_init_marker(path: Option<PathBuf>) {
    *MARKER.lock().unwrap_or_else(|e| e.into_inner()) = path;
}

/// The marker's line for one scope: what started, and when.
fn marker_text(what: &str, secs: u64) -> String {
    format!("GPU {what} started (unix time {secs})\n")
}

/// Write the marker with a line per running scope, or remove it when none runs.
fn write_marker(marker: &std::path::Path, s: &Scopes) {
    if s.running.is_empty() {
        let _ = std::fs::remove_file(marker);
        return;
    }
    if let Some(d) = marker.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = s.running.iter().map(|(_, line)| line.as_str()).collect();
    let _ = std::fs::write(marker, text);
}

/// Enter a driver-work scope on `marker`: its line joins the file. Returns the scope's number.
fn arm(marker: &std::path::Path, active: &Mutex<Scopes>, what: &str) -> u64 {
    let mut s = active.lock().unwrap_or_else(|e| e.into_inner());
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let id = s.next;
    s.next = s.next.wrapping_add(1);
    s.running.push((id, marker_text(what, secs)));
    write_marker(marker, &s);
    id
}

/// Leave scope `id`: its line goes; the last one removes the file.
fn disarm(marker: &std::path::Path, active: &Mutex<Scopes>, id: u64) {
    let mut s = active.lock().unwrap_or_else(|e| e.into_inner());
    let before = s.running.len();
    s.running.retain(|(i, _)| *i != id);
    if s.running.len() != before || s.running.is_empty() {
        write_marker(marker, &s);
    }
}

/// Removes the scope's line on the way out, a panic included (the process survived).
struct Armed<'a>(Option<(PathBuf, u64)>, &'a Mutex<Scopes>);

impl Drop for Armed<'_> {
    fn drop(&mut self) {
        if let Some((m, id)) = &self.0 {
            disarm(m, self.1, *id);
        }
    }
}

/// Run driver work `f` (`what`: "device creation", "kernel compilation"…) between writing and
/// removing the init marker (if one is set).
pub(crate) fn with_marker<T>(what: &str, f: impl FnOnce() -> T) -> T {
    let marker = MARKER.lock().unwrap_or_else(|e| e.into_inner()).clone();
    with_marker_on(marker, &ACTIVE, what, f)
}

/// [`with_marker`] on an explicit marker path and scope list (the tests use their own, so that
/// device-creating tests running in parallel cannot join their scopes).
fn with_marker_on<T>(marker: Option<PathBuf>, active: &Mutex<Scopes>, what: &str, f: impl FnOnce() -> T) -> T {
    let armed = marker.map(|m| {
        let id = arm(&m, active, what);
        (m, id)
    });
    let _armed = Armed(armed, active);
    f()
}

/// Run device creation (and the kernel build that follows it) `f` under the init marker.
pub(crate) fn with_init_marker<T>(backends: Backends, f: impl FnOnce() -> T) -> T {
    with_marker(&format!("device and kernel creation (backends {backends:?})"), f)
}

/// [`with_init_marker`] for the device of a known card `key` in role `role` (`editing`,
/// `helper`): the marker names them ([`marker_scopes`]), so a crash there costs the next launch
/// only that choice, not the GPU.
pub(crate) fn with_init_marker_for<T>(backends: Backends, key: Option<&crate::cards::AdapterKey>, role: &str, f: impl FnOnce() -> T) -> T {
    match key {
        Some(k) => with_marker(&format!("device and kernel creation (backends {backends:?}) [card={k}; role={role}]"), f),
        None => with_init_marker(backends, f),
    }
}

/// A scope a crash marker names: the card's key and role when it was a device of a known card
/// (`None`: other driver work, e.g. listing the cards or a denoise network's kernels).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerScope {
    pub card: Option<(crate::cards::AdapterKey, String)>,
}

/// The scopes a crash marker's text names, one a line ([`with_init_marker_for`]).
pub fn marker_scopes(text: &str) -> Vec<MarkerScope> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let card = l.split_once("[card=").and_then(|(_, rest)| {
                let (key, rest) = rest.split_once("; role=")?;
                let (role, _) = rest.split_once(']')?;
                Some((crate::cards::AdapterKey::parse(key)?, role.trim().to_string()))
            });
            MarkerScope { card }
        })
        .collect()
}

/// If the init marker `path` is present — the last process died while creating the GPU device —
/// remove it and return what it recorded.
pub fn take_init_marker(path: &std::path::Path) -> Option<String> {
    let text = read_init_marker(path)?;
    let _ = std::fs::remove_file(path);
    Some(text)
}

/// What the init marker `path` recorded, if it is present — without removing it. For sessions
/// that must leave the disk as they found it (`--memory`, issues #164 and #169): GPU rendering
/// still starts off, and the marker stays for the next ordinary launch to report and clear.
pub fn read_init_marker(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string())
}

/// What draws the desktop window: egui-wgpu on one wgpu backend, or eframe's OpenGL renderer
/// (glow) as the last resort when no wgpu backend can build the window's pipeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowRenderer {
    /// egui-wgpu on exactly one backend.
    Wgpu(Backends),
    /// eframe's glow (OpenGL) renderer.
    Glow,
}

impl WindowRenderer {
    /// The name used in `LIGHTCRAFT_GPU_BACKEND`, the remembered-renderer file and the logs.
    pub fn name(self) -> &'static str {
        match self {
            WindowRenderer::Glow => "glow",
            WindowRenderer::Wgpu(b) if b == Backends::DX12 => "dx12",
            WindowRenderer::Wgpu(b) if b == Backends::VULKAN => "vulkan",
            WindowRenderer::Wgpu(b) if b == Backends::METAL => "metal",
            WindowRenderer::Wgpu(b) if b == Backends::GL => "gl",
            WindowRenderer::Wgpu(_) => "wgpu",
        }
    }

    /// One renderer name (`dx12`, `vulkan`, `metal`, `gl`, `glow`); anything else (lists, `auto`,
    /// `off`, unknown names) is `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        if s == "glow" {
            return Some(WindowRenderer::Glow);
        }
        match parse_backends(&s)? {
            BackendChoice::Use(b) if [Backends::DX12, Backends::VULKAN, Backends::METAL, Backends::GL].contains(&b) => Some(WindowRenderer::Wgpu(b)),
            _ => None,
        }
    }
}

/// The renderer the environment forces for the window: `LIGHTCRAFT_GPU_BACKEND` (`app`), else
/// `WGPU_BACKEND` (`wgpu`), when it names exactly one renderer.
pub fn forced_window_renderer(app: Option<&str>, wgpu: Option<&str>) -> Option<WindowRenderer> {
    app.and_then(WindowRenderer::parse).or_else(|| wgpu.and_then(WindowRenderer::parse))
}

/// The order in which the window's renderers are tried on `os`: the forced one (environment)
/// first, then the one remembered from the last good start, then the platform chain — on Windows
/// DX12, Vulkan, GL, then glow. DX12 stays first: with Vulkan loaded, a broken Vulkan driver can
/// crash the process uncatchably (issue #136), so Vulkan is touched only after DX12 failed.
/// `failed` (the renderer the last start died in) goes last, as a last resort only. Backends not
/// in `compiled`, and glow without `glow`, are left out; the list is never empty.
pub fn window_renderer_order(
    forced: Option<WindowRenderer>,
    remembered: Option<WindowRenderer>,
    failed: Option<WindowRenderer>,
    os: &str,
    compiled: Backends,
    glow: bool,
) -> Vec<WindowRenderer> {
    let platform = match os {
        "windows" => vec![Backends::DX12, Backends::VULKAN, Backends::GL],
        "macos" | "ios" => vec![Backends::METAL],
        _ => vec![Backends::VULKAN, Backends::GL],
    };
    let chain = platform.into_iter().map(WindowRenderer::Wgpu).chain([WindowRenderer::Glow]);
    let usable = |r: &WindowRenderer| match r {
        WindowRenderer::Glow => glow,
        WindowRenderer::Wgpu(b) => !b.is_empty() && compiled.contains(*b),
    };
    let not_failed = |r: &WindowRenderer| Some(*r) != failed;
    let candidates = forced.into_iter().chain(remembered.filter(not_failed)).chain(chain.filter(not_failed)).chain(failed);
    let mut out: Vec<WindowRenderer> = Vec::new();
    for r in candidates {
        if usable(&r) && !out.contains(&r) {
            out.push(r);
        }
    }
    if out.is_empty() {
        out.push(WindowRenderer::Wgpu(default_window_backends(os) & compiled));
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
pub(crate) mod tests {

    /// Review of PR #3544: wgpu's pick that can't render here gives way to a usable listed card.
    #[test]
    fn an_unusable_wgpu_pick_gives_way_to_a_usable_card() {
        let info = |name: &str, device| wgpu::AdapterInfo {
            name: name.into(),
            vendor: 0x10de,
            device,
            ..wgpu::AdapterInfo::new(wgpu::DeviceType::DiscreteGpu, wgpu::Backend::Dx12)
        };
        let mut e = crate::cards::entries_from(&[(info("Card A", 1), 64, None), (info("Card B", 2), 64, None)], None);
        assert_eq!(super::usable_instead(&e, Some(0)), None, "wgpu's pick is usable");
        e[0].usable = false;
        assert_eq!(super::usable_instead(&e, Some(0)), Some(1));
        assert_eq!(super::usable_instead(&e, None), None, "wgpu's pick isn't listed: kept");
        e[1].usable = false;
        assert_eq!(super::usable_instead(&e, Some(0)), None, "nothing usable: kept");
    }

    /// #250: the indirect-call validation pipeline is off unless asked for; other flags are kept.
    #[test]
    fn indirect_call_validation_is_off_unless_asked_for() {
        use wgpu::InstanceFlags as F;
        let base = F::VALIDATION_INDIRECT_CALL | F::DEBUG;
        assert_eq!(instance_flags(base, None), F::DEBUG);
        assert_eq!(instance_flags(base, Some("0")), F::DEBUG);
        assert_eq!(instance_flags(F::empty(), Some("1")), F::VALIDATION_INDIRECT_CALL);
        assert_eq!(instance_flags(F::empty(), None), F::empty());
    }
    use super::*;

    #[test]
    fn parses_backend_names() {
        assert_eq!(parse_backends("dx12"), Some(BackendChoice::Use(Backends::DX12)));
        assert_eq!(parse_backends(" D3D12 "), Some(BackendChoice::Use(Backends::DX12)));
        assert_eq!(parse_backends("vulkan"), Some(BackendChoice::Use(Backends::VULKAN)));
        assert_eq!(parse_backends("vk,gl"), Some(BackendChoice::Use(Backends::VULKAN | Backends::GL)));
        assert_eq!(parse_backends("metal"), Some(BackendChoice::Use(Backends::METAL)));
        assert_eq!(parse_backends("auto"), Some(BackendChoice::Auto));
        for off in ["off", "OFF", "none", "cpu", "0"] {
            assert_eq!(parse_backends(off), Some(BackendChoice::Off), "{off}");
        }
        assert_eq!(parse_backends(""), None);
        assert_eq!(parse_backends("directx9"), None);
    }

    #[test]
    fn lightcraft_variable_wins_over_wgpu_backend() {
        assert_eq!(choose(None, None), BackendChoice::Auto);
        assert_eq!(choose(None, Some("dx12")), BackendChoice::Use(Backends::DX12));
        assert_eq!(choose(Some("vulkan"), Some("dx12")), BackendChoice::Use(Backends::VULKAN));
        assert_eq!(choose(Some("off"), Some("dx12")), BackendChoice::Off);
        // unknown LightCraft value: WGPU_BACKEND still applies
        assert_eq!(choose(Some("bogus"), Some("gl")), BackendChoice::Use(Backends::GL));
        // WGPU_BACKEND never turns the GPU off
        assert_eq!(choose(None, Some("off")), BackendChoice::Auto);
    }

    /// Issue #136: Windows must not load the Vulkan driver unless asked to.
    #[test]
    fn windows_defaults_to_dx12_only() {
        assert_eq!(default_compute_backends("windows"), Backends::DX12);
        assert_eq!(default_window_backends("windows"), Backends::DX12);
        assert!(!default_window_backends("windows").contains(Backends::VULKAN));
        assert_eq!(default_compute_backends("macos"), Backends::METAL);
        assert!(default_compute_backends("linux").contains(Backends::VULKAN));
        assert!(default_window_backends("linux").contains(Backends::VULKAN | Backends::GL));
    }

    /// Issue #471: DX12 shaders compile with FXC, never a `dxcompiler.dll` found on the search path,
    /// unless `WGPU_DX12_COMPILER` asks for it.
    #[test]
    fn dx12_compiles_with_fxc_unless_asked() {
        assert!(matches!(dx12_compiler(None), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("bogus")), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("FXC")), wgpu::Dx12Compiler::Fxc));
        assert!(matches!(dx12_compiler(Some("dxc")), wgpu::Dx12Compiler::DynamicDxc { .. }));
        assert!(matches!(dx12_compiler(Some("auto")), wgpu::Dx12Compiler::Auto));
        if std::env::var_os("WGPU_DX12_COMPILER").is_none() {
            assert!(matches!(backend_options().dx12.shader_compiler, wgpu::Dx12Compiler::Fxc));
        }
    }

    /// `LIGHTCRAFT_GPU_ADAPTER` picks a card by part of its name, any case; software adapters never.
    #[test]
    fn gpu_adapter_variable_matches_part_of_the_card_name() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu};
        assert_eq!(wanted_adapter_from(None), None);
        assert_eq!(wanted_adapter_from(Some("  ")), None);
        assert_eq!(wanted_adapter_from(Some(" RTX 4070 ")).as_deref(), Some("rtx 4070"));
        assert!(adapter_name_matches("4070", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(adapter_name_matches("geforce rtx 4070 super", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(adapter_name_matches(" Super ", "NVIDIA GeForce RTX 4070 SUPER"));
        assert!(!adapter_name_matches("4070", "NVIDIA GeForce RTX 5070"));
        assert!(!adapter_name_matches("", "NVIDIA GeForce RTX 5070"), "a blank name matches nothing");
        // two cards: the screen's card first in wgpu's order, as here
        let two = [("NVIDIA GeForce RTX 5070", DiscreteGpu), ("NVIDIA GeForce RTX 4070 SUPER", DiscreteGpu), ("Microsoft Basic Render Driver", Cpu)];
        assert_eq!(find_named_adapter("4070", two), Some(1));
        assert_eq!(find_named_adapter("5070", two), Some(0));
        // "070" is in both: the first one in wgpu's order
        assert_eq!(find_named_adapter("070", two), Some(0));
        assert_eq!(find_named_adapter("radeon", two), None, "no match: the caller falls back to the default choice");
        assert_eq!(find_named_adapter("basic render", two), None, "a software adapter is never picked");
        assert_eq!(find_named_adapter("intel", [("Intel(R) UHD Graphics 770", IntegratedGpu)]), Some(0));
        assert_eq!(find_named_adapter("4070", [] as [(&str, wgpu::DeviceType); 0]), None);
    }

    /// A fake adapter list (a laptop: built-in card first, a dedicated one, WARP; and two of a
    /// kind): no match, several matches, software, and the one log line each gives.
    #[test]
    fn a_fake_adapter_list_picks_in_a_defined_order() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu, IntegratedGpu, VirtualGpu};
        let laptop = [
            Candidate::of("Intel(R) Iris(R) Xe Graphics", IntegratedGpu),
            Candidate::of("NVIDIA GeForce RTX 4060 Laptop GPU", DiscreteGpu),
            Candidate::of("Microsoft Basic Render Driver", Cpu),
        ];
        // no match: the default choice, and a warning that lists the cards
        let none = choose_named_adapter("radeon", &laptop);
        assert_eq!(none, NamedChoice::default());
        let (warn, line) = named_choice_note("radeon", &none, &laptop);
        assert!(warn && line.contains("names none") && line.contains("RTX 4060 Laptop GPU") && line.contains("default choice"), "{line}");
        // several: dedicated before built-in although the built-in card comes first in wgpu's order
        let g = choose_named_adapter("g", &laptop);
        assert_eq!((g.index, g.matches.clone()), (Some(1), vec![1, 0]));
        let (warn, line) = named_choice_note("g", &g, &laptop);
        assert!(!warn && line.contains("matches 2") && line.contains("using NVIDIA GeForce RTX 4060 Laptop GPU"), "{line}");
        // one: says which
        let one = choose_named_adapter("iris", &laptop);
        assert_eq!(one.index, Some(0));
        assert_eq!(
            named_choice_note("iris", &one, &laptop),
            (false, "gpu: LIGHTCRAFT_GPU_ADAPTER=iris: using Intel(R) Iris(R) Xe Graphics".to_string())
        );
        // a software renderer: never, and the warning says so
        let warp = choose_named_adapter("basic render", &laptop);
        assert_eq!((warp.index, warp.software.clone()), (None, vec![2]));
        let (warn, line) = named_choice_note("basic render", &warp, &laptop);
        assert!(warn && line.contains("only a software renderer"), "{line}");
        // a card's whole name beats a longer name that contains it, wherever it is listed
        let twins = [
            Candidate::of("NVIDIA GeForce RTX 4070 SUPER", DiscreteGpu),
            Candidate::of("NVIDIA GeForce RTX 4070", DiscreteGpu),
            Candidate::of("Virtual RTX 4070", VirtualGpu),
        ];
        assert_eq!(choose_named_adapter("nvidia geforce rtx 4070", &twins).matches, vec![1, 0]);
        // same kind: wgpu's order; virtual after dedicated
        assert_eq!(choose_named_adapter("4070", &twins).matches, vec![0, 1, 2]);
        // the same card on two backends (Vulkan and GL on Linux): the first listed
        let both = [Candidate::of("AMD Radeon RX 7800 XT", DiscreteGpu), Candidate::of("AMD Radeon RX 7800 XT", DiscreteGpu)];
        assert_eq!(choose_named_adapter("7800", &both).index, Some(0));
        assert_eq!(choose_named_adapter("7800", &[]), NamedChoice::default());
    }

    /// The render device, the denoise network's and the face detector's each ask for a card: the
    /// line about it is logged once, not three times.
    #[test]
    fn a_choice_is_logged_once() {
        let line = "gpu: LIGHTCRAFT_GPU_ADAPTER=test-only-name names none of the graphics cards (); using the default choice";
        assert!(log_once(true, line));
        assert!(!log_once(true, line));
        assert!(!log_once(false, line));
        assert!(log_once(true, &format!("{line} (another)")));
    }

    #[test]
    fn resolves_against_the_build() {
        let compiled = Backends::DX12 | Backends::VULKAN;
        let def = Backends::DX12;
        assert_eq!(resolve(BackendChoice::Auto, def, compiled), Some(def));
        assert_eq!(resolve(BackendChoice::Off, def, compiled), None);
        assert_eq!(resolve(BackendChoice::Use(Backends::VULKAN), def, compiled), Some(Backends::VULKAN));
        assert_eq!(resolve(BackendChoice::Use(Backends::VULKAN | Backends::GL), def, compiled), Some(Backends::VULKAN));
        // GL isn't compiled in: the default, not an empty set (no adapter at all)
        assert_eq!(resolve(BackendChoice::Use(Backends::GL), def, compiled), Some(def));
    }

    #[test]
    fn init_marker_is_written_during_creation_and_removed_after() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-marker-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        // a local scope list: other tests create real devices through the process-wide one
        let active = Mutex::new(Scopes::new());
        let seen = with_marker_on(Some(m.clone()), &active, "device creation", || m.exists());
        assert!(seen, "the marker exists while the device is created");
        assert!(!m.exists(), "and is removed afterwards");
        // a marker left behind by a crashed process is reported once; reading it leaves it in place
        std::fs::write(&m, "GPU device and kernel creation started (backends DX12)").unwrap();
        assert!(read_init_marker(&m).unwrap().contains("DX12"));
        assert!(m.exists(), "read_init_marker leaves the marker for the next launch");
        assert!(take_init_marker(&m).unwrap().contains("DX12"));
        assert_eq!(take_init_marker(&m), None);
        assert_eq!(read_init_marker(&m), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn with_init_marker_writes_the_process_wide_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-global-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        set_init_marker(Some(m.clone()));
        let seen = with_init_marker(Backends::DX12, || m.exists());
        set_init_marker(None);
        // not checked for removal: device-creating tests running in parallel may still hold the file
        // (the test above checks removal on its own scope counter)
        assert!(seen, "the marker set with set_init_marker exists while the device is created");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The window's renderer fallback: forced, remembered, the platform chain, the failed one last.
    #[test]
    fn window_renderer_fallback_order() {
        use WindowRenderer::{Glow, Wgpu};
        let all = Backends::DX12 | Backends::VULKAN | Backends::GL | Backends::METAL;
        let (dx, vk, gl) = (Wgpu(Backends::DX12), Wgpu(Backends::VULKAN), Wgpu(Backends::GL));
        assert_eq!(window_renderer_order(None, None, None, "windows", all, true), [dx, vk, gl, Glow]);
        assert_eq!(window_renderer_order(None, None, None, "windows", all, false), [dx, vk, gl]);
        // a build without GL
        assert_eq!(window_renderer_order(None, None, None, "windows", Backends::DX12 | Backends::VULKAN, true), [dx, vk, Glow]);
        // remembered from the last good start goes first
        assert_eq!(window_renderer_order(None, Some(vk), None, "windows", all, true), [vk, dx, gl, Glow]);
        // the environment wins over the remembered one
        assert_eq!(window_renderer_order(Some(gl), Some(vk), None, "windows", all, true), [gl, vk, dx, Glow]);
        assert_eq!(window_renderer_order(Some(Glow), None, None, "windows", all, true)[0], Glow);
        // the last start died in DX12: tried last only (also when it was remembered)
        assert_eq!(window_renderer_order(None, Some(dx), Some(dx), "windows", all, true), [vk, gl, Glow, dx]);
        // a forced backend that is not in the build is skipped
        assert_eq!(window_renderer_order(Some(Wgpu(Backends::METAL)), None, None, "windows", Backends::DX12, false), [dx]);
        // never empty
        assert!(!window_renderer_order(None, None, None, "windows", Backends::empty(), false).is_empty());
        assert_eq!(window_renderer_order(None, None, None, "macos", all, true), [Wgpu(Backends::METAL), Glow]);
    }

    #[test]
    fn window_renderer_names_round_trip() {
        for n in ["dx12", "vulkan", "metal", "gl", "glow"] {
            assert_eq!(WindowRenderer::parse(n).map(WindowRenderer::name), Some(n));
        }
        assert_eq!(WindowRenderer::parse(" DX12 "), Some(WindowRenderer::Wgpu(Backends::DX12)));
        for n in ["auto", "off", "vulkan,gl", "", "bogus"] {
            assert_eq!(WindowRenderer::parse(n), None, "{n}");
        }
        assert_eq!(forced_window_renderer(Some("glow"), Some("dx12")), Some(WindowRenderer::Glow));
        assert_eq!(forced_window_renderer(Some("auto"), Some("vulkan")), Some(WindowRenderer::Wgpu(Backends::VULKAN)));
        assert_eq!(forced_window_renderer(None, None), None);
        // `glow` only concerns the window: compute keeps its default, without a warning
        assert_eq!(parse_backends("glow"), Some(BackendChoice::Auto));
    }

    #[test]
    fn overlapping_scopes_share_one_marker_until_the_last_ends() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-scopes-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let active = Mutex::new(Scopes::new());
        let a = arm(&m, &active, "device creation");
        assert!(read_init_marker(&m).unwrap().contains("device creation"));
        // a second scope (a denoise network's kernels) joins the file: both are named
        let b = arm(&m, &active, "denoise kernel compilation");
        let text = read_init_marker(&m).unwrap();
        assert!(text.contains("device creation") && text.contains("denoise kernel compilation"), "{text}");
        disarm(&m, &active, a);
        assert!(m.exists(), "the second scope is still running");
        let text = read_init_marker(&m).unwrap();
        assert!(!text.contains("device creation") && text.contains("denoise"), "only what still runs: {text}");
        disarm(&m, &active, b);
        assert!(!m.exists(), "the last scope removes it");
        // a stray extra disarm does not break the next scope
        disarm(&m, &active, b);
        let c = arm(&m, &active, "kernel compilation");
        assert!(m.exists());
        disarm(&m, &active, c);
        assert!(!m.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The marker of a device of a chosen card names the card and its role, so a crash there
    /// costs the next launch that choice only.
    #[test]
    fn the_marker_names_the_card_being_started() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-card-marker-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let active = Mutex::new(Scopes::new());
        let key = crate::cards::AdapterKey::parse("dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1").unwrap();
        let a = arm(&m, &active, &format!("device and kernel creation (backends DX12) [card={key}; role=helper]"));
        let b = arm(&m, &active, "adapter listing");
        let text = read_init_marker(&m).unwrap();
        let scopes = marker_scopes(&text);
        assert_eq!(scopes.len(), 2, "{text}");
        assert_eq!(scopes[0].card, Some((key, "helper".to_string())));
        assert_eq!(scopes[1].card, None);
        disarm(&m, &active, a);
        disarm(&m, &active, b);
        // older markers (one line, no card) and junk
        assert_eq!(marker_scopes("GPU device and kernel creation (backends DX12) started (unix time 1)"), vec![MarkerScope { card: None }]);
        assert_eq!(marker_scopes("x [card=not a key; role=editing] y"), vec![MarkerScope { card: None }]);
        assert!(marker_scopes("").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panic_inside_the_scope_still_removes_the_marker() {
        let dir = std::env::temp_dir().join(format!("lc-gpu-panic-{}", std::process::id()));
        let m = dir.join("gpu-init.marker");
        let _ = std::fs::remove_dir_all(&dir);
        let active = Mutex::new(Scopes::new());
        let r = std::panic::catch_unwind(|| with_marker_on(Some(m.clone()), &active, "kernel compilation", || panic!("compile")));
        assert!(r.is_err());
        assert!(!m.exists(), "the process survived, so nothing is left to report");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
