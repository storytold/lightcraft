//! MIDI controller input: parsing raw messages and a configurable map from them to develop
//! controls and commands. Pure functions, no I/O; the map comes from a TOML file the user writes
//! (`docs/midi/` has examples), the port handling lives in the desktop app
//! (`apps/lightcraft/src/midi.rs`) and the dispatch in the UI crate. See `docs/midi.md`.

use lightcraft_develop::controls;
use serde::Deserialize;

/// A MIDI message LightCraft reacts to. Channels are 0-based here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message {
    ControlChange {
        channel: u8,
        cc: u8,
        value: u8,
    },
    /// 14 bits, 0..16383.
    PitchBend {
        channel: u8,
        value: u16,
    },
    /// Note On with a non-zero velocity (a press); releases and Note Off are dropped.
    NoteOn {
        channel: u8,
        note: u8,
    },
}

/// What a message does.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Put a develop control at this value.
    Set { control: String, value: f64 },
    /// Move a develop control by this much.
    Nudge { control: String, delta: f64 },
    /// Run a command with no parameters.
    Command(String),
}

/// Parse one raw MIDI message; `None` for anything the map has no use for or that is malformed.
pub fn parse(bytes: &[u8]) -> Option<Message> {
    let status = *bytes.first()?;
    let a = *bytes.get(1)?;
    let b = *bytes.get(2)?;
    // data bytes have the top bit clear
    if a > 127 || b > 127 {
        return None;
    }
    let channel = status & 0x0F;
    match status & 0xF0 {
        0xB0 => Some(Message::ControlChange { channel, cc: a, value: b }),
        0xE0 => Some(Message::PitchBend { channel, value: u16::from(a) | (u16::from(b) << 7) }),
        0x90 if b > 0 => Some(Message::NoteOn { channel, note: a }),
        _ => None,
    }
}

/// The value a controller position sets: `value` (0..127) spread over the control's range, snapped
/// to its step and clamped. `None` for an unknown control.
pub fn value_for(control: &str, value: u8) -> Option<f64> {
    let spec = controls::find(control)?;
    let v = spec.value_at(f64::from(value.min(127)) / 127.0);
    let snapped = if spec.step > 0.0 { (v / spec.step).round() * spec.step } else { v };
    Some(spec.clamp(snapped))
}

/// How a relative knob writes its direction in a control-change value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    /// 1..63 clockwise, 65..127 counter-clockwise by `value - 64` (Mackie; the SMC-Mixer).
    #[default]
    SignMagnitude,
    /// 1..63 clockwise, 65..127 counter-clockwise by `128 - value`.
    TwosComplement,
    /// 65..127 clockwise by `value - 64`, 1..63 counter-clockwise by `64 - value`.
    Offset,
}

impl Encoding {
    /// Signed ticks a value stands for; `None` for "no movement".
    fn ticks(self, value: u8) -> Option<i32> {
        let v = i32::from(value);
        let t = match self {
            Encoding::SignMagnitude => match v {
                1..=63 => v,
                65..=127 => -(v - 64),
                _ => 0,
            },
            Encoding::TwosComplement => match v {
                1..=63 => v,
                65..=127 => v - 128,
                _ => 0,
            },
            Encoding::Offset => v - 64,
        };
        (t != 0).then_some(t)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    ControlChange(u8),
    PitchBend,
    Note(u8),
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Set(String),
    Nudge { control: String, step: f64, encoding: Encoding },
    Run(String),
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    source: Source,
    /// 0-based; `None` matches any channel.
    channel: Option<u8>,
    target: Target,
}

impl Binding {
    fn action(&self, msg: Message) -> Option<Action> {
        let on_channel = |ch: u8| self.channel.is_none_or(|c| c == ch);
        match (self.source, msg) {
            (Source::ControlChange(n), Message::ControlChange { channel, cc, value }) if n == cc && on_channel(channel) => match &self.target {
                Target::Set(control) => Some(Action::Set { control: control.clone(), value: value_for(control, value)? }),
                Target::Nudge { control, step, encoding } => {
                    Some(Action::Nudge { control: control.clone(), delta: f64::from(encoding.ticks(value)?) * step })
                }
                Target::Run(command) => (value > 0).then(|| Action::Command(command.clone())),
            },
            (Source::PitchBend, Message::PitchBend { channel, value }) if on_channel(channel) => match &self.target {
                // the SMC-Mixer's faders send 7-bit steps in the top of the 14 bits
                Target::Set(control) => Some(Action::Set { control: control.clone(), value: value_for(control, (value >> 7) as u8)? }),
                _ => None,
            },
            (Source::Note(n), Message::NoteOn { channel, note }) if n == note && on_channel(channel) => match &self.target {
                Target::Run(command) => Some(Action::Command(command.clone())),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Which messages do what. The first binding that matches a message wins. The default map has no
/// bindings: MIDI is off until the user provides one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Map {
    /// Names of the input ports to listen to; `None` listens to all of them.
    inputs: Option<Vec<String>>,
    bindings: Vec<Binding>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    inputs: Option<Vec<String>>,
    bindings: Vec<Raw>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    ControlChange,
    PitchBend,
    Note,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    on: Kind,
    channel: Option<u8>,
    number: Option<u8>,
    set: Option<String>,
    nudge: Option<String>,
    run: Option<String>,
    step: Option<f64>,
    encoding: Option<Encoding>,
}

impl Raw {
    fn bind(self) -> Result<Binding, String> {
        let channel = match self.channel {
            None => None,
            Some(c @ 1..=16) => Some(c - 1),
            Some(c) => return Err(format!("channel {c} is not 1-16")),
        };
        let number = match (self.on, self.number) {
            (Kind::PitchBend, None) => 0,
            (Kind::PitchBend, Some(_)) => return Err("pitch_bend has no number".into()),
            (_, Some(n @ 0..=127)) => n,
            (_, Some(n)) => return Err(format!("number {n} is not 0-127")),
            (_, None) => return Err("missing number".into()),
        };
        let source = match self.on {
            Kind::ControlChange => Source::ControlChange(number),
            Kind::PitchBend => Source::PitchBend,
            Kind::Note => Source::Note(number),
        };
        let control = |id: &str| controls::find(id).map(|_| id.to_string()).ok_or_else(|| format!("{id} is not a develop control"));
        let target = match (self.set, self.nudge, self.run) {
            (Some(id), None, None) if self.on != Kind::Note => Target::Set(control(&id)?),
            (None, Some(id), None) if self.on == Kind::ControlChange => {
                let step = self.step.unwrap_or(1.0);
                if !step.is_finite() || step == 0.0 {
                    return Err(format!("step {step} must be a non-zero number"));
                }
                Target::Nudge { control: control(&id)?, step, encoding: self.encoding.unwrap_or_default() }
            }
            (None, None, Some(id)) if self.on != Kind::PitchBend => {
                if crate::cmd::find_command(&id).is_none() {
                    return Err(format!("{id} is not a command"));
                }
                Target::Run(id)
            }
            (None, None, None) => return Err("needs one of set, nudge or run".into()),
            _ => {
                return Err(
                    "set needs control_change or pitch_bend, nudge needs control_change, run needs control_change or note, and only one of them"
                        .into(),
                );
            }
        };
        if self.step.is_some() && !matches!(target, Target::Nudge { .. }) {
            return Err("step only goes with nudge".into());
        }
        if self.encoding.is_some() && !matches!(target, Target::Nudge { .. }) {
            return Err("encoding only goes with nudge".into());
        }
        Ok(Binding { source, channel, target })
    }
}

impl Map {
    /// Parse a TOML map. The error names the first binding that is wrong.
    pub fn from_toml(text: &str) -> Result<Map, String> {
        // the crate's own message is a boxed excerpt of several lines: keep the short text and the line
        let file: File = toml::from_str(text).map_err(|e| {
            let line =
                e.span().and_then(|s| text.get(..s.start)).map_or(String::new(), |before| format!(" (line {})", before.matches('\n').count() + 1));
            format!("{}{line}", e.message())
        })?;
        let bindings = file
            .bindings
            .into_iter()
            .enumerate()
            .map(|(i, raw)| raw.bind().map_err(|e| format!("binding {}: {e}", i + 1)))
            .collect::<Result<_, _>>()?;
        if let Some(names) = &file.inputs
            && names.iter().any(|n| n.trim().is_empty())
        {
            return Err("inputs: a port name is empty".into());
        }
        Ok(Map { inputs: file.inputs, bindings })
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    /// No bindings: nothing to listen for, so no port needs to be open.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Whether the input port called `name` is one the map wants (names compare without case; a map
    /// without `inputs` wants every port).
    pub fn wants_input(&self, name: &str) -> bool {
        self.inputs.as_ref().is_none_or(|names| names.iter().any(|n| n.trim().eq_ignore_ascii_case(name.trim())))
    }

    /// What a message does, if anything.
    pub fn action(&self, msg: Message) -> Option<Action> {
        self.bindings.iter().find_map(|b| b.action(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SMC-Mixer example in the docs, which these tests use as a realistic map.
    fn smc() -> Map {
        Map::from_toml(include_str!("../../../docs/midi/smc-mixer.toml")).unwrap()
    }
    fn cc(cc: u8, value: u8) -> Message {
        Message::ControlChange { channel: 0, cc, value }
    }
    fn fader(channel: u8, value: u16) -> Message {
        Message::PitchBend { channel, value }
    }
    fn press(note: u8) -> Message {
        Message::NoteOn { channel: 0, note }
    }
    fn set(control: &str, value: f64) -> Option<Action> {
        Some(Action::Set { control: control.into(), value })
    }
    fn nudge(control: &str, delta: f64) -> Option<Action> {
        Some(Action::Nudge { control: control.into(), delta })
    }
    fn run(command: &str) -> Option<Action> {
        Some(Action::Command(command.into()))
    }

    #[test]
    fn parses_the_three_message_kinds_and_rejects_the_rest() {
        assert_eq!(parse(&[0xB2, 16, 64]), Some(Message::ControlChange { channel: 2, cc: 16, value: 64 }));
        assert_eq!(parse(&[0xE3, 0, 127]), Some(Message::PitchBend { channel: 3, value: 16256 }));
        assert_eq!(parse(&[0x91, 99, 127]), Some(Message::NoteOn { channel: 1, note: 99 }));
        assert_eq!(parse(&[0x90, 99, 0]), None); // release
        assert_eq!(parse(&[0x80, 99, 64]), None); // note off
        assert_eq!(parse(&[]), None);
        assert_eq!(parse(&[0xB0, 16]), None);
        assert_eq!(parse(&[0xB0, 200, 1]), None); // not a data byte
    }

    #[test]
    fn the_smc_mixer_example_is_valid() {
        let map = smc();
        assert_eq!(map.len(), 8 + 8 + 6 + 3);
        assert!(map.wants_input("SMC-Mixer"));
        assert!(!map.wants_input("MIDIIN2 (SMC-Mixer)"));
    }

    #[test]
    fn the_default_map_is_empty_and_wants_every_port() {
        let map = Map::default();
        assert!(map.is_empty());
        assert_eq!(map.action(press(46)), None);
        assert!(map.wants_input("anything"));
    }

    #[test]
    fn inputs_pick_ports_without_regard_to_case() {
        let map = Map::from_toml(
            r#"inputs = [ "loopMIDI Port", "Keystation 49" ]
bindings = []"#,
        )
        .unwrap();
        assert!(map.wants_input("LoopMIDI port"));
        assert!(map.wants_input("Keystation 49"));
        assert!(!map.wants_input("Keystation 49 MIDI 2"));
        assert!(
            Map::from_toml(
                r#"inputs = [""]
bindings = []"#
            )
            .is_err()
        );
    }

    #[test]
    fn built_in_fader_travel_spans_the_range() {
        let map = smc();
        assert_eq!(map.action(fader(1, 0)), set("light.contrast", -100.0));
        assert_eq!(map.action(fader(1, 16256)), set("light.contrast", 100.0));
        assert_eq!(map.action(fader(0, 16256)), set("light.exposure", 5.0));
        assert_eq!(map.action(fader(8, 100)), None); // no ninth fader
    }

    #[test]
    fn built_in_knobs_are_relative() {
        let map = smc();
        assert_eq!(map.action(cc(17, 1)), nudge("wb.tint", 1.0));
        assert_eq!(map.action(cc(17, 65)), nudge("wb.tint", -1.0));
        assert_eq!(map.action(cc(16, 3)), nudge("wb.temp", 150.0));
        assert_eq!(map.action(cc(16, 127)), nudge("wb.temp", -3150.0));
        assert_eq!(map.action(cc(18, 1)), nudge("color.vibrance", 1.0));
        assert_eq!(map.action(cc(19, 1)), nudge("color.saturation", 1.0));
        assert_eq!(map.action(cc(16, 0)), None);
        assert_eq!(map.action(cc(16, 64)), None);
        assert_eq!(map.action(cc(15, 1)), None);
        assert_eq!(map.action(cc(24, 1)), None);
    }

    #[test]
    fn built_in_buttons_rotate_and_step() {
        let map = smc();
        assert_eq!(map.action(press(46)), run("photo.rotateLeft"));
        assert_eq!(map.action(press(47)), run("photo.rotateRight"));
        assert_eq!(map.action(press(98)), run("library.previous"));
        assert_eq!(map.action(press(99)), run("library.next"));
        assert_eq!(map.action(press(95)), run("photo.delete")); // Record
        assert_eq!(map.action(press(94)), run("develop.auto")); // Play
        assert_eq!(map.action(press(93)), run("develop.treatment")); // Pause
        assert_eq!(map.action(press(92)), None); // Forward
    }

    #[test]
    fn relative_encodings() {
        use Encoding::*;
        let ticks = |e: Encoding, v| e.ticks(v);
        assert_eq!((ticks(SignMagnitude, 1), ticks(SignMagnitude, 65), ticks(SignMagnitude, 64)), (Some(1), Some(-1), None));
        assert_eq!((ticks(TwosComplement, 1), ticks(TwosComplement, 127), ticks(TwosComplement, 0)), (Some(1), Some(-1), None));
        assert_eq!((ticks(Offset, 65), ticks(Offset, 63), ticks(Offset, 64)), (Some(1), Some(-1), None));
    }

    #[test]
    fn a_custom_map_with_channels_absolute_knobs_and_cc_buttons() {
        let map = Map::from_toml(
            r#"bindings = [
  { on = "control_change", channel = 2, number = 21, set = "light.contrast" },
  { on = "control_change", number = 21, set = "light.exposure" },
  { on = "control_change", number = 9, nudge = "wb.tint", step = 2, encoding = "twos_complement" },
  { on = "control_change", number = 64, run = "library.next" },
]"#,
        )
        .unwrap();
        // channel 2 (0-based 1) takes the first binding, any other channel falls through to the second
        assert_eq!(map.action(Message::ControlChange { channel: 1, cc: 21, value: 127 }), set("light.contrast", 100.0));
        assert_eq!(map.action(Message::ControlChange { channel: 0, cc: 21, value: 127 }), set("light.exposure", 5.0));
        assert_eq!(map.action(cc(9, 127)), nudge("wb.tint", -2.0));
        assert_eq!(map.action(cc(64, 127)), run("library.next"));
        assert_eq!(map.action(cc(64, 0)), None); // a pedal release
    }

    #[test]
    fn mistakes_in_a_map_are_errors_naming_the_binding() {
        let bad = |binding: &str| Map::from_toml(&format!("bindings = [{binding}]")).unwrap_err();
        assert!(bad(r#"{ on = "note", number = 1, run = "no.such.command" }"#).contains("binding 1: no.such.command is not a command"));
        assert!(bad(r#"{ on = "pitch_bend", channel = 1, set = "light.nope" }"#).contains("light.nope is not a develop control"));
        assert!(bad(r#"{ on = "pitch_bend", channel = 17, set = "light.exposure" }"#).contains("channel 17"));
        assert!(bad(r#"{ on = "note", number = 128, run = "library.next" }"#).contains("number 128"));
        assert!(bad(r#"{ on = "note", run = "library.next" }"#).contains("missing number"));
        assert!(bad(r#"{ on = "pitch_bend", number = 1, set = "light.exposure" }"#).contains("no number"));
        assert!(bad(r#"{ on = "note", number = 1 }"#).contains("needs one of"));
        assert!(bad(r#"{ on = "note", number = 1, set = "light.exposure" }"#).contains("set needs"));
        assert!(bad(r#"{ on = "control_change", number = 1, nudge = "wb.tint", step = 0 }"#).contains("non-zero"));
        assert!(bad(r#"{ on = "control_change", number = 1, set = "light.exposure", step = 2 }"#).contains("step only"));
        assert!(bad(r#"{ on = "note", number = 1, run = "library.next", colour = "red" }"#).contains("colour"));
        assert!(Map::from_toml("not = \"a map\"").is_err());
        assert!(Map::from_toml("bindings = [").is_err());
        assert!(Map::from_toml("").is_err());
        // a TOML syntax error is one short line that says where
        let e = Map::from_toml("inputs = []\nbindings = [").unwrap_err();
        assert!(e.contains("line 2") && !e.contains('\n'), "{e}");
    }

    #[test]
    fn range_ends_hit_min_and_max() {
        assert_eq!(value_for("light.contrast", 0), Some(-100.0));
        assert_eq!(value_for("light.contrast", 127), Some(100.0));
        assert_eq!(value_for("nope", 1), None);
    }
}
