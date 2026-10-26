# iPad app

Last updated: 2026-10-25

LightCraft runs as a native iPad app. The rule for this change: **it must not change the macOS, Windows, Linux or web
apps.** Everything iPad-specific lives in `ios/`, behind `cfg(target_os = "ios")`, or behind the touch layout flag
(`crate::theme::touch_ui`, which only the iPad host switches on; `glass::active` is touch + dark).

## What it adds

**Host (`ios/`)**
- `ios/LightCraftiPad.xcodeproj` (generated from `ios/project.yml`): iPad only, iOS 16+, a Rust build phase that builds
  `liblightcraft_ios.a` for the simulator or the device and links it with `-force_load`.
- `ios/lightcraft-ios`: the Rust static library (eframe/wgpu on Metal), the C ABI the Swift side calls, the Pencil stroke
  queue and the segmenter callback.
- `ios/LightCraft/*.swift`: app entry, scene delegate, Photos/Files pickers, security-scoped folder bookmarks, the Apple
  Vision subject-mask bridge and the Pencil gesture recogniser.

**Touch UI (`crates/ui-egui`, touch layout only)**
- Liquid-Glass look (`glass.rs`): floating translucent shells, capsule buttons, rims and shadows. There is no real
  backdrop blur (the photo and the UI share one layer), so the material is approximated.
- Its own top and bottom bars (`panels/topbar_touch.rs`, `panels/bottombar_touch.rs`): `topbar.rs` / `bottombar.rs`
  hand over to them on the iPad and are otherwise unchanged. The bottom bar slides sideways with a fade and chevron
  cue when it is too narrow; a draggable floating rater appears when there is no room for the stars.
- Landscape, portrait, Split View and Slide Over layouts; 40pt hit areas; capsule buttons; iOS switches; solid slider
  thumbs; finger-sized stars (`rating.rs`) and list rows; the Quick Develop panel and rail entry; filter chips (rating,
  flag, label, edited, original missing) and a missing-original badge and banner.
- Gestures (`touch.rs`, `pencil.rs`): two-finger tap = undo, three-finger tap = redo, Pencil double-tap = zoom, Pencil
  digits 1-5 = rating, circle = 0, tick = pick, cross = reject (one-stroke $1 and multi-stroke $P recognisers).
- Pick / reject are drawn as a tick and a cross on the iPad only.

**Library on the iPad (`crates/engine`, `crates/catalog`)**
- No `flock` on iOS (a suspended app holding a lock is killed with 0xDEAD10CC).
- `container_relink.rs`: the app container UUID changes between installs, so absolute paths are re-based at launch.
- Demo photos are removed at launch; folders the user picked are restored from bookmarks; a library that fails to open
  is reported instead of silently replaced.
- AI masks use a host segmenter hook (`segment/platform.rs`): the hook is `None` unless a host installs one, and iOS
  plugs in Apple Vision so no model needs to be downloaded.
- `develop.adjust` and `develop.wb` accept `ids` (Quick Develop nudges several photos in one undo step); without `ids`
  they act as before.

**Tooling**
- `lightcraft-cli snapshot --touch` renders the iPad layout headlessly.
- `tools/edit_flow/ui_lint.py` (geometric UI lint over every screen), `panel_audit.py` and flow scripts;
  `docs/button-test-checklist.md`.

## How it stays iPad-only

| Mechanism | Where |
| --- | --- |
| `ios/` and `cfg(target_os = "ios")` | host, lock skip, export-folder fallback |
| `theme::touch_ui(ctx)` | every layout, size, colour, gesture and the touch bars in `ui-egui` |
| `glass::active(ctx)` | the glass shells and the dock style (touch + dark) |
| Hooks default to "off" | the segmenter hook is `None` until a host installs one |
| Separate files for whole layouts | `topbar_touch.rs`, `bottombar_touch.rs` |

The desktop theme list, dialogs, menus, shortcuts, left sidebar, mixer and export UI are the ones on `main`.
Not carried over from the earlier iPad work, to keep this change iPad-only: the web PWA files, the auto-tone / auto-WB
changes, the Develop left sidebar, the mixer and grading changes, the iPad dialog restyle and the model-mirror / token
settings. They can come back as their own changes.

## Methodology

1. **Reproduce on a device and the simulator**, then headlessly: `lightcraft-cli snapshot --touch --demo --script X.jsonl
   --size 1180x820` (landscape), `820x1180` (portrait), `700x820` / `504x820` (Split View) and `320x820` (Slide Over).
2. **Review bottom-up**: atoms (a component gallery), then every button, panel, dialog, screen and user flow.
3. **Geometric lint** over every registered widget rectangle: hit targets under 32pt, overlapping controls, widgets off
   the screen.
4. **AI review as assistants, not authorities**: Gemini judged screenshots against the iPadOS Human Interface Guidelines,
   Gemini and DeepSeek reviewed diffs, and CodeRabbit reviewed the PR. Every finding was checked against the code before
   acting.
5. **Desktop parity check**: the pre-change build was built separately and desktop renders (grid, detail, panels,
   dialogs) were pixel-diffed against this branch.
6. **Gates**: `cargo fmt`, `cargo clippy -D warnings` (ui-egui, engine, catalog, cli and `ios/lightcraft-ios` for
   `aarch64-apple-ios`), `cargo xtask layers` and `assets`, the ui-egui and engine tests, and installs with `devicectl`.

## Limits

- Not testable in the simulator: Vision masks ("Could not create inference context") and real Pencil input; check on a
  physical iPad.
- The iPad's dialogs are the desktop's windows for now (no glass restyle).
- CJK fonts are absent unless `CRAFT_FONTS_DIR` is set; the dock tab strip has square corners (third-party `craft_ui`).
