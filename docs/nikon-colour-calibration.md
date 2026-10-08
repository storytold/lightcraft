# Nikon base colour and chart calibration

**Framework implemented; no Nikon body is claimed to be measured/calibrated by this change.**
In particular, the private Z 8 `_DSC2360.NEF` / JPEG pair is an ordinary photograph. It is useful for
checking decoding, UI and compatibility, and cannot identify an accurate Z 8 colour matrix or an
absolute Kelvin scale. No Z 8 profile, camera matrix, chart reference values or private media are bundled.

## Choose an interpretation

The Edit panel's RAW processing selector dispatches `develop.rawColor`. CLI, control and MCP use
the same command. All edits missing `raw_color` use **legacy**, including existing catalogs, history,
versions, presets and LightCraft XMP payloads. Newly imported photos also remain legacy until selected.
Compatibility here means the requested base commit **d614dc7**, which already includes the separate
Tint-direction change. This does not migrate or recover the meaning of older, pre-d614dc7 Tint edits.

| Mode | Sensor colour | Display style / white balance |
|---|---|---|
| `legacy` | Existing d614dc7 processing | Existing JPEG estimate, relative WB where previously used |
| `base` | Valid standard file calibration first, otherwise an explicit own-profile snapshot | No fitted JPEG tone, hue table or creative look baked into the source; Kelvin/Tint interpreted through the camera model |
| `matchCamera` | Same calibrated base | Optional guarded JPEG **tone/chroma** fit; it cannot replace the calibrated sensor matrix; model-based WB |

With no valid model, both new modes retain the **exact existing source interpretation and estimate**
and show “Estimated colour — no valid camera calibration; previous look retained”. WB stays relative;
6500/0 is the internal neutral reference, not measured capture temperature. This compatibility fallback
may already include the historical JPEG colour/look estimate. Choosing Base without supplying a
calibration does not force the generic neutral matrix onto a photo whose existing estimate looks better.
Stable new sensor cropping/levels are used by calibrated processing and the chart sampler; fallback
keeps legacy geometry/levels as well, to avoid silently changing existing appearance. `chart inspect`
reports the stable decoder independently of this rendering fallback.

Example (paths refer to your own files):

```sh
lightcraft-cli run --import photo.NEF develop.rawColor mode=base profilePath=my-camera.profile.json \
    develop.rawColorInfo app.export path=base.png longEdge=1600
lightcraft-cli run --import photo.NEF develop.rawColor mode=matchCamera profilePath=my-camera.profile.json \
    app.export path=camera-style.png longEdge=1600
```

`profile` accepts the profile JSON directly (including in web/MCP); `profile: null` clears it.
`profilePath` reads a native local file up to 128 KiB. Make/model must match exactly after trimming.
The complete profile is stored with the edit, so replacing a file later cannot change its rendering.
Changing the interpretation resets WB to As Shot, because relative controls cannot be translated to
absolute Kelvin without a model. Other edits remain; Undo restores the entire previous setting.
`develop.rawColorInfo` returns the effective model, source, as-shot values, relative-WB flag and whether
the requested JPEG style was actually accepted. A rejected style leaves the calibrated base.

The legacy `calibrate` subcommand is now explicitly labelled a compatibility alias for
`match-camera`: it pools ordinary RAW/embedded-JPEG pairs into **JPEG look estimates**, not sensor
calibrations. These files are a different schema/directory from the new explicit calibration snapshots
and cannot be passed to `develop.rawColor` as measured profiles.

## What the stable decoder assumes

- Same clean-room pure-Rust Nikon Huffman / uncompressed decoder. Unsupported compression still
  returns an actionable error or the existing preview-only notice; this work does not add Nikon HE/HE*.
- Nikon BlackLevel (`0x003d`) is a four-site CFA pattern, in 14-bit units. Twelve-bit files divide it by
  four. Values must be finite/nonnegative; black at or above saturation is rejected in stable mode.
- Saturation uses an explicit standard WhiteLevel when valid, otherwise a lossy decoding curve's
  endpoint, otherwise `(1 << bits) - 1`. **The code-range fallback is an estimate**, not a per-body
  measured saturation level. Unlike legacy processing, it is never fitted to the scene's brightest patch.
- Nikon CropArea (`0x0045`: left, top, width, height) is bounds checked and applied after demosaic.
  The CFA origin stays at the original sensor origin, including odd crop offsets. Without a valid crop,
  stable mode keeps the full sensor extent; it does not guess masked columns from scene brightness.
- As-shot neutral comes from explicit standard neutral/white data or Nikon WB_RBLevels (`0x000c`).
  A calibrated render requires a positive neutral that the model can interpret. Missing or incompatible
  WB returns an error instead of inventing an absolute capture temperature.
- Only standard XYZ-to-camera ColorMatrix tags with identified illuminants are used as file
  calibration. Finite values, conditioning, positive white responses and interpolation are checked.
  Standard CameraCalibration, AnalogBalance and valid ForwardMatrix tags retain their reference-camera semantics.
  CameraCalibration is used only when the camera/profile calibration signatures agree (both absent
  means the default empty signature); a missing correction at either light means identity.
  An arbitrary Nikon maker-note tag called “ColorMatrix” is **not** assumed to have DNG semantics.
- Own profiles contain two XYZ-to-unbalanced-camera matrices; interpolation is linear in inverse
  Kelvin, clamped outside their measured temperature range. Green-normalised neutral fixes the
  exposure scale. The selected model converts sensor RGB to linear Rec.2020 D65 with Bradford
  adaptation; global WB recomputes the camera transform, with positive Tint adding magenta.
- The optional JPEG style honours its own ICC/EXIF metadata, or Nikon ColorSpace for an untagged
  embedded JPEG, before transfer decoding/resampling. It fits only a display curve on calibrated
  sources. Existing legacy JPEG estimation remains untouched for compatibility.

A two-matrix model is an approximation. It does not measure sensor spectral sensitivities and cannot
remove metamerism, mixed-light variation, fluorescent/LED spectra, all picture styles or local camera
processing. DCP/LCP imports and Adobe data remain unsupported by this workflow.

## Capture and reference requirements

1. Use a physical chart you are licensed to photograph and reference measurements you own or may use.
   Record chart edition/serial and the source/license of the reference values. Generic manufacturer
   sRGB swatches and another developer's rendering are not sensor-calibration ground truth.
2. Make separate warm (e.g. roughly 2856 K) and daylight (roughly 6500 K) captures. Measure illuminant
   **xy** and chart patch **XYZ under that light**, relative to diffuse white Y=1. Measured spectral
   reflectances integrated with the measured light are also suitable. D50 Lab values alone do not
   become accurate warm/daylight references by merely assigning another Kelvin value.
3. Keep lighting even, avoid glare, defocus slightly to suppress chart texture, and expose so all sampled
   channels are above the noise floor and below clipping. Include a spectrally neutral patch. Use the
   same ISO/sensor mode you intend to validate; record them in provenance. Check high/low ISO separately.
4. Reserve **different original captures**, including warm, daylight and at least one intermediate light,
   before fitting. Include a neutral ramp and additional exposure levels. Prefer a second chart/target
   with measured values, and also evaluate skin and saturated colours outside the training chart.
   Validation on another set of pixels from the same ordinary photograph is not independent evidence.

## Sample chart interiors

Create a layout JSON for each capture. A region is `[left, top, width, height]` in fractions of the
**unrotated default crop**, not a perspective-corrected JPEG. Coordinates must refer to patch interiors,
with margins away from borders. There must be 12–256 named, distinct patches; use all useful colours.
The following shows the schema of one region; supply your own measured values and the remaining regions:

```json
{
  "provenance": {
    "author": "Your chosen name",
    "source": "Your measurement project / capture log and reference source",
    "license": "Your applicable data licence",
    "created": "YYYY-MM-DD",
    "chart": "Chart model, edition, serial, ISO and capture conditions",
    "reference": "Instrument / spectral integration method and measured XYZ source"
  },
  "white": {"x": 0.3127, "y": 0.3290},
  "neutral_patch": 0,
  "patches": [
    {"name": "neutral", "rect": [0.10, 0.10, 0.08, 0.08], "xyz": [0.17109, 0.18, 0.19603]}
  ]
}
```

These illustrative numbers describe a neutral under standard D65, **not a measured chart or a Nikon
profile**. Do not reuse them as measurements. Use a common provenance record across the project and
separate layouts with the actual xy/XYZ for each light.

```sh
lightcraft-cli chart inspect warm.NEF
lightcraft-cli chart sample warm.NEF warm-layout.json warm-samples.json
lightcraft-cli chart sample day.NEF day-layout.json day-samples.json
lightcraft-cli chart sample holdout-warm.NEF warm-holdout-layout.json holdout-warm.json
lightcraft-cli chart sample holdout-day.NEF day-holdout-layout.json holdout-day.json
lightcraft-cli chart sample holdout-middle.NEF middle-layout.json holdout-middle.json
lightcraft-cli chart fit my-camera.profile.json warm-samples.json day-samples.json
lightcraft-cli chart validate my-camera.profile.json validation.json \
    holdout-warm.json holdout-day.json holdout-middle.json
```

Outputs must not already exist. Chart RAW inputs are regular files up to 256 MiB; JSON inputs are capped at 8 MiB. Sampling hashes the original with SHA-256, averages Bayer sites
before demosaic/WB, uses bounds-checked regions, and rejects clipped/dark samples. It is currently a
Bayer sampler. No image or preview pixels are stored in profiles: only matrices, identity, provenance
and training capture hashes. Sample files contain measured patch averages and supplied XYZ values.
The fitter uses least squares without offsets/tone curves. Each capture's neutral green / reference Y
normalises its exposure; this avoids accidentally learning exposure changes as colour. It rejects
singular/uninformative charts and requires two lighting groups (≤150 K spread in each group).

Validation refuses camera mismatches, duplicate originals and any training SHA-256. Reports contain
per-capture mean/p95/max CIE76 ΔE, neutral chroma, and coverage of warm/daylight/intermediate light.
These errors are relative to supplied XYZ and **neutral-normalised exposure**; they do not by
themselves prove absolute exposure correctness. There is no automatic “camera calibrated” badge.
Inspect patch residuals and repeat captures; retain the report, measurement sources and capture hashes
with the profile. Validate clipped highlights, multiple exposure levels and other scenes separately.
Do not tune on the held-out files and continue calling them independent validation.

## Regressions and actual evidence

Tests use original procedural matrices, synthetic sensors/ramps/charts and separate synthetic capture
identifiers. They exercise fitting/interpolation, neutral greys, Tint direction, model-based WB,
exposure gain, malformed input rejection, old-settings default/undo, cache invalidation, portable
profiles and CPU/GPU equivalence. Synthetic accuracy demonstrates the framework, not any Nikon body's
measured accuracy. Smart previews carry their source model/WB and are keyed by the interpretation;
old previews are not reused for a new calibrated interpretation.

The actual private Z 8 sample and final CI/UI evidence are recorded in
[nikon-colour-validation.md](nikon-colour-validation.md). The absence of independent chart captures
is an explicit remaining gap. No Lightroom-parity or measured Z 8 accuracy claim is made.
