//! The graphics cards LightCraft can render on, and which one renders what (Settings ▸ Performance
//! ▸ Graphics card for editing / Second graphics card).
//!
//! A card is named by an [`AdapterKey`], saved in the settings as text
//! (`dx12:10de:2f04:NVIDIA GeForce RTX 5070#1`): the backend, PCI vendor and device ids, the name
//! the driver gives it, and `#n` to tell identical cards apart (wgpu's `device_pci_bus_id` isn't
//! unique on DX12). Everything here is plain data and rules, without a GPU: listing the cards
//! ([`entries_from`]), choosing the editing card ([`resolve_card`]) and the helper
//! ([`resolve_helper`]). `backend.rs` applies them to wgpu's adapters.
//!
//! The editing card, in order: `LIGHTCRAFT_GPU_ADAPTER` (part of a name, for tests and diagnosis),
//! the card saved in the settings, the card that drives the screen, wgpu's high-performance pick. A
//! saved card that isn't there (taken out, driver gone) falls back to Automatic for this session
//! and the choice is kept (decision 4, 31 Oct 2026).

use std::fmt;
use std::sync::Mutex;

/// A graphics card's identity (see the module doc). Parsed leniently: text that isn't a key is no
/// key (the setting then means Automatic).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AdapterKey {
    /// wgpu's backend in lower case (`dx12`, `vulkan`, `metal`, `gl`).
    pub backend: String,
    /// PCI vendor id (`10de` NVIDIA, `1002` AMD, `8086` Intel).
    pub vendor: u32,
    /// PCI device id.
    pub device: u32,
    /// The driver's name for it.
    pub name: String,
    /// 1 for the first card of this model in wgpu's order, 2 for an identical second one, …
    pub index: u32,
}

impl AdapterKey {
    /// Parse `backend:vendor:device:name#n` (hex ids). `None` for anything else (an empty
    /// setting, `auto`, a name alone).
    pub fn parse(s: &str) -> Option<AdapterKey> {
        let s = s.trim();
        let (backend, rest) = s.split_once(':')?;
        let (vendor, rest) = rest.split_once(':')?;
        let (device, rest) = rest.split_once(':')?;
        let (name, index) = match rest.rsplit_once('#') {
            Some((n, i)) => (n, i.trim().parse::<u32>().ok().filter(|i| *i >= 1)?),
            None => (rest, 1),
        };
        let backend = backend.trim().to_ascii_lowercase();
        let name = name.trim();
        if backend.is_empty() || name.is_empty() || !backend.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        Some(AdapterKey {
            backend,
            vendor: u32::from_str_radix(vendor.trim(), 16).ok()?,
            device: u32::from_str_radix(device.trim(), 16).ok()?,
            name: name.to_string(),
            index,
        })
    }

    /// The key of the `index`-th card (1-based) of this model.
    pub fn of(info: &wgpu::AdapterInfo, index: u32) -> AdapterKey {
        AdapterKey {
            backend: backend_name(info.backend),
            vendor: info.vendor,
            device: info.device,
            name: info.name.trim().to_string(),
            index: index.max(1),
        }
    }

    /// Same model as `other` (backend, ids and name), whichever of identical cards it is.
    pub fn same_model(&self, other: &AdapterKey) -> bool {
        self.backend == other.backend && self.vendor == other.vendor && self.device == other.device && self.name == other.name
    }
}

impl fmt::Display for AdapterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{:04x}:{:04x}:{}#{}", self.backend, self.vendor, self.device, self.name, self.index)
    }
}

/// wgpu's backend name in lower case (`dx12`, `vulkan`, `metal`, `gl`).
pub fn backend_name(b: wgpu::Backend) -> String {
    format!("{b:?}").to_ascii_lowercase()
}

/// What kind of card it is, in the words the settings use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterKind {
    /// A graphics card of its own (with its own memory).
    Dedicated,
    /// Graphics built into the processor (shares the computer's memory).
    BuiltIn,
    /// A virtual machine's or a remote session's adapter.
    Virtual,
    /// A software renderer (WARP, llvmpipe): never used, the CPU pipeline is faster.
    Software,
    Other,
}

impl AdapterKind {
    pub fn of(t: wgpu::DeviceType) -> AdapterKind {
        match t {
            wgpu::DeviceType::DiscreteGpu => AdapterKind::Dedicated,
            wgpu::DeviceType::IntegratedGpu => AdapterKind::BuiltIn,
            wgpu::DeviceType::VirtualGpu => AdapterKind::Virtual,
            wgpu::DeviceType::Cpu => AdapterKind::Software,
            wgpu::DeviceType::Other => AdapterKind::Other,
        }
    }

    /// The order among several cards a name matches ([`crate::backend::kind_rank`]).
    pub fn rank(self) -> u8 {
        match self {
            AdapterKind::Dedicated => 0,
            AdapterKind::BuiltIn => 1,
            AdapterKind::Virtual => 2,
            AdapterKind::Other => 3,
            AdapterKind::Software => 4,
        }
    }

    /// `dedicated`, `builtIn`, `virtual`, `software`, `other` (the `app.gpu` reply).
    pub fn id(self) -> &'static str {
        match self {
            AdapterKind::Dedicated => "dedicated",
            AdapterKind::BuiltIn => "builtIn",
            AdapterKind::Virtual => "virtual",
            AdapterKind::Software => "software",
            AdapterKind::Other => "other",
        }
    }
}

/// One graphics card as the settings list it ([`crate::adapters`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterEntry {
    pub key: AdapterKey,
    pub name: String,
    pub kind: AdapterKind,
    /// Its own memory (bytes), when the system says.
    pub memory: Option<u64>,
    /// wgpu's backend (`dx12` …).
    pub backend: String,
    /// The window is drawn on it (the card the screen is connected to).
    pub drives_screen: bool,
    /// LightCraft can render on it; else why not ([`AdapterEntry::why_not`]).
    pub usable: bool,
    pub why_not: Option<String>,
}

/// Fewest storage buffers per shader stage the render kernels need.
pub const MIN_STORAGE_BUFFERS: u32 = 10;

/// The window's card, as its adapter describes it (any backend: the window may draw with Vulkan
/// while compute uses DX12, so only the ids and the name are compared).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenCard {
    pub vendor: u32,
    pub device: u32,
    pub name: String,
}

impl ScreenCard {
    pub fn of(info: &wgpu::AdapterInfo) -> ScreenCard {
        ScreenCard { vendor: info.vendor, device: info.device, name: info.name.trim().to_string() }
    }

    fn is(&self, key: &AdapterKey) -> bool {
        self.vendor == key.vendor && self.device == key.device && self.name == key.name
    }
}

/// The settings' list from wgpu's adapters, in its order: (adapter info, its storage buffers per
/// shader stage, its memory). Identical cards get `#1`, `#2` …; the screen's card is the first of
/// its model.
pub fn entries_from(adapters: &[(wgpu::AdapterInfo, u32, Option<u64>)], screen: Option<&ScreenCard>) -> Vec<AdapterEntry> {
    let mut out: Vec<AdapterEntry> = Vec::with_capacity(adapters.len());
    let mut screen_taken = false;
    for (info, storage, memory) in adapters {
        let n = out.iter().filter(|e| e.key.same_model(&AdapterKey::of(info, 1))).count();
        let key = AdapterKey::of(info, u32::try_from(n).unwrap_or(u32::MAX).saturating_add(1));
        let kind = AdapterKind::of(info.device_type);
        let why_not = if kind == AdapterKind::Software {
            Some("a software renderer: the processor is faster".to_string())
        } else if *storage < MIN_STORAGE_BUFFERS {
            Some(format!("too few storage buffers per shader stage ({storage} < {MIN_STORAGE_BUFFERS})"))
        } else {
            None
        };
        let drives_screen = !screen_taken && screen.is_some_and(|s| s.is(&key));
        screen_taken |= drives_screen;
        out.push(AdapterEntry {
            name: key.name.clone(),
            key,
            kind,
            memory: *memory,
            backend: backend_name(info.backend),
            drives_screen,
            usable: why_not.is_none(),
            why_not,
        });
    }
    out
}

/// How the editing card was chosen ([`resolve_card`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How {
    /// `LIGHTCRAFT_GPU_ADAPTER` names it.
    Variable,
    /// Chosen in the settings.
    Chosen,
    /// Automatic: the card that drives the screen.
    Screen,
    /// Automatic without a screen's card known (or usable): wgpu's high-performance pick.
    Default,
}

/// The editing card: which entry (`None`: let wgpu pick the high-performance one), how, and the
/// saved card when it isn't there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub index: Option<usize>,
    pub how: How,
    pub missing: Option<AdapterKey>,
}

/// Which of `entries` edits: `variable` (`LIGHTCRAFT_GPU_ADAPTER`, part of a name, any case), else
/// the `saved` card (exactly, else the same model), else the screen's, else wgpu's pick. Only
/// usable cards are picked; among several the variable matches, in the order of
/// [`crate::backend::choose_named_adapter`], and the choice is logged once.
pub fn resolve_card(entries: &[AdapterEntry], variable: Option<&str>, saved: Option<&AdapterKey>) -> Resolution {
    let Some(w) = variable.map(str::trim).filter(|w| !w.is_empty()) else { return resolve_unnamed(entries, saved) };
    let (warn, line, choice) = named_note(entries, w);
    if let Some(i) = choice.index {
        crate::backend::log_once(warn, &line);
        return Resolution { index: Some(i), how: How::Variable, missing: None };
    }
    // (the line names what is used instead: the saved card, the screen's, or wgpu's pick)
    let r = resolve_unnamed(entries, saved);
    crate::backend::log_once(warn, &fallback_line(&line, &r, entries));
    r
}

/// The key of the card [`resolve_card`] edits on with `saved` as the setting (a key in any case of
/// backend and ids, or Automatic): for the settings, which name the second card's choices by it
/// (review of PR #3506). `None`: wgpu picks.
pub fn editing_key(entries: &[AdapterEntry], variable: Option<&str>, saved: &str) -> Option<AdapterKey> {
    let saved = AdapterKey::parse(saved);
    resolve_card(entries, variable, saved.as_ref()).index.and_then(|i| entries.get(i)).map(|e| e.key.clone())
}

/// [`resolve_card`] without the variable: the saved card, else the screen's, else wgpu's pick.
fn resolve_unnamed(entries: &[AdapterEntry], saved: Option<&AdapterKey>) -> Resolution {
    let mut missing = None;
    if let Some(k) = saved {
        let found = entries.iter().position(|e| e.usable && e.key == *k).or_else(|| entries.iter().position(|e| e.usable && e.key.same_model(k)));
        if let Some(i) = found {
            return Resolution { index: Some(i), how: How::Chosen, missing };
        }
        missing = Some(k.clone());
    }
    match entries.iter().position(|e| e.usable && e.drives_screen) {
        Some(i) => Resolution { index: Some(i), how: How::Screen, missing },
        None => Resolution { index: None, how: How::Default, missing },
    }
}

/// `line` (a name that picked no card, "…; using the default choice") ending with what `r` uses.
fn fallback_line(line: &str, r: &Resolution, entries: &[AdapterEntry]) -> String {
    let head = line.strip_suffix("; using the default choice").unwrap_or(line);
    let name = r.index.and_then(|i| entries.get(i)).map_or("?", |e| e.name.as_str());
    match r.how {
        How::Chosen => format!("{head}; using the card chosen in Settings ({name})"),
        How::Screen => format!("{head}; using the screen's card ({name})"),
        How::Variable | How::Default => format!("{head}; using the default choice"),
    }
}

/// What the name `w` picks among `entries` ([`named_choice`]), and the line that says so: a name
/// that matches only cards LightCraft can't use names them and why (review of PR #3511), rather than
/// saying it names none.
fn named_note(entries: &[AdapterEntry], w: &str) -> (bool, String, crate::backend::NamedChoice) {
    let (choice, candidates) = named_choice(entries, w);
    let rejected: Vec<String> = crate::backend::choose_named_adapter(w, &candidates)
        .matches
        .iter()
        .filter_map(|i| entries.get(*i))
        .filter(|e| !e.usable)
        .map(|e| format!("{} ({})", e.name, e.why_not.as_deref().unwrap_or("not usable")))
        .collect();
    let (warn, line) = if choice.index.is_none() && choice.software.is_empty() && !rejected.is_empty() {
        (
            true,
            format!(
                "gpu: LIGHTCRAFT_GPU_ADAPTER={w} names only graphics cards LightCraft can't use: {}; using the default choice",
                rejected.join(", ")
            ),
        )
    } else {
        crate::backend::named_choice_note(w, &choice, &candidates)
    };
    (warn, line, choice)
}

/// What the name `w` picks among `entries` ([`crate::backend::choose_named_adapter`]), leaving out
/// hardware cards that can't be used (too few storage buffers).
fn named_choice<'a>(entries: &'a [AdapterEntry], w: &str) -> (crate::backend::NamedChoice, Vec<crate::backend::Candidate<'a>>) {
    let candidates: Vec<crate::backend::Candidate<'a>> =
        entries.iter().map(|e| crate::backend::Candidate { name: &e.name, rank: e.kind.rank(), software: e.kind == AdapterKind::Software }).collect();
    let mut choice = crate::backend::choose_named_adapter(w, &candidates);
    choice.matches.retain(|i| entries.get(*i).is_some_and(|e| e.usable));
    choice.index = choice.matches.first().copied();
    (choice, candidates)
}

/// What the settings say about the second graphics card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HelperChoice {
    /// Automatic: another usable card than the editing one, dedicated first.
    Auto,
    /// Don't use one: the default, until the user turns a second card on (review of #3506).
    #[default]
    Off,
    /// This card (not when it is the editing card).
    Card(AdapterKey),
    /// A second device on the editing card itself (`LIGHTCRAFT_GPU_HELPER=same`: tests of the two-
    /// device logic on a one-card machine, benchmarks).
    Same,
    /// The card whose name contains this (`LIGHTCRAFT_GPU_HELPER=4070`).
    Named(String),
}

impl HelperChoice {
    /// The setting's text: `""` Automatic, `off`, or a card's key. Unknown text is Automatic.
    pub fn parse_setting(s: &str) -> HelperChoice {
        let t = s.trim();
        match t.to_ascii_lowercase().as_str() {
            "" | "auto" | "automatic" => HelperChoice::Auto,
            "off" | "none" | "no" | "0" | "false" => HelperChoice::Off,
            _ => AdapterKey::parse(t).map_or(HelperChoice::Auto, HelperChoice::Card),
        }
    }

    /// `LIGHTCRAFT_GPU_HELPER`: `off`, `same`, `auto`, a key, or part of a card's name.
    pub fn parse_variable(s: &str) -> Option<HelperChoice> {
        let t = s.trim();
        if t.is_empty() {
            return None;
        }
        Some(match t.to_ascii_lowercase().as_str() {
            "auto" | "automatic" => HelperChoice::Auto,
            "off" | "none" | "no" | "0" | "false" => HelperChoice::Off,
            "same" => HelperChoice::Same,
            _ => AdapterKey::parse(t).map_or_else(|| HelperChoice::Named(t.to_string()), HelperChoice::Card),
        })
    }

    /// The setting's text for this choice (`Named` is the variable's only: Automatic).
    pub fn setting(&self) -> String {
        match self {
            HelperChoice::Off => "off".into(),
            HelperChoice::Card(k) => k.to_string(),
            HelperChoice::Same => "same".into(),
            _ => String::new(),
        }
    }
}

/// Which of `entries` helps (`None`: no helper), the editing card being entry `primary`.
/// `variable_pins` — `LIGHTCRAFT_GPU_ADAPTER` is set: work stays on that one card, so Automatic
/// means no helper (agents test on one card and must leave the other alone); a helper named
/// outright still counts.
pub fn resolve_helper(entries: &[AdapterEntry], primary: Option<usize>, choice: &HelperChoice, variable_pins: bool) -> Option<usize> {
    let other = |i: &usize| Some(*i) != primary;
    match choice {
        HelperChoice::Off => None,
        HelperChoice::Same => primary.filter(|i| entries.get(*i).is_some_and(|e| e.usable)),
        // the exact card (never the editing card), else an identical one that isn't the editing
        // card (review of PR #3511: the first identical card may be the editing card)
        HelperChoice::Card(k) => match entries.iter().position(|e| e.usable && e.key == *k) {
            Some(i) => Some(i).filter(other),
            None => (0..entries.len()).filter(other).find(|i| entries.get(*i).is_some_and(|e| e.usable && e.key.same_model(k))),
        },
        HelperChoice::Named(n) => {
            (0..entries.len()).filter(other).find(|i| entries.get(*i).is_some_and(|e| e.usable && crate::backend::adapter_name_matches(n, &e.name)))
        }
        HelperChoice::Auto if variable_pins => None,
        HelperChoice::Auto => {
            let candidates = || (0..entries.len()).filter(other).filter(|i| entries.get(*i).is_some_and(|e| e.usable));
            candidates()
                .find(|i| entries.get(*i).is_some_and(|e| e.kind == AdapterKind::Dedicated))
                .or_else(|| candidates().find(|i| entries.get(*i).is_some_and(|e| e.kind != AdapterKind::Virtual)))
        }
    }
}

// ----------------------------------------------------------------------------------- the state

/// The choices in force and what is known about the cards (process-wide).
struct State {
    /// Graphics card for editing (`None`: Automatic).
    card: Option<AdapterKey>,
    helper: HelperChoice,
    /// The card the window draws on, once the app told ([`set_screen_card`]).
    screen: Option<ScreenCard>,
    /// The last listing ([`crate::list_adapters`]).
    listed: Option<Vec<AdapterEntry>>,
    /// Saved cards already reported missing in this session (once each).
    told_missing: Vec<AdapterKey>,
}

static STATE: Mutex<State> = Mutex::new(State { card: None, helper: HelperChoice::Off, screen: None, listed: None, told_missing: Vec::new() });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The saved editing card (`None`: Automatic).
pub(crate) fn card() -> Option<AdapterKey> {
    state().card.clone()
}

/// Set the editing card; `true` when it changed.
pub(crate) fn set_card(key: Option<AdapterKey>) -> bool {
    let mut s = state();
    let changed = s.card != key;
    s.card = key;
    changed
}

/// The helper as the settings say, before `LIGHTCRAFT_GPU_HELPER`.
pub(crate) fn helper_setting() -> HelperChoice {
    state().helper.clone()
}

/// Set the helper's choice; `true` when it changed.
pub(crate) fn set_helper(choice: HelperChoice) -> bool {
    let mut s = state();
    let changed = s.helper != choice;
    s.helper = choice;
    changed
}

/// The helper in force: `LIGHTCRAFT_GPU_HELPER` when set, else the settings'.
pub(crate) fn helper_choice() -> HelperChoice {
    helper_variable().unwrap_or_else(helper_setting)
}

/// `LIGHTCRAFT_GPU_HELPER`, parsed.
pub(crate) fn helper_variable() -> Option<HelperChoice> {
    std::env::var("LIGHTCRAFT_GPU_HELPER").ok().as_deref().and_then(HelperChoice::parse_variable)
}

pub(crate) fn screen() -> Option<ScreenCard> {
    state().screen.clone()
}

pub(crate) fn set_screen(card: ScreenCard) {
    let mut s = state();
    s.screen = Some(card.clone());
    // the listing marks the screen's card
    if let Some(l) = s.listed.as_mut() {
        let mut taken = false;
        for e in l.iter_mut() {
            e.drives_screen = !taken && card.is(&e.key);
            taken |= e.drives_screen;
        }
    }
}

pub(crate) fn listed() -> Option<Vec<AdapterEntry>> {
    state().listed.clone()
}

pub(crate) fn set_listed(entries: Vec<AdapterEntry>) {
    state().listed = Some(entries);
}

/// `true` the first time `key` is reported missing in this session.
pub(crate) fn first_report_of_missing(key: &AdapterKey) -> bool {
    let mut s = state();
    if s.told_missing.contains(key) {
        return false;
    }
    s.told_missing.push(key.clone());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, vendor: u32, device: u32, t: wgpu::DeviceType) -> wgpu::AdapterInfo {
        wgpu::AdapterInfo { name: name.into(), vendor, device, ..wgpu::AdapterInfo::new(t, wgpu::Backend::Dx12) }
    }

    /// The user's machine: the screen's card first in wgpu's order, the helper, Windows' software
    /// renderer.
    fn two_cards() -> Vec<(wgpu::AdapterInfo, u32, Option<u64>)> {
        use wgpu::DeviceType::{Cpu, DiscreteGpu};
        vec![
            (info("NVIDIA GeForce RTX 5070", 0x10de, 0x2f04, DiscreteGpu), 64, Some(12 << 30)),
            (info("NVIDIA GeForce RTX 4070 SUPER", 0x10de, 0x2783, DiscreteGpu), 64, Some(12 << 30)),
            (info("Microsoft Basic Render Driver", 0x1414, 0x8c, Cpu), 64, None),
        ]
    }

    fn screen_5070() -> ScreenCard {
        ScreenCard { vendor: 0x10de, device: 0x2f04, name: "NVIDIA GeForce RTX 5070".into() }
    }

    #[test]
    fn keys_round_trip_and_parse_leniently() {
        // (review of #3506) the second card is off until the user turns it on; an older settings
        // file's "" still means Automatic
        assert_eq!(HelperChoice::default(), HelperChoice::Off);
        assert_eq!(HelperChoice::parse_setting(""), HelperChoice::Auto);
        assert_eq!(HelperChoice::parse_setting("off"), HelperChoice::Off);
        let k = AdapterKey { backend: "dx12".into(), vendor: 0x10de, device: 0x2f04, name: "NVIDIA GeForce RTX 5070".into(), index: 1 };
        assert_eq!(k.to_string(), "dx12:10de:2f04:NVIDIA GeForce RTX 5070#1");
        assert_eq!(AdapterKey::parse(&k.to_string()), Some(k.clone()));
        // without the index: the first card of the model; spaces and case of the backend don't matter
        assert_eq!(AdapterKey::parse(" DX12:10DE:2F04:NVIDIA GeForce RTX 5070 "), Some(k.clone()));
        // a name with colons and a hash in it
        let odd = AdapterKey { backend: "vulkan".into(), vendor: 0x1002, device: 0x73bf, name: "AMD Radeon: RX #6800".into(), index: 2 };
        assert_eq!(AdapterKey::parse(&odd.to_string()), Some(odd));
        for bad in [
            "",
            "auto",
            "NVIDIA GeForce RTX 5070",
            "dx12:zz:2f04:x#1",
            "dx12:10de:2f04:#1",
            "dx12:10de:2f04:x#0",
            "dx12:10de:2f04:x#-1",
            "d x:1:2:x",
            ":1:2:x",
        ] {
            assert_eq!(AdapterKey::parse(bad), None, "{bad:?}");
        }
        // ids past 16 bits (some drivers) and absurd ones
        assert!(AdapterKey::parse("metal:0:0:Apple M4 Pro#1").is_some());
        assert_eq!(AdapterKey::parse("dx12:fffffffff:0:x#1"), None, "out of range");
    }

    #[test]
    fn identical_cards_are_numbered_and_the_screen_is_marked() {
        use wgpu::DeviceType::DiscreteGpu;
        let twins = [
            (info("NVIDIA GeForce RTX 4090", 0x10de, 0x2684, DiscreteGpu), 64, None),
            (info("NVIDIA GeForce RTX 4090", 0x10de, 0x2684, DiscreteGpu), 64, None),
        ];
        let screen = ScreenCard { vendor: 0x10de, device: 0x2684, name: "NVIDIA GeForce RTX 4090".into() };
        let e = entries_from(&twins, Some(&screen));
        assert_eq!(e[0].key.index, 1);
        assert_eq!(e[1].key.index, 2);
        assert!(e[0].drives_screen && !e[1].drives_screen, "the first of identical cards drives the screen");
        assert_ne!(e[0].key, e[1].key);
        assert!(e[0].key.same_model(&e[1].key));

        let e = entries_from(&two_cards(), Some(&screen_5070()));
        assert_eq!(e.len(), 3);
        assert!(e[0].drives_screen && !e[1].drives_screen && !e[2].drives_screen);
        assert!(e[0].usable && e[1].usable);
        assert!(!e[2].usable && e[2].kind == AdapterKind::Software && e[2].why_not.is_some());
        // a card with too few storage buffers is listed but not usable
        let weak = [(info("Old Card", 1, 2, DiscreteGpu), 8, None)];
        let e = entries_from(&weak, None);
        assert!(!e[0].usable && e[0].why_not.as_deref().is_some_and(|w| w.contains("storage buffers")));
    }

    #[test]
    fn the_editing_card_follows_the_variable_then_the_setting_then_the_screen() {
        let e = entries_from(&two_cards(), Some(&screen_5070()));
        let k4070 = e[1].key.clone();
        // nothing chosen: the screen's card
        assert_eq!(resolve_card(&e, None, None), Resolution { index: Some(0), how: How::Screen, missing: None });
        // chosen in the settings
        assert_eq!(resolve_card(&e, None, Some(&k4070)), Resolution { index: Some(1), how: How::Chosen, missing: None });
        // the same model under another index (a card moved to another slot) still counts
        let moved = AdapterKey { index: 2, ..k4070.clone() };
        assert_eq!(resolve_card(&e, None, Some(&moved)).index, Some(1));
        // the variable wins over the setting
        assert_eq!(resolve_card(&e, Some("5070"), Some(&k4070)), Resolution { index: Some(0), how: How::Variable, missing: None });
        // a variable naming no card is ignored
        assert_eq!(resolve_card(&e, Some("radeon"), Some(&k4070)).index, Some(1));
        // the software renderer is never picked, by name or by key
        assert_eq!(resolve_card(&e, Some("basic render"), None).how, How::Screen);
        assert_eq!(resolve_card(&e, None, Some(&e[2].key)).missing, Some(e[2].key.clone()));
        // a name both cards have: the first in wgpu's order (both dedicated)
        assert_eq!(resolve_card(&e, Some("070"), None), Resolution { index: Some(0), how: How::Variable, missing: None });
    }

    /// Review of PR #3506: the settings learn the editing card as it is picked: the variable over
    /// the setting, a key written in another case, the same model under another number.
    #[test]
    fn the_settings_learn_the_editing_card_as_it_is_picked() {
        let e = entries_from(&two_cards(), Some(&screen_5070()));
        let (k5070, k4070) = (e[0].key.clone(), e[1].key.clone());
        assert_eq!(editing_key(&e, None, ""), Some(k5070.clone()), "Automatic: the screen's");
        assert_eq!(editing_key(&e, None, &k4070.to_string()), Some(k4070.clone()));
        let shouted = format!("DX12:10DE:2783:{}#1", k4070.name);
        assert_eq!(editing_key(&e, None, &shouted), Some(k4070.clone()), "a key in upper case");
        assert_eq!(editing_key(&e, None, &format!("dx12:10de:2783:{}#2", k4070.name)), Some(k4070.clone()), "same model");
        assert_eq!(editing_key(&e, Some("4070"), &k5070.to_string()), Some(k4070.clone()), "the variable wins");
        assert_eq!(editing_key(&e, Some("5070"), &k4070.to_string()), Some(k5070));
        // no screen's card known and nothing chosen: wgpu picks, no key (review of PR #3506)
        assert_eq!(editing_key(&entries_from(&two_cards(), None), None, ""), None);
    }

    /// A fake laptop list: the variable picks among several matches in the defined order (dedicated
    /// before built-in, whatever wgpu lists first), and never a card that can't be used.
    #[test]
    fn the_variable_picks_among_several_cards_in_a_defined_order() {
        use wgpu::DeviceType::{DiscreteGpu, IntegratedGpu};
        let laptop = vec![
            (info("Intel(R) Iris(R) Xe Graphics", 0x8086, 0x9a49, IntegratedGpu), 64, None),
            (info("NVIDIA GeForce RTX 4060 Laptop GPU", 0x10de, 0x28e0, DiscreteGpu), 64, Some(8 << 30)),
            (info("NVIDIA Old Graphics", 0x10de, 0x0001, DiscreteGpu), 4, None),
        ];
        let e = entries_from(&laptop, None);
        // "graphics" is in all three: the old card can't be used, so the built-in one
        assert_eq!(resolve_card(&e, Some("graphics"), None).index, Some(0));
        // "g": dedicated before built-in
        assert_eq!(resolve_card(&e, Some("g"), None).index, Some(1));
        // only the unusable card: the default choice
        assert_eq!(resolve_card(&e, Some("old"), None), Resolution { index: None, how: How::Default, missing: None });
        // and the log says why, naming that card (not "names none of the graphics cards")
        let (warn, line, _) = named_note(&e, "old");
        assert!(warn && line.contains("NVIDIA Old Graphics (") && line.contains("storage buffers") && !line.contains("names none"), "{line}");
        let (_, line, _) = named_note(&e, "radeon");
        assert!(line.contains("names none of the graphics cards"), "{line}");
        // the log names what is used instead of a name that picked nothing
        let line = "gpu: LIGHTCRAFT_GPU_ADAPTER=radeon names none of the graphics cards (A, B); using the default choice";
        let two = entries_from(&two_cards(), Some(&screen_5070()));
        let saved = resolve_card(&two, Some("radeon"), Some(&two[1].key));
        assert_eq!(
            fallback_line(line, &saved, &two),
            "gpu: LIGHTCRAFT_GPU_ADAPTER=radeon names none of the graphics cards (A, B); using the card chosen in Settings (NVIDIA GeForce RTX 4070 SUPER)"
        );
        let screen = resolve_card(&two, Some("radeon"), None);
        assert!(fallback_line(line, &screen, &two).ends_with("using the screen's card (NVIDIA GeForce RTX 5070)"));
        let none = Resolution { index: None, how: How::Default, missing: None };
        assert_eq!(fallback_line(line, &none, &two), line);
    }

    /// Decision 4: a chosen card that isn't there falls back to Automatic and the choice is kept.
    #[test]
    fn a_missing_card_falls_back_to_automatic() {
        let e = entries_from(&two_cards(), Some(&screen_5070()));
        let gone = AdapterKey::parse("dx12:10de:2b85:NVIDIA GeForce RTX 5090#1").unwrap();
        let r = resolve_card(&e, None, Some(&gone));
        assert_eq!(r, Resolution { index: Some(0), how: How::Screen, missing: Some(gone.clone()) });
        // without the screen known (headless): wgpu's pick
        let e = entries_from(&two_cards(), None);
        assert_eq!(resolve_card(&e, None, Some(&gone)), Resolution { index: None, how: How::Default, missing: Some(gone) });
        assert_eq!(resolve_card(&[], None, None), Resolution { index: None, how: How::Default, missing: None });
    }

    #[test]
    fn the_helper_is_another_card() {
        use wgpu::DeviceType::IntegratedGpu;
        let e = entries_from(&two_cards(), Some(&screen_5070()));
        // Automatic: the other dedicated card, never the software renderer
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Auto, false), Some(1));
        assert_eq!(resolve_helper(&e, Some(1), &HelperChoice::Auto, false), Some(0), "editing on the 4070: the 5070 helps");
        // the variable pins the work to one card: no automatic helper
        assert_eq!(resolve_helper(&e, Some(1), &HelperChoice::Auto, true), None);
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Off, false), None);
        // a named card, but never the editing card itself
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Card(e[1].key.clone()), false), Some(1));
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Card(e[0].key.clone()), false), None);
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Card(e[2].key.clone()), false), None, "not usable");
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Named("4070".into()), true), Some(1));
        // two identical cards, the saved #3 gone: the identical card that isn't the editing one
        let mut twins = vec![e[1].clone(), e[1].clone()];
        twins[1].key.index = 2;
        let gone = AdapterKey { index: 3, ..e[1].key.clone() };
        assert_eq!(resolve_helper(&twins, Some(0), &HelperChoice::Card(gone.clone()), false), Some(1));
        assert_eq!(resolve_helper(&twins, Some(1), &HelperChoice::Card(gone), false), Some(0));
        assert_eq!(resolve_helper(&twins, Some(0), &HelperChoice::Card(twins[0].key.clone()), false), None, "the editing card itself");
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Named("5070".into()), false), None);
        // a second device on the editing card (tests on one card)
        assert_eq!(resolve_helper(&e, Some(0), &HelperChoice::Same, false), Some(0));
        assert_eq!(resolve_helper(&e, None, &HelperChoice::Same, false), None);
        // one card only: no helper
        let one = entries_from(&two_cards()[..1], Some(&screen_5070()));
        assert_eq!(resolve_helper(&one, Some(0), &HelperChoice::Auto, false), None);
        // a built-in GPU helps when there is no other dedicated card
        let mixed = entries_from(
            &[two_cards()[0].clone(), (info("Intel(R) UHD Graphics 770", 0x8086, 0x4680, IntegratedGpu), 32, None)],
            Some(&screen_5070()),
        );
        assert_eq!(resolve_helper(&mixed, Some(0), &HelperChoice::Auto, false), Some(1));
    }

    #[test]
    fn helper_settings_and_variable_parse() {
        assert_eq!(HelperChoice::parse_setting(""), HelperChoice::Auto);
        assert_eq!(HelperChoice::parse_setting("off"), HelperChoice::Off);
        assert_eq!(HelperChoice::parse_setting("garbage"), HelperChoice::Auto, "unknown text is Automatic");
        let k = "dx12:10de:2783:NVIDIA GeForce RTX 4070 SUPER#1";
        assert_eq!(HelperChoice::parse_setting(k), HelperChoice::Card(AdapterKey::parse(k).unwrap()));
        assert_eq!(HelperChoice::parse_setting(k).setting(), k);
        assert_eq!(HelperChoice::Off.setting(), "off");
        assert_eq!(HelperChoice::Auto.setting(), "");
        assert_eq!(HelperChoice::parse_variable(" "), None);
        assert_eq!(HelperChoice::parse_variable("same"), Some(HelperChoice::Same));
        assert_eq!(HelperChoice::parse_variable("4070"), Some(HelperChoice::Named("4070".into())));
        assert_eq!(HelperChoice::parse_variable("OFF"), Some(HelperChoice::Off));
    }
}
