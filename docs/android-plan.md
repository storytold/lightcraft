# Android implementation plan

Spec: android-architecture.md and the user-supplied Android tablet requirements.
Execution: inline; isolated fresh clone on android-tablet. No changes to user repos.

- [ ] 1. Build actual shared UI as Android cdylib, package NativeActivity and prove
  ARM64 linking. Files: apps/lightcraft-android, android. Validate SDK/NDK and APK.
- [ ] 2. Implement and test SAF source identities, discovery, lazy reads, persistent
  grants, import jobs and availability. Keep library journal on internal storage.
- [ ] 3. Implement export destinations, collision rules and sharing using shared
  export workers; validate failure/cancellation and unchanged original bytes.
- [ ] 4. Integrate lifecycle saves, touch/stylus, insets, memory pressure and
  persistent tablet settings. Test gesture arbitration and coordinate mapping.
- [ ] 5. Audit every upstream parity row, exercise editing/library persistence,
  run existing CI and Android tests, capture matching UI screenshots, benchmark.
- [ ] 6. Package signed debug/release APKs, source changes and installation report.

Review focus: revoked SAF grants, nonseekable providers, corrupt large RAWs,
process death during a save/export, and changing Android surfaces during gestures.

## Ledger

Repository inspected; engine loader/probe/preview/bytes hooks support host injection.
No Android device connected. SDK/NDK/JDK installation being prepared.
Ruling: proceed under the user's explicit instruction to implement all milestones
without intermediate approval; do not add workflow approval gates.

## SAM-enabled Android testing plan

The current fallback APK deliberately omits the optional SAM 3 runtime and uses
the local subject/saliency mask for Object selection. Build a separate testing
variant before considering neural masking part of the normal release package.

### Build and packaging

- [ ] Add an Android-only Cargo feature path that enables
      `lightcraft-engine/sam` and its `lightcraft-segment` dependency.
- [ ] Confirm Candle/SAM 3 compiles and links for `aarch64-linux-android` with
      the selected NDK, and record the resulting APK size and native memory use.
- [ ] Keep the existing fallback APK unchanged; use a distinct application
      version or build artifact name such as `LightCraft-android-arm64-sam-test`.
- [ ] Do not bundle model weights in the APK. The model is approximately 3.4 GB
      and has its own SAM License (Meta), separate from LightCraft licensing.

### Model setup and runtime

- [ ] Give the Android app an app-private model directory and expose its status
      (not installed, downloading, ready, loading, busy, failed).
- [ ] Add an explicit, acknowledged download flow for the SAM 3 files with
      resumable download, free-space checks, cancellation, retry, and clear
      license/size messaging.
- [ ] Keep inference off the UI thread, bound peak memory, and unload the model
      after an idle period so normal editing remains responsive.
- [ ] Preserve the local fallback whenever the runtime, model, or device memory
      is unavailable; never make ordinary editing depend on SAM 3.

### Functional validation

- [ ] Test Object selection with include and exclude taps, repeated clicks,
      component add/subtract/intersect, and cancellation.
- [ ] Test Describe selection with prompts such as “sky” and “the red car”,
      including no-match and malformed-input errors.
- [ ] Verify the generated segmentation survives save/reopen, export, undo/redo,
      rotation, process recreation, and source URI revocation.
- [ ] Compare SAM results and fallback results on the same fixtures; label the
      UI clearly so a tester knows which path produced the mask.

### Device and performance validation

- [ ] Run on the Y700 Gen 5 and at least one lower-memory ARM64 tablet.
- [ ] Measure model load time, first-photo encoding time, click latency, detail
      pass latency, peak RSS, sustained thermal behavior, and battery impact.
- [ ] Test portrait/landscape rotation, stylus input, background/foreground
      transitions, low-memory callbacks, and interrupted downloads.
- [ ] Record device model, Android version, GPU backend, model revision, and
      fixture dimensions with every benchmark.

### Exit criteria

The SAM testing variant is ready for wider internal testing only when it can
download and verify the model, perform Object and Describe selections without
blocking the UI, recover cleanly from missing/corrupt model files, and fall back
to local masks when resources are insufficient. It must remain explicitly marked
as a test build until physical-device results and licensing review are complete.
