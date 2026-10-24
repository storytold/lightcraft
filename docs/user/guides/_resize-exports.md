---
title: Choose how exports are resized
description: Pick a resampling filter such as Lanczos, Mitchell or Nearest Neighbor when an export makes photos smaller or larger.
order: 45
---

<!-- Draft until export resampling ships: then drop the leading _ from the file name, list it in
guides/index.md, link it from step 5 of export-photos.md ("**Resampling** picks how the photo is
resized: see [Choose how exports are resized](resize-exports.md)."), link it from its What's New
entry, and set `verified` to that release. -->

When you export at a smaller or larger size, open **File** > **Export…**, clear **Full size**, and pick a filter under **Resampling**. **Automatic** is the fastest; a named filter such as **Lanczos 3** gives you control over how sharp the result looks.

## Steps

1. Select the photos and choose **File** > **Export…**.
2. Clear **Full size**, then set **Resize to** and the size you want (see [Export photos](export-photos.md)).
3. Open the **Resampling** list and choose a filter (see the table below).
4. Leave **Resample in linear light** ticked unless you want to match another app (see [Linear light](#linear-light)).
5. Click **OK**.

**Resampling** only shows when **Full size** is cleared: a full-size export is not resized.

## Which filter to choose

| Filter | Good for | What to expect |
|---|---|---|
| **Automatic** | Everyday exports | The fastest. LightCraft renders straight at the new size. |
| **Lanczos 3** | Shrinking or enlarging photos | Sharp and detailed. A faint halo can show along very hard edges. |
| **Lanczos 4** | The most detail | The sharpest, with the strongest halos. |
| **Lanczos 2** | Sharp but gentle | A little softer than **Lanczos 3**, with fewer halos. |
| **Spline36** | Enlarging photos | Almost as sharp as **Lanczos 3**, with fewer halos. |
| **Spline16** | Enlarging, gently | Like **Lanczos 2**, with fewer halos. |
| **Mitchell-Netravali** | A natural look | Balanced sharpness, hardly any halos. |
| **Bicubic (Catmull-Rom)** | Matching other apps | The "bicubic" most other apps use. |
| **Hermite** | Smooth results | Soft, never brighter or darker than the photo around an edge. |
| **B-Spline** | The smoothest result | Soft; never shows halos. |
| **Gaussian** | Soft, blurred output | Soft; never shows halos. |
| **Bilinear** | Speed | Plain and a little soft. |
| **Box (Area Average)** | Shrinking a lot | Averages the pixels each new pixel covers. |
| **Nearest Neighbor** | Pixel art and screenshots | Keeps hard pixel edges; jagged on photos. |

A good starting point: **Lanczos 3** or **Spline36** for photos, **Nearest Neighbor** for pixel art.

## Linear light

**Resample in linear light** shows once you pick a filter. Ticked (the default), LightCraft blends the light itself, so fine detail, thin lines and edges keep their brightness. Clear it to blend the stored values instead, as many other apps do: the result then matches them more closely, but fine bright detail can come out darker.

## Things to know

- A named filter renders the photo at its full size first and then resizes it, so exports take longer and use more memory than **Automatic**. If a photo would need more memory than your computer has, LightCraft stops that export and suggests **Automatic**.
- Output sharpening (**Sharpen**) and the watermark are applied after resizing, so they look the same whichever filter you choose.
- **Resampling** is saved with your export presets and with **File** > **Export with Previous**.
