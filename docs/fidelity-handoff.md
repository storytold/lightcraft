# Lightroom fidelity handoff

Stopped at the user's request on 2026-10-11. Resume only when asked.

## Scope

- Verify the user's Nikon D3500 edits: B&W, profiles/presets, curves, tone, conventional noise reduction and sharpening.
- Optics, lens blur, AI tools and cross-camera consistency are out of scope.
- Preserve original photos, sidecars and library edits. Use disposable physical copies and private LightCraft sessions with XMP auto-write disabled.
- Keep Lightroom control in the background. Retire owned temporary files when no longer needed; avoid new test infrastructure.

## Landed behavior and evidence

Imported-profile monochrome treatment is independent of global treatment and survives profile-only copy, presets, sync, undo and Amount 0.
Explicit session-only type-0 HSV tables now render on the CPU before camera tone/B&W. Only their fingerprint is saved; use `profile.loadTable` / `profile.unloadTable` as described in [control-protocol.md](control-protocol.md#explicit-runtime-profile-tables).
Supported scope is Process 2, SDR and bounded Amount 0–200; unresolved or unsupported resources remain diagnosed.

All eight `cargo xtask ci` gates pass. Seven native RGB16 chart states pass 43 lifecycle checks; missing resources, Amount 0 and unloading preserve the expected pixels. Headless UI loading/unloading was checked.
The final native comparison is against the same inputs and fixed regions used by the measured candidate.

**Fidelity is not solved.** Chart mean ΔE00 improves, but six B&W cells worsen. On the saved D3500 Amount 120 photo, grid mean ΔE00 improves **4.771952 → 4.405292**, while the fixed center crop worsens **2.660664 → 2.734120**. See [the full limits and measurements](target-app-parity.md#runtime-profile-table-evidence-2026-10-11). Do not turn these into an overall fidelity percentage.

## Resume on the Mac

Repository: `/Users/admin.ilya.dorman/Dev/lightcraft`, branch `fix/lightroom-render-fidelity`.
Read the local `plan/MAC_HANDOFF.md`, `plan/STATUS.md` and `plan/LIGHTROOM_EXPORT_WORKFLOW.md` first; `plan/` and its private evidence are intentionally not pushed.

```sh
source plan/env.sh
cargo xtask ci
```

Evidence is under `plan/lightroom-reference/exports/continue-20261010/`:

- `runtime-render-validation/`: source/binary freezes, CI, native chart/photo comparisons and UI snapshots.
- `native_control_diagnosis/profile-runtime-resolution/`: profile-stage and Amount reference measurements.
- `fidelity-audit-profile-table-20261010/`: independent reviews.

Lightroom exports use the already paired local controller and a serial queue:

```sh
python -B plan/lightroom-reference/tools/controller_export_queue.py /absolute/path/to/reviewed-queue.json
```

There are 115 verified scripted submissions. Use new output paths, exact source/settings guards and the retained TIFF16/full-size/AdobeRGB/no-output-sharpening settings. Do not resubmit uncertain jobs. Never print or copy the pairing token.

## Next work

1. Review the existing D3500 full-profile versus parameters-only paired response before changing more rendering code; the full-photo mean alone can hide cancellation of other errors. The older parameters-only capture has a stride-16 grid but no center crop.
2. Investigate profile-table ordering relative to Exposure using the proposed own-chart source-composition pair. No new exposure reference has been exported. A failed composition check alone does not prove the reverse order.
3. Continue the remaining tone/B&W, curve and conventional detail gaps against D3500 saved edits, retaining worsening regions.

Fresh upstream `0ba118de92a81f7463ca0e6d79e1323137b1adae` adds four release/docking commits; the last integrated upstream is `964d9338`. No new rendering-fidelity implementation was found. Rebase is deferred at this stopping point. Preserve our guarded `PresetAmountState` behavior when resolving the docking container conflict; retain both sides' additive docs and locale keys, then run CI and the existing preset/UI checks.

A sixbox-to-Mac SSH shell round trip was verified using sixbox's existing `macbook-air` alias. Lightroom control/export over that SSH route was **not** verified, and no migration or SSH configuration change was made. A future migration needs the local evidence and export-worker arrangement; a Git clone alone is insufficient.
