# Lightroom fidelity handoff

Resumed on 2026-10-11 at the user's request. The first integration pulled `172ec37c` and rebased onto `0dd6593d`; the second integrates upstream `76b53778`. Both have native measurements. The second integration passes all eight quality gates, and its final binary reproduces all four measured V2 native renders exactly.

## Scope

- Verify the user's Nikon D3500 edits: B&W, profiles/presets, curves, tone, conventional noise reduction and sharpening.
- Optics, lens blur, AI tools and cross-camera consistency are out of scope.
- Preserve original photos, sidecars and library edits. Use disposable physical copies and private LightCraft sessions with XMP auto-write disabled.
- Keep Lightroom control in the background. Retire owned temporary files when no longer needed; avoid new test infrastructure.

## Landed behavior and evidence

Imported-profile monochrome treatment is independent of global treatment and survives profile-only copy, presets, sync, undo and Amount 0.
Explicit session-only type-0 HSV tables now render on the CPU before camera tone/B&W. Only their fingerprint is saved; use `profile.loadTable` / `profile.unloadTable` as described in [control-protocol.md](control-protocol.md#explicit-runtime-profile-tables).
Supported scope is Processes 2 and 3, SDR and bounded Amount 0–200; unresolved or unsupported resources remain diagnosed.

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

There are now 150 accepted native scripted exports, including local sharpening, colour-NR structure/channel charts and companion-control references. Three recent requested-Refine100 chart exports carry an explicit metadata-omission qualification; their numeric100 setting was not independently read back. Use new output paths, exact source/settings guards and the retained TIFF16/full-size/AdobeRGB/no-output-sharpening settings. Do not resubmit uncertain jobs. Never print or copy the pairing token.

The controller returns no `SharpenEdgeMasking` value. Keep its 13 supported readiness checks and verify Masking in the exported XMP: this run explicitly retains 100 but omits zero, Radius and Detail at global Sharpness 0. Lightroom also normalizes legacy gradients to mask groups and adds identity `LocalCurveRefineSaturation=100`. Preserve these distinctions; missing metadata is not a verified numerical value.

`openImage` acknowledgment is not image-load completion. A chart-to-NEF transition with the same
13 neutral values exported the previous chart. Native dimensions and `RawFileName` rejected it.
After recording and archiving that completed wrong-image output, a fresh job added the supported
`SharpenDetail=50` readiness value, distinguishing the intended photo from the prior chart's 100;
the correct 6000×4000 photo and full settings then verified. Use a distinguishing, source-backed
readiness value when available, and always verify the actual exported identity. Controller
Temperature/Tint readings are not necessarily the Kelvin/tint numbers in XMP. This remains a
verified one-shot export workflow with failure detection, not an identity guarantee from `openImage`.

## Previously measured integration

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

## Second upstream integration measured

The 16 fork commits have been rebased onto `76b53778`. Preserve numeric Process 2 as this fork's fixed-domain curves with its original spatial tone base. Process 3 adds upstream's median-prefiltered Highlights/Shadows and is explicit opt-in; new imports, Reset and Update to Current Process still select V2 pending photographic evidence. Upstream's numeric V2 had a different meaning and cannot be identified reliably from the saved number alone; automatic appearance-preserving migration from that lineage is not claimed.

Combined catalog format 12, explicit-Kelvin settings schema 6, camera-look version 8 and render-cache version 62 separate the combined writer from both prior lineages. The upstream Draft NR cache must clear the fork's sharpening plane along with log luminance and Texture. No experimental CNR model is included.

Three fixed two-scale CNR probes were rejected: late application, early as-shot-WB coordinates, and early unbalanced camera coordinates. On the corrected matching settings, the last improves pooled chroma-response error by21.84%/2.37% on the two D3500 scenes, but the second photo has regional regressions and pooled XYZ-response error worsens30.77%; exports take17.66/21.30s. These are diagnostic results, not overall fidelity gains. The earlier photographic comparison with missing Exposure+2.06/Shadows+43 on DSC0805 was retracted and replaced by the exact sidecar-import comparisons.

Private records are under `resume-20261011/color-nr-matched-native`, `color-nr-source-stage-native`, `color-nr-pre-wb-native` and `upstream-integration-76b53778`. Eight native captures from pinned CLI `4145003d4505` compare combined V2 with the old binary and optional V3 with V2, using identical sources, imported settings, Lightroom references and regions. V2 preserves every DSC0805 pixel and ICC byte at Color NR 0 and 25. DSC0626 has only small sampled numerical drift (maximum direct ΔE00 0.003841/0.003134); no material fidelity gain is claimed.

V3 changes DSC0805's active Shadows adjustment: grid mean ΔE00 at Color NR 0 improves 8.19710→8.17058, but two interior regions worsen and export time rises 3.68→5.11 seconds. DSC0626 is pixel-identical to V2. Independent review reproduces 43,228/43,226 numerical checks within 7.816e-14. These mixed results keep V3 opt-in.

Final CLI `303c06aeda65` passes all eight `cargo xtask ci` gates. Four fresh imports/exports preserve every measured V2 native RGB value and ICC byte, with exact settings and diagnostics. The final corrections distinguish Legacy/Kelvin white balance in the source-stage cache and restore strict alternate-sidecar validation before saving. Existing compatibility tests were adjusted for Kelvin serialization and macOS path/dialog behavior; no new test functions were added. Owned full TIFFs were removed after verification, and original sources/sidecars remain unchanged.

## Current channel-only Refine correction

Still-unreleased V2/V3 SDR now bypasses Refine Saturation for individual-channel curves when the
master, parametric controls and built-in master fade are inactive. V1 and HDR retain their prior
path; saved settings are not rewritten. Existing development V2/V3 appearances can change.
Render-cache version 63 prevents stale rendered previews; camera-look version 8 is unchanged.
The explicit pre-release exception is recorded in [process-versions](process-versions.md#the-rule).

Three fixed native charts cover red/green affine and nonlinear blue curves. All twenty coloured
interiors improve and four gray interiors remain unchanged in each. Candidate Refine 100 and
master-affine Refine 0/50 native controls preserve prior pixels/ICC. Each Lightroom requested-100
export omits the Refine and CompatibleVersion fields, so the measured target is fully verified
explicit 0; the requested-100 identity evidence has no independent numeric-100 readback.

One already-seen D3500 training photo with the red-affine Refine-0 edit improves direct grid mean
ΔE00 11.146628 → 4.939543. Every direct ΔE00 summary improves across all thirteen fixed scopes,
but direct absolute-Y mean/RMS worsen everywhere and other distribution regressions remain.
Paired-response means/RMS improve, using a shared retained303c neutral anchor rather than a fresh
candidate-neutral capture. This is no heldout or general photographic parity claim. See the
[complete scope and metric caveats](target-app-parity.md#one-d3500-photographic-witness).

Private records under `resume-20261011/fidelity-audit/`: `refine-channel-curve`,
`refine-channel-extra/native-comparison-01`, and `refine-channel-photo/native-comparison-02`.
Earlier failed export/import attempts and their narrowly scoped amendments remain preserved.

Final CLI `5b882e762716` passes all eight `cargo xtask ci` gates. Fresh native captures of the
D3500 edit and both red-chart Refine states match every measured candidate RGB16 pixel and ICC
byte, with exact settings, diagnostics and owned-source guards. One existing test was extended;
no new test functions were added. Redundant full TIFFs were retired after verified compact retention.

## Next work

1. Measure an affine master plus the already measured nonlinear blue curve at Refine 0/100 to test curve ordering and interaction. Unlike identical affine curves, these operations do not commute. Keep the empirical master-only endpoints and require the mixed-100 composition gate before interpreting the result. The master endpoint formula remains unresolved; no new endpoint model or fitted replacement is proposed. Preserve the channel-only correction and V1/HDR behavior.
2. Investigate profile-table ordering relative to Exposure using own sources. No new exposure reference has been exported. A failed composition check alone does not prove the reverse order. Do not read or hash Adobe app-bundle contents under the current project instructions.
3. Continue remaining tone/B&W and conventional NR/sharpening gaps without recycling rejected candidates. The proxy-order replay left the existing winners unchanged; the shared Lightroom-targeted source-colour fit failed the training no-regression gate, and its four heldout scenes remain sealed. No source fit or experimental CNR model was adopted. Global Sharpening Detail and luminance-NR Contrast remain stored no-ops. The saved Amount 120 paired-response audit is complete; baseline source-colour error can dominate and full-photo means can hide cancellation.

A sixbox-to-Mac SSH shell round trip was verified using sixbox's existing `macbook-air` alias. Lightroom control/export over that SSH route was **not** verified, and no migration or SSH configuration change was made. A future migration needs the local evidence and export-worker arrangement; a Git clone alone is insufficient.
