//! What a surface shows: the labels, icons and values of its keys and dials, pushed to it as the
//! `surface.layout` event (see `docs/control-surfaces.md`).

use serde::Serialize;

use crate::binding::Binding;
use crate::devices::{ControlKind, DeviceDef, PageGroup};
use crate::runtime::Runtime;
use crate::targets::target_spec;

/// The app's side of a layout: current values, translated labels, toggle states.
pub trait Host {
    /// The current value of a target, if it has one now (no active photo: none).
    fn value(&self, target: &str) -> Option<f64>;
    /// A short, translated label for a binding.
    fn label(&self, binding: &Binding) -> String;
    /// The icon a key shows for `binding` (the host may know better: an AI-mask preset's own).
    fn icon(&self, binding: &Binding) -> Option<&'static str> {
        crate::icons::icon_for(binding)
    }
    /// On / off for bindings that toggle something (Auto Sync, Before/After…), if known.
    fn state(&self, binding: &Binding) -> Option<bool>;
    /// Translate an English UI string.
    fn tr(&self, english: &str) -> String;
}

/// One key or button.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyView {
    pub control: String,
    /// The binding (`null`: the key does nothing on this page).
    pub binding: Option<Binding>,
    pub label: String,
    /// An icon name (`surface.icons` returns its SVG).
    pub icon: Option<&'static str>,
    /// The value of a value key (`+0.35`), when values are shown.
    pub value: Option<String>,
    /// Lit: the dial moves this key's value, the modifier is held, the toggle is on.
    pub active: bool,
    /// How it is lit: `focus` (its value is on the dial), `held` (a modifier or a momentary
    /// command while its key is down), `on` (a mode or toggle that is on); none when not lit.
    /// Surfaces draw focus and the other two differently.
    pub state: Option<&'static str>,
}

/// One encoder.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialView {
    pub control: String,
    pub binding: Option<Binding>,
    pub label: String,
    pub icon: Option<&'static str>,
    pub value: Option<String>,
    /// Where the value sits between its ends, 0…1.
    pub position: Option<f64>,
}

/// A key page as a device with hardware page buttons shows it (its page `n`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyPageView {
    pub name: String,
    pub keys: Vec<KeyView>,
}

/// Everything a surface shows.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLayout {
    pub device: &'static str,
    pub profile: String,
    pub context: String,
    /// The current page of the key group and the dial group.
    pub key_page: Option<String>,
    pub dial_page: Option<String>,
    /// The keys of the current key page.
    pub keys: Vec<KeyView>,
    /// Key pages 1…n for devices that page their keys themselves; while a context page is
    /// active, every one of them shows it.
    pub key_pages: Vec<KeyPageView>,
    /// Plain buttons (the Dialpad's four).
    pub buttons: Vec<KeyView>,
    pub dials: Vec<DialView>,
    /// The value the dial moves instead of its own, if a key put one there.
    pub focus: Option<String>,
    pub apply_to_selected: bool,
    pub follow_context: bool,
    /// `#RRGGBB`.
    pub background: String,
    pub foreground: String,
    pub show_values: bool,
    pub haptics: bool,
}

/// Device pages offered by keypads that page in hardware (the Logi profile has this many).
pub const DEVICE_KEY_PAGES: usize = 3;

fn key_view(r: &Runtime, host: &dyn Host, control: &str, binding: Option<Binding>) -> KeyView {
    let show = r.settings().look.show_values;
    let (label, icon, value, state) = match &binding {
        None => (String::new(), None, None, None),
        Some(b) => {
            let value = match b {
                Binding::Value { target } if show => target_spec(target).and_then(|s| host.value(target).map(|v| s.format(v))),
                _ => None,
            };
            let state = match b {
                Binding::Value { target } => (r.focus() == Some(target.as_str())).then_some("focus"),
                Binding::Modifier { .. } => r.held_on(control).is_some().then_some("held"),
                Binding::Command { command, .. } if crate::runtime::MOMENTARY.contains(&command.as_str()) => {
                    r.momentary_on(control).then_some("held")
                }
                Binding::ApplyToSelected => r.settings().apply_to_selected.then_some("on"),
                Binding::FollowContext => r.settings().follow_context.then_some("on"),
                _ => host.state(b).unwrap_or(false).then_some("on"),
            };
            (host.label(b), host.icon(b), value, state)
        }
    };
    KeyView { control: control.to_string(), binding, label, icon, value, active: state.is_some(), state }
}

/// The layout of `device` now.
pub fn build(r: &Runtime, device: &DeviceDef, host: &dyn Host) -> DeviceLayout {
    let look = &r.settings().look;
    let mut keys = Vec::new();
    let mut buttons = Vec::new();
    let mut dials = Vec::new();
    let key_controls: Vec<&str> = device.controls.iter().filter(|c| c.kind == ControlKind::Key).map(|c| c.id).collect();
    for c in device.controls {
        let b = r.binding(c.id, None);
        match c.kind {
            ControlKind::Key => keys.push(key_view(r, host, c.id, b)),
            ControlKind::Button => buttons.push(key_view(r, host, c.id, b)),
            ControlKind::Encoder => {
                let (label, icon, value, position) = match &b {
                    Some(bound @ Binding::Value { target }) => {
                        let spec = target_spec(target);
                        let v = host.value(target);
                        let value = if look.show_values { spec.as_ref().zip(v).map(|(s, v)| s.format(v)) } else { None };
                        (host.label(bound), host.icon(bound), value, spec.zip(v).map(|(s, v)| s.position(v)))
                    }
                    Some(other) => (host.label(other), host.icon(other), None, None),
                    None => (String::new(), None, None, None),
                };
                dials.push(DialView { control: c.id.to_string(), binding: b, label, icon, value, position });
            }
        }
    }
    let key_pages = (1..=r.key_page_count().max(DEVICE_KEY_PAGES))
        .map(|n| {
            let page = r.device_key_page(n);
            KeyPageView {
                name: page.map(|p| p.name.clone()).unwrap_or_default(),
                keys: key_controls.iter().map(|c| key_view(r, host, c, r.binding(c, Some(n)))).collect(),
            }
        })
        .collect();
    DeviceLayout {
        device: device.id,
        profile: r.settings().profile.clone(),
        context: r.context().to_string(),
        key_page: r.pages().get(&PageGroup::Keys).cloned(),
        dial_page: r.pages().get(&PageGroup::Dials).cloned(),
        keys,
        key_pages,
        buttons,
        dials,
        focus: r.focus().map(str::to_string),
        apply_to_selected: r.settings().apply_to_selected,
        follow_context: r.settings().follow_context,
        background: look.background.clone(),
        foreground: look.foreground.clone(),
        show_values: look.show_values,
        haptics: look.haptics,
    }
}
