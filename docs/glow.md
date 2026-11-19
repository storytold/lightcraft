# Glow

Effects > Glow adds non-destructive light scattering. Raise Glow above zero to enable the
style selector and advanced controls. Double-click a slider to reset it, or reset Effects.

- **Bloom** diffuses extracted highlights into their surroundings.
- **Diffusion** also scatters a quarter of the underlying image, for softer, luminous shadows.
- **Halation** adds a red-orange halo outside bright sources, leaving their cores intact.
- **Motion** trails highlight light in one direction.
- **Streak** trails it in opposite directions along the same axis.

Amount (0–100) controls strength. Range (0–100) broadens the contributing tones from bright
highlights towards midtones. Spread controls the radius relative to the image's long edge.
Warmth (−100–100) changes the scattered light's red/blue balance. Motion and Streak additionally
expose Direction (−180–180°, 0° horizontal, 90° downwards) and Softness (0–100).

The implementation is original Rust code derived from public behavioral descriptions:
[Adobe help](https://helpx.adobe.com/lightroom-classic/desktop/process-and-develop-photos/retouch-photos.html)
and [the video reference](https://www.youtube.com/watch?v=2QHZzyugCew).
Adobe's proprietary kernels and exact slider response curves are not public; this is an
independent interpretation, not a claim of pixel-equivalent Lightroom output.

## Rendering and storage

`effects.glow` is saved with develop settings, history, presets, copy/paste, versions and the
LightCraft payload in sidecars. Styles serialize as `bloom`, `diffusion`, `halation`, `motion`,
`streak`. Numeric controls use `effects.glow.amount`, `.range`, `.spread`, `.warmth`, `.direction`,
`.softness` through `develop.set`; use `develop.merge` for the style.

Glow runs after denoise/Lens Blur and before tone mapping on linear Rec.2020 radiance. Exposure
is considered during highlight extraction. It preserves HDR headroom and supports every output
depth. Amount zero and the Effects eye off bypass the operator. Default Glow settings are omitted
from serialized settings, preserving old settings hashes. Reset/copy uses explicit defaults.
The Glow preview key (`glow::key`) covers the Glow settings and exposure. A kernel change alone
does not invalidate cached Glow previews; they refresh on the next Glow or exposure edit.

A grid with at most 768 pixels per edge area-averages extracted radiance so small lamps survive
reduction. Fractional box radii approximate Gaussian scattering without slider dead zones.
A causal exponential filter follows the selected angle for Motion; Streak averages the forward
and reverse filters. Each directional pass is linear in the grid size, without repeated point
stamps or edge replication. Scattering reads zero radiance outside the frame.
A peak-aware perceptual highlight knee retains saturated emitters. CPU and GPU renderers share the same CPU scattering operator. The
GPU path performs a readback/upload when Glow is enabled. Zoom-window renders of frames up to 2.5 MP
render the whole frame and crop it (exact). A window of a larger frame adds the light field of the
whole frame instead: the field is computed once, on Glow's grid of at most 768 px per edge, from a
reduced whole-frame render through the stages before Glow, and is reused by every window of the view
(`pipeline::glow_field`). All windows share one field, so tiles never seam; measured against the same crop of
the full render the 8-bit difference is at most 2 levels (Amount 80, Spread 100, bright lamps on a 2.8 MP frame),
because the reduced render is not bit-identical to the full-resolution pixels. Whole-frame renders are unchanged.

Limitations: global effect only; no local-mask Glow, no Adobe `crs:Glow*` interoperability, and no
measured Lightroom equivalence or interactive-performance guarantee. Large previews may take
longer with directional styles. Existing source files remain untouched.

Tests cover identity/disabled behavior, light spread, halo color, directional symmetry,
Range/Warmth/Direction, cache invalidation, deep export, window cropping and settings persistence.

## Local validation (2026-11-16)

- macOS Apple Silicon app and CLI built; app packaged and ad-hoc signed.
- Native app opened in memory with demo photos, French UI and Glow controls inspected; render completed on Apple M2 (Metal), without fallback.
- Five styles exported successfully from a procedural light-source fixture.
- Develop tests: 51 passed. Pipeline: 344 passed, 12 ignored; one legacy V1/V2 golden test failed.
- The exact same legacy test was run at unmodified upstream commit a4676b1: it fails with the same hashes (V1 `0x483059a3ad92326c`, V2 `0x9653aed0ea32aff3`). Its expected fixtures were not changed.
- Glow CPU/GPU equivalence test passed on Apple M2 using the real GPU path (mean error < 0.5 and maximum error ≤ 3 in 8-bit steps).
- Clippy with warnings denied passed for develop, pipeline, GPU and UI libraries.
- Focused final run: six new Glow tests passed (settings + five rendering tests); the existing pet-eye glow test also passed.
- Formatting, layer boundaries, crate-root guards, asset attribution and parity references passed. Parity used the already built CLI registry rather than rebuilding it.
- Full workspace CI and WASM were not run.

## Motion regression coverage

The settings Amount 23, Range 81, Spread 47, Warmth 46, Direction 0, Softness 50
previously rejected a blue lamp completely. The old fixed 64-tap trail also left gaps for a
sharp, long trail, and clamping directional reads repeated a lamp at the frame edge, creating
more than 31 times its input energy in a synthetic test. All three regression tests failed
before the change and pass with the continuous directional filter and highlight knee.
Softness now interpolates fractional blur radii instead of rounding small adjustments away.

Only source code and procedural tests are intended for upstream contribution; personal photo
fixtures, settings snapshots and application bundles are excluded.

UI labels use the existing localization path. French labels are localized; other catalogs
retain existing translations and receive explicit English fallback entries for new Glow keys.
