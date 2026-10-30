# Control surfaces

> **Last reviewed:** 2026-10-16 · **Last updated:** 2026-10-25 · **Change:** minor (Keys and Dials in a window of its own, opened from Settings ▸ Controls, which follows the Settings window's one size; the Dialpad drawn as the device; the Actions Ring set up in LightCraft with drag and drop; Touch to find; the Loupedeck+ drawn by zones and set up with no driver steps); previously minor (Erase while held; destructive keys need a hold; no Delete on built-in layouts; lit keys say why they are lit) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

Hardware dials, keys and buttons that drive LightCraft: the **Logitech MX Creative Console** (Keypad + Dialpad) first,
MIDI controllers and Tangent panels later. LightCraft owns the whole engine — layouts, what each control does, dial
speed and acceleration, pages that follow the tool you use, one undo step per dial turn, the on-screen readout — and a
thin plugin on the device's side only forwards ticks and presses and draws what LightCraft sends.

The engine is a port of **TactileBridge**, its author's own (never released) Capture One bridge for the MX Creative
Console, contributed under the MIT licence: Portions © RAD Films - Roman Alurkoff (from TactileBridge), MIT. Its
code, settings design, layouts, tuning values and hand-drawn icon pack came over; nothing specific to Capture One did
(no Capture One control lists, no reading of Capture One's files, no Tangent hub impersonation, no screen scraping for
the active tool, no simulated keystrokes): LightCraft knows its own state.

Code: [`crates/surface`](../crates/surface) (`lightcraft-surface`, L2, no UI), `crates/ui-egui/src/surface.rs` (the
app side), `crates/ui-egui/src/panels/controls.rs` (Settings ▸ Controls), `crates/ui-egui/src/panels/surface_osd.rs`
(the readout), `apps/lightcraft/src/control_server.rs` (the transport). The MX Creative Console plugin lives in its own
repository (`lightcraft-logi`, a Logi Actions SDK plugin in C#).

## Using it

1. **Settings ▸ Controls ▸ Use control surfaces.** LightCraft then listens on `127.0.0.1:7981` (the **Port** field
   under *Profiles and Setup*; loopback only) for surfaces. Off by default.
2. Install the surface's plugin. MX Creative Console: when Logi Options+ is on the computer and the plugin isn't,
   Settings ▸ Controls says so with an **Install the plugin** button (also under *Profiles and Setup*, with Update and
   Remove). LightCraft hands its copy of the plugin to Logi's own installer; **Logi Options+ asks you to confirm**
   ("Ready to install plugins" → Continue), installs it with its default profiles and reloads its plugin service by
   itself — a few seconds in which the console pauses. See [Installing the plugin](#installing-the-logi-plugin).
3. The plugin asks to pair: LightCraft shows "… asks to control LightCraft" and lists the request in Settings ▸
   Controls with a six-digit code; the keypad shows the same code. Check they match and click **Approve**. The plugin
   keeps the token it gets; **Forget** under *Paired* revokes it.
4. Pick a **profile** (Default, Wedding, Portrait, Fashion, Landscape, Real Estate, or your own).

What happens then:

- **Dials** move develop values (Exposure, Temp, a mask's sliders, the brush size, the zoom…). Ticks are gathered and
  applied once per *Apply movement every* (16 ms); turning fast accelerates on TactileBridge's curve. A turn is **one
  undo step**: it ends when the dial rests for *Turn ends after* (800 ms), when another dial starts, when a key runs a
  command, or when the photo changes.
- **Apply to Selected** (a key, or *Dials change every selected photo*) carries each turn to every selected photo,
  in that same single undo step (LightCraft's Auto Sync at the end of the gesture). This is TactileBridge's Auto Sync
  without the copy-and-paste round trip.
- **Keys** run commands (any engine or UI command, with parameters), switch pages or profiles, hold a modifier
  (*fine* ¼× / *big* 4× steps while held), reset the dial's value, or — bound to a value — put that value on the dial
  until pressed again (TactileBridge's adjustment dial toggle). A dial press resets its value.
- **Pages.** A profile's pages bind controls; keys and dials page separately (the Keypad and the Dialpad), and
  controls a page leaves free fall back to the profile's base bindings (the Dialpad's buttons). The MX Creative
  Keypad's own page buttons show key pages 1–3; the Dialpad's fourth button turns the dial pages in the built-in
  profiles.
- **Pages follow the app** (TactileBridge's tool switching): Crop puts the crop page on the keypad and Straighten on
  the dial; Masking shows mask tools, a selected mask its sliders, its brush the brush size and feather; Remove the
  spot size; a dialog OK / Cancel. Leaving the tool brings back the page you chose. Any page can be mapped to any
  context in Settings ▸ Controls ▸ *Pages that follow the app*.
- **The on-screen readout** (TactileBridge's OSD) shows the value being turned (name, value, a bar) or the command a
  key ran, at the top or bottom of the photo, with its opacity, colour and display time (`panels::surface_osd`;
  Settings ▸ Controls ▸ Readout and Look).
- **The surface** shows each key's icon (TactileBridge's hand-drawn pack), label and live value. A key is lit for one
  of three reasons, each drawn its own way by the plugin: its value is on the dial (`focus`: the white key), it is
  held down (`held`: a held modifier, or Erase), or a mode it switches is on (`on`: the brush, Before / After, Auto
  Sync …). One-shot keys (New Brush, Add / Subtract) are never lit.
- **Own icons** for functions that shared one of the pack's generic drawings (Invert Mask, Erase, New Brush,
  Previous / Next Mask, Mask On/Off, Zoom To Fit, Zoom 100%, Select All / None, Rotate Left / Right, Reset): drawn
  for LightCraft in the pack's style, next to the pack's originals, which stay as they are.
- **Erase works while held**, like Alt with the brush: `cmd:brush.erase` with `{on}` (a console key sends on at the
  press and off at the release; without `on` it toggles). The brush page has it, plus Brush Size and Feather keys.
- **Destructive keys need a hold.** Delete, Delete Permanently, Clear History, Delete All Masks, Reset, deleting a
  version or a preset run from a console key only after it is held for 0.7 s; a quick press runs nothing and the
  readout says *Hold the key: …*. The built-in layouts have no Delete key (their old Delete key clears the colour
  label). Deleting photos, from anywhere, says they went to Recently Deleted, that the files stay on disk, and that
  Undo brings them back. While LightCraft isn't running the keys keep the
  icons they last showed, dimmed. Logi Options+ shows the same keys in its editor.
- **LightCraft's actions in Logi Options+.** Besides the keys LightCraft sets, the plugin lists every LightCraft
  action and value in Options+'s actions panel, in numbered categories with their icons (*1. Light*, *2. Color* …,
  then *Rating & Labels*, *Browse & Select*, *Edit* …), so they can also be dragged onto keys, dials or the Actions
  Ring there. Values turn on a dial (one undo step per turn, pressing resets) and show their live value.

## Settings ▸ Controls

Built around the device, as Logi Options+ shows it. At the top: **Use control surfaces**, the **profile**, then any
pairing request (with its code, **Approve** / **Deny**) and, when Logi Options+ is installed without the plugin, an
**Install the plugin** button. Under it the **device bar** switches between surfaces, each with a status dot:

| Surface | What it shows |
|---|---|
| MX Creative Keypad | a drawing of the 3 × 3 keys and the two page buttons at its lower corners; connected / offline |
| MX Creative Dialpad | drawn as the device: buttons 1 and 2 top left, the roller top right, the big dial in the middle, button 3 bottom left, button 4 bottom right; connected / offline |
| Actions Ring | its eight slots around a ring (`ring.1`–`ring.8`, the same on every page), with undo, redo, pick, reject, previous / next photo, before / after and fit to start with; the plugin puts its pass-through on the ring's slots in Logi Options+ (`DefaultProfile72.lp5`) |
| Loupedeck+ | drawn in zones as on the console: the colour knobs P1–P8 (one colour mode at a time, Hue / Sat / Lum / B&W) with the mode buttons, the tone knobs, the Control Dial with the arrows, C1–C6 and L1–L3, then the buttons; a knob's press is the small chip at its corner; Loupedeck's profiles found on this computer to import (see [Loupedeck+](#loupedeck)) |
| Loupedeck Live / CT, MIDI controllers, Tangent panels | coming later |

The sections under the device bar:

| Section | What it holds |
|---|---|
| Keys and Dials (**Edit Keys and Dials…**, a resizable window of its own over Settings; picking a device in the bar opens it too) | page tabs (the device's pages, **All pages** for what a control does wherever a page leaves it empty, tool pages in a menu); the drawing — every control shows its icon and name, a control a page leaves empty shows the All pages one dimmed; click a control, drag an action from the list onto it, or **touch it on the device** (*Touch to find*, on while Settings ▸ Controls or this window is open: the control touched is selected here, and the device doesn't change the photo meanwhile; it acts again the moment they close; switched off, **Learn** does it once) and its editor opens beside the drawing: what it does now, **Clear**, a value's own speed and acceleration, a command's JSON parameters (advanced), and the **action picker** — every action in categories with icons and a search box (keys list their one-press actions first, then the values they can put on the dial; dials list values and paging); show a page, switch profile and every other command of the app are there too. Narrow windows put the editor under the drawing |
| Pages and Tools | each page's groups (Keys / Dials) and *Tool page*, Rename / Delete, Add page; *Pages that follow the app*: on / off, one page per context |
| Dial Feel | global speed, acceleration, fine and big step factors, invert; one value's speed, acceleration, slow / fast turning (floor / ceiling), fast / slow thresholds, invert; TactileBridge's presets for every value (Wedding, Real Estate, Fashion, Landscape) |
| Readout and Look | apply to every selected photo, when a turn ends, how often movement is applied, steady readout while turning; the on-screen readout (on / off, for key presses too, opacity, display time, position, background colour); keys: values on the device, haptics, key colours |
| Profiles and Setup | the Logi Options+ plugin (found / installed version / connected; Install, Update, Reinstall, Remove); profiles (Duplicate / Rename / Delete, Add the built-in profiles, **Bring over my TactileBridge setup** when `%LOCALAPPDATA%\TactileBridge\settings.json` exists, Import… `settings.json` / `.lp5` / `.lcsurface`, Export… `.lcsurface`, the last import's report); connection (port, connected surfaces, paired surfaces with Forget) |

Profiles are shared by the Keypad and the Dialpad (one MX Creative Console); surfaces that come later get their own.

### Loupedeck+

The Loupedeck+ (knobs, the Control Dial and buttons, no displays; Loupedeck's device type `Loupedeck15`) works through
the same plugin. Its controls are Loupedeck's own numbers: `ld.r<n>` turns (P1–P8 send 1–8, 9–16, 17–24 or 25–32 by
the console's Hue / Sat / Lum / B&W mode; the labelled knobs 33–47; the Control Dial 48) and `ld.b<n>` presses (a
knob's press is its number; C1–C6 49–54; the rating keys 66–70, with Fn 71–75; the arrows 78 / 79; P1–P8's presses
80–87; Copy, Paste, Undo, Redo, Screen Mode, Hue, Sat, Lum, Col/BW and Before/After 92–102; Custom Mode 110; L1–L3
114–116). The plugin's `DefaultProfile15.lp5` forwards every one of them, and is picked whenever LightCraft is the app
in front. The Logi service uses a plugin's default profile only when it first makes the app's profile, so a LightCraft
profile made by an older plugin stays empty and nothing reached LightCraft until each control was assigned by hand in
the driver: the plugin (1.0.7) fills such a profile with its pass-through actions when it loads, before the service
reads it (controls with no action, or one of LightCraft's own value actions assigned as that workaround; any other
action stays; the original is kept in `PluginDataLightCraftprofile-backups`). The Actions Ring's profile gets the
same.

**No setup:** a control nothing binds does what its label says (Loupedeck's own default layout: the Exposure knob
moves Exposure, P1–P8 the colour mixer, Undo undoes, the rating keys rate …). **Import:** Settings ▸ Controls ▸
Loupedeck+ lists the profiles Loupedeck's software keeps for the console on this computer
(`%LOCALAPPDATA%\Loupedeck\Applications\Loupedeck15\<app>\Profiles\<id>\ProfileInfo.json`, read only) with an
**Import** button each, and **Import a Loupedeck profile…** takes a `ProfileInfo.json` or an exported `.lp5`;
`cmd:surface.import` takes either too. The editor mode's layout (Loupedeck's `loupe` mode: its first workspace, then
the Fn workspace for what that leaves empty) goes into every LightCraft profile (the console has no pages), replacing
an earlier import; presets match LightCraft presets by name. Only the mapping is read: no icons or images. The report
lists what was left out (Loupedeck macros, presets LightCraft doesn't have, Loupedeck's own Custom Mode button, the
Hue / Sat / Lum mode buttons). Code: `crates/surface/src/loupedeck.rs`.

### Installing the Logi plugin

The plugin is built in its own repository (`lightcraft-logi`: `scripts/package.ps1` → `dist/LightCraft.lplug4`, a
ZIP of the plugin DLL, its `metadata/` and its default profiles `profiles/DefaultProfile70.lp5` / `…71.lp5`).
LightCraft looks for **`LightCraft.lplug4` next to its executable** (or in `plugins/` beside it), or at
`LIGHTCRAFT_LOGI_PLUGIN`. *Install the plugin* (`cmd:surface.installPlugin`) writes the plugin's
`%LOCALAPPDATA%\Logi\LogiPluginService\PluginData\LightCraft\config.json` (the port LightCraft listens on) and
runs `%ProgramFiles%\Logi\LogiPluginService\LogiPluginServiceTool.exe install-plugin LightCraft.lplug4` — what
double-clicking a `.lplug4` does. Logi Options+ then asks the user to confirm, installs the plugin into
`…\LogiPluginService\Plugins\LightCraft` and its profiles, and reloads its plugin service (Logi restarts it; the
devices pause for a few seconds). *Remove the plugin* (`cmd:surface.uninstallPlugin`) runs `uninstall-plugin` the same
way. `cmd:surface.plugin` reports `{status: {host, installed, package, packagePath}, busy, message}`. Nothing else in
Logi Options+ is touched: other plugins, their profiles and the user's Options+ settings stay as they are. Windows
only for now; elsewhere the page says to open `LightCraft.lplug4` by hand.

**Releases (maintainer decision).** A release that ships the plugin builds `LightCraft.lplug4` from a pinned
`lightcraft-logi` commit (a .NET build on Windows) and puts it next to `lightcraft.exe` in the Windows package (and
the macOS app's `Contents/MacOS` once the macOS path is verified). Without it LightCraft runs as before and the page
says no package came with it. Publishing the plugin on the Logi Marketplace instead is the other option; then
LightCraft would only link to it.

Every action of the editor is also a command (`cmd:surface.profile`, `cmd:surface.page`, `cmd:surface.pageNext`,
`cmd:surface.pagePrevious`, `cmd:surface.applyToSelected`, `cmd:surface.followContext`, `cmd:surface.import`,
`cmd:surface.export`, `cmd:surface.settings` (read or replace the whole settings), `cmd:surface.state`,
`cmd:surface.input` (the input a surface sends)), plus the commands keys need: `cmd:surface.maskPrevious`,
`cmd:surface.maskNext`, `cmd:surface.dialogConfirm`, `cmd:surface.dialogCancel`.

## Settings file

`control-surfaces.json` in LightCraft's settings folder (next to `ui.json`; `LIGHTCRAFT_CONFIG_DIR` moves it). Written
atomically half a second after a change. A damaged file, or one from a newer LightCraft, is reported (Settings ▸
Controls and a notice), kept as it is, and LightCraft runs on the defaults until you change something. Every number is
clamped into range on reading.

```jsonc
{
  "version": 3,                 // format version
  "enabled": true,              // surfaces may connect (the listener runs)
  "port": 7981,
  "profile": "Wedding",         // the profile in use
  "followContext": true,        // pages follow the app
  "applyToSelected": false,     // turns go to every selected photo
  "motion": {"speed": 1.0, "acceleration": 1.0, "invert": false, "fine": 0.25, "coarse": 4.0},
  "tuning": {                   // per value; absent = defaults
    "wb.temp": {"speed": 0.3, "acceleration": 1.0, "accelerationMin": 1.0, "accelerationMax": 10.0,
                "fastMs": 20, "slowMs": 200, "invert": false}
  },
  "transport": {"flushMs": 16, "echoSuppression": true, "echoSuppressionMs": 100, "gestureEndMs": 800},
  "osd": {"enabled": true, "opacity": 0.9, "background": "#202020", "displayMs": 1800,
          "position": "bottom", "showCommands": true},
  "look": {"background": "#000000", "foreground": "#FFFFFF", "showValues": true, "haptics": true},
  "profiles": [{
    "name": "Wedding",
    "description": "…",
    "base": {"pad.1": {"kind": "command", "command": "develop.copy"},
             "pad.4": {"kind": "pageNext", "group": "dials"}},
    "pages": [
      {"name": "Cull", "groups": ["keys"], "bindings": {
         "key.1": {"kind": "command", "command": "library.previous"},
         "key.4": {"kind": "command", "command": "photo.rate", "params": {"rating": 1}}}},
      {"name": "Exposure/WB", "groups": ["dials"], "bindings": {
         "dial": {"kind": "value", "target": "light.exposure"}, "roller": {"kind": "value", "target": "wb.temp"}}},
      {"name": "Crop", "auto": true, "bindings": {"dial": {"kind": "value", "target": "crop.angle"}}}
    ],
    "contexts": {"default": ["Cull", "Exposure/WB"], "crop": ["Crop"]}
  }],
  "paired": [{"client": "lightcraft-logi", "name": "LightCraft plugin for Logi Options+",
              "tokenSha256": "…64 hex digits…", "approvedAt": 1792000000}]
}
```

- **Bindings** (`kind`): `value {target}`, `command {command, params?}`, `modifier {modifier: fine|coarse}`,
  `pageNext {group?: keys|dials}`, `pagePrevious {group?}`, `page {page}`, `profile {profile}`, `profileNext`, `reset`,
  `applyToSelected`, `followContext`.
- **Targets**: develop control ids (`develop.controls` lists them; indexed ones such as `pointColor.0.hueShift` too);
  `mask.<field>` for the selected mask (`exposure`, `contrast`, `highlights`, `shadows`, `whites`, `blacks`, `temp`,
  `tint`, `texture`, `clarity`, `dehaze`, `hue`, `saturation`, `sharpness`, `noise`, `moire`, `defringe`, `amount`);
  `ui.brushSize`, `ui.brushFeather`, `ui.brushFlow`, `ui.removeSize`, `ui.removeFeather`, `ui.removeOpacity`,
  `ui.zoom` (tool and view values, no undo step).
- **Pages**: `groups` empty = keys and dials; `auto: true` = a tool page, shown only by its context (paging by hand
  skips it).
- **Contexts**, most specific first; a context falls back to its parents, then `default`:

| Context | When |
|---|---|
| `dialog.<kind>` | a dialog is open (`dialog.export`, `dialog.import`, `dialog.settings`, …; the dialog's `kind` as `ui.inspect` shows it) |
| `library` | the photo grid, Survey or People (or no photo open) |
| `develop`, `develop.<section>` | Detail; the Edit panel's section opened last (`light`, `color`, `effects`, `detail`, `optics`, …) |
| `crop`, `remove`, `redeye` | those tools |
| `masking`, `masking.mask`, `masking.mask.brush`, `masking.mask.gradient` | Masking; a mask selected; its brush or gradient tool |

A **layout file** (`.lcsurface`) is one profile as JSON (the `profiles[]` element above).

## The surface protocol

Version **1**. JSON lines on a loopback TCP port, the same framing and rules as the
[control protocol](control-protocol.md): one request object per line, `{"id", "method", "params"}`, one reply per
line, `{"id", "ok": true, "result"}` or `{"id", "ok": false, "error"}`. The control-surface port (Settings ▸ Controls,
`7981`) serves only `surface.*` methods; an app started with `--control PORT` serves them there too, beside everything
else. Between replies, a subscribed connection receives **events**: lines with an `event` field and no `id`. A line is
never split between a reply and an event.

**Pairing.** The control port has no authentication, so surfaces pair: only a paired connection may subscribe or send
input. `surface.pair` creates a request with a six-digit code; the user approves it in Settings ▸ Controls; polling
`surface.pair {request}` then returns a 64-hex-digit token, once. Every later connection presents it in
`surface.hello`. LightCraft stores only its SHA-256; re-pairing the same client replaces its token; Forget revokes it
and tells connected surfaces (`surface.unpaired`). Requests expire after 10 minutes; at most four wait at once.
In-process requests (headless scripts, tests) count as paired.

| Method | Params | Result | Paired? |
|---|---|---|---|
| `surface.hello` | `{protocol: 1, client, name?, token?}` | `{protocol, app, version, paired, enabled, devices: [{id, name, controls: [{id, kind, label}]}]}`; a valid token pairs this connection | no |
| `surface.pair` | `{client, name}` → a new request (one the same client id had waiting is withdrawn); `{request}` → poll | `{status: "pending", request, code}` / `{status: "approved", token}` (once) / `{status: "denied"}` | no |
| `surface.icons` | `{names?: [..]}` (all when omitted) | `{name: svg}` | no |
| `surface.catalog` | — | `{groups: [..], entries: [{key, group, label, icon, value}]}`: every action in categories, labels in the app's language (`lightcraft_surface::catalog`); `key` is stable (`v.light.exposure`, `c.photo.rate.rating-5`, `s.applyToSelected`) | no |
| `surface.subscribe` | `{device?: "mx-creative-console"}` | `{layout}`; then `surface.layout` events whenever it changes | yes |
| `surface.unsubscribe` | — | — | yes |
| `surface.layout` | `{device?}` | the layout now | yes |
| `surface.input` | `{events: [{control, ticks}` \| `{control, press: true\|false, page?}` \| `{action, ticks}` \| `{action, press}]}` (at most 256) | `{effects}` | yes |
| `surface.watch` | `{targets: ["light.exposure", …]}` (at most 128; after `subscribe`) | — ; the pushed layout then carries `values: {target: {value, position}}` | yes |

`surface.input` is pipelined: LightCraft reads the next request on the connection without waiting for the reply to
this one, so a fast turn sent a tick per request never queues up one tick per frame in the socket (it used to: a busy
app took seconds to catch up, then "replayed" the turn and the key presses behind it). The app still runs a
connection's requests in order and answers them in order; every other method is answered before the next request is
read. Ticks are summed and applied once per frame; a key press runs at once (what the dial gathered goes first); a
dial turned back follows the new way at once (the old way's ticks not applied yet are dropped and its acceleration
starts again from the floor). Value feedback is pushed as events, never awaited.

Errors: an unpaired connection gets *this surface isn't paired …*; switched off: *control surfaces are switched off*;
a newer client protocol: *this LightCraft speaks surface protocol 1*. Junk input is refused with an error, never
acted on: `ticks` must be a number (clamped to ±1000 per event), `page` 1–64, control ids are trimmed to 64 characters.

**Events**: `{"event": "surface.layout", "layout": {…}}` (a subscriber's device layout changed: a value, a page, the
context, a toggle — sent at most once per frame, and held back while the turned value hasn't caught up with the dial),
`{"event": "surface.unpaired"}` (the user forgot this surface; the connection should pair again).

**Layout** (`lightcraft_surface::layout::DeviceLayout`):

```jsonc
{
  "device": "mx-creative-console", "profile": "Default", "context": "crop",
  "keyPage": "Crop", "dialPage": "Crop",
  "keys":    [{"control": "key.1", "binding": {"kind": "command", "command": "crop.rotateAspect"},
               "label": "Crop Orient.", "icon": "switch-crop-orientation", "value": null, "active": false}, …],
  "keyPages": [{"name": "Page 1", "keys": [ … nine keys … ]}, … at least three …],
  "buttons": [ … pad.1–pad.4 … ],
  "dials":   [{"control": "dial", "binding": {"kind": "value", "target": "crop.angle"}, "label": "Straighten",
               "icon": "straighten-tool", "value": "+1.25", "position": 0.51}, …],
  "focus": null, "applyToSelected": false, "followContext": true,
  "background": "#000000", "foreground": "#FFFFFF", "showValues": true, "haptics": true
}
```

Labels and values come translated and formatted; `icon` names an SVG from `surface.icons`. `keyPages[n-1]` is what a
keypad with its own page buttons shows on its page *n* (the context's page on every one while a tool page is active);
such a keypad sends `page` with its presses.

**Actions** (`{action: key}` events) come from a plugin's own list of LightCraft's actions (`surface.catalog`): turning
a value moves it (one undo step per turn, with the value's dial feel), pressing a value resets it, pressing anything
else does what a key bound to it does (a modifier holds until the action is released). An unknown key is refused.

**Devices and controls.** `mx-creative-console`: `key.1`–`key.9` (Keypad LCD keys, row by row), `dial` (the Dialpad's big dial, Logi encoder 1), `roller` (Logi encoder 0),
`pad.1`–`pad.4` (Dialpad buttons). A dial press is `{control: "dial", press: true}`; it runs `dial.press` if a page
binds it, else resets the dial's value. Logi's device types: `Loupedeck70` = MX Creative Keypad, `Loupedeck71` = MX
Creative Dialpad, `Loupedeck72` = the Actions Ring (TactileBridge's `…70` / `…71` / `…72` profile variants).

## Coming next: MIDI and Tangent

The engine is device-neutral: controls are string ids, a device is a list of them with their kind (encoder, button,
LCD key) and page group. What each still needs:

- **MIDI**: an input backend that fits the pure-Rust rule (the common `midir` crate links ALSA's C library on Linux); a `midi` device whose controls are `midi.cc.<channel>.<n>` (relative encoders: two's-complement and
  binary-offset modes; absolute faders as values) and `midi.note.<channel>.<n>` (buttons); learn mode already reports
  any control id; feedback to motor faders / LED rings from the pushed layout (`position`).
- **Tangent** (Wave, Element, Ripple): written from Tangent's **public** developer documentation only (the TUBE /
  Hub API); a `tangent` device mapping panel controls to ids, the same layouts and protocol. TactileBridge's Tangent
  hub impersonation for Capture One is not part of LightCraft and is not used.
- Both reuse pairing (an in-process client counts as local), profiles, contexts, motion and the readout as they are.

## From TactileBridge

Settings ▸ Controls ▸ **Bring over my TactileBridge setup** (or `surface.import {path}` with its `settings.json`)
brings over the dial feel (global and per value), the readout, the key colours, haptics and LED switches, automatic
switching, Auto Sync and every tool context, as a new profile named *TactileBridge* (made active; your other profiles
stay). Import… takes TactileBridge's Logi profiles (`.lp5`) too. The report lists what came over and what was left
out, and why. Tuning is kept as it was; LightCraft's slider units differ from Capture One's in places (Contrast is
±100 per 1, Temp 50 K per tick at speed 1), so a value may want its speed retuned.

### Every TactileBridge feature and setting

✅ ported · 🔁 done natively by LightCraft · ✕ dropped (Capture One–specific or not needed)

| TactileBridge | LightCraft | |
|---|---|---|
| Dial acceleration curve (`GetAcceleration`: floor, ceiling, fast / slow thresholds, quadratic ramp) | `lightcraft_surface::motion` | ✅ |
| Global speed, acceleration, invert (`Controls.*`) | `motion.speed`, `motion.acceleration`, `motion.invert`; Settings ▸ Controls ▸ Dial Feel | ✅ |
| Per-adjustment tuning (`AdjustmentProfiles`: speed, acceleration, min, max, fast ms, slow ms, invert) | `tuning.<target>`; Dial Feel ▸ Value | ✅ (keys mapped to LightCraft targets) |
| Workflow presets (Wedding, Real Estate, Fashion, Landscape) | Dial Feel ▸ Presets for every value | ✅ (Landscape's 18× ceiling is 15×, as TactileBridge's own clamp made it) |
| Echo suppression (`EnableEchoSuppression`, `EchoSuppressionMs`) | `transport.echoSuppression(Ms)`: pushes held while the turned value catches up | ✅ |
| Flush interval (`Bridge.FlushIntervalMs`, defined but unused in TactileBridge) | `transport.flushMs`: ticks coalesced, applied once per interval | ✅ (implemented) |
| Idle reconcile, state retention, periodic value poll, coalescing diagnostics (`Bridge.*`) | — | 🔁 LightCraft's values are authoritative and pushed on change: nothing to poll or reconcile |
| On-screen display (`OsdOverlay`: pill, title, value, bar, icon; enabled, opacity, colour, display time, 4 positions, fade) | `panels::surface_osd` in the window | ✅ (with the icon) |
| Plugin colours (`PluginColors`) | `look.background`, `look.foreground`, pushed live | ✅ (no plugin redeploy needed) |
| Haptic feedback switch | `look.haptics`: the plugin raises Logi plugin events (key pressed, value reset, value at its limit) Options+ can map to haptics | ✅ (TactileBridge had the switch only) |
| LED value indicator | `look.showValues` | ✅ |
| Tool contexts (`ToolContext.Contexts`: 9 keys + up to 4 encoders per Capture One tool) | profile pages with `auto: true`, mapped to contexts | ✅ (Crop → `crop`; Draw / Erase Mask → `masking.mask.brush`; Draw Healing Mask → `remove`; Direct Color Editor → `develop.color`; Import / Export / Process → `dialog.import` / `dialog.export`) |
| Automatic tool switching (`ToolStateAutomation`: UI Automation + BitBlt scan for the orange tool highlight; dialog detection by window title) | contexts read from LightCraft's own state | 🔁 (`followContext`) |
| Adjustment dial toggle on a pad key (focus, lit rounded border) | a key bound to a value; the plugin draws the lit border | ✅ |
| Modifier keys (keystrokes with Ctrl / Shift sent to Capture One) | commands run directly; new *hold for fine / big steps* modifiers | 🔁 / ✅ |
| Auto Sync (debounced Copy + Apply after the dial rests; `IdleTimeoutMs`, `CopyApplyDelayMs`; the AutoSync key) | Apply to Selected: the gesture's change goes to every selected photo in its one undo step; `gestureEndMs` | 🔁 (copy-apply delay dropped) |
| Dial press resets the parameter | dial press resets its value (or runs `dial.press`) | ✅ |
| Display smoothing on the dial tile | — | ✕ the plugin shows LightCraft's value as pushed |
| Kelvin conversion (`KelvinConvert`) | LightCraft's own Kelvin scale | 🔁 |
| Curated profiles (Default, Wedding, Portrait, Fashion, Landscape, Real Estate; `…70` Keypad, `…71` Dialpad, `…72` Actions Ring) | built-in profiles (`lightcraft_surface::defaults`): key pages from `70`, dial pages and buttons from `71` (`72` repeats them) | ✅ (the Dialpad's fourth button turns the dial pages; its TactileBridge action is on a key page, except Fashion's *Layer on/off*, which the Brush and Mask tool pages have) |
| Default Options+ profiles shipped by the plugin | the LightCraft plugin ships one Keypad and one Dialpad profile of generic keys | ✅ (layouts live in LightCraft, so nothing to assign in Options+) |
| Icon pack (`actionicons` = `actionsymbols` = `customicons`, 336 SVGs) | `assets/surface-icons` (335, kebab-case names) | ✅ (the duplicate `Clarity.min.svg` left out; the Keystone icons since 2026-10-16) |
| Icon map (`CaptureOneIconMap`) | `lightcraft_surface::icons::icon_for` | ✅ |
| Settings window (`SettingsForm`: Global, Per Adjustment with filter, Bridge, Hardware, OSD, Plugin Colors, Tool Context, Tool Switching tabs) | Settings ▸ Controls | ✅ (the Redeploy Plugin button is gone: changes apply live) |
| Capture One actions and parameters (`CaptureOneCommand`, `CaptureOneAdjustment`) | LightCraft commands and targets, table below | ✅ / ✕ per action |
| Color Editor values via UI Automation (`CE*`) | colour mixer targets (`mixer.<band>.<hue/sat/lum>`) | 🔁 |
| Mode buttons switching the dial (SPEC 7.1) | `pageNext {group: dials}` on a button | ✅ |
| Tangent TIPC hub impersonation (`TipcBridge`, `TipcProtocol`, `TipcConnection`, `CaptureOneConfig`) | — | ✕ Capture One–specific |
| Capture One control list (`ControlDatabase`, `controls.xml`), inspection tools (`_inspect`, `temp_inspect*`, `reflect_lr.ps1`, `inspect_sdk*`) | — | ✕ never ported |
| Keyboard simulation (`KeyboardSimulator`) | — | ✕ commands instead |
| Named-pipe bridge (`PipeServer`, `BridgeClient`, `PipeProtocol`) | the surface protocol on the control channel | 🔁 |
| Tray app, hub window, telemetry, logs (`Program`, `HubWindow`, `BridgeTelemetry`, `Log`) | LightCraft's own log and Settings ▸ Controls status | 🔁 |
| Production plans (installer, auto-update, crash reporting, licensing, code signing, Windows service) | — | ✕ LightCraft has its own release process |
| Branding and design references (`media/`, `design-mockups/`) | — | ✕ not shipped |

### Every TactileBridge action

Generated from `lightcraft_surface::tactilebridge::mapping_table()` (its tests check that every target exists and
every dropped action says why).

**Dial values**

| TactileBridge | LightCraft | Note |
|---|---|---|
| `Kelvin` | `wb.temp` |  |
| `Tint` | `wb.tint` |  |
| `Exposure` | `light.exposure` |  |
| `Contrast` | `light.contrast` |  |
| `Brightness` | — | Lightroom and LightCraft have no Brightness slider (use Exposure, Whites or the Tone Curve) |
| `Saturation` | `color.saturation` |  |
| `Highlight` | `light.highlights` |  |
| `Shadow` | `light.shadows` |  |
| `White` | `light.whites` |  |
| `Black` | `light.blacks` |  |
| `MasterToneX` | `grading.global.hue` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `MasterToneY` | `grading.global.sat` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `ShadowToneX` | `grading.shadows.hue` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `ShadowToneY` | `grading.shadows.sat` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `ShadowLightness` | `grading.shadows.lum` |  |
| `MidtoneToneX` | `grading.midtones.hue` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `MidtoneToneY` | `grading.midtones.sat` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `MidtoneLightness` | `grading.midtones.lum` |  |
| `HighlightToneX` | `grading.highlights.hue` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `HighlightToneY` | `grading.highlights.sat` | Color Grading uses hue and saturation where Capture One's Color Balance uses x / y |
| `HighlightLightness` | `grading.highlights.lum` |  |
| `BWRed` | `bw.red` |  |
| `BWYellow` | `bw.yellow` |  |
| `BWGreen` | `bw.green` |  |
| `BWCyan` | `bw.aqua` | Cyan is called Aqua in the B&W mixer |
| `BWBlue` | `bw.blue` |  |
| `BWMagenta` | `bw.magenta` |  |
| `BWHighlightsHue` | `grading.highlights.hue` | B&W split toning is done with Color Grading |
| `BWHighlightsSat` | `grading.highlights.sat` | B&W split toning is done with Color Grading |
| `BWShadowsHue` | `grading.shadows.hue` | B&W split toning is done with Color Grading |
| `BWShadowsSat` | `grading.shadows.sat` | B&W split toning is done with Color Grading |
| `Clarity` | `effects.clarity` |  |
| `Structure` | `effects.texture` | Texture is the closest to Capture One's Structure |
| `Vignetting` | `vignette.amount` | post-crop vignette |
| `Distortion` | `optics.distortion` |  |
| `SharpnessFalloff` | — | LightCraft has no sharpness falloff correction |
| `LightFalloff` | `optics.vignetting` | lens vignetting (light falloff) correction |
| `Defringe` | `optics.defringePurple` | purple fringe amount |
| `UniformLight` | — | a Capture One feature LightCraft does not have |
| `SharpeningAmount` | `detail.sharpenAmount` |  |
| `SharpeningRadius` | `detail.sharpenRadius` |  |
| `SharpeningThreshold` | `detail.sharpenMasking` | Masking is the closest to a sharpening threshold |
| `SharpeningHalo` | — | LightCraft has no halo suppression slider |
| `NRLuminance` | `detail.nrLuminance` |  |
| `NRDetail` | `detail.nrDetail` |  |
| `NRColor` | `detail.nrColor` |  |
| `NRSinglePixel` | — | LightCraft has no single-pixel noise slider |
| `GrainImpact` | `grain.amount` |  |
| `GrainGranularity` | `grain.size` |  |
| `MoireAmount` | `mask.moire` | moiré is a mask adjustment in LightCraft (as in Lightroom) |
| `MoirePattern` | — | LightCraft's moiré reduction has no pattern size |
| `LayerOpacity` | `mask.amount` | a mask's Amount |
| `SpotRadius` | `ui.removeSize` | the Remove brush size |
| `BrushSize` | `ui.brushSize` |  |
| `BrushHardness` | `ui.brushFeather` | Feather (softness) — turn the other way for harder |
| `BrushOpacity` | — | LightCraft brushes have Size, Feather and Flow |
| `BrushFlow` | `ui.brushFlow` |  |
| `Zoom` | `ui.zoom` |  |
| `OverlayOpacity` | — | a Capture One feature LightCraft does not have |
| `OverlayScale` | — | a Capture One feature LightCraft does not have |
| `OverlayHorizontal` | — | a Capture One feature LightCraft does not have |
| `OverlayVertical` | — | a Capture One feature LightCraft does not have |
| `PenSize` | — | LightCraft has no annotations |
| `CERedHue` | `mixer.red.hue` |  |
| `CERedSat` | `mixer.red.sat` |  |
| `CERedLight` | `mixer.red.lum` |  |
| `CEOrangeHue` | `mixer.orange.hue` |  |
| `CEOrangeSat` | `mixer.orange.sat` |  |
| `CEOrangeLight` | `mixer.orange.lum` |  |
| `CEYellowHue` | `mixer.yellow.hue` |  |
| `CEYellowSat` | `mixer.yellow.sat` |  |
| `CEYellowLight` | `mixer.yellow.lum` |  |
| `CEGreenHue` | `mixer.green.hue` |  |
| `CEGreenSat` | `mixer.green.sat` |  |
| `CEGreenLight` | `mixer.green.lum` |  |
| `CECyanHue` | `mixer.aqua.hue` | Cyan is called Aqua in the colour mixer |
| `CECyanSat` | `mixer.aqua.sat` | Cyan is called Aqua in the colour mixer |
| `CECyanLight` | `mixer.aqua.lum` | Cyan is called Aqua in the colour mixer |
| `CEBlueHue` | `mixer.blue.hue` |  |
| `CEBlueSat` | `mixer.blue.sat` |  |
| `CEBlueLight` | `mixer.blue.lum` |  |
| `CEPurpleHue` | `mixer.purple.hue` |  |
| `CEPurpleSat` | `mixer.purple.sat` |  |
| `CEPurpleLight` | `mixer.purple.lum` |  |
| `CEMagentaHue` | `mixer.magenta.hue` |  |
| `CEMagentaSat` | `mixer.magenta.sat` |  |
| `CEMagentaLight` | `mixer.magenta.lum` |  |

**Keys**

| TactileBridge | LightCraft | Note |
|---|---|---|
| `ClearRating` | `cmd:photo.rate` `{"rating":0}` |  |
| `Rate1Star` | `cmd:photo.rate` `{"rating":1}` |  |
| `Rate2Stars` | `cmd:photo.rate` `{"rating":2}` |  |
| `Rate3Stars` | `cmd:photo.rate` `{"rating":3}` |  |
| `Rate4Stars` | `cmd:photo.rate` `{"rating":4}` |  |
| `Rate5Stars` | `cmd:photo.rate` `{"rating":5}` |  |
| `ColorTagNone` | `cmd:photo.label` `{"label":"none"}` |  |
| `ColorTagRed` | `cmd:photo.label` `{"label":"red"}` |  |
| `ColorTagOrange` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `ColorTagYellow` | `cmd:photo.label` `{"label":"yellow"}` |  |
| `ColorTagGreen` | `cmd:photo.label` `{"label":"green"}` |  |
| `ColorTagBlue` | `cmd:photo.label` `{"label":"blue"}` |  |
| `ColorTagPink` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `ColorTagPurple` | `cmd:photo.label` `{"label":"purple"}` |  |
| `PreviousImage` | `cmd:library.previous` |  |
| `NextImage` | `cmd:library.next` |  |
| `FirstImage` | — | no LightCraft command jumps to the first or last photo yet |
| `LastImage` | — | no LightCraft command jumps to the first or last photo yet |
| `SelectAll` | `cmd:library.selectAll` |  |
| `DeselectAll` | `cmd:library.selectNone` |  |
| `InvertSelection` | — | no LightCraft command inverts the selection yet |
| `SelectPrimaryOnly` | — | a Capture One feature LightCraft does not have |
| `DeselectPrimary` | — | a Capture One feature LightCraft does not have |
| `SelectSameColorTag` | — | a Capture One feature LightCraft does not have |
| `SelectSameSequenceId` | — | a Capture One feature LightCraft does not have |
| `SelectSameVariantPos` | — | a Capture One feature LightCraft does not have |
| `PreviousSet` | — | a Capture One feature LightCraft does not have |
| `NextSet` | — | a Capture One feature LightCraft does not have |
| `PreviousCollection` | — | a Capture One feature LightCraft does not have |
| `NextCollection` | — | a Capture One feature LightCraft does not have |
| `Undo` | `cmd:edit.undo` |  |
| `Redo` | `cmd:edit.redo` |  |
| `ResetAdjustments` | `cmd:develop.reset` |  |
| `CopyAdjustments` | `cmd:develop.copy` |  |
| `ApplyAdjustments` | `cmd:develop.paste` |  |
| `CopyAndApplyAdj` | `cmd:develop.sync` | copies the active photo's settings to the other selected photos |
| `AutoAdjust` | `cmd:develop.auto` |  |
| `RotateLeft` | `cmd:photo.rotateLeft` |  |
| `RotateRight` | `cmd:photo.rotateRight` |  |
| `SnapTo90` | `cmd:develop.resetControl` `{"control":"crop.angle"}` | straightens back to 0° |
| `NewEmptyLayer` | `cmd:mask.add` `{"kind":"brush"}` | a new brush mask |
| `NewFilledLayer` | `cmd:mask.add` `{"center":[0.5,0.5],"feather":0.0,"kind":"radial","rx":1.0,"ry":1.0}` | a new mask over the whole photo |
| `NewCloneLayer` | `cmd:panel.remove` | the Remove tool (heal and clone) |
| `NewHealLayer` | `cmd:panel.remove` | the Remove tool (heal and clone) |
| `RemoveLayer` | `cmd:mask.delete` | deletes the selected mask |
| `SelectLayerUp` | `cmd:surface.maskPrevious` | selects the mask above |
| `SelectLayerDown` | `cmd:surface.maskNext` | selects the mask below |
| `SelectBackground` | `cmd:mask.select` `{"id":null}` | deselects the mask |
| `ToggleLayerOnOff` | `cmd:mask.visible` | shows or hides the selected mask |
| `InvertMask` | `cmd:mask.invert` |  |
| `FillMask` | — | a Capture One feature LightCraft does not have |
| `ClearMask` | — | a Capture One feature LightCraft does not have |
| `RasterizeMask` | — | a Capture One feature LightCraft does not have |
| `RefineMask` | — | Refine runs on a mask's own component in LightCraft |
| `FeatherMask` | — | a Capture One feature LightCraft does not have |
| `LumaRange` | `cmd:mask.add` `{"kind":"luminanceRange"}` | a new luminance range mask |
| `ToggleSelectionPoints` | `cmd:view.maskPins` | mask pins |
| `ZoomIn` | `cmd:view.zoomIn` |  |
| `ZoomOut` | `cmd:view.zoomOut` |  |
| `ZoomToFit` | `cmd:view.zoomFit` |  |
| `Zoom100` | `cmd:view.zoom100` |  |
| `Zoom200` | `cmd:view.navigate` `{"zoom":{"percent":200}}` |  |
| `Zoom400` | `cmd:view.navigate` `{"zoom":{"percent":400}}` |  |
| `IncreaseZoom` | `cmd:view.zoomIn` |  |
| `DecreaseZoom` | `cmd:view.zoomOut` |  |
| `ToggleMultiView` | `cmd:view.survey` | Survey view |
| `ToggleExposureWarn` | `cmd:view.clipping` | clipping warnings |
| `ToggleFocusMask` | — | LightCraft has no focus mask yet |
| `ToggleOverlay` | — | a Capture One feature LightCraft does not have |
| `ToggleBeforeAfter` | `cmd:view.beforeAfter` |  |
| `ShowGuides` | `cmd:view.cropOverlay` | cycles the crop overlay |
| `ShowGrid` | `cmd:view.cropOverlay` | cycles the crop overlay |
| `RecipeProofing` | `cmd:view.softProof` | soft proofing |
| `FullScreen` | `cmd:view.fullScreenPreview` |  |
| `SlideShow` | `cmd:view.slideshow` |  |
| `ShowHideViewer` | — | a Capture One feature LightCraft does not have |
| `ShowHideBrowser` | `cmd:view.filmstrip` | the filmstrip |
| `ToggleTools` | `cmd:panel.edit` | opens or closes the Edit panel |
| `SelectTool` | `cmd:tool.none` | no tool: click and drag pans |
| `MoveTool` | `cmd:tool.none` | no tool: click and drag pans |
| `PanTool` | `cmd:tool.none` | no tool: click and drag pans |
| `ZoomInTool` | `cmd:view.zoomIn` |  |
| `ZoomOutTool` | `cmd:view.zoomOut` |  |
| `CropTool` | `cmd:panel.crop` | Crop & Rotate (straighten there) |
| `SwitchCropOrientation` | `cmd:crop.rotateAspect` |  |
| `StraightenTool` | `cmd:panel.crop` | Crop & Rotate (straighten there) |
| `RotateFreehandTool` | `cmd:panel.crop` | Crop & Rotate (straighten there) |
| `SpotRemovalTool` | `cmd:panel.remove` | the Remove tool (heal and clone) |
| `WBPickerTool` | `cmd:tool.wbPicker` |  |
| `ApplyNormalization` | — | a Capture One feature LightCraft does not have |
| `PickNormTarget` | — | a Capture One feature LightCraft does not have |
| `PickHighlightLevel` | — | LightCraft's targeted adjustment works on the photo with the pointer |
| `PickShadowLevel` | — | LightCraft's targeted adjustment works on the photo with the pointer |
| `PickCurvePoint` | — | LightCraft's targeted adjustment works on the photo with the pointer |
| `PickLumaCurve` | — | LightCraft's targeted adjustment works on the photo with the pointer |
| `PickColorCorrection` | — | LightCraft's targeted adjustment works on the photo with the pointer |
| `DirectColorEditor` | `cmd:section.color` | the Color section (Point Color is there) |
| `PickSkinColor` | — | a Capture One feature LightCraft does not have |
| `AddColorReadout` | — | a Capture One feature LightCraft does not have |
| `DeleteColorReadout` | — | a Capture One feature LightCraft does not have |
| `OverlayTool` | — | a Capture One feature LightCraft does not have |
| `WatermarkTool` | — | a Capture One feature LightCraft does not have |
| `FocusMeterTool` | — | a Capture One feature LightCraft does not have |
| `PickFocusPoint` | — | a Capture One feature LightCraft does not have |
| `LoupeTool` | `cmd:view.zoomToggle` | zooms to 100 % and back |
| `KeystoneTool` | `cmd:tool.guidedUpright` | Guided Upright |
| `KeystoneVertical` | `cmd:geometry.upright` `{"mode":"vertical"}` | Upright: Vertical |
| `KeystoneHorizontal` | `cmd:geometry.upright` `{"mode":"level"}` | Upright: Level |
| `DrawMaskTool` | `cmd:tool.brush` |  |
| `DrawHealTool` | `cmd:panel.remove` | the Remove tool (heal and clone) |
| `DrawCloneTool` | `cmd:panel.remove` | the Remove tool (heal and clone) |
| `EraseMaskTool` | `cmd:tool.brush` `{"op":"subtract"}` | a subtracting brush on the selected mask |
| `LinearGradientTool` | `cmd:tool.linear` |  |
| `RadialGradientTool` | `cmd:tool.radial` |  |
| `Process` | `cmd:dialog.export` | the Export dialog |
| `ExportOriginals` | `cmd:dialog.export` | the Export dialog |
| `ExportVariants` | `cmd:dialog.export` | the Export dialog |
| `ImportImages` | `cmd:file.addPhotos` |  |
| `Print` | `cmd:dialog.contactSheet` | LightCraft prints contact sheets as PDF |
| `EditAllSelected` | `cmd:develop.autoSync` | Auto Sync: edits change every selected photo |
| `Rename` | `cmd:dialog.rename` |  |
| `NewVariant` | `cmd:photo.virtualCopy` | a virtual copy |
| `CloneVariant` | `cmd:photo.virtualCopy` | a virtual copy |
| `Delete` | `cmd:photo.delete` | moves to Recently Deleted |
| `MoveToTrash` | `cmd:photo.delete` | moves to Recently Deleted |
| `RegeneratePreviews` | `cmd:library.buildPreviews` |  |
| `SetCompareVariant` | `cmd:photo.setReference` | the reference photo |
| `ClearCompareVariant` | — | a Capture One feature LightCraft does not have |
| `PickVariant` | `cmd:photo.pick` | flags as Pick |
| `PromoteVariant` | `cmd:stack.moveUp` | one place up in the stack |
| `DemoteVariant` | `cmd:stack.moveDown` | one place down in the stack |
| `ShowInLibrary` | `cmd:view.back` | back to the grid |
| `ShowInFileSystem` | `cmd:app.showInFinder` |  |
| `Capture` | — | tethered capture is not in LightCraft yet |
| `CompositionMode` | — | tethered capture is not in LightCraft yet |
| `LiveView` | — | tethered capture is not in LightCraft yet |
| `AutoFocus` | — | tethered capture is not in LightCraft yet |
| `AutoFocusFine` | — | tethered capture is not in LightCraft yet |
| `ShutterSpeedUp` | — | tethered capture is not in LightCraft yet |
| `ShutterSpeedDown` | — | tethered capture is not in LightCraft yet |
| `ApertureUp` | — | tethered capture is not in LightCraft yet |
| `ApertureDown` | — | tethered capture is not in LightCraft yet |
| `ExposureBiasUp` | — | tethered capture is not in LightCraft yet |
| `ExposureBiasDown` | — | tethered capture is not in LightCraft yet |
| `ISOUp` | — | tethered capture is not in LightCraft yet |
| `ISODown` | — | tethered capture is not in LightCraft yet |
| `BWEnable` | `cmd:develop.treatment` |  |
| `ToggleGrading` | — | Capture One's grading switch has no single LightCraft counterpart |
| `SelectPen` | — | LightCraft has no annotations |
| `SelectEraser` | — | LightCraft has no annotations |
| `PenBlack` | — | LightCraft has no annotations |
| `PenWhite` | — | LightCraft has no annotations |
| `PenRed` | — | LightCraft has no annotations |
| `PenYellow` | — | LightCraft has no annotations |
| `PenGreen` | — | LightCraft has no annotations |
| `PenBlue` | — | LightCraft has no annotations |
| `ToggleSelectImage` | — | these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly |
| `NextGroup` | — | these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly |
| `PreviousGroup` | — | these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly |
| `NextImageImport` | — | these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly |
| `PreviousImageImport` | — | these sent keystrokes to Capture One's import window; LightCraft's import dialog takes the keyboard directly |
| `FilterRatingNone` | `cmd:library.filter` `{"rating":0,"ratingOp":"exactly"}` | shows unrated photos |
| `FilterRating1Star` | `cmd:library.filter` `{"rating":1,"ratingOp":"atLeast"}` |  |
| `FilterRating2Stars` | `cmd:library.filter` `{"rating":2,"ratingOp":"atLeast"}` |  |
| `FilterRating3Stars` | `cmd:library.filter` `{"rating":3,"ratingOp":"atLeast"}` |  |
| `FilterRating4Stars` | `cmd:library.filter` `{"rating":4,"ratingOp":"atLeast"}` |  |
| `FilterRating5Stars` | `cmd:library.filter` `{"rating":5,"ratingOp":"atLeast"}` |  |
| `FilterTagNone` | `cmd:library.filter` `{"label":null}` |  |
| `FilterTagRed` | `cmd:library.filter` `{"label":"red"}` |  |
| `FilterTagOrange` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `FilterTagYellow` | `cmd:library.filter` `{"label":"yellow"}` |  |
| `FilterTagGreen` | `cmd:library.filter` `{"label":"green"}` |  |
| `FilterTagBlue` | `cmd:library.filter` `{"label":"blue"}` |  |
| `FilterTagPink` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `FilterTagPurple` | `cmd:library.filter` `{"label":"purple"}` |  |
| `SelectUnrated` | `cmd:library.selectBy` `{"rating":0,"ratingOp":"eq"}` |  |
| `Select1Star` | `cmd:library.selectBy` `{"rating":1,"ratingOp":"eq"}` |  |
| `Select2Stars` | `cmd:library.selectBy` `{"rating":2,"ratingOp":"eq"}` |  |
| `Select3Stars` | `cmd:library.selectBy` `{"rating":3,"ratingOp":"eq"}` |  |
| `Select4Stars` | `cmd:library.selectBy` `{"rating":4,"ratingOp":"eq"}` |  |
| `Select5Stars` | `cmd:library.selectBy` `{"rating":5,"ratingOp":"eq"}` |  |
| `SelectTagNone` | `cmd:library.selectBy` `{"label":"none"}` |  |
| `SelectTagRed` | `cmd:library.selectBy` `{"label":"red"}` |  |
| `SelectTagOrange` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `SelectTagYellow` | `cmd:library.selectBy` `{"label":"yellow"}` |  |
| `SelectTagGreen` | `cmd:library.selectBy` `{"label":"green"}` |  |
| `SelectTagBlue` | `cmd:library.selectBy` `{"label":"blue"}` |  |
| `SelectTagPink` | — | LightCraft's colour labels are red, yellow, green, blue and purple (as Lightroom's) |
| `SelectTagPurple` | `cmd:library.selectBy` `{"label":"purple"}` |  |
| `AutoSync` | Apply to Selected | dials change every selected photo |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-25 | minor | Keys and Dials (the drawn device and its editor) in a resizable window of its own, opened from Settings ▸ Controls, which keeps the Settings window's one size |
| 2026-10-14 | major | Created: the control-surface engine (ported from TactileBridge), Settings ▸ Controls, the surface protocol v1, the MX Creative Console |
| 2026-10-15 | major | Settings ▸ Controls rebuilt around a drawing of the device with a side editor and categorised action picker; device bar; installing the Logi plugin from LightCraft (`surface.installPlugin` / `uninstallPlugin` / `plugin`); `surface.catalog`, `surface.watch` and action events, so Logi Options+ lists LightCraft's actions and values; icons in the readout |
| 2026-10-17 | minor | Dialpad drawn as the device, Keypad page buttons at its corners, Loupedeck+ in zones; Actions Ring slots set in LightCraft; drag and drop from the action list; Touch to find |
| 2026-10-16 | minor | Loupedeck+: a device of its own (`ld.r<n>` / `ld.b<n>`), works as labelled with no setup, imports a Loupedeck profile's layout (`surface.import`, Settings ▸ Controls ▸ Loupedeck+) |
| 2026-10-16 | minor | 13 icons of LightCraft's own in the pack's style for functions that shared a generic icon |
| 2026-10-16 | minor | Erase while held (`brush.erase`), key states (focus / held / on), destructive keys need a 0.7 s hold, no Delete key on built-in layouts, Previous / Next Mask, the brush page's size and feather keys; settings format 3 |
| 2026-10-15 | minor | Zero setup for the Logi plugin (on by itself, trusted through its data folder); the console shows the page being edited; dialog pages opt-in; `dial` / `roller` are the real big dial / roller (settings format 2 swaps older files), so a value key's value lands on the big dial |
