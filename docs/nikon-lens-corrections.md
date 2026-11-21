# Nikon embedded distortion corrections

Nikon writes its own distortion correction into NEFs from its mirrorless bodies, in the raw SubIFD tag `0xc7d5`
(NikonNEFInfo). LightCraft reads it for the NIKON Z 6, NIKON Z 7, NIKON Z 6_2 (Z 6II), NIKON Z 8 and NIKON Z f and turns it into an
`OpcodeList3` `WarpRectilinear`, so it goes through the same profile correction control, optics path, orientation
handling and DNG export as the other embedded corrections. Other bodies are left alone for now even if they have the
same tag. Body recognition is limited to files checked against their camera JPEGs.

This is distortion only. The vignetting block (`0x0006`) and what looks like lateral CA (`0x0007`) aren't applied
yet. Like the Sony, Fujifilm and Panasonic corrections it's on by default and can be turned off under Lens
Corrections.

## Where the data comes from

The [ExifTool Nikon tag-name documentation](https://exiftool.org/TagNames/Nikon.html) gives tag identities and offsets
for NikonNEFInfo and its DistortionInfo entry (`0x0005`), including DistortionCorrection and
RadialDistortionCoefficient1 to 3. The ExifTool executable also extracts metadata and embedded JPEG data black-box.
The mapping was worked out independently by comparing LightCraft's uncorrected render of each NEF with the
full-size JPEG the camera embeds in it. That JPEG is always distortion-corrected with Z lenses ("On (Required)"), and
with other lenses when Auto distortion control is on. The comparison matched textured patches between the two images
(normalised cross-correlation on the green channel at quarter size).

NikonNEFInfo starts with `"Nikon\0"` and a 4-byte version, followed by a small TIFF stream. The two entries used here:

| Entry | Bytes | What it holds |
|---|---|---|
| `0x0005` DistortionInfo | 0–3 | `"0100"` |
| | 4 | correction mode: 1 "On (Optional)", 3 "On (Required)"; 0 none, 2 "Off" and unknown modes are skipped |
| | 8–15 | normalisation radius in mm, a signed rational. It's 21.6 (the FX half diagonal) in every file I looked at |
| | 16–19 | number of terms, 4 |
| | 20, 28, 36 | the three coefficients, called `k1`, `k2`, `k3` here after ExifTool's names |
| | 64–71 | the zoom `q` the camera uses to fill the frame |
| `0x0008` | 0–3, 4–11, 12–19 | `"0100"`, then the optical center's offset from the center of the image area in pixels (x right, y down) |

The `0x0008` offset is where the in-body stabilization left the sensor. It's zero with a VR lens, since the lens does
the stabilizing, and up to about 100 px with the Z lenses. The center of the distortion measured in the JPEG follows
it closely, for example (−90.1, +18.6) recorded and (−91.2, +17.5) measured.

From an output pixel to the raw, around the optical center, the mapping is

```
rho    = r_out / half diagonal of the image area
r_src  = r_out * (1 + k3*rho^2 + k2*rho^4 + k1*rho^6) / (1 + q)
```

So the stored order goes from the highest power down. Reading them the other way round (`k1` with `rho^2`) doesn't fit
the JPEG at all. `q` really is the zoom: fitting a separate zoom for every file gives `1 / (1 + q)` to four decimals
(0.9675 fitted and 0.9676 from `q` on one of them), so nothing is fitted per file. The decoder turns this into DNG
terms the same way `docs/sony-lens-corrections.md` describes for Sony, with the center in active-area coordinates and
the coefficients rescaled to the farthest corner of the active area.

## How well it matches

I checked it on 83 of my own Z 6II photos with three lenses between 20 and 200 mm, 45 Z 8 photos (below), plus the
CC0 Z f sample in the corpus. The numbers are the radial error against the camera JPEG at full size (6048 px wide), out to 0.95 half
diagonals:

| Lens | Focal length | Frames | Uncorrected median / p95 | Corrected median / p95 |
|---|---:|---:|---:|---:|
| VR 70-200mm f/2.8G (F-mount, FTZ) | 70 mm | 11 | 10.9 / 15.9 px | 0.19 / 0.57 px |
| VR 70-200mm f/2.8G (F-mount, FTZ) | 105–140 mm | 15 | 34–42 / 38–46 px | 0.13–0.18 / 0.59–0.90 px |
| VR 70-200mm f/2.8G (F-mount, FTZ) | 175–200 mm | 21 | 45–46 / 50–51 px | 0.10–0.18 / 0.48–0.95 px |
| NIKKOR Z 24-70mm f/4 S | 70 mm | 8 | 126.4 / 141.2 px | 0.48 / 4.38 px |
| NIKKOR Z 24-70mm f/4 S | 38–59 mm | 6 | 59–101 / 69–121 px | 0.36–0.76 / 4.6–6.8 px |
| NIKKOR Z 24-70mm f/4 S | 33–34 mm | 4 | 32–44 / 43–53 px | 1.0–1.3 / 8.5–9.6 px |
| NIKKOR Z 24-70mm f/4 S | 24 mm | 7 | 16.0 / 105.6 px | 6.90 / 23.56 px |
| NIKKOR Z 20mm f/1.8 S | 20 mm | 5 | 8.2 / 53.9 px | 1.49 / 5.46 px |
| Tamron 35-150mm (Z f, CC0 corpus) | 150 mm | 1 | 70.4 px median | 0.22 px median |

On the Z 8 (8256 px wide JPEGs) it looks the same. The 70-200 f/2.8G on the FTZ matches within 0.14 to 0.21 px median
at 70, 135 and 200 mm. The NIKKOR Z 24-120mm f/4 S is within 0.6 to 1.0 px median from 36 to 120 mm and 1.5 px at 24
to 28 mm, with the same kind of error toward the edges at the wide end as the Z lenses on the Z 6II. A few 14-24mm
f/2.8G frames had Auto distortion control off, so their JPEGs aren't corrected and match about the same either way.

I also rendered a few with `lightcraft-cli render` at default settings and measured those against the camera JPEG.
The median error was 0.10 px for the 70-200 at 200 mm and 0.12 px for the Z f sample. The 24-70 was 0.32 px at 70 mm,
on a frame with a 92 px sensor shift, and 8.4 px at 24 mm. The 20 mm came out at 1.3 px. On the Z 8 it was 0.14 px for the 70-200 at 200 mm, 0.45 and 0.68 px for the 24-120
at 68 and 120 mm, 1.8 px for the Z 20 mm and 3.7 px for the 24-120 at 24 mm. That checks the
center, crop and DNG conversion along with the model itself.

The F-mount lens on the FTZ matches about as well as I can measure. The native Z lenses still have a smooth error
toward the edges that doesn't depend on the sensor shift and is the same in every frame at a given focal length: about
+22 px at 0.7 to 0.9 half diagonals at 24 mm, +8 px at 33 mm and +3.5 px at 70 mm. It seems the camera applies an
extra correction for Z lenses that isn't in the readable coefficients. NikonNEFInfo `0x000e` looks encrypted and I
didn't try to decode it. Even so, the readable correction takes most of the distortion out. At 24 mm the error at 0.7
half diagonals goes from −82 px to +25 px, and at 70 mm from about −137 px to under 4 px.

## Z 6 and Z 7 validation

The six CC0 raw.pixls.us files in `docs/raw/coverage-sweep.tsv` are six exposures
of two repeated scenes, with one NIKKOR Z 24-70mm f/4 S. The Z 6 files (3584,
3581, 3582) are at 52 mm and f/6.7; the Z 7 files (2792, 2789, 2790) are at
56 mm and f/5.6. They do not establish coverage at other focal lengths or lenses.
All six file SHA-256 hashes were checked before decoding.

`docs/raw/nikon_lens_probe.py` matches log-green patches in a quarter-size sensor
proxy against the full-size embedded JPEG. It requires normalized correlation
at least 0.90 and a peak at least 0.02 above competing matches outside its
7-by-7 neighborhood. The stored coefficients, zoom and optical center are used
without fitting a correction to each photo. Radial errors below are full-size
pixels over reliable matches between 0.15 and 0.95 image half-diagonals.

| Body | Files | Uncorrected radial median | Corrected radial median | Corrected radial p95 |
|---|---:|---:|---:|---:|
| Z 6 | 3 | 82.84-84.00 px | 0.88-0.96 px | 4.69-4.99 px |
| Z 7 | 3 | 124.21-124.80 px | 1.37-1.46 px | 6.18-6.38 px |

Reliable patches reach about 0.90-0.91 half-diagonals on the Z 6 and 0.94 on
the Z 7. The extreme corners remain unvalidated geometrically. The remaining
smooth edge error is consistent with the native Z lens residual described
above. These results support extending the existing model to these bodies,
with the same full-frame crop, layout, radius and mapping guards.

Fresh production exports, including the refitted camera look, were compared
against the same references across all 40 coverage-sweep rows. The measurement
method, base/candidate results and unchanged-image hashes are recorded in
[`raw/nikon-distortion-sweep.tsv`](raw/nikon-distortion-sweep.tsv). The historical file and sensor-hash provenance
in the TSV is retained. Only rows whose rendered pixels change are updated.

The production comparison uses CPU CLI exports at a 1200 px long edge, the
recorded reference orientation and a center crop to the camera reference aspect,
then Lab L* at 200 px. The outer ring starts at 0.75 image half-diagonals.
A lens database is disabled for both runs. The camera look is refitted by each
binary, and all settings remain at their import defaults.

| File ID | Base r | Corrected r | Base mean dL | Corrected mean dL | Base outer mean dL | Corrected outer mean dL |
|---|---:|---:|---:|---:|---:|---:|
| 3584 | 0.8769 | 0.9964 | +0.299 | +0.173 | -3.479 | -1.609 |
| 3581 | 0.8772 | 0.9964 | +0.320 | +0.196 | -3.470 | -1.611 |
| 3582 | 0.8775 | 0.9964 | +0.430 | +0.212 | -3.431 | -1.592 |
| 2792 | 0.7497 | 0.9914 | +1.924 | +1.215 | -2.982 | -2.068 |
| 2789 | 0.7483 | 0.9910 | +2.188 | +1.271 | -2.881 | -2.028 |
| 2790 | 0.7480 | 0.9910 | +2.240 | +1.251 | -2.868 | -2.073 |

Here dL is LightCraft minus camera JPEG. All six improve in correlation,
absolute mean dL and outer-ring mean absolute error. The other 34 production
RGB outputs are byte-identical. Residual tone and corner brightness differences
remain; neither Auto Picture Control nor Nikon shading is reproduced exactly.

## Vignetting remains unfinished

No Nikon vignette correction or fitted per-body strength is applied. The
investigation used the six Z 6 / Z 7 files and one public file each from the
Z 6II, Z 8 and Z f. That is nine exposures, five scenes, four lenses and five
focal lengths in total; the repeated scenes are not independent lens evidence.

The `0100` block at `0x0006` has a 21.600098 mm radius and a term count of eight
in these files. Nine signed rationals start at offset 20. A provisional model
reads them in descending polynomial order: odd terms are zero and the last
term is one. It includes an unnamed rational `a` at offset 20 as well as the
three named coefficients at offsets 36, 52 and 68:

```text
P(rho) = 1 + c3*rho^2 + c2*rho^4 + c1*rho^6 + a*rho^8
candidate gain = 1 + strength * (P(rho) - 1)
```

Here `rho` is the unwarped source radius over the image-area half diagonal,
about the recorded optical center. The ordering, powers, normalization and
strength are hypotheses derived from the files and camera-JPEG comparisons.
Tag identities and the named coefficient offsets came from ExifTool's tag-name
documentation; its executable also extracts file metadata and embedded JPEG data
black-box. No external decoder implementation was read.

The unnamed term matters: at `rho = 1`, the named three alone give gains of
0.912 on the Z 6 and 2.790 on the Z 7; the complete polynomial gives 1.705 and
1.673. However, diagnostic renders that retain the old camera-look fit overbrighten
the corners at full strength and worsen the Z 8 and Z f comparison. Those
experiments do not predict production output after a camera-look refit. Treating the rational at offset 96 as the strength
also fails to establish one consistent interpretation across bodies and scenes.

The diagnostic fitter uses a global log-tone spline and color-ratio terms,
with alternating spatial tiles reserved for validation. It rejects undersampled
splits and radial bins and non-finite statistics. Effective fitted strengths
vary among captures. Scene brightness correlated with radius, Auto Picture
Control on the Z 6, and Auto Active D-Lighting on the Z 6II sample limit the
inference. No contrast response was tuned to close those differences.

Further investigation needs other lenses, apertures and focal lengths, whole
captures held out, flat or near-flat scenes where available, and different
correction settings. Private pairs must stay in place, with diagnostics outside
the repository and anonymous results. Vignetting requires a later review.
If validated, `FixVignetteRadial` before the warp in `OpcodeList3` would use the
existing profile toggle, amount, orientation and DNG path. A `GainMap` would
be baked into the source and would not provide that lens toggle.

## Existing libraries and process versions

These are source corrections under `docs/process-versions.md`; they do not
introduce a rendering process. Qualifying Z 6 / Z 7 NEFs can acquire the warp,
and mode-2 NEFs on previously validated bodies lose the unwanted warp. Modes
1 (On Optional) and 3 (On Required) are accepted; 0, 2 and unknown modes are skipped.

The camera-look fit uses the recognized geometry even when profile corrections
are switched off, so existing edited photos on any process can change tone or
color when decoded again. `LOOK_VERSION` and `RENDER_CACHE_VERSION` are advanced
together. Old smart previews are rebuilt from online originals and retained
with their stored pixels, tone and lens data while originals are offline.
Build Smart Previews reads the original independently of decoded-source caches,
so a proxy cached by the offline loupe cannot prevent rebuilding after reconnect.

The 2026-11-05 audit of all 747 open PR patches found a highest assigned
`LOOK_VERSION` of 25 (#1206) and a highest `RENDER_CACHE_VERSION` of
8,589,934,600 (#160, `8 | (2 << 32)`). A 2026-11-11 recheck found
`LOOK_VERSION` 26 in #3943 and 27 in #3429, so this change uses 28 and
8,589,934,601.
Other PRs still assign 14 or 76, and #784 assigns 24 and 81. Whichever look or
render change lands later must renumber above the versions already on main.

Reload from Disk refreshes catalog lens metadata. It enables a newly found
profile for an untouched photo without an import preset; an edited photo keeps
its settings, including an explicit off switch. Reload is required to update
an older catalog's Optics metadata. A file profile takes precedence over a
lens-database profile, so a Z 6 / Z 7 photo previously using database corrections
can also change when its embedded profile is recognized. The production sweep
above disables the optional database in both runs. Existing DNGs without these
opcodes, other bodies and preview-only raw variants do not acquire this NEF correction.

## What's left out

Only `NIKON Z 6`, `NIKON Z 7`, `NIKON Z 6_2`, `NIKON Z 8` and `NIKON Z f` are corrected, matched exactly. A matching tag on another body doesn't tell us its
pixel pitch or image area, so other bodies can be added once someone checks their files. Only the full-frame 3:2
image area is corrected, because the stored 21.6 mm radius is the FX half diagonal, so DX and other crops are skipped.
The decoder also skips data it doesn't recognise (another layout, radius or term count, or a correction mode other than 1 or 3),
values that are clearly wrong (coefficients past ±1, a zoom past ±0.5, a sensor shift over 5% of the frame) and any
mapping that folds back on itself before the farthest corner.

The decoder and its unit tests live in craft-raw ([storytold/craft-libs](https://github.com/storytold/craft-libs),
`crates/raw`; the Z 6 / Z 7 change is storytold/craft-libs#1314). The tests in `nef_lens` cover the polynomial and zoom,
the sensor-shift center and refusing other bodies, layouts and crops. `nikon_in_file_distortion_becomes_a_warp` runs
a synthetic Z 6II NEF through the header and full decode and a DNG export. LightCraft's
`nikon_in_file_distortion_is_limited_to_validated_bodies` (`crates/raw/tests/it/corpus.rs`) checks the corpus: the Z f
sample gets the warp and the D5100, D7000, D7500 and Coolpix B700 samples don't.

None of my photos, crops or reference images are committed, only the numbers above.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-05 | minor | Z 6 / Z 7 distortion, correction-mode guards, reconnect-safe smart-preview rebuilds, cache-version collision audit and probe geometry guards; vignette model remains unshipped |
