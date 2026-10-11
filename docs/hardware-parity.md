# Hardware parity

> **Last reviewed:** 2026-10-10 · **Last updated:** 2026-10-12 · **Change:** minor (PTP/MTP cameras import on Windows; pen pressure for brush masks; monitor profile from colord on Linux); previously major (created from the code, gpu-pipeline.md and the parity checklist) · **Target:** Adobe Lightroom Classic 15.5 / Lightroom 9.6

What Lightroom does with the machine's hardware, per platform, and what LightCraft does. Sources:
[gpu-pipeline.md](gpu-pipeline.md), [display-profiles.md](display-profiles.md), [denoise.md](denoise.md),
[ai-masks.md](ai-masks.md), `crates/gpu`, `crates/engine/src/devices.rs`, `crates/ui-egui/src/panels/second.rs`.

| Feature | Target | LightCraft: macOS | Windows | Linux / BSD | Web | Status |
|---|---|---|---|---|---|---|
| GPU for display and editing | Metal / DX12, "Use GPU for display / image processing / export" | wgpu Metal: geometry, resize, blur, WB, NR, guided filter, dehaze, masks, finish; CPU fallback per render | DX12 only (Vulkan opt-in, #136) | Vulkan (CPU ↔ GPU equivalence verified on an AMD Radeon 8060S, RADV) | CPU only (no WebGPU yet) | 🟡 defringe, spot removal, heuristic masks, long brushes and high-bit exports stay on CPU |
| GPU export | ✅ | ✅ (6000 × 4000 in ~0.3 s vs ~1.2 s CPU) | ✅ | ✅ | — | ✅ (black-export fix on an Intel iGPU, #78) |
| AI on GPU / neural engine | Metal / DirectML / Core ML, model per feature | AI denoise on GPU or CPU (setting); SAM 3 on Metal via candle; faces on CPU | CPU / GPU denoise | CPU / GPU denoise | — | 🟡 no Core ML / Neural Engine / DirectML |
| HDR (EDR) display | ✅ (macOS EDR, Windows HDR) | ⬜ | ⬜ | ⬜ | ⬜ | ⬜ (LR-VIEW-HDR-DISPLAY) |
| Monitor profile | automatic per display, follows the window | ✅ the screen's profile, followed across screens | 🟡 picked by hand (WCS needs FFI) | 🟡 colord's profile (one display, or all sharing one); no follow across monitors | — | 🟡 Windows OS profile not read; Linux per-monitor follow missing |
| Second window / second monitor | ✅ (Classic) | ✅ (⌘F11) | ✅ | ✅ | — | ✅ (Classic secondary-window keys missing) |
| Pen pressure / tilt (brush masks) | ✅ pressure for size / flow | ⬜ winit reports no tablet pressure on macOS | 🟡 pen force from winit touch events (untested on a tablet) | ⬜ winit has no X11 / Wayland tablet input | 🟡 pointer-event force where winit passes it | 🟡 brush size and flow follow pen pressure wherever egui gets a touch force (Brush ▸ Pen Pressure); no tilt; macOS / Linux tablets wait on winit |
| Trackpad gestures | pinch zoom, pan | ✅ pinch, two-finger pan | 🟡 (winit) | 🟡 | 🟡 | ✅ on macOS |
| Card readers / cameras as import sources | ✅ (devices panel, eject after import) | ✅ mounted volumes with DCIM | ✅ mounted volumes, and PTP/MTP cameras and phones through Windows Portable Devices (`crates/cameras`) | ✅ mounted volumes | — | 🟡 no eject-after-import check; PTP/MTP cameras on Windows only (macOS: ImageCaptureCore, Linux: PTP over USB) |
| Tethered capture | ✅ Classic (Canon, Nikon, Sony, Leica) | ⬜ | ⬜ | ⬜ | — | ⬜ |
| Printers | ✅ Print module, printer colour management | ⬜ (PDF contact sheets only) | ⬜ | ⬜ | — | ⬜ |
| Control surfaces (MIDI / Loupedeck via plug-ins) | via SDK plug-ins | control channel and MCP can drive every command | same | same | — | 🟡 different mechanism; no MIDI mapping |
| Multi-core CPU | ✅ | ✅ worker pools, parallel JPEG decode, parallel import copy | ✅ | ✅ | single thread | ✅ |
| Memory pressure | managed | ✅ memory budget, `crates/sysmem` returns allocator pages on macOS | ✅ budget | ✅ budget | — | ✅ |

## Score

**~40% ready** (estimated): the GPU editing path is real and fast (measured: 24 MP exposure drag
3.9 ms on GPU vs 22–42 ms CPU; NR drag 18 ms vs 301–471 ms), but HDR display, pen pressure,
tethering, printing, automatic monitor profiles and neural-engine inference are missing. GPU
correctness is proven on Apple GPUs and on one AMD GPU on Linux (Radeon 8060S, Vulkan / RADV: the
equivalence suite passes with the same max 1–2 LSB as on Metal, [gpu-pipeline.md](gpu-pipeline.md#correctness-cpu-oracle-and-equivalence-tests)).
NVIDIA, Intel and Windows DX12 still rely on user reports.

## Remaining (Opus 5.5 agent hours)

| Work | Hours | Needs |
|---|---:|---|
| HDR display (EDR on macOS, Windows HDR, Linux where possible) | 10–20 | an HDR display to verify (human) |
| OS monitor profile: Windows WCS (an FFI helper crate, like `crates/sysmem`), follow the window across displays on Linux | 4–8 | multi-monitor setup |
| Pen pressure for brush masks: done where winit reports a touch force; macOS and Linux tablets need winit tablet events (or a pure-Rust tablet layer) | 3–8 | a tablet |
| Remaining stages on GPU (defringe, spot removal, masks) | 10–20 | |
| Tethered capture (PTP over USB in pure Rust, Canon / Nikon / Sony first) | 25–45 | cameras (human) |
| Printing with colour management | 15–25 | printers (human) |
| Neural-engine / DirectML inference for AI features | 10–20 | depends on the AI model decision |
| **Total** | **80–150 h** (the core set for beta, HDR display + monitor profile + pen pressure: 20–40 h) | |

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-10-12 | minor | Monitor profile: Linux reads the display profile from colord; macOS row reflects #787 |
| 2026-10-12 | minor | CPU ↔ GPU equivalence suite run on an AMD Radeon 8060S (Vulkan, RADV, Linux): 16 tests / 135 cases pass, max 2 LSB, mean ≤ 0.0003; first GPU verified outside Apple |
| 2026-10-12 | minor | Pen pressure: strokes record per-point pressure from egui touch force, scaling dab size and flow; per-platform status |
| 2026-10-15 | minor | Cameras as import sources: PTP/MTP cameras and phones on Windows (issue #1005) |
| 2026-10-10 | major | Created |
