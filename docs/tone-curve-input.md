# Tone Curve input histogram

The backdrop shows sampled tones entering the selected curve. The Histogram panel still shows
rendered output. Issue [#987](https://github.com/storytold/lightcraft/issues/987), reported by
@zelefogs, requested this distinction; #1091 supplied the earlier output backdrop.

## Where the sample comes from

The CPU pipeline shares its existing geometry, white balance, noise reduction and spatial stages.
Its finish stage applies exposure, local adjustments, detail, tone, colour, vignette and the output
gamut or proof transform. It then applies the sRGB transfer function in the preview's output
primaries. These encoded component values are the curve axes.

| Selected curve | Sampled values |
|---|---|
| Parametric | Before every curve |
| Point (master) | After Parametric, before Point |
| Red, Green or Blue | That component after Parametric and Point, before the independent RGB curves |

Point and Parametric display a luminance summary of the encoded components, using the output
primaries' luminance weights. Both curves actually act on each RGB component. The normal renderer
composes the curves into 1,024-entry lookup tables. The diagnostic constructs the same tables with
only the preceding curves, after built-in profile expansion. It does not change the normal tables
or how existing edits render.

The renderer does not store an intermediate image between its fused curves. This reconstructed
prefix does not imply that applying the selected curve separately reproduces every output bit;
table interpolation and nonlinear composition can differ.

Refine Saturation, grain, LUT profiles, proof warning colours, diagnostic overlays and final output
re-encoding are excluded. Built-in profiles can change upstream settings and preceding curves;
those contributions remain. This is why simply disabling curves and measuring final output would
not measure the requested stage.

## Sampling and identity

The UI uses the existing thumbnail source and a separate render request, with at most 256 pixels
per edge (65,536 samples). It samples the whole crop. Zoom windows and panning do not change the
distribution. Float values are binned before quantization. Nonfinite values and values outside
0..1 are excluded rather than placed at the graph's endpoints.

This is an approximation from a downsampled development, not a full-resolution pixel count.
Initial thumbnail loading follows the normal source loader and can require decoding an original.
Subsequent diagnostics reuse the thumbnail source and spatial cache. Moving the selected curve or
its successors does not invalidate the diagnostic key. Upstream settings, source identity, photo,
channel, crop, display profile and proof settings do. An idle frame reuses the prepared job.

The diagnostic has its own preview slot and accepts only its current request. The UI also checks
the current photo and key before drawing. Loading or failure leaves an empty backdrop with a status
label; a previous photo's distribution is never used as a fallback. Library and display resets
clear the diagnostic. It never reads or writes rendered thumbnail or view caches.

Native previews use the selected monitor primaries and proof transform. Browser worker previews
currently use their existing sRGB request semantics. HDR edits use the SDR preview rendition,
including its brightness and contrast offsets; this graph does not represent the HDR export's
headroom. The help text states this limit. No output toggle or new edit setting is added.

## Verification and limits

Procedural regressions cover exposure, nonidentity curves and their order, built-in and LUT
profiles, crop and window handling, size bounds, HDR and nonfinite boundaries, display primaries,
proof transforms, cache separation and late completions across edits, photos, channels and library
resets. A GPU comparison checks each curve prefix against the existing GPU renderer. Headless UI
tests check channel selection, output separation, loading, failure and no-photo states.

These tests establish LightCraft's stage contract. They do not establish Lightroom colour parity,
real-camera accuracy or performance on every platform.
