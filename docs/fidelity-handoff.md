# Lightroom fidelity handoff

Resumed on 2026-10-11 at the user's request. Pulled `172ec37c` and rebased the 15 local commits onto upstream `0dd6593d`.

## Scope

- Verify the user's Nikon D3500 edits: B&W, profiles/presets, curves, tone, conventional noise reduction and sharpening.
- Optics, lens blur, AI tools and cross-camera consistency are out of scope.
- Preserve original photos, sidecars and library edits. Use disposable physical copies and private LightCraft sessions with XMP auto-write disabled.
- Keep Lightroom control in the background. Retire owned temporary files when no longer needed; avoid new test infrastructure.

## Landed behavior and evidence

Imported-profile monochrome treatment is independent of global treatment and survives profile-only copy, presets, sync, undo and Amount 0.
Explicit session-only type-0 HSV tables now render on the CPU before camera tone/B&W. Only their fingerprint is saved; use `profile.loadTable` / `profile.unloadTable` as described in [control-protocol.md](control-protocol.md#explicit-runtime-profile-tables).
Supported scope is Process 2, SDR and bounded Amount 0–200; unresolved or unsupported resources remain diagnosed.

The earlier runtime-table implementation passed all eight `cargo xtask ci` gates. Seven native RGB16 chart states pass 43 lifecycle checks; missing resources, Amount 0 and unloading preserve the expected pixels. Headless UI loading/unloading was checked.
The final native comparison is against the same inputs and fixed regions used by the measured candidate.

**Fidelity is not solved.** Chart mean ΔE00 improves, but six B&W cells worsen. On the saved D3500 Amount 120 photo, grid mean ΔE00 improves **4.771952 → 4.405292**, while the fixed center crop worsens **2.660664 → 2.734120**. See [the full limits and measurements](target-app-parity.md#runtime-profile-table-evidence-2026-10-11). Do not turn these into an overall fidelity percentage.

## Resume on the Mac

Repository: `/Users/admin.ilya.dorman/Dev/lightcraft`, branch `fix/lightroom-render-fidelity`.
Read the local `plan/MAC_HANDOFF.md`, `plan/STATUS.md` and `plan/LIGHTROOM_EXPORT_WORKFLOW.md` first; `plan/` and its private evidence are intentionally not pushed.

```sh
source plan/env.sh
```

Evidence is under `plan/lightroom-reference/exports/continue-20261010/`:

- `runtime-render-validation/`: source/binary freezes, CI, native chart/photo comparisons and UI snapshots.
- `native_control_diagnosis/profile-runtime-resolution/`: profile-stage and Amount reference measurements.
- `fidelity-audit-profile-table-20261010/`: independent reviews.

Lightroom exports use the already paired local controller and a serial queue:

```sh
python -B plan/lightroom-reference/tools/controller_export_queue.py /absolute/path/to/reviewed-queue.json
```

There are now 120 native-verified scripted exports, including five local-sharpening exports on this resume. Use new output paths, exact source/settings guards and the retained TIFF16/full-size/AdobeRGB/no-output-sharpening settings. Do not resubmit uncertain jobs. Never print or copy the pairing token.

The controller returns no `SharpenEdgeMasking` value. Keep its 13 supported readiness checks and verify Masking in the exported XMP: this run explicitly retains 100 but omits zero, Radius and Detail at global Sharpness 0. Lightroom also normalizes legacy gradients to mask groups and adds identity `LocalCurveRefineSaturation=100`. Preserve these distinctions; missing metadata is not a verified numerical value.

## Current measured integration

Private evidence: `plan/lightroom-reference/exports/resume-20261011/`.

- `local-sharpening/measurement-04/`: the authored D3500 local-sharpening references. Reverse-order repetitions of both active cases match every native pixel and ICC byte. Removing the local/global Masking interaction worsens paired luminance error by about 50% on the fixed grid and 45% in the center. The CPU/GPU interaction is restored; its formula and sharpening strength remain unmatched.
- `fidelity-audit/near-black-white/`: upstream source changes leave DSC0805 pixel-identical and remove a false-white DSC0626 pixel (its ΔE00 99.052→3.503). Mean improvement is tiny, and every worsening region/bin is retained. The combined experiment does not isolate RAW white estimation from the near-black colour guard.
- Camera-look version 7 and render-cache version 53 invalidate stale source proxies and rendered output. Preset Amount handling survives the docking rebase.
- The first integration CI exposed an interaction between our SDR white ceiling and upstream camera-tone HDR extension. The exact ceiling remains SDR-only; the existing test now covers the integrated behavior. This does not expand the Lightroom measurement scope to HDR.

The corrected implementation passes all eight `cargo xtask ci` gates. Its pinned
CLI is `lightcraft-cli-restored-81ea9683db9b` in that evidence directory. Against
the pulled implementation, restoring Masking reduces paired luminance-response
error by **33.5% on the fixed grid and 31.1% in the center**. The no-local-sharpening
and unmasked controls remain pixel-identical. Native comparisons and independent
review are complete; this fixes the regression, not the remaining sharpening gap.

## Next work

1. Continue conventional sharpening/colour-NR response measurements on the user's D3500. Local Masking is not independent in the measured Lightroom case; preserve that counterevidence. Global Sharpening Detail and luminance-NR Contrast remain stored no-ops.
2. Investigate profile-table ordering relative to Exposure using own sources. No new exposure reference has been exported. A failed composition check alone does not prove the reverse order. Do not read or hash Adobe app-bundle contents under the current project instructions.
3. Continue remaining tone/B&W and curve gaps. The existing saved Amount 120 paired-response audit is complete; baseline source-colour error dominates and the full-photo mean can hide cancellation.

A sixbox-to-Mac SSH shell round trip was verified using sixbox's existing `macbook-air` alias. Lightroom control/export over that SSH route was **not** verified, and no migration or SSH configuration change was made. A future migration needs the local evidence and export-worker arrangement; a Git clone alone is insufficient.
