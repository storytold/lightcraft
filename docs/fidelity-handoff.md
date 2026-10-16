# Lightroom fidelity handoff

The 2026-10-15 integration rebases the accepted fidelity branch onto upstream `main` at
`0da998f2` (0.8.0), 699 commits after its previous base. The user's fidelity investigation remains
paused. No new Lightroom exports or calibration candidates are part of this integration.

Relevant upstream changes include original-resolution scaling for preview noise reduction,
correct radial-mask inversion and legacy feather import, hard-edge spot safety, crop rotation
and straightening, named-profile preset resolution and CLI settings/export handling. AHD and
camera-look processing also gain speed and reduce memory use. Built-in profile-name mappings
remain approximations. These changes do not establish Lightroom parity: the remaining D3500
colour and master Refine Saturation mismatch is unresolved.

Integration preserves explicit saved Look parameters over name-only profile substitutions,
persisted unsupported-edit diagnostics, native detail dimensions and the accepted curve behavior.
Catalog format **14**, camera-look version **10** and render-cache version **66** distinguish the
combined writer and invalidate older caches. Optimized-development CLI `7f521de62369` reproduces
all four accepted D3500 controls exactly: complete 6000×4000 RGB16 pixel hashes, ICC bytes and
saved settings match CLI `4449dd66d7af`. Source/sidecar guards pass, and temporary full TIFFs were
retired after compact verification. These CNR0 native controls do not measure active preview NR
or establish new fidelity. Private evidence: `plan/rebase-20261015/native-verification.json`.
All eight `cargo xtask ci` gates pass, including CPU/GPU equivalence on Apple M4 Metal.
The run reports 165 checks skipped for absent optional corpus, fonts or model data; the four
private D3500 comparisons above ran separately. Existing tests were adapted; no new test
functions or infrastructure were added. Verification receipts remain after 163 MB of redundant
owned RAW copies and duplicate compact arrays were retired against retained equivalents.

## Previous integration (2026-10-13)

Resumed on 2026-10-13 after publishing PR #1226. Rebased onto upstream `main` at `b3d392b4` (0.7.1), including its guarded camera dark-tone fit, preset-list caching and virtualization, history cursor and sidecar fixes. Integration preserves the fork's previous shadow fit as the baseline for the new dark-tone gate. Rendering fidelity remains incomplete.

Four fresh D3500 native comparisons against the accepted CLI `1a39853139c6` are **pixel-identical by complete RGB16 SHA-256 and ICC identity**: DSC0805 and DSC0626 with their corrected saved CNR0 edits, plus train02 neutral and +1 EV. Each export is 6000×4000, with identical complete imported/exported settings and diagnostics. DSC0626's four new toe attempts fail the 1% dark-error improvement gate; the prior fit remains. This is preservation evidence, not a new fidelity gain. Fixed grid, center and all nine interior regions retain their Lightroom comparisons, including missing-profile and white-balance limitations.

The combined writer now uses catalog format **13** to retain both fork settings and upstream history-cursor data. Camera-look version **9** and render-cache version **65** invalidate older caches. The preset Amount UI was inspected in a headless app snapshot; the existing checks cover Amount, saved edits and independent sidecar ownership. Private evidence: `plan/lightroom-reference/exports/resume-20261013/`. All eight `cargo xtask ci` gates pass after integration. Final CLI `4449dd66d7af` reproduces all four measured native outputs and settings exactly. The repaired all-masks overlay was inspected in a headless app snapshot; both regions remain visible. Existing tests were adjusted, with no new test functions or infrastructure. Optional craft-fonts were unset; WASM checks pass with existing target-specific warnings.

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

There are now 174 accepted native scripted exports, including local sharpening, colour-NR structure/channel charts, exposure and compressed-NEF sensor charts. Three requested-Refine100 chart exports carry an explicit metadata-omission qualification; their numeric100 setting was not independently read back. Use new output paths, exact source/settings guards and the retained TIFF16/full-size/AdobeRGB/no-output-sharpening settings. Do not resubmit uncertain jobs. Never print or copy the pairing token.

The controller returns no `SharpenEdgeMasking` value. Keep its 13 supported readiness checks and verify Masking in the exported XMP: this run explicitly retains 100 but omits zero, Radius and Detail at global Sharpness 0. Lightroom also normalizes legacy gradients to mask groups and adds identity `LocalCurveRefineSaturation=100`. Preserve these distinctions; missing metadata is not a verified numerical value.

`openImage` acknowledgment is not image-load completion. A chart-to-NEF transition with the same
13 neutral values exported the previous chart. Native dimensions and `RawFileName` rejected it.
After recording and archiving that completed wrong-image output, a fresh job added the supported
`SharpenDetail=50` readiness value, distinguishing the intended photo from the prior chart's 100;
the correct 6000×4000 photo and full settings then verified. Use a distinguishing, source-backed
readiness value when available, and always verify the actual exported identity. Controller
Temperature/Tint readings are not necessarily the Kelvin/tint numbers in XMP. This remains a
verified one-shot export workflow with failure detection, not an identity guarantee from `openImage`.

For repeated same-setting targets, first load a surviving owned reference with a different supported
readiness value (the latest queues use Detail 100 before the target's 50), wait for all expected
values and export readiness, then submit the target once. If the initial active value is null,
record that state and bootstrap the owned reference separately; never assume the previous image
is still loaded. The intermediate step only opens a disposable reference; it changes no edits and
does not export. Full exported identity and CRS still decide acceptance. Saved export preferences
are compared before/after; Output Sharpening None is backed by the retained same-session UI witness,
because the preferences reader does not expose it. No foreground input is needed.

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

## Measured mixed-curve ordering correction

The V2/V3 SDR correction applies the existing master/parametric refinement
before individual-channel curves. It changes placement only; the master Refine formula remains
approximate. Refine 100 retains the old composed LUT path exactly. Master-only, channel-only,
V1, HDR and imported-profile placement are preserved. Native RGB16/ICC checks pass for all eight
master-only 0/50 and red/green/blue channel-only 0/100 controls, plus mixed 100.

The mixed affine-master/nonlinear-blue chart improves pooled 24-interior mean ΔE00 **2.646066 → 0.383281**.
The fixed next already-seen D3500 training source (train02), chosen before candidate scores,
improves grid phase (8,8) mean ΔE00 **9.949168 → 1.365652**. All 13 photo scopes improve mean ΔE00 and
paired XYZ RMS; some Y/lightness summaries and tails worsen. Both edited binaries use the
shared303c neutral anchor, with no fresh baseline5b or candidate neutral capture. The full
[measurements and limits](target-app-parity.md#mixed-masterchannel-ordering-correction-2026-10-11)
retain every worsening and unsupported comparison.

Private evidence: `refine-mixed-order/native-candidate-score-01`,
`refine-mixed-order/candidate-controls-native-equivalence-01.json`, and
`refine-mixed-photo/native-comparison-01`. The original failed analytic-affine composition gate
remains failed; empirical forward compatibility does not identify Lightroom's internal order.
Mixed0 and100 both have explicit numeric readback; mixed100 omits only CompatibleVersion.

Final CLI `1a39853139c6` passes all eight `cargo xtask ci` gates, including the existing CPU/GPU
curve-equivalence checks on an available GPU. Both mixed chart states and the D3500 photograph
match every measured candidate RGB16 pixel and ICC byte. Independent reviews passed; one existing
test expectation was adjusted, with no new test functions. Cache64 invalidates prior rendered
previews. Refine99.99999 and100 produce identical RGB16/ICC on the fixed chart; this does not prove
continuity for arbitrary curves. Sequential LUT evaluation remains an approximation. No generic
curve/camera parity or readiness increase is claimed.

## Pause boundary: color investigation (2026-10-12)

No new color candidate was accepted after the mixed-curve correction. A fresh original D3500
neutral/+1 EV pair measures whole-frame mean ΔE00 2.237445 and 2.670111 respectively. At +1 EV,
LightCraft has 17,276,292 blue-channel samples at 65535 versus Lightroom's two; this is not the
same as all-channel white clipping. A wider output-gamut diagnostic leaves the brightness excess.
The fitted camera-tone continuation is a concrete lead, not an isolated proof of the entire cause.

A parameter-free rational continuation reduced that photo's +1 EV luminance RMS error by 38.14%
and mean ΔE00 by 5.37%, while worsening opponent-chroma RMS by 19.55%. After isolating it from
source fitting, it failed the predeclared mean-ΔE00 rule on another training scene. It was rejected
before heldout inspection. The shared white-balance-dependent source-color fit and conventional
NR candidates also remain rejected. None of these improvements is a landed or aggregate fidelity gain.

The final compressed-NEF transport experiment passes: LightCraft's own codec tables regenerate
the original compressed strip byte for byte, preserving the complete decoded CFA and metadata.
An appended-strip copy changes only its raw StripOffsets field in the original prefix. Its one
Lightroom export has exactly the original reference's **24,000,000 RGB16 pixels and ICC bytes**, with
the unique filename and full CRS verified. This qualifies transport of the original samples;
no synthetic calibration chart or new camera model has been validated.

Local records under `resume-20261011/fidelity-audit/`:

- `d3500-exposure-response-01/measurement-01/`: complete original neutral/+1 measurements.
- `d3500-exposure-response-01/rational-shoulder-02/rejection.json`: rejected continuation and other-scene results.
- `d3500-compressed-transport-01/native-equivalence-01.json`: exact transport result, with metadata, export and retirement receipts beside it.

Completed candidate binaries and redundant pixel buffers were retired after verification. The
original reference compacts, source copies, reports and helper source remain local. Historical
guards for retired files are provenance, not runnable capture inputs. The pinned accepted CLI is
`resume-20261011/lightcraft-cli-mixed-final-1a39853139c6` (built before the version-only rebase).
Do not use an unverified shared target binary as a fidelity baseline.

## Sensor-color investigation resumed (2026-10-13)

Two authored D3500 CFA charts pass exact full-sensor compression/metadata roundtrips and
one-shot Lightroom identity/settings checks. Each has 48 fixed flat interiors. The mixed-color
chart avoids the output clipping seen at the primary corners. Identical neutral sensor samples
at identical coordinates nevertheless differ by up to 1.539 L* between charts, with matching
exported CRS apart from filename and exact ICC equality. Re-exporting an exact copy of the primary
under a fresh filename reproduces every native pixel and ICC byte after the intervening mixed chart.

This rules out pooling those outputs into a context-independent pointwise calibration without
further investigation. Histogram, minimum sensor value, surrounding chroma and spatial layout
changed together; Lightroom's internal normalization remains unknown. No new camera model,
product improvement or heldout validation follows from these diagnostic exports. Their unrelated
retained embedded JPEG must not drive a LightCraft chart fit. Complete native RGB16/ICC/XMP
compacts and protocols remain in `resume-20261013/d3500-cfa-chart*`; redundant TIFFs were retired.
A permutation preserving every Bayer-phase histogram and a separate 4,096-sample black patch
both leave all 48 measured interiors pixel-identical to the mixed chart. Neither identifies the
remaining contextual mechanism; the black-patch result rules out only that specific intervention.
Replacing only the output-cropped eight-pixel sensor rim also leaves all 48 primary-chart
interiors unchanged. The entire 24-million-sample visible CFA crop was verified identical;
this null result does not identify Lightroom's internal margin handling.

The new upstream dark-refinement gate compares against this branch's inherited shadow fit.
Its reliability exclusions apply to the additional refinement, not retrospectively to all samples
in that baseline. Existing regressions cover rejection with both explicit and colour-only baselines.

## Continuing work

1. Isolate the repeatable sensor-chart context effect before fitting source color/exposure. The fixed histogram-preserving column permutation and a small black-anchor intervention both left all 48 measured interiors pixel-identical; a substitution of only the output-cropped sensor rim is also unchanged. Broader histogram or tone adaptation remains unisolated. Keep the failed DNG/uncompressed transports rejected, and the four source-color heldouts sealed.
2. Continue the unresolved master Refine formula and profile-table/Exposure ordering using own sources. The nonlinear table/precomposed EV0 transport failed; the fixed EV0 table render is close but does not establish general ordering or exposure parity. Preserve these failed gates rather than recycling candidates.
3. Continue remaining tone/B&W and conventional NR/sharpening gaps. Global Sharpening Detail and luminance-NR Contrast remain stored no-ops. Baseline source-color error can dominate, and whole-photo means can hide regional regressions. D3500 only; optics, lens blur and AI remain excluded.

A sixbox-to-Mac SSH shell round trip was verified using sixbox's existing `macbook-air` alias. Lightroom control/export over that SSH route was **not** verified, and no migration or SSH configuration change was made. A future migration needs the local evidence and export-worker arrangement; a Git clone alone is insufficient.
