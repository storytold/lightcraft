# CR3 profile correspondence geometry

CR3 colour calibration can propose a resolved lens profile’s geometry before collecting
sensor/JPEG training pairs. It reuses the guarded correspondence algorithm proposed in
#2675, with an additional comparison against CR3’s existing framing fallback.
It does not change CR3 decoding or the lens warp applied when developing the photograph.

A resolved external lens profile may propose geometry only for file-local colour calibration,
with a fallback camera matrix and no in-file lens warp. The sensor, sensor clip mask, and embedded JPEG are
oriented together before evaluating the already oriented lens profile. White balance and
baseline exposure are applied exactly once before geometry assessment.

The existing fixed-proxy edge test, bounded framing search (scale 0.9–1.1 and translation up to
two proxy pixels), minimum edge coverage, 0.9 agreement, and independent held-out improvement
of at least 0.04 are retained. Geometry uses luminance edge directions, without fitting RGB
values. Accepted geometry moves the sensor and clip mask together; invalid borders remain NaN.
CR3 proposals must also preserve or improve agreement against the existing framed fallback
on both training and held-out edges, compared on the exact intersection of valid edges with
the same reference weights. The published minimum 0.04 improvement over the unframed sensor
is retained separately; the fallback comparison adds no extra improvement threshold. Rejected or absent geometry takes the existing CR3 framing path. All subsequent colour,
tone, hue/saturation, clipping, and held-out acceptance gates stay unchanged.

The proposal reaches file-local colour and tone, pooled camera-profile pairs, and colour tags
for merge/DNG conversion. Cache versions advance to look 14 and render 71. No camera allowlist,
new profile matching rules, or weaker acceptance gate is introduced. Other RAW formats
retain their existing correspondence behaviour. The non-CR3 and fixed-mount changes
proposed by #2675 are separate work.

Synthetic tests cover lens-only and lens-plus-framing acceptance, unrelated/flat/already aligned
rejection, crop-relative profile orientation at multiple resolutions, clip-mask contributions,
and the complete CR3 correspondence route for EXIF orientations 1–8 with non-unity white balance.
The opt-in `corpus_cr3_profile_geometry_diagnostics` test exports `geometry.tsv` and plain,
selected, and JPEG proxy PNGs when `LIGHTCRAFT_CR3_DIAGNOSTICS` names an output directory.
It reads CR3 files from `$LIGHTCRAFT_CORPUS/raw` and the normal configured lens database.
The TSV appends `status` and `reason` columns. `measured` means both proxy calls returned
images; acceptance or rejection is recorded separately in the existing measurement columns.
If either call returns no proxy, the row records `missing_plain_proxy`,
`missing_chosen_proxy`, or `missing_both_proxies`, leaves all measurement fields empty,
and exports no PNGs for that file. The diagnostic continues with the next CR3. The reason
does not attribute a cause: the proxy API returns only `None`, which can cover unavailable
previews or sensor proxies. An unavailable row is not a passed geometry or colour check.
The PNGs show linear proxy RGB converted to sRGB, without a camera tone curve; they are geometry
inspection aids, not developed image fidelity comparisons. Real-sample assessment and unchanged
fallback controls are required before any publication. This experiment does not establish
camera-wide colour parity or validate arbitrary Canon lenses, picture styles, or CRX modes.

## Public-sample assessment

On 22 SHA-256-verified, individually CC0-listed raw.pixls.us CR3 files using lensfun
XML data at `43f9e001`, the R100 RAW/C-RAW pair with RF-S18–45 at 18 mm accepts the
new geometry. The other 20 files retain pixel-identical 1600-pixel renders with optics
on/off, including eight R6 Mark II full-frame/crop, RAW/C-RAW and Dual-Pixel variants
of one scene. All 22 retain identical renders without a lens database.

With the same independent baseline-derived radial registration applied to both
outputs, mean DE76 at 768×512 versus the embedded JPEG improves 4.54 to 3.79 (R100 RAW)
and 4.15 to 3.79 (C-RAW) with optics on. Unregistered whole-frame error slightly worsens
because optical framing still differs. This is a modest calibration benefit and does
not remove the photograph's visible lens distortion. The camera JPEG is not a
colorimetric reference. Four 24 MP files (R100 RAW/C-RAW and R6 II full-frame RAW/C-RAW)
render on CPU/Metal within 1/255 per channel, with optics on/off.

The sample set does not establish all-camera correctness. R6 II’s eight variants
share one scene, not eight independent scenes. Pooled-profile and DNG-tag forwarding
are tested synthetically; this is not a real multi-camera calibration or DNG roundtrip.
