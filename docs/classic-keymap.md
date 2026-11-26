# Lightroom Classic keymap profile

> **Last reviewed:** 2026-10-16 · **Last updated:** 2026-10-16 · **Change:** minor (created: the Classic keymap profile) · **Target:** Adobe Lightroom Classic 15.5

LightCraft ships one keymap with two **profiles**: `Lightroom` (the default, unchanged — the
Lightroom-desktop keys with the Classic additions listed in [ui-parity](ui-parity.md#shortcuts))
and `Classic`, which resolves the same commands to Lightroom Classic's own defaults. Classic
muscle memory works without giving up the desktop keys: the profile is a preference, not a fork.
Only public Adobe descriptions were consulted (no Adobe code, assets or application data):

- [Lightroom Classic keyboard shortcuts](https://helpx.adobe.com/lightroom-classic/help/keyboard-shortcuts.html)
  (Library and Develop tables, checked 2026-10-16)

## Switching

- **Help ▸ Keyboard Shortcuts** → *Keymap Profile* at the top (Lightroom / Lightroom Classic).
- The control channel: `app.setKeymapProfile {"profile": "classic" | "lightroom"}`; an unknown
  name is refused (`unknown keymap profile`) and changes nothing.
- The choice is saved with the UI state (`ui.json` → `settings.keymapProfile`) and read back; a
  `ui.json` from before profiles reads as `lightroom`, and so does an unknown saved value.
- Your own rebindings (`settings.keymap`) sit **above** whichever profile is active and are kept
  when you switch. ↺ in the editor restores the *active profile's* key, Reset All resets to it,
  and the editor lists that profile's defaults and fixed keys.

## How a key resolves

1. the user's own binding for the command (`""` = none),
2. the active profile's default (`CLASSIC_OVERRIDES`, `crates/ui-egui/src/keymap_profiles.rs`,
   else the command's declared key),
3. the fixed keys (rating digits `0`–`5`, `6`–`9` labels, Home / End, Shift+←/→ in the grids),
   which yield to 1 and 2,
4. the secondary keys (`ALIASES`; the Classic profile adds `⇧P`, `⌘I`, `Ctrl+Y`), which yield
   to 1 and 2 as well.

`no_conflicting_bindings` (`crates/ui-egui/src/shortcuts.rs`) runs under both profiles: no key
fires two non-contextual commands, and an alias goes quiet when its key belongs to a binding.
`no_override_duplicates_the_shipped_default` keeps the Classic table free of rows that only
repeat what the shipped profile already resolves to.

## The mapping

Shipped = the Lightroom profile, Classic = the Lightroom Classic profile, “row” = the action's
line in Adobe's public table above.

| Action | Shipped | Classic | Adobe row |
|---|---|---|---|
| Loupe (open the photo) | `D` | `E` | Enter Library Loupe view |
| Develop panel (Edit) | `E` | `D` | Open selected photo in the Develop module |
| Compare | `⇧C` | `C` | Enter Library Compare view |
| Lights Out | `⇧L` | `L` | Cycle forward / backward through Lights Out modes |
| Info overlay cycle | `⌘I` | `I` (+ `⌘I` kept) | Cycle / Show-hide info overlay |
| Info panel | `I` | — | (no Adobe key for the metadata panel) |
| People view (grids) | — | `O` | Open People View in the Library module |
| Mask overlay (Develop) | `O` | `O` | Show/hide local adjustment mask overlay |
| Tool overlay (pins) | `⇧H` | `H` | Show/hide local adjustment pin |
| Visualize HDR | — | `⇧H` | Toggle HDR view |
| Mask overlay mode | — | `⌥O` | Cycle Overlay mode in Crop / Masking |
| Filter bar | `⇧F` | `⇧F` + `\` in grids | Show/hide Library Filter bar |
| Show original / Before | `\` | `\` in the Loupe | View Before only |
| Deselect all | `⌘⇧A` | `⌘D` | Deselect all photos (`⌘D` or `⌘⇧A`) |
| Export dialog | — (`⇧E` alias) | `⌘⇧E` | Export selected photo(s) |
| Edit in PhotoCraft | `⌘⇧E` | `⌘E` | Edit in Photoshop |
| Export with previous | `⌘⌥⇧E` | `⌘⌥⇧E` | Export with previous settings |
| Create Version | `⌘⇧S` (`⇧M` alias) | `⌘N` (Develop side) | Create a new snapshot |
| New Album | `⌘N` | `⌘N` (Library side) | (Adobe documents Classic's `⌘N` as New Collection, which has no counterpart here) |
| Create Preset | `⌘⇧P` | `⌘⇧N` | Create a new preset |
| New Folder | `⌘⇧N` | `⌘⇧N` | Create a new folder in the Library module |
| Sync settings | — | `⌘⇧S` | Sync settings |
| Auto (auto tone) | `⇧A` | `⌘U` | Auto tone |
| Match total exposures | — | `⌘⌥⇧M` | Match total exposures |
| Paste from previous | `⌘⌥V` | `⌘⌥V` | Paste settings from previous photo |
| Auto Sync | `⌘⌥⇧A` | `⌘⌥⇧A` | Auto Sync |
| Crop | `C` | `R` | Select the Crop tool |
| Spot removal (Remove) | `H` | `Q` | Select the Spot Removal tool |
| Adjustment Brush | `B` | `K` | Select the Adjustment Brush tool |
| Linear Gradient | `L` | `M` | Select the Graduated Filter tool |
| Radial Gradient | `R` | `⇧M` | Create a Radial filter |
| Guided Upright | `⇧G` | `⇧T` | Select the Guided Upright tool |
| Masking panel | `M` | `⇧W` | Open / close Masking |
| Keywords panel | `K` | — | (Adobe gives keywords `⌘K`; no command here, see below) |
| Presets panel | `⇧P` | — | (so `⇧P` flags and advances) |
| Flag pick + advance | `⇧P` in grids only | `⇧P` in **every** view | culling keys (flag as pick) |
| Redo (Windows key) | — | `Ctrl+Y` | — |

Unchanged and shared by both profiles: the rating / label / flag keys (`0`–`9`, `P` `X` `U`,
`` ` ``, `⌘↑` `⌘↓`), Home / End and `⇧←` / `⇧→` in the grids, Grid toggle `G`, Survey `N`,
`=` / `-` grid thumbnail size, `F` full screen, `⌘/` Keyboard Shortcuts, `F6` / `F7` filmstrip /
left panel, `⌘⇧D` select only active, `⌘R` show in Finder, `F2` rename, `⌫` delete, and the
commands whose declared key already is Classic's (New Folder `⌘⇧N`, Export with Previous
`⌘⌥⇧E`, Paste from Previous `⌘⌥V`, Auto Sync `⌘⌥⇧A`).

## Keys the Classic profile shares by context

Lightroom Classic puts several of these keys in two tables (Library / Develop), and so do we —
the *right panel* decides which side, not the view (`handle`, `crates/ui-egui/src/shortcuts.rs`):

| Key | no edit tool open | an edit tool open (Edit, Crop, Remove, Masking, Red Eye, Profiles) |
|---|---|---|
| `⌘N` | New Album | Create Version (Adobe: new snapshot in Develop) |
| `⌘⇧N` | New Folder | Create Preset (Adobe itself shares `⌘⇧N` between the two tables) |
| `O` | People view (in a grid) | Show mask overlay |
| `\` | Filter bar (in a grid) | Show original / Before |
| `⇧P` | flag pick + advance (every view, both sides) | the same |
| `⇧{` / `⇧}` | move the photo up / down its stack (grids) | brush feather (the shipped aliases) |

Notes on the edges, all covered by `crates/ui-egui/src/tests_keymap.rs` (`mod classic`):

- The panel decides even in the other view: with the Edit panel open in a grid `⌘N` still
  creates a version (the panel is visible there — it is part of the docked workspace), and with
  no edit tool open in the Loupe `⌘N` opens New Album. Close the panel (or press its key again)
  to switch sides.
- Opening an edit tool from a grid takes you to the Loupe (existing behaviour: an edit tool
  lives there), so Classic's `D` changes the view as a consequence of the panel, not of a
  Loupe binding — the Loupe's key is `E`.
- The fixed stack keys and the `\` grid rule exist only under the Classic profile; in the
  shipped profile `⇧{` / `⇧}` keep resizing / feathering the brush and `\` shows the original.
- A binding you gave a command wins over all of this: assign `⇧{` to anything and the stack
  key steps aside.

## Deliberate differences from Adobe (Classic profile)

- **Module switchers**: `⌘⌥1` (Library), `⌘⌥2` (Develop) and `⌘⌥5` (Slideshow) work as fixed
  aliases; the rest of the `⌘⌥3`–`⌘⌥7` row and the `⌘0`–`⌘9` panel tabs have nothing to switch
  (single window by design), and `F8` / `Tab` / `F5` panel keys are not bound — see
  [parity-checklist](parity-checklist.md) (KEYC-MODULES, KEYC-PANELS) and [gaps](gaps.md).
- **Keywords**: Adobe gives its Keywords panel `⌘K`; LightCraft has no keyword-focus command, so
  the panel keeps no key here and `K` is Classic's Adjustment Brush (KEYC-METADATA).
- **Reference view `⇧R` and `⌘⌥0` screen modes** are not bound (KEYC-VIEWS); everything else in
  that row (`E G C N L F I`, `⇧H`, `O`) is above.
- Lightroom-only features keep the shipped keys in both profiles (grid thumbnail size, Home /
  End, `⇧F` filter bar, people / second window / slideshow keys).
- LightCraft features Adobe has no key for keep their shipped keys (painter, quick collection
  `B`, smart albums).

## Contributing this upstream

This is a clean-room feature: the Classic table above was transcribed row by row from Adobe's
public shortcut page (linked at the top), with no Adobe code or assets, and each row is either a
shipped key already in place or a line in `CLASSIC_OVERRIDES` / `CLASSIC_ALIASES` with its Adobe
row named in this document. A pull
request would carry:

- `crates/ui-egui/src/keymap_profiles.rs` (profiles and tables) and the profile plumbing in
  `state.rs` (`AppSettings::keymap_profile`), `shortcuts.rs` (`binding`, `assign`, `handle` and
  its context gates) and `panels/keymap.rs` (the profile selector) — additive: the shipped
  profile's keys are untouched;
- the tests as evidence: `keymap_profiles::tests`, `shortcuts::tests` (per-profile conflicts and
  defaults) and `tests_keymap.rs::classic` (end-to-end keys, the shared-key contexts and the
  `ui.json` round trip);
- `docs/classic-keymap.md` (this file) and the checklist / gaps updates that go with it, per the
  repo's rules in `AGENTS.md`.

Check the repo's contribution rules (`AGENTS.md`: `cargo xtask ci` green, one task per commit,
parity checklist and gaps updated in the same commit) before opening it.
