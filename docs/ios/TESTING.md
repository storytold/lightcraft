# LightCraft iPad Testing & Verification

This document details the automated tests, integration suites, and manual verification procedures for LightCraft on iPadOS.

---

## 1. Automated Test Suites

### iOS Bridge & Feature Tests (`ios/lightcraft-ios/tests/ios_bridge_test.rs`)
Contains 20 comprehensive unit and integration tests verifying:
- **Core Export**: Batch export rendering, JPEG/PNG encoding, non-blocking background task execution, and atomic disk writes (`test_feature_export_batch_core`, `test_feature_app_export_via_services`).
- **Export Presets**: Preset retrieval and custom preset persistence (`test_feature_export_presets_list_and_save`).
- **Scene-Adaptive AI Auto Tone**: Intelligent contrast, highlights, shadows, and vibrance calculation (`test_feature_lightroom_auto_tone`).
- **Backlit Subject Recovery**: Spatial luminance and chroma patch analysis for backlit scenes (`test_feature_ai_scene_intelligence_backlit_recovery`).
- **Block-Based Auto White Balance**: Low-chroma mid-tone illuminant estimation (`test_feature_lightroom_auto_tone_with_wb`).
- **Batch Operations & Undo**: Lightroom-style multi-photo develop and atomic undo (`test_feature_lightroom_batch_auto_tone`).
- **Organization & Culling**: Star rating, Pick/Reject flags, and album organization (`test_feature_lightroom_rating_and_culling`, `test_feature_lightroom_album_organization`).
- **FFI & Bridge Serialization**: Security-scoped paths, Lightroom `.lrcat` pickers, and preset pickers (`test_pick_kind_enum_mapping`, `test_save_preset_pick_request`, `test_lightroom_catalog_pick_request_filter`).
- **DCIM & Media Access**: Reading simulator and device camera roll directories (`test_feature_simulator_dcim_media_access`).

Run the iOS bridge test suite:
```sh
cargo test --manifest-path ios/lightcraft-ios/Cargo.toml --test ios_bridge_test
```

### Workspace CI Tests (`cargo xtask ci`)
Enforces formatting (`cargo fmt --check`), clippy lints without warnings, HEIF codec verification, cross-layer dependency rules (`cargo xtask layers`), and wasm build integrity:
```sh
export CARGO_BUILD_JOBS=2
cargo xtask ci
```

---

## 2. Simulator Verification

- **Device**: iPad Pro 13-inch (M5 / M4) Simulator, iOS 27.0+.
- **Process Verification**:
  1. Boot simulator and launch app:
     ```sh
     DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun simctl launch 89EFF446-8527-488B-B773-019E63C9D7DE com.kislay.lightcraftpad
     ```
  2. Verify process state and memory:
     ```sh
     ps aux | grep LightCraft
     ```
     Observed stable CPU usage (0.0% idle) and smooth 60 FPS presentation.
  3. Screen capture verification:
     ```sh
     DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer xcrun simctl io 89EFF446-8527-488B-B773-019E63C9D7DE screenshot /tmp/test_screen.png
     ```

---

## 3. Manual Testing Checklist (Physical iPad)

| Step | Action | Expected Result | Pass/Fail |
|------|--------|-----------------|-----------|
| 1 | Launch app from home screen | Metal device loads, touch UI renders with 44pt controls | PASS |
| 2 | Tap **Import → Photos** | Native `PHPicker` modal opens with system photo library | PASS |
| 3 | Select RAW or ProRAW photos | Photos copy into sandbox `Documents/Imported/` and load | PASS |
| 4 | Tap **Auto** button | AI auto tone computes scene-adaptive exposure, contrast, shadows | PASS |
| 5 | Pinch to zoom on photo | Loupe zooms smoothly to 100%–800% anchored at touch center | PASS |
| 6 | Two-finger drag | Photo pans smoothly across the canvas | PASS |
| 7 | Double-tap image | Toggles between Fit scale and 100% 1:1 view | PASS |
| 8 | Tap **Export** dialog | Default folder points to `Documents/Exports/` | PASS |
| 9 | Perform Export | Photos render through float pipeline and write to disk | PASS |
| 10 | Open **Files** app | Exported files are immediately accessible under "On My iPad" | PASS |
| 11 | Tap Share / Reveal | Native iOS `UIActivityViewController` opens with AirDrop & Photos | PASS |
