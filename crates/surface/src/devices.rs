//! The controls of each supported device.
//!
//! A control is addressed by a stable string id (`key.1`, `dial`, …) in profiles, in the surface
//! protocol and in the editor. Unknown ids are accepted everywhere (a MIDI or Tangent device adds
//! its own); [`find_control`] only knows the built-in devices.

use serde::{Deserialize, Serialize};

/// What kind of physical control it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlKind {
    /// An endless rotary encoder (dial, roller, knob).
    Encoder,
    /// A plain button.
    Button,
    /// A button with its own display (an LCD key): it shows an icon, a label and a value.
    Key,
}

/// Which page cursor resolves a control: keys follow the key page, dials (and the buttons around
/// them) the dial page, so the two can be paged separately, as the MX Creative Keypad and Dialpad
/// are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PageGroup {
    #[default]
    Keys,
    Dials,
}

impl PageGroup {
    pub const ALL: [PageGroup; 2] = [PageGroup::Keys, PageGroup::Dials];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ControlDef {
    pub id: &'static str,
    pub kind: ControlKind,
    /// English label (the editor translates it).
    pub label: &'static str,
    pub group: PageGroup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceDef {
    /// Stable id used in the protocol (`surface.subscribe {device}`).
    pub id: &'static str,
    pub name: &'static str,
    pub controls: &'static [ControlDef],
}

const fn key(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Key, label, group: PageGroup::Keys }
}
const fn pad(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Button, label, group: PageGroup::Dials }
}
/// A slot of the Actions Ring (the on-screen ring of Logi Options+): pressed like a key.
const fn ring(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Button, label, group: PageGroup::Keys }
}
const fn enc(id: &'static str, label: &'static str) -> ControlDef {
    ControlDef { id, kind: ControlKind::Encoder, label, group: PageGroup::Dials }
}

/// Logitech MX Creative Console: the Keypad (3 × 3 LCD keys; Logi device type `Loupedeck70`) and
/// the Dialpad (a dial, a roller and four buttons; `Loupedeck71`), and the on-screen Actions Ring
/// (eight slots, `ring.1`–`ring.8`; `Loupedeck72`), set on All pages (the ring has no pages).
pub static MX_CREATIVE_CONSOLE: DeviceDef = DeviceDef {
    id: "mx-creative-console",
    name: "Logitech MX Creative Console",
    controls: &[
        key("key.1", "Key 1"),
        key("key.2", "Key 2"),
        key("key.3", "Key 3"),
        key("key.4", "Key 4"),
        key("key.5", "Key 5"),
        key("key.6", "Key 6"),
        key("key.7", "Key 7"),
        key("key.8", "Key 8"),
        key("key.9", "Key 9"),
        enc("dial", "Dial"),
        enc("roller", "Roller"),
        pad("pad.1", "Dialpad button 1"),
        pad("pad.2", "Dialpad button 2"),
        pad("pad.3", "Dialpad button 3"),
        pad("pad.4", "Dialpad button 4"),
        ring("ring.1", "Ring slot 1"),
        ring("ring.2", "Ring slot 2"),
        ring("ring.3", "Ring slot 3"),
        ring("ring.4", "Ring slot 4"),
        ring("ring.5", "Ring slot 5"),
        ring("ring.6", "Ring slot 6"),
        ring("ring.7", "Ring slot 7"),
        ring("ring.8", "Ring slot 8"),
    ],
};

/// Loupedeck+: knobs, the Control Dial and buttons, without displays (`Loupedeck15`). Its controls
/// are Loupedeck's own numbers ([`crate::loupedeck`]).
pub static LOUPEDECK_PLUS: DeviceDef = DeviceDef { id: "loupedeck-plus", name: "Loupedeck+", controls: crate::loupedeck::CONTROLS };

/// Every built-in device.
pub fn devices() -> &'static [&'static DeviceDef] {
    static ALL: [&DeviceDef; 2] = [&MX_CREATIVE_CONSOLE, &LOUPEDECK_PLUS];
    &ALL
}

pub fn find_device(id: &str) -> Option<&'static DeviceDef> {
    devices().iter().copied().find(|d| d.id == id)
}

/// A built-in control by id.
pub fn find_control(id: &str) -> Option<&'static ControlDef> {
    devices().iter().flat_map(|d| d.controls.iter()).find(|c| c.id == id)
}

/// The page group of a control; unknown controls page with the keys unless they turn.
pub fn group_of(id: &str) -> PageGroup {
    match find_control(id) {
        Some(c) => c.group,
        None if id.starts_with("dial") || id.starts_with("roller") || id.starts_with("enc") || id.starts_with("knob") => PageGroup::Dials,
        None => PageGroup::Keys,
    }
}

/// Does the control turn (an encoder)?
pub fn is_encoder(id: &str) -> bool {
    match find_control(id) {
        Some(c) => c.kind == ControlKind::Encoder,
        None => group_of(id) == PageGroup::Dials && !id.starts_with("pad"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn console_has_nine_keys_a_dial_a_roller_and_four_pads() {
        let d = find_device("mx-creative-console").unwrap();
        assert_eq!(d.controls.iter().filter(|c| c.kind == ControlKind::Key).count(), 9);
        assert_eq!(d.controls.iter().filter(|c| c.kind == ControlKind::Encoder).count(), 2);
        assert_eq!(d.controls.iter().filter(|c| c.kind == ControlKind::Button).count(), 12, "four Dialpad buttons, eight ring slots");
        assert_eq!(d.controls.iter().filter(|c| c.id.starts_with("ring.")).count(), 8);
        let mut ids: Vec<_> = d.controls.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), d.controls.len(), "control ids are unique");
    }

    #[test]
    fn loupedeck_plus_knobs_turn_and_its_buttons_press() {
        let d = find_device("loupedeck-plus").unwrap();
        assert!(is_encoder("ld.r33") && is_encoder("ld.r48") && !is_encoder("ld.b95"));
        assert_eq!(group_of("ld.b95"), PageGroup::Dials);
        let mut ids: Vec<_> = d.controls.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), d.controls.len(), "control ids are unique");
    }

    #[test]
    fn groups_and_encoders() {
        assert_eq!(group_of("key.4"), PageGroup::Keys);
        assert_eq!(group_of("dial"), PageGroup::Dials);
        assert_eq!(group_of("pad.2"), PageGroup::Dials);
        assert_eq!(group_of("knob.7"), PageGroup::Dials);
        assert_eq!(group_of("midi.note.36"), PageGroup::Keys);
        assert!(is_encoder("dial") && is_encoder("roller") && is_encoder("knob.3"));
        assert!(!is_encoder("key.1") && !is_encoder("pad.1"));
    }
}
