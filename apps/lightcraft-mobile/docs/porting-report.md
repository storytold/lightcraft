# LightCraft → iOS / iPadOS / Android: porting assessment

Assessed at upstream `549cbe7` (2026-10-07); the spike was built against that commit. The spike is now `apps/lightcraft-mobile` (see its README).

## Verdict

The engine is already portable. The cost is almost entirely **UX** (touch and phone layouts) plus a
**thin native shell** per OS.

- ~106k LOC of pure Rust. `unsafe` is denied workspace-wide. There's no C, no `std::arch`
  SIMD, and GPU compute runs on wgpu/WGSL.
- `lightcraft-engine`, `-ui-egui`, `-gpu` and `-catalog` `cargo check` cleanly for
  `aarch64-apple-ios` and `aarch64-linux-android` with **zero code changes**. Only the
  desktop shell fails, because `rfd` file dialogs have no mobile backend.
- **The unmodified UI and engine run on iPad, iPhone and Android tablet** in a ~90-line shell
  (screens below). Live develop works by touch, and the on-disk journal survives a hard kill.

## What the spike proved

| Check | iOS (Simulator 26.5) | Android (emulator, API 35 tablet) |
|---|---|---|
| Core crates compile | ✅ no changes | ✅ no changes |
| Links as app | ✅ 46 MB debug binary | ✅ 44 MB release `.so`, NativeActivity |
| Library on disk in sandbox | ✅ 24 demo photos, opened in 37 ms | ✅ `internal_data_path()` |
| GPU compute | ✅ Metal (`buffers ≤ 256 MiB`) | not verified (no logger wired) |
| Window renderer | ✅ Metal, **after** requesting adapter limits | ✅ **GL only**: Vulkan panicked in `Surface::configure` |
| Touch slider drag → live render + histogram | ✅ | ✅ |
| Kill −9 then relaunch keeps the edit | ✅ (1 journal record replayed) | not tested |
| iOS 27 runtime | ✅ after adopting UIScene in the shell (it crashed at launch before: winit 0.30.13 has no scene lifecycle) | — |
| Phone layout | ✅ first pass: a compact layout below 600 pt, with the safe area respected (before: photo squeezed to ~90 pt, top bar under the Dynamic Island) | not tested (tablet emulator only) |
| Vertical swipe scrolls the edit panel | ✅ (egui ignored the first touch gesture after launch; the shell now declares the touch screen) | not tested |

### Blockers found by running it (not visible from reading code)

1. **iOS 27 + winit 0.30 = launch crash** (`_UIApplicationEvaluateRuntimeIssueForNoSceneLifecycleAdoption`).
   This is **fixed in the shell** and verified on the iOS 27.0 and 26.5 Simulators, including
   background → foreground. `ios/Info.plist` declares a scene manifest, and `src/ios_scene.rs`
   (objc2) registers the scene delegate it names. That delegate attaches the window winit created
   to the connected scene. winit keeps getting lifecycle events from the `UIApplication…`
   notifications, which UIKit still posts. Limits: one scene only, so no iPad multiwindow. It can
   be removed once eframe moves to winit 0.31.
2. **eframe's default wgpu limits are rejected** by the Simulator's Metal adapter
   (`max_inter_stage_shader_variables 16 > 15`). Fixed in the spike by setting `required_limits = adapter.limits()`.
3. **Android emulator Vulkan surface fails.** The window has to be able to fall back to GL, but
   the workspace builds wgpu without `gles` (`Cargo.toml`, "no GL"). Expect the same on some
   real low-end and old devices.

## Code-level findings (from three parallel audits)

**Platform coupling (small).**
- Every gate is `wasm32` vs native, so mobile takes the desktop paths: threads, `std::fs`,
  fsync journal, wgpu.
- `memory::total_ram()` returns `None` on iOS and Android, which sets a fixed 1.5 GiB
  budget. That's too high for jetsam.
- `library::default_dir()` uses `$HOME/Pictures`: wrong on iOS, `None` on Android.
- `sysmem` pressure relief is gated to `macos`, not `ios`.
- `devices.rs` (`/Volumes` DCIM scan), `open`/`xdg-open`, the control TCP server and MCP
  are all irrelevant on mobile.

**Storage model (medium).**
- The catalog stores **absolute paths** and reads originals with `std::fs::read`.
- That's incompatible with PHPhotoLibrary ids, Android `content://` URIs, iOS
  container-path changes and security-scoped URLs.
- **The web build already solved this:** opaque ids, a copy-into-library import via async
  pickers, and the `Store` trait. A mobile host can reuse that pattern, plus the
  `FileLoader`/`FileProbe` byte hooks on `MediaCache`.
- XMP sidecars next to originals, "move" import and folder watching need to be off on
  mobile.

**Memory (medium).**
- Full-res sources are Rgb32f (24 MP ≈ 288 MB; 61 MP ≈ 730 MB).
- The decode gate lets one oversize job bypass the budget.
- The thumbnail disk cache is 2 GiB.
- GPU buffers are whole images, with no tiling. On Android GPUs with 128–256 MB storage
  limits, **24 MP exports fall back to CPU**.
- Needs explicit budgets, OS memory-warning hooks, and tiled GPU export.

**Lifecycle (small).**
- Per-command fsync journaling is already crash-safe (verified).
- Still to add on suspend: `end_interaction`, saving `view.json`, pausing GPU work.
  iOS rejects background GPU submits, which would latch the process to CPU.

**UI (large: the dominant cost).**
- Zero touch code: no pinch (`zoom_delta`) and no multi-touch.
- 69 hover checks, several of them functional: section eye toggles, the brush-size ring,
  hover-preview of presets.
- 23 context menus. About 130 shortcuts, though nearly all actions are also reachable from
  menus or buttons.
- Hit targets are 10–30 pt (crop handles 12 pt, mask grips 10 pt) vs 44 pt guidance.
- Grid cells and full-width sliders use `click_and_drag` inside ScrollAreas, so drags fight
  scrolling.
- Fixed panel widths (right panel 250–520, min photo 360) with no breakpoints. The desktop
  window minimum is 900×560.
- No IME, soft-keyboard or safe-area handling (since added in the shell: safe areas, keyboard
  insets, Android IME composing).
- **The good news:** a single `MenuNode` model already renders as a full in-window menu,
  and host OS features go through an injectable `Services` struct.

**Engine boundary.**
- Writes go through 232 JSON commands (stable ids). Reads touch `Session` directly
  (~230 field accesses from the UI), and there's no FFI.
- A native SwiftUI/Compose UI is therefore possible but needs a uniffi façade plus an
  extracted "app-core" layer (render scheduling, import, export).

**Project velocity.**
- 540 commits in 3 days (history starts 2026-10-05), 92% by one author, with an
  agent-driven workflow (AGENTS.md / CLAUDE.md).
- A private fork of `ui-egui` would rot within days. Mobile UI changes should go
  **upstream** behind a layout mode.

## Effort estimate (one experienced Rust dev; person-weeks)

### Path A: keep egui and make it touch-first (recommended)

| Work item | iPad + Android tablet | + phones |
|---|---|---|
| Shells: Xcode wrapper with UIScene fix, Gradle/GameActivity, signing, icons, safe areas | 4–6 | — |
| Services: PHPicker/SAF import (web's async pattern), share-sheet export, `open_url` | 2–3 | — |
| Storage: opaque source ids, copy-in import, sidecars/move/devices off, caches → Caches/ | 1–2 | — |
| Lifecycle and memory: suspend hooks, budgets, memory warnings, GL fallback | 2–3 | — |
| Touch pass: drag vs scroll, pinch/pan, long-press, 44 pt targets, hover replacements, keyboard/IME | 6–10 | — |
| Phone layout: single column, bottom-sheet panels, tab bar, modal tools, full-screen dialogs (first pass done: compact layout, sheet, drawer, safe area, dialogs) | — | 10–16 |
| GPU tiling for exports on 128–256 MB devices (done: CPU and GPU tiled exports) | 2–3 | — |
| CI (`cargo check` for both targets), device-farm QA | 1–2 | 1–2 |
| **Total** | **18–29 wk (≈4–7 months)** | **+11–18 wk** |

All three platforms (iPad, iPhone, Android tablets and phones) come to **≈ 7–11 person-months**.

### Path B: native SwiftUI + Jetpack Compose over the Rust engine

uniffi façade 4–6, app-core extraction 5–7, SwiftUI app 18–24, Compose app 14–20, for a total
of **≈ 45–60 wk (11–14 person-months)**. The result feels native, but every feature has to be
built twice and kept in sync with a very fast-moving upstream.

### Path 0: wrap the existing WASM build (days)

Load `apps/lightcraft-web` in WKWebView or a TWA. It gets you a demo, not a product: no
GPU compute in the browser build, single-threaded wasm workers, OPFS storage limits, and the
same desktop UI.

## Recommendation

1. Go **iPad-first with Path A**. The spike shows the desktop UI is already close to usable
   on a 13" iPad, and an Apple Pencil maps well onto the brush and mask tools.
2. The **winit/UIScene** blocker is solved in the shell; that leaves no hard blockers.
3. Do the touch pass **upstream**, as a `LayoutMode::{Desktop, Tablet, Phone}` in `ui-egui`.
   That benefits the web build on tablets too.
4. Phones have a first-pass compact layout: a bottom-sheet editor, a drawer, the safe area, a three-across grid and phone-sized dialogs. Slider drags no longer fight scrolling: by touch, the first movement decides between adjusting and scrolling. The iOS soft keyboard lifts the layout and keeps the focused field in view. On Android the same happens via window insets read over JNI, which also clears the navigation bar. The loupe has pinch zoom and swipe-between-photos. Photos import from the system photo picker on both platforms. Finished exports open the share sheet. Photos inside the library survive an iOS reinstall (the engine relinks a moved library on open). The memory budget follows the device's limit and shrinks on memory warnings. Full-resolution work is capped on phones so a big original can't get the app killed (measured: a 48 MP full-size render peaks at 2.87 GB, or 760 MB with a 16 MP cap). Full-size exports over the cap render tile by tile at full size, matching the whole render within one 8-bit step (peak 2.0 GB instead of 6.9 GB for a 60 MP DNG, 1.17 GB instead of 3.4 GB for a 48 MP JPEG). Tiles render on the GPU when memory allows, and a GPU render over the device's buffer limit (24 MP on Android's 128–256 MB limits) is tiled on the GPU instead of falling back to the CPU. Android keyboards compose through the shell's own input connection (no GameActivity needed): verified with Korean, with the system CJK font for scripts the bundled fonts lack. Still to do: tiling the remaining whole-image edits (spot removal, Refine Edges, Auto Mask, subject masks), and verifying English autocorrect / swipe typing and Japanese / Chinese keyboards on a device (the keyboard's view of a field's text and cursor now follows egui's).

## Spike artifacts

Everything is in `apps/lightcraft-mobile`. See its `README.md`.

- `src/lib.rs`: the shared shell (eframe plus `LightcraftApp`). It holds `ios_main`, plus the
  `android_main` NativeActivity entry point. On Android the window uses GL unless
  `/data/local/tmp/lc_vulkan` exists.
- `scripts/ios-sim.sh`, `scripts/android-apk.sh`: build, bundle and run. The APK is packaged
  with `aapt2`, `zipalign` and `apksigner`, without Gradle.
- `docs/screens/`: iPad, iPhone and Android tablet screenshots.
