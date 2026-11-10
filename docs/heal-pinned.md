# Healing in LightCraft and Lightroom: algorithms, differences and a proposal

> **Changes:** [revision history](#revision-history)

From process V8 ([process versions](process-versions.md)) Heal and Remove spots use the pinned heal this paper
proposes (`crates/pipeline/src/spots/pinned.rs`). [What shipped](#what-shipped-in-process-v8) lists how the
implementation differs from the proposal.

## Abstract

LightCraft's Heal belonged to a different mathematical class from Lightroom's. Adobe's published Healing Brush solves
a boundary-value problem that pins the patch to the pixels around it. LightCraft instead added a Gaussian-weighted
average of the tone difference, in scene-linear light.

That difference explains most of what users notice, together with three more: detail added at its linear amplitude,
bilinear reads of the source, and a source search driven by tone. LightCraft left faint discs on gradients and smudges
from nearby edges. Its heals looked blotchy in shadows, flat in highlights, and smoother than the grain around them.

Section 5 proposes a pinned, log-space fill that matches the target's detail level and reads the source at whole
pixels, shipped behind a process version so existing edits keep their look.

## 1. Introduction and scope

Users report that LightCraft's Heal "feels different" from Lightroom's. This paper explains why in terms of the maths
and proposes changes.

The work follows LightCraft's clean-room rules:

- Lightroom's side comes only from Adobe's published paper, patents and help pages, community reports, and
  Lightroom's own XMP output.
- No Adobe application files were read or disassembled, and no GPL code (GIMP, darktable, RawTherapee) was read.

LightCraft's side comes from its source code: `crates/pipeline/src/spots.rs`, `crates/gpu/src/wgsl/spot.wgsl` and
`crates/engine/src/cmd/masks.rs`.

## 2. How Lightroom's Heal works

Lightroom's Heal is a seamless-cloning tool: it keeps the source's detail and solves for a smooth correction that
matches the surroundings exactly at the spot's edge. Adobe describes Heal as matching "the texture, lighting, and
shading of the sampled area to the selected area"
([Adobe help](https://helpx.adobe.com/lightroom-classic/desktop/help/healing-tool.html)). Adobe has not published
Camera Raw's heal code. What is public is the method family, set out below.

### 2.1 The Healing Brush equation

Georgiev's paper on Photoshop's Healing Brush gives the algorithm in three steps
([Georgiev 2004](https://ftp.fau.de/gimp/references/Photoshop_Healing_Brush_a_Tool_for_Seamless_Clonin.pdf)). Let
f be the photo, g the source patch moved onto the spot, and Ω the spot.

1. Take the difference h_in = f − g.
2. Fill h_in inside Ω from its values on the boundary alone, by solving a smooth partial differential equation.
3. The healed patch is h = g + h₀.

Photoshop solves the biharmonic equation, started from the harmonic (Laplace) solution:

```math
\Delta^2 h_0 = 0 \ \text{in } \Omega, \qquad h_0 = f - g \ \text{and} \ \partial_n h_0 = \partial_n (f - g) \ \text{on } \partial\Omega, \qquad h = g + h_0
```

With the Laplace equation alone this is Poisson image editing (Δh = Δg inside, h = f on the edge;
[Pérez et al. 2003](https://www.cs.princeton.edu/courses/archive/fall14/cos526/papers/perez03.pdf)). Georgiev chose the
biharmonic form because it matches the slope at the edge as well as the value. The paper notes the cost: a colour that
differs strongly anywhere on the boundary "contaminates the reconstruction inside".

### 2.2 How it is solved

Adobe's patent on the method adds the engineering ([US 6,587,592](https://patents.google.com/patent/US6587592)):

- The boundary is a band of pixels outside the region, two pixels deep for the biharmonic kernel.
- Small kernels are iterated, with boundary values fed back on every pass.
- A pyramid, each level 2×2 smaller, is solved coarse to fine, so fine levels start almost healed.
- Texture is handled as TextureHeal(i, t, M) = t + SmoothHeal(i − t, M), the same subtract-solve-add as above.
- The boundary may be blurred first to reduce noise.
- A soft brush blends the healed values back into the original by mask density.

### 2.3 Texture contrast under different lighting

The additive form copies the source's detail at its absolute amplitude. Detail from a bright area pasted into a shadow
therefore looks too strong. Georgiev's covariant-derivative view leads to an equation that is invariant to relighting.
Adobe's later patent turns it into a ratio-domain heal that "matches texture contrast"
([US 7,605,821](https://patents.google.com/patent/US7605821);
[Georgiev 2005](https://diglib.eg.org/handle/10.2312/egs.20051024.061-064)):

```math
I_1 = f / g, \qquad \Delta I_2 = 0 \ (\text{or } \nabla^4 I_2 = 0) \ \text{in } \Omega, \quad I_2 = I_1 \ \text{on } \partial\Omega, \qquad h = I_2 \, g
```

The patent gives an equivalent log form: solve on log f − log g and exponentiate. Photoshop's original healing works
on gamma-encoded pixel values. That sits between the two: it scales detail partly with brightness.

### 2.4 Evidence in Lightroom's own files

Lightroom stores each spot in the XMP sidecar under `crs:RetouchAreas`, with a `crs:Method` attribute:

- A Camera Raw 14 sample shows a heal spot with `crs:Method="gaussian"` and `crs:HealVersion="2"`
  ([Adobe community](https://community.adobe.com/t5/camera-raw-discussions/photoshop-camera-raw-14-0-throws-away-masks/td-p/12503361)).
- Sidecars observed when LightCraft's importer was written carry `crs:Method="poisson"` (#703).

So Lightroom has at least two heal methods, one named after the Poisson equation. Adobe does not document what
"gaussian" means.

### 2.5 Observed behaviour

- **Source search.** Lightroom picks a source automatically, prefers nearby areas, but jumps far away, even outside
  the crop, when it judges a match better
  ([Laura Shoe, Lightroom 5](https://laurashoe.com/2013/04/18/healing-and-cloning-on-steroids-in-lightroom-5)). "/" or
  Refresh picks another ([Julieanne Kost](https://jkost.com/blog/?p=21093)).
- **Feather.** The default is 50. Lowering it removes leftover smudges at the spot's edge
  ([Lightroom Killer Tips](https://lightroomkillertips.com/removing-distractions-with-updated-healing-tool/)).
- **Bleeding.** Users report colour bleeding into heals near crop or frame edges
  ([Adobe community](https://community.adobe.com/t5/lightroom-classic-bugs/p-bleeding-color-with-healing-tool/idi-p/12248642)).
  This is what a boundary-value fill does when its boundary crosses a contrasting edge.
- **Order.** Adobe's guidance puts Remove/Heal right after Denoise and before lens corrections, profile and global
  adjustments, so it runs on scene-referred data
  ([Adobe community](https://community.adobe.com/t5/lightroom-classic-discussions/p-recommended-order-of-edits-lr-amp-cr/m-p/13928407)).

Unknown from public sources: Camera Raw's working domain (gamma, log or linear), its exact solver, and how it ranks
source candidates.

## 3. How LightCraft's Heal worked (process V1 to V7)

LightCraft's Heal added a Gaussian-weighted average of the tone difference measured around the spot. Nothing pinned
the result to the spot's edge, and the addition happened in scene-linear light. The code called this "a fast, stable
approximation of gradient-domain healing".

### 3.1 The correction

For each spot, LightCraft works in a box of the stroke plus two radii. Pixels where the spot's alpha is below 0.02
count as outside (m = 1). The difference f − g outside is blurred with a Gaussian of σ = 0.6 r and divided by the
blurred mask (normalized convolution):

```math
h(x) = g(x) + \frac{\big(G_{\sigma} * [\, m\,(f - g)\,]\big)(x)}{\big(G_{\sigma} * m\big)(x)}, \qquad \sigma = 0.6\,r
```

At the edge this gives a weighted average of the difference over a band about one radius wide, not the difference at
the edge itself. Wherever the difference varies across that band, the patch does not meet its surroundings. A dark
feature inside the band, even one that never touches the spot, shifts the whole patch.

### 3.2 Domain, blend and source pixels

- **Domain.** The heal runs in white-balanced scene-linear RGB, after the geometry warp and before noise reduction and
  tone mapping (`lin_cpu` in `crates/pipeline/src/lib.rs`). Detail is therefore moved at its absolute linear amplitude.
- **Blend.** The result is mixed back with the original: out = f + α (h − f), where
  α = (1 − smoothstep(r(1 − 0.8 F), r, d)) · opacity and F is feather from 0 to 1. At the default feather of 50 the
  ramp starts at 0.6 r. The on-canvas inner circle is drawn at r(1 − F), so 0.5 r.
- **Source pixels.** The source is read with bilinear interpolation at the stored offset. The offset is stored
  normalized, so at most render sizes it lands between pixels. That averages two to four pixels and filters out grain.
- **GPU.** `spot.wgsl` implements the same maths, so both renderers behave alike.

### 3.3 Automatic source

A new spot gets its source once, when it is added (`pick_source` in `crates/engine/src/cmd/masks.rs`):

- The search runs on a copy of the photo at most 512 pixels on its long edge, before development.
- It tries 48 candidates: 16 directions at 2.4, 3.2 and 4.2 radii.
- Each candidate is scored on 72 samples (24 spokes at three radii between 1.05 r and 1.6 r). The score is the sum of
  squared differences of the square roots of linear values against the same samples around the target.
- A second term penalizes how far the source's inside varies from the mean of the target's surroundings.

Both terms reward a source with the same brightness as the target, which the heal would correct anyway. They also
reward a flat source: for random texture, the squared difference against a flat patch is half that against an equally
textured one.

### 3.4 Naming

LightCraft's default mode, Remove, is Heal with an automatic source. Lightroom's Remove (since Lightroom Classic 13.3)
is content-aware and can use generative AI; LightCraft's closest mode is Content-Aware (Fill), which uses PatchMatch
completion.

## 4. Why LightCraft's Heal feels different

Five mechanisms explain the difference. The first two matter most, because most photos have gradients and edges near a
blemish.

| What users see | Cause in LightCraft | Lightroom (published method) |
| --- | --- | --- |
| A faint disc or ring where the spot sits on a gradient | correction is a weighted average about one radius wide, not pinned to the edge | pinned to the edge value and slope |
| A smudge pulled from something near the spot | the same band feeds every pixel of the patch | only boundary pixels matter, but a touching edge bleeds hard |
| Blotchy, too-contrasty heals in shadows; flat ones in highlights | detail added at its linear amplitude | ratio-domain variant designed to "match texture contrast" |
| The patch looks smoother than the grain around it | bilinear reads at fractional offsets average neighbouring pixels | not public |
| Odd or bland automatic sources | 48 nearby candidates, scored on tone, on a 512 px copy; the score favours flat sources | searches the whole photo, prefers nearby |

Two further differences affect expectations rather than pixels. LightCraft's "Remove" is a heal, while Lightroom's is
content-aware. LightCraft's on-canvas feather circle is drawn at 0.5 r for the default feather, but the fall-off starts
at 0.6 r.

## 5. Proposal

- Replace the averaged correction with a pinned, log-space fill that matches the target's detail level.
- Read sources at whole pixels.
- Treat source search as a separate, measured step.
- Ship behind a process version so existing edits keep their look.

### 5.1 Design

The source keeps its detail, scaled to the target's detail level. The fill supplies only the smooth correction, and
it is solved to agree with the photo on the spot's edge. Written out, with ḡ the source blurred at 1.5 px in log space:

```math
\tilde g = \bar g + k\,(g - \bar g), \quad k = \operatorname{clamp}\!\Big(\frac{\sigma_{\mathrm{ring}}(f)}{\sigma_{\mathrm{in}}(g)},\ 0.5,\ 2\Big), \qquad \Delta P = 0 \ \text{in } \Omega, \quad P = \log f - \log \tilde g \ \text{on } \partial\Omega, \qquad h = \tilde g\, e^{P}
```

Here σ_ring(f) is the fine-detail spread of log luminance in the target's ring from 1.05 r to 2 r, and σ_in(g) the same
inside the source, within 0.9 r. The gain is needed because fine detail in real photos is mostly sensor noise, which is
relatively stronger in shadows: a pure ratio heal would carry too little grain into a shadow, and an additive linear
one too much.

The steps for one spot:

1. Read the source at a whole-pixel offset.
2. Take the log of source and photo; scale the source's fine detail by k.
3. Take the log ratio of photo to source on the pixels outside the spot, blurred over the outside only (1.5 px).
4. Fill that ratio inside the spot from the ring of pixels touching it (the core change).
5. Multiply the matched source by e raised to the fill.
6. Feather-blend into the photo as before: out = f + α (h − f).

### 5.2 Quick wins

- **Whole-pixel source offsets** for Heal, Clone and Remove, in the CPU and GPU renderers. The source moves by at most
  half a pixel.
- **Feather circle** drawn where the fall-off starts, r(1 − 0.8 F).
- **Remove explained** in the UI as Heal with an automatic source, pointing to Content-Aware for Lightroom-style removal.

### 5.3 The pinned fill

1. **Solver.** A Laplace solve on the spot's box from the ring of pixels touching the spot, coarse to fine; biharmonic
   smoothing near the edge (Georgiev) is an option.
2. **Domain.** Log of linear RGB per channel, with a small floor so zero stays finite. Values clamp at 0 after
   decoding.
3. **Boundary.** A normalized blur of 1.5 px over outside pixels only, the pre-blur the patent mentions. One ring of
   noisy pixels then cannot ripple through the fill.
4. **Detail matching.** The gain k above, measured on luminance and clamped to 0.5–2×.
5. **Strokes and feather.** Brushed strokes use the same solve over the stroke's mask. The feather blend stays as it is.
6. **GPU.** The same solve in `spot.wgsl`, so both renderers match.
7. **Compatibility.** The new heal sits behind a process version (Update to Current Process), so older edits render
   unchanged.

One known cost: a pinned edge bleeds where the spot touches a contrasting edge. A robust boundary that down-weights
outlier boundary pixels, a soft form of Photoshop's selection rule, is worth testing before it is adopted.

### 5.4 Source search, measured

- **Benchmark first.** Clean targets on `lightcraft-scenes` images and CC0 corpus photos, scored by the heal error of
  the pinned heal.
- **Wider candidates.** Today's rings, plus a coarse search of the whole photo with a preference for nearby sources.
- **Scores to test.** Keep the brightness term, because it matches noise level. Add a texture-energy term measured over
  larger areas than 72 samples. Score at a size where the spot is at least about 16 px across.
- **Adopt on evidence.** Only what beats today's picker on the benchmark ships.

### 5.5 Risks

| Risk | Mitigation |
| --- | --- |
| Existing edits change look | process-version gate; old edits keep the averaged heal |
| Slow solves on long brushed strokes (windows up to 8192 px) | coarse-to-fine solver; a size limit |
| Detail gain amplifies noise | luminance-only gain clamped to 0.5–2× |
| Colour shifts near black in log space | a floor on the log, plus tests on dark scenes |
| Crash paths in a new solver | bounded loops, allocations sized by the box, NaN-free floor, per LightCraft's never-crash rules |

## What shipped in process V8

- **Fill:** the membrane (Laplace) fill of `lightcraft_inpaint::membrane_fill` (cascadic SOR, ported from PhotoCraft),
  known only on the ring of pixels touching the spot. No biharmonic step: on test scenes it measured no better, and the
  harmonic fill keeps the maximum principle (no overshoot past the edge values).
- **Domain, boundary, detail gain:** as in 5.3, with the floor 1/4096 and the gain from medians of absolute fine detail.
- **Whole pixels:** every spot, Clone included.
- **GPU:** no kernel yet; on V8 photos with spots the GPU renderer reads the image back for the spot stage and keeps the
  rest of the render on the device. V1 to V7 keep the device kernels.
- **Not done:** the feather-circle drawing, the Remove wording and the source search (5.2, 5.4).

Regression tests in `spots::pinned::tests` cover each mechanism in section 4 that the change addresses; each also
checks that the averaged heal fails it.

## 6. Limitations and open questions

What Lightroom does is inferred from Adobe's published methods; Camera Raw's own code is not public.

- **Lightroom's internals.** Its working domain, solver, source resampling and source ranking are not documented. The
  proposal follows the published methods, not a measured Lightroom.
- **What "gaussian" means.** Lightroom's `crs:Method="gaussian"` beside `"poisson"` is unexplained; it may be a second
  heal algorithm.
- **Tuning.** The thresholds and the detail-gain clamp are starting values.
- **Colour.** The design works per channel in log space. Whether a luminance-only correction looks closer to Lightroom
  on skin is open.

## References

- Georgiev, T. [Photoshop Healing Brush: a Tool for Seamless Cloning](https://ftp.fau.de/gimp/references/Photoshop_Healing_Brush_a_Tool_for_Seamless_Clonin.pdf). Workshop on Applications of Computer Vision, ECCV 2004.
- Georgiev, T. [Image Reconstruction Invariant to Relighting](https://diglib.eg.org/handle/10.2312/egs.20051024.061-064). Eurographics 2005 short papers.
- Pérez, P., Gangnet, M., Blake, A. [Poisson Image Editing](https://www.cs.princeton.edu/courses/archive/fall14/cos526/papers/perez03.pdf). SIGGRAPH 2003.
- Chien, Georgiev, Hamburg. [US 6,587,592: Generating replacement data values for an image region](https://patents.google.com/patent/US6587592). Adobe, 2003.
- Georgiev, T. [US 7,605,821: Poisson image-editing technique that matches texture contrast](https://patents.google.com/patent/US7605821). Adobe.
- Adobe. [Lightroom Classic: Use the Healing tool](https://helpx.adobe.com/lightroom-classic/desktop/help/healing-tool.html).
- Kost, J. [Clone, Heal and Content-Aware Remove in Lightroom Classic](https://jkost.com/blog/?p=21093).
- Shoe, L. [Healing and cloning on steroids in Lightroom 5](https://laurashoe.com/2013/04/18/healing-and-cloning-on-steroids-in-lightroom-5).
- Lightroom Killer Tips. [Removing distractions with the updated healing tool](https://lightroomkillertips.com/removing-distractions-with-updated-healing-tool/).
- Adobe community. [Camera Raw 14.0 throws away masks](https://community.adobe.com/t5/camera-raw-discussions/photoshop-camera-raw-14-0-throws-away-masks/td-p/12503361) (XMP sample with `crs:Method="gaussian"`).
- Adobe community. [Bleeding color with healing tool](https://community.adobe.com/t5/lightroom-classic-bugs/p-bleeding-color-with-healing-tool/idi-p/12248642).
- Adobe community. [Recommended order of edits](https://community.adobe.com/t5/lightroom-classic-discussions/p-recommended-order-of-edits-lr-amp-cr/m-p/13928407).

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-10 | new | The paper, and the pinned heal as process V8 |
