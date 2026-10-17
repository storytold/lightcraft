# MIDI controller input

> **Last updated:** 2026-10-17 · **Change:** major (first version: configurable TOML map, `midi.reload`, example for the M-VAVE SMC-Mixer)

A MIDI control surface can drive Develop sliders, run commands and step through photos. Windows and macOS
only for now (Linux's ALSA backend is a C library, which the pure-Rust rule rules out; web has no input).

**MIDI is off until you give LightCraft a map.** Without one, no MIDI port is opened, so other programs keep
their controllers (Windows gives a port to one program at a time).

- What each message does comes from a **map** in TOML: `midi.toml` in the config folder.
- The ports named by the map are opened at startup and re-scanned every 3 s, so a device plugged in later works.
- A turn of one fader or knob is **one undo step**: it ends after 0.4 s without an event, or when another
  control moves. Develop edits need an active photo; commands run regardless.

## Set up

1. Copy an example from [`docs/midi/`](midi/) (for now [`smc-mixer.toml`](midi/smc-mixer.toml), for the
   M-VAVE SMC-Mixer) to **`midi.toml` in LightCraft's config folder** (`%APPDATA%\LightCraft` on Windows,
   `~/Library/Application Support/LightCraft` on macOS; the folder with `models/` and `camera-profiles/`), or
   point `LIGHTCRAFT_MIDI_MAP` at the file.
2. Restart LightCraft, or after later edits run the **`midi.reload`** command (control channel, MCP or the
   command list). It reports how many bindings it loaded. If the file has a mistake, the message names the
   binding and the map in use stays as it was; at startup a broken map leaves MIDI off, with a notice.
3. To see what your controller sends, run with `LIGHTCRAFT_LOG=debug`: every message is logged with its port
   (`MIDI SMC-Mixer: [E0, 00, 7F]`) in the log file (`logs/lightcraft.log` in the config folder).

## The map

```toml
inputs = ["SMC-Mixer"]   # optional: only these ports (exact names, any case). Leave out for all ports.
bindings = [
  { on = "pitch_bend", channel = 1, set = "light.exposure" },
  { on = "control_change", number = 16, nudge = "wb.temp", step = 50 },
  { on = "note", number = 46, run = "photo.rotateLeft" },
]
```

Name the ports you want in `inputs`. A device can show up as several ports that repeat each other (the
SMC-Mixer has two on Windows), and listening to all of them can run a command twice.

Each binding has a **source** and one **target**. The first binding that matches a message wins.

| Key | Meaning |
|---|---|
| `on` | `"control_change"`, `"pitch_bend"` or `"note"` (a press; releases are ignored) |
| `channel` | 1–16. Leave it out to match any channel. Pitch bend is per channel, so faders use it. |
| `number` | Controller or note number, 0–127. Not used for `pitch_bend`. |
| `set` | A develop control id (`light.exposure`, `wb.temp`, …). **Absolute**: the controller's travel spans the control's range, snapped to its step. Goes with `control_change` and `pitch_bend`. There is no pickup, so a fader jumps the slider to its position when touched. |
| `nudge` | A develop control id. **Relative**: each tick moves it by `step` (default 1), through `develop.adjust`. Goes with `control_change`. |
| `encoding` | With `nudge`: how the knob writes direction. `"sign_magnitude"` (default; 1–63 clockwise, 65–127 counter-clockwise by value − 64, as the SMC-Mixer and Mackie surfaces do), `"twos_complement"` (127 = −1) or `"offset"` (64 = no move, above clockwise). |
| `step` | With `nudge`: the amount per tick, in the control's units (Kelvin for `wb.temp`). |
| `run` | A command id, run with no parameters. Goes with `note`, or `control_change` for a pedal or button that sends CC (runs on a non-zero value). |

Control ids are the Develop sliders (`crates/develop/src/controls.rs`); command ids are the ones in
[control-protocol.md](control-protocol.md). Unknown ids, out-of-range numbers, unknown keys and targets that
don't fit the source are reported as errors.

## Example: M-VAVE SMC-Mixer

[`midi/smc-mixer.toml`](midi/smc-mixer.toml), for the mixer in **DAW mode** (hold SHIFT and press `«` on the
device). Its MIDI behaviour is documented in the SMC-Mixer project's `docs/midi-notes.md`. CC mode is not
supported: its button CC numbers overlap the knobs and faders.

| Control | Message | Does |
|---|---|---|
| Fader 1–8 | pitch bend, channel 1–8 | `light.exposure`, `light.contrast`, `light.highlights`, `light.shadows`, `light.whites`, `light.blacks`, `effects.texture`, `effects.clarity` |
| Knob 1–4 | CC 16–19, relative | `wb.temp` (50 K per tick), `wb.tint`, `color.vibrance`, `color.saturation` |
| Knob 5–8 | CC 20–23, relative | `effects.dehaze`, `detail.sharpenAmount`, `detail.nrLuminance`, `detail.nrColor` |
| `«`, `»` | notes 46, 47 | `photo.rotateLeft`, `photo.rotateRight` |
| Left, Up | notes 98, 96 | `library.previous` |
| Right, Down | notes 99, 97 | `library.next` |
| Play | note 94 | `develop.auto` (Auto Settings) |
| Pause | note 93 | `develop.treatment` (toggles Black & White) |
| Record | note 95 | `photo.delete` (moves to Recently Deleted: undoable, the file stays) |

The strip buttons, the other transport buttons and the LEDs are not used. More examples are welcome.

Code: parse + map in `crates/engine/src/midi.rs` (`Map::from_toml`, `Map::action`), file lookup, reload and
port thread in `apps/lightcraft/src/midi.rs`, dispatch in `LightcraftApp::drain_midi` and the `midi.reload`
UI command (`crates/ui-egui/src/lib.rs`, `menus.rs`), tests in `tests_midi.rs`.

## Not done yet
Reloading when the file changes, an editing UI, MIDI learn, pickup mode, other value ranges (`min` / `max`,
invert), several maps per device, LED feedback (the SMC-Mixer takes Note On on its second output port), Linux.
