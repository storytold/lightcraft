//! Everything the user tunes about control surfaces, saved as `control-surfaces.json` in
//! LightCraft's settings folder (format: `docs/control-surfaces.md` → Settings file).
//!
//! Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT: the tuning model, its
//! defaults, ranges and the workflow presets come from TactileBridge's `TactileBridgeSettings`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::profile::Profile;

/// The settings file's format version.
/// 2: `dial` / `roller` name the Dialpad's encoders as they are (format 1 had them the wrong
/// way round: the Logi device's encoder 0 is the roller); format 1 files are swapped on load, so
/// every encoder keeps doing what it did.
/// 3: the brush's Erase key is the momentary `brush.erase` (format 2 had a subtracting new brush,
/// `tool.brush {op: subtract}`, from TactileBridge's Erase Mask Tool).
pub const FORMAT_VERSION: u32 = 3;
/// The port surfaces connect to by default.
pub const DEFAULT_PORT: u16 = 7981;

/// Per-value dial feel (TactileBridge's `AdjustmentTuning`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tuning {
    /// Multiplies the step per tick (0.1–5).
    pub speed: f64,
    /// Multiplies the acceleration ceiling (0.1–4).
    pub acceleration: f64,
    /// Acceleration when turning slowly (0.1–10).
    pub acceleration_min: f64,
    /// Acceleration when turning fast (≥ the floor, ≤ 15).
    pub acceleration_max: f64,
    /// Ticks closer together than this (ms) get the full ceiling (5–250).
    pub fast_ms: u32,
    /// Ticks further apart than this (ms) get the floor (fast + 10 – 1200).
    pub slow_ms: u32,
    /// Turn the other way for this value (combines with the global invert).
    pub invert: bool,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning { speed: 1.0, acceleration: 1.0, acceleration_min: 1.0, acceleration_max: 10.0, fast_ms: 20, slow_ms: 200, invert: false }
    }
}

fn clamp(v: f64, lo: f64, hi: f64, default: f64) -> f64 {
    if v.is_finite() { v.clamp(lo, hi.max(lo)) } else { default }
}

impl Tuning {
    /// The tuning within TactileBridge's ranges (a hand-edited or imported file can say anything).
    pub fn sanitized(&self) -> Tuning {
        let d = Tuning::default();
        let fast = self.fast_ms.clamp(5, 250);
        let min = clamp(self.acceleration_min, 0.1, 10.0, d.acceleration_min);
        Tuning {
            speed: clamp(self.speed, 0.1, 5.0, d.speed),
            acceleration: clamp(self.acceleration, 0.1, 4.0, d.acceleration),
            acceleration_min: min,
            acceleration_max: clamp(self.acceleration_max, min, 15.0, d.acceleration_max.max(min)),
            fast_ms: fast,
            slow_ms: self.slow_ms.clamp(fast + 10, 1200),
            invert: self.invert,
        }
    }
}

/// TactileBridge's workflow presets, applied to every value at once (Settings ▸ Controls ▸ Dial
/// feel). Name, English description, tuning.
pub fn tuning_presets() -> Vec<(&'static str, &'static str, Tuning)> {
    let t = |speed, acceleration, acceleration_max, fast_ms, slow_ms| Tuning {
        speed,
        acceleration,
        acceleration_min: 1.0,
        acceleration_max,
        fast_ms,
        slow_ms,
        invert: false,
    };
    vec![
        ("Wedding", "Soft, smooth and precise: gentle acceleration for delicate, controlled colour work.", t(0.7, 0.8, 4.0, 40, 280)),
        ("Real Estate", "Balanced: a standard dial feel with responsive mid-range acceleration.", t(1.0, 1.0, 10.0, 20, 180)),
        ("Fashion", "Bold and expressive: faster response with strong acceleration for dynamic edits.", t(1.3, 1.2, 14.0, 16, 150)),
        // TactileBridge asked for an 18× ceiling; its own clamp made it 15×
        ("Landscape", "Wide range: slow base speed with a very high peak for sweeping adjustments.", t(0.6, 1.5, 15.0, 12, 200)),
    ]
}

/// How dials turn values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MotionSettings {
    /// Multiplies every step (0.1–5).
    pub speed: f64,
    /// Multiplies every acceleration ceiling (0.1–4).
    pub acceleration: f64,
    /// Turn every dial the other way.
    pub invert: bool,
    /// Step factor while a Fine modifier is held.
    pub fine: f64,
    /// Step factor while a Coarse modifier is held.
    pub coarse: f64,
}

impl Default for MotionSettings {
    fn default() -> Self {
        MotionSettings { speed: 1.0, acceleration: 1.0, invert: false, fine: 0.25, coarse: 4.0 }
    }
}

/// Timing of the pipe from dial to photo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TransportSettings {
    /// Dial movement is gathered and applied at most this often (ms, 8–100).
    pub flush_ms: u32,
    /// While turning, values sent back to the surface are held until every tick so far has been
    /// applied, so the readout never jumps back.
    pub echo_suppression: bool,
    /// How long after the last tick the readout counts as "turning" (ms, 0–1000).
    pub echo_suppression_ms: u32,
    /// A dial gesture ends (one undo step; Apply to Selected copies it over) after this long
    /// without ticks (ms, 200–10000).
    pub gesture_end_ms: u32,
}

impl Default for TransportSettings {
    fn default() -> Self {
        TransportSettings { flush_ms: 16, echo_suppression: true, echo_suppression_ms: 100, gesture_end_ms: 800 }
    }
}

/// Where the on-screen readout appears.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OsdPosition {
    /// Above the bottom edge.
    #[default]
    Bottom,
    /// At the bottom edge.
    BottomEdge,
    /// Below the top edge.
    Top,
    /// At the top edge.
    TopEdge,
}

/// The on-screen readout in the LightCraft window (TactileBridge's OSD).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct OsdSettings {
    pub enabled: bool,
    /// 0.1–1.
    pub opacity: f64,
    /// `#RRGGBB`.
    pub background: String,
    /// How long it stays (ms, 500–10000), then fades.
    pub display_ms: u32,
    pub position: OsdPosition,
    /// Also show commands (key presses), not only dial values.
    pub show_commands: bool,
}

impl Default for OsdSettings {
    fn default() -> Self {
        OsdSettings {
            enabled: true,
            opacity: 0.9,
            background: "#202020".into(),
            display_ms: 1800,
            position: OsdPosition::Bottom,
            show_commands: true,
        }
    }
}

/// What the surface itself shows (TactileBridge's plugin colours and hardware switches).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DeviceLook {
    /// Key background, `#RRGGBB`.
    pub background: String,
    /// Text, value and accent colour, `#RRGGBB`.
    pub foreground: String,
    /// Show live values on keys and the dial display.
    pub show_values: bool,
    /// Haptic feedback where the device supports it.
    pub haptics: bool,
}

impl Default for DeviceLook {
    fn default() -> Self {
        DeviceLook { background: "#000000".into(), foreground: "#FFFFFF".into(), show_values: true, haptics: true }
    }
}

/// A surface client the user approved (Settings ▸ Controls ▸ Paired surfaces).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PairedClient {
    /// The client's own id (`lightcraft-logi`).
    pub client: String,
    /// Its display name.
    pub name: String,
    /// SHA-256 of the token it was given (hex). The token itself is never stored here.
    pub token_sha256: String,
    /// When it was approved (Unix seconds).
    pub approved_at: u64,
}

/// The whole settings file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SurfaceSettings {
    pub version: u32,
    /// Surfaces may connect and drive the app. Off by default; turned on by itself when a
    /// surface plugin is installed (the Logi Options+ plugin), unless the user turned it off.
    pub enabled: bool,
    /// The user switched control surfaces off: never turn them on by themselves again.
    pub off_by_user: bool,
    /// Dialogs put their own page on the keys (OK / Cancel…). Off by default: the keys keep the
    /// page you work with while a dialog is open.
    pub dialog_pages: bool,
    /// The loopback port surfaces connect to while enabled (the surfaces-only listener).
    pub port: u16,
    /// The profile in use (by name).
    pub profile: String,
    /// Pages follow what the app shows (TactileBridge's automatic tool switching).
    pub follow_context: bool,
    /// Dials change every selected photo, one undo step per gesture (TactileBridge's Auto Sync).
    pub apply_to_selected: bool,
    pub motion: MotionSettings,
    /// Target id → its own tuning; values not listed use the default tuning.
    pub tuning: BTreeMap<String, Tuning>,
    pub transport: TransportSettings,
    pub osd: OsdSettings,
    pub look: DeviceLook,
    pub profiles: Vec<Profile>,
    pub paired: Vec<PairedClient>,
}

impl Default for SurfaceSettings {
    fn default() -> Self {
        SurfaceSettings {
            version: FORMAT_VERSION,
            enabled: false,
            off_by_user: false,
            dialog_pages: false,
            port: DEFAULT_PORT,
            profile: "Default".into(),
            follow_context: true,
            apply_to_selected: false,
            motion: MotionSettings::default(),
            tuning: BTreeMap::new(),
            transport: TransportSettings::default(),
            osd: OsdSettings::default(),
            look: DeviceLook::default(),
            profiles: crate::defaults::profiles(),
            paired: Vec::new(),
        }
    }
}

/// `#RRGGBB` (or `RRGGBB`), else `fallback`.
pub fn hex_color(s: &str, fallback: &str) -> String {
    let t = s.trim().trim_start_matches('#');
    if t.len() == 6 && t.chars().all(|c| c.is_ascii_hexdigit()) { format!("#{}", t.to_ascii_uppercase()) } else { fallback.to_string() }
}

/// `#RRGGBB` → (r, g, b).
pub fn rgb(s: &str) -> Option<(u8, u8, u8)> {
    let t = s.trim().trim_start_matches('#');
    if t.len() != 6 || !t.is_ascii() {
        return None;
    }
    let p = |i: usize| t.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok());
    Some((p(0)?, p(2)?, p(4)?))
}

impl SurfaceSettings {
    /// Parse a settings file. A damaged or newer file is an error the caller shows (it keeps the
    /// file and runs on defaults); values out of range are clamped.
    pub fn from_json(text: &str) -> Result<SurfaceSettings, String> {
        if !text.trim_start().starts_with('{') {
            return Err("control-surfaces.json can't be read: it is not a settings object".into());
        }
        let mut s: SurfaceSettings = serde_json::from_str(text).map_err(|e| format!("control-surfaces.json can't be read: {e}"))?;
        if s.version > FORMAT_VERSION {
            return Err(format!("control-surfaces.json is from a newer LightCraft (format {}, this one reads {FORMAT_VERSION})", s.version));
        }
        if s.version < 2 {
            for p in &mut s.profiles {
                swap_encoders(&mut p.base);
                for pg in &mut p.pages {
                    swap_encoders(&mut pg.bindings);
                }
            }
        }
        if s.version < 3 {
            let erase = crate::binding::Binding::command_with("tool.brush", serde_json::json!({"op": "subtract"}));
            for p in &mut s.profiles {
                for b in p.base.values_mut().chain(p.pages.iter_mut().flat_map(|pg| pg.bindings.values_mut())) {
                    if *b == erase {
                        *b = crate::binding::Binding::command("brush.erase");
                    }
                }
            }
        }
        s.sanitize();
        Ok(s)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Clamp every value into range and repair references, whatever the file said.
    pub fn sanitize(&mut self) {
        let d = MotionSettings::default();
        self.version = FORMAT_VERSION;
        if self.port < 1024 {
            self.port = DEFAULT_PORT;
        }
        self.motion.speed = clamp(self.motion.speed, 0.1, 5.0, d.speed);
        self.motion.acceleration = clamp(self.motion.acceleration, 0.1, 4.0, d.acceleration);
        self.motion.fine = clamp(self.motion.fine, 0.01, 1.0, d.fine);
        self.motion.coarse = clamp(self.motion.coarse, 1.0, 20.0, d.coarse);
        for t in self.tuning.values_mut() {
            *t = t.sanitized();
        }
        let tr = &mut self.transport;
        tr.flush_ms = tr.flush_ms.clamp(8, 100);
        tr.echo_suppression_ms = tr.echo_suppression_ms.min(1000);
        tr.gesture_end_ms = tr.gesture_end_ms.clamp(200, 10_000);
        let osd = &mut self.osd;
        osd.opacity = clamp(osd.opacity, 0.1, 1.0, 0.9);
        osd.display_ms = osd.display_ms.clamp(500, 10_000);
        osd.background = hex_color(&osd.background, "#202020");
        self.look.background = hex_color(&self.look.background, "#000000");
        self.look.foreground = hex_color(&self.look.foreground, "#FFFFFF");
        if self.profiles.is_empty() {
            self.profiles = crate::defaults::profiles();
        }
        // the Actions Ring came later: profiles saved before it get its default slots (once: a
        // ring set up anywhere is the user's own)
        if !self.profiles.iter().any(|p| p.base.keys().any(|c| c.starts_with("ring."))) {
            for p in &mut self.profiles {
                for (c, b) in crate::defaults::ring() {
                    p.base.entry(c).or_insert(b);
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for (i, p) in self.profiles.iter_mut().enumerate() {
            if p.name.trim().is_empty() {
                p.name = format!("Profile {}", i + 1);
            }
            let base = p.name.clone();
            let mut n = 2;
            while !seen.insert(p.name.clone()) {
                p.name = format!("{base} {n}");
                n += 1;
            }
            p.sanitize();
        }
        if !self.profiles.iter().any(|p| p.name == self.profile) {
            self.profile = self.profiles.first().map(|p| p.name.clone()).unwrap_or_default();
        }
        self.paired.retain(|c| c.token_sha256.len() == 64 && c.token_sha256.chars().all(|c| c.is_ascii_hexdigit()));
    }

    pub fn active_profile(&self) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.name == self.profile).or(self.profiles.first())
    }

    /// The tuning of a target (its own, else the default).
    pub fn tuning_of(&self, target: &str) -> Tuning {
        self.tuning.get(target).map(Tuning::sanitized).unwrap_or_default()
    }
}

/// Exchange the dial's and the roller's bindings (format 1 → 2).
fn swap_encoders(map: &mut BTreeMap<String, crate::binding::Binding>) {
    for (a, b) in [("dial", "roller"), ("dial.press", "roller.press")] {
        let x = map.remove(a);
        let y = map.remove(b);
        if let Some(v) = x {
            map.insert(b.to_string(), v);
        }
        if let Some(v) = y {
            map.insert(a.to_string(), v);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_2_files_get_the_momentary_erase() {
        let mut v = serde_json::to_value(SurfaceSettings::default()).unwrap();
        v["version"] = serde_json::json!(2);
        v["profiles"][0]["base"]["key.2"] = serde_json::json!({"kind": "command", "command": "tool.brush", "params": {"op": "subtract"}});
        let s = SurfaceSettings::from_json(&v.to_string()).unwrap();
        assert_eq!(s.profiles[0].base["key.2"], crate::Binding::command("brush.erase"));
    }

    #[test]
    fn format_1_files_swap_the_dial_and_the_roller() {
        let mut v = serde_json::to_value(SurfaceSettings::default()).unwrap();
        v["version"] = serde_json::json!(1);
        let p = v["profiles"][0]["pages"].as_array_mut().unwrap().iter_mut().find(|p| p["name"] == "Dial Page").unwrap();
        p["bindings"] = serde_json::json!({"dial": {"kind": "value", "target": "light.exposure"}, "roller": {"kind": "value", "target": "wb.temp"}});
        let s = SurfaceSettings::from_json(&v.to_string()).unwrap();
        let pg = s.profiles[0].page("Dial Page").unwrap();
        assert_eq!(pg.bindings["roller"], crate::Binding::value("light.exposure"), "what encoder 0 did, it still does");
        assert_eq!(pg.bindings["dial"], crate::Binding::value("wb.temp"));
        assert_eq!(s.version, FORMAT_VERSION);
        // a format 2 file is left alone
        let again = SurfaceSettings::from_json(&s.to_json()).unwrap();
        assert_eq!(again.profiles[0].page("Dial Page").unwrap().bindings["roller"], crate::Binding::value("light.exposure"));
    }

    use super::*;

    #[test]
    fn defaults_round_trip() {
        let s = SurfaceSettings::default();
        let back = SurfaceSettings::from_json(&s.to_json()).unwrap();
        assert_eq!(back, s);
        assert!(s.active_profile().is_some());
    }

    #[test]
    fn hostile_values_are_clamped() {
        let text = r##"{"motion": {"speed": 1e300, "acceleration": -4, "fine": 0, "coarse": 1000},
            "tuning": {"light.exposure": {"speed": "NaN", "fastMs": 1, "slowMs": 0, "accelerationMin": 50, "accelerationMax": 0}},
            "transport": {"flushMs": 0, "gestureEndMs": 4294967295},
            "osd": {"opacity": 7, "background": "zzz", "displayMs": 1},
            "look": {"foreground": "#12345g"},
            "profile": "missing",
            "profiles": [{"name": ""}, {"name": ""}],
            "paired": [{"client": "x", "tokenSha256": "short"}]}"##;
        // "NaN" is not a number: the file is rejected rather than half-read
        assert!(SurfaceSettings::from_json(text).is_err());
        let text = text.replace("\"NaN\"", "1e9");
        let s = SurfaceSettings::from_json(&text).unwrap();
        assert_eq!((s.motion.speed, s.motion.acceleration, s.motion.fine, s.motion.coarse), (5.0, 0.1, 0.01, 20.0));
        let t = &s.tuning["light.exposure"];
        assert_eq!((t.speed, t.fast_ms, t.slow_ms, t.acceleration_min, t.acceleration_max), (5.0, 5, 15, 10.0, 10.0));
        assert_eq!((s.transport.flush_ms, s.transport.gesture_end_ms), (8, 10_000));
        assert_eq!((s.osd.opacity, s.osd.background.as_str(), s.osd.display_ms), (1.0, "#202020", 500));
        assert_eq!(s.look.foreground, "#FFFFFF");
        assert_eq!(s.profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Profile 1", "Profile 2"]);
        assert_eq!(s.profile, "Profile 1");
        assert!(s.paired.is_empty());
    }

    #[test]
    fn newer_or_damaged_files_are_errors() {
        assert!(SurfaceSettings::from_json("{\"version\": 99}").unwrap_err().contains("newer"));
        assert!(SurfaceSettings::from_json("{").is_err());
        assert!(SurfaceSettings::from_json("[]").is_err());
        assert_eq!(SurfaceSettings::from_json("{}").unwrap().profiles.len(), crate::defaults::profiles().len());
    }

    #[test]
    fn colours() {
        assert_eq!(hex_color("ff7800", "#000000"), "#FF7800");
        assert_eq!(hex_color("#ff78", "#000000"), "#000000");
        assert_eq!(rgb("#FF7800"), Some((255, 120, 0)));
        assert_eq!(rgb("#ÿÿÿ"), None);
        assert_eq!(rgb(""), None);
    }

    #[test]
    fn presets_are_in_range() {
        for (_, _, t) in tuning_presets() {
            assert_eq!(t.sanitized(), t);
        }
    }
}
