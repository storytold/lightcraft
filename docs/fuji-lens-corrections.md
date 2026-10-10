# Fujifilm embedded lens corrections

LightCraft reads the distortion and vignetting tables that Fujifilm cameras store in the raw IFD of every RAF
(`0xf00b` GeometricDistortionParams, `0xf010` VignettingParams, signed rationals) and turns them into the existing
`OpcodeList3` pair, `WarpRectilinear` and `FixVignetteRadial`. The profile-correction control, the CPU / GPU optics
path, EXIF orientation handling and DNG export use them like any other embedded correction.

Corrected: **distortion** (with the camera's frame-filling zoom) and **vignetting**, for the 59 bodies listed in
`crates/raw/src/vendor/raf_lens.rs` (`VALIDATED`). **Not corrected: lateral chromatic aberration** (the third table,
`0xf00f`); see "Chromatic aberration" below. Bodies without a checked sample, and tables in a layout or normalisation
that was not understood, stay uncorrected.

## Clean-room evidence

The tag names come from the [ExifTool FujiFilm tag-name documentation](https://exiftool.org/TagNames/FujiFilm.html);
ExifTool was only run as a black box (`exiftool -j -n`, `-v3`) to print the arrays. Everything else was measured:
the 129 CC0 RAFs with these tables under `raw.pixls.us` (Fujifilm, 65 bodies) against the camera's own embedded JPEG, which the
cameras correct in-camera, and against LightCraft's uncorrected raw render. No other decoder's code or data,
lens database or Adobe profile was consulted. Scripts and per-file results: `data/testing/raf-lens/` (local).

## Layout and units

Every table is an array of signed rationals with `n` knots:

```
[scale, knot_1 … knot_n, value_1 … value_n]
```

- `scale × n` is the half diagonal of the recorded image (the RAF crop) in pixels: 3749 for 6240×4160, 4643 for
  7728×5152, 2942 for 4896×3264, 7280 for 11648×8736, on 61 of the 65 bodies. Files that store another number (X-S20,
  X-T30 III and X-M5 write the 40 MP value, 4643; an X-T5 in an in-camera crop) are left uncorrected.
- Knots are positions along the radius. The **last knot is the farthest corner**: `ρ = knot_last · r`, `r` the
  distance from the centre in half diagonals. Older bodies use 11 knots `0, 0.1 … 1`, newer ones 9 knots evenly
  spaced in `r²` (`√(k/8)`, last `√(9/8) = 1.0607`), GFX 50 bodies 8 (`√(k/8)`, last 1). The distortion zero and the 100 %
  illumination at the axis are anchored at `ρ = 0` when the first knot lies above it.
- Distortion values are percent: the bracket `1 + D/100` multiplies the radius.
- Vignetting values are percent of the axial illumination at the (source) radius.
- The chromatic-aberration table has the layout `[scale, knots, red × n, blue × n, scale]` (fractions of the radius,
  the displacement of the red and blue planes relative to green).

## Geometry

The camera's JPEG samples the raw at

```
source = z · r · (1 + D(ρ(source)) / 100)         ρ = knot_last · source
z      = 1 / max over the output border radii of (1 + D(ρ(source)) / 100)
```

The table is indexed by the **source** (sensor) radius, not the output radius, and the zoom `z` keeps the output
rectangle inside the source (as for the Sony table, the bracket is largest at the nearest border radius). Source
radius is solved per radius by fixed-point iteration, then `source(r) = r (k0 + k1 r² + k2 r⁴ + k3 r⁶)` is fitted
(minimax, 1025 samples) for the DNG warp; a fit that strays more than 0.0008 half diagonals (largest over the 118 sample
tables 0.0007, median 0.00012) or folds is rejected. The correction itself is the warp's bracket; the camera's rounding
and interpolation between knots are not reproduced beyond linear interpolation between knots.

Vignetting is the gain `100 / V(ρ(r))` at the source radius, fitted by a 5-term minimax polynomial in `r²`
(`FixVignetteRadial`); a fit more than 0.025 off the table is rejected.

## Validation

Method: LightCraft's uncorrected render of each RAF (1200 px long edge) against the camera JPEG (`camera-jpeg/`,
resized to the same size). Per file the high-passed luminance is correlated (NCC) after a warp with a free global scale and
shift; the warp is (a) similarity only, (b) the table with its sign flipped, (c) the table, lookup at the output
radius, (d) the table, lookup at the source radius with the computed zoom (adopted), (e) a free three-term radial
polynomial (the upper bound). 67 body/lens/focal-length combinations from 65 bodies, 26 named lenses and the fixed-lens cameras (129 files).

| Warp | median NCC | mean NCC |
|---|---:|---:|
| similarity only (nothing corrected) | 0.941 | 0.865 |
| table, opposite sign | 0.493 | 0.530 |
| table, lookup at the output radius | 0.989 | 0.869 |
| **table, lookup at the source radius, computed zoom (adopted)** | **0.991** | 0.872 |
| free radial polynomial | 0.994 | 0.985 |

The sign is determined (the 48 combinations with more than 1 % distortion reach a median of 0.29, at most 0.91, with the wrong sign), the
source-radius lookup is slightly better where distortion is large (GFX100RF 0.985 vs 0.971, X-S20 0.989 vs 0.971, X-S1
0.977 vs 0.961). The table reaches the free fit's correlation within 0.02 for 51 of 67 combinations including the
computed zoom, and for 60 of 67 when the zoom is free (shape right). The zoom `z` agrees with the free fit's scale
within 0.0047 on all 51.

**Where the framing differs:** on five corrected combinations the camera did not zoom (XC16–50 at 16 mm on the X-A10,
XC15–45 on the X-A5 and X-T100, the F770EXR and HS50EXR) and on two more it zoomed about 1 % differently (X-T30 III, XQ2):
the shape matches but the JPEG shows 4 % more of the sensor than the computed zoom does. The same body and table
zoom on other lenses (the X-A1 and X-A2 with the XC16–50), so the rule is not in the table. LightCraft uses the computed zoom
everywhere (no empty border, as for the Sony table). Six bodies whose shape did not match well enough (F550EXR, S1,
SL1000, X10, XF1, XQ1) are not corrected.

**Through the shipped code path** (`lightcraft-cli render` of the same 67 combinations with the embedded
correction applied, then the free fit of the corrected render against the camera JPEG, global scale removed so
the framing difference does not count): the residual radial shape error falls from a median of
0.72 % of the half diagonal (largest 2.60 %) to a median of 0.05 % (largest 0.42 %), 54 of the 57 corrected combinations land within 0.25 %
(2 px at 1440 px), and the similarity-only correlation rises from a median of 0.957 to 0.993. Per-lens rows are in
`data/testing/raf-lens/out/after_table.md`.

### Vignetting

Brightness of the camera JPEG against the aligned uncorrected render, in linear light, modelled per file as
`log Y_jpeg = poly3(log Y_render) + β(radius bin)` (the polynomial absorbs the tone curve): the JPEG's gain β against
`-ln(V/100)` of the table over 42 combinations whose corner gain exceeds 5 %: correlation median 0.987 (37 of 42 above
0.9), proportionality factor 0.98 (interquartile 0.73–1.31), against the tone curve's own slope of about 1.05; flat
tables (X-M5, X-T2, X-T3) give a gain of zero in the JPEG. Through the shipped code path the mean |β| over the outer three radius bins of the corrected render falls from a median of
0.184 to 0.031 (log units; largest 0.81 to 0.15) on the 38 corrected combinations with a corner gain above 5 %, and the corner
β at least halves on 30 of them. Seven combinations show only a fraction of the amplitude
(S1, SL1000, X-A2, X-A3, X-S1, X-T1, X-Pro3); the camera setting that governs them is not recorded.

### Chromatic aberration (not applied)

The CA table is real: on the raw mosaics themselves (uncompressed RAFs, colour planes built by normalised convolution so
no demosaic is involved) the radial shift of the red and blue sites against green correlates with the table over 46
curves from 30 files at a median 0.93 (31 above 0.8), with the table's sign, at about 0.7 of its amplitude (the same
estimator reads 1.0–1.15 on a synthetic mosaic with the table applied). The camera JPEGs show no measurable red/blue
shift, but 4:2:0 chroma shares the luminance edges, so that proves nothing.

LightCraft's own demosaic, however, already registers the colour planes: its render shows red/blue shifts of only
0.05–0.2 px where the table has 1.5–5 px. Applying the table as per-channel warp planes (tried; the opcode supports
them) shifted the planes the wrong way: the median measured shift rose from 0.10 to 0.13 px and grew by more than 1.5×
on 11 of 28 curves (X-T4 blue 0.16 → 0.58 px, X-T3 red 0.06 → 0.59 px). The table is therefore not applied; a correction
before demosaicing would be needed.

## Tests

- `crates/raw/src/vendor/raf_lens.rs`: layout, normalisation, the model list, malformed tables, the zoom and the
  vignette gain on the X-T4's real arrays, and the X-T4 geometry against its measured free fit.
- `crates/raw/src/vendor/raf.rs`: a synthetic RAF through header and full decode, only for validated models, and a DNG
  round trip.
- `crates/raw/tests/corpus.rs` `fuji_embedded_lens_tables_follow_the_validated_models`: the corpus' `raf-fuji-*` files,
  or any folder of RAFs in `LIGHTCRAFT_FUJI` (the pixls set: 129 decoded, 88 warps, 98 vignette gains).
