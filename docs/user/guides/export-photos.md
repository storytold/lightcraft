---
title: Export photos
description: Save edited photos as JPEG, PNG, TIFF and other files with the size, folder and file names you choose.
order: 40
verified: 0.7.1
---

Select the photos, then choose **File** > **Export…**. Pick a format, a size and a folder, then click **OK**. Your originals are not changed.

## Steps

1. Select one or more photos in the grid.
2. Choose **File** > **Export…**. The **Export** window shows how many photos it will export.
3. Set **Format**: **JPEG**, **PNG**, **TIFF**, **PSD**, **WebP**, **AVIF**, **JPEG XL**, **DNG** or **Original files**. **DNG** writes raw photos as DNG with the edits embedded. **Original files** copies the files unchanged, each with an XMP sidecar that holds its edits.
4. For **JPEG**, **AVIF** or lossy **JPEG XL**, set **Quality**. For **JPEG XL**, tick **Lossless** to keep every pixel exactly, at the cost of larger files. **Effort** trades time for file size, and **Progressive** makes the file show a coarse version first while it loads.
5. Under the size options, tick **Full size**, or clear it and choose **Resize to** one of **Long Edge**, **Short Edge**, **Width**, **Height**, **Width & Height** (the width and the height each limited to its number, as the photo stands), **Dimensions** (the larger number to the long edge, whichever way round the photo is), **Megapixels** or **Percentage**, then enter the value.
6. Set **Color space** (**sRGB**, **P3**, **Adobe RGB**, **ProPhoto** or **Rec.2020**) if you need something other than the default. Lossy **JPEG XL** writes **Adobe RGB** and **ProPhoto** as **Rec.2020**. Tick **Lossless** to keep them.
7. Optional: set **Sharpen** for **Screen**, **Matte** or **Glossy** output, set **Metadata** to **All**, **No camera**, **Copyright** or **None**, and tick **Remove location info** or **Watermark**.
8. Next to **Folder**, click **Choose…** and pick where the files go. **Subfolder** adds a folder inside it.
9. Set **File name** to a template such as `{name}-{seq}`. Click **Tags** to see the tags you can insert.
10. Set **If file exists** to **Add number**, **Overwrite** or **Skip**.
11. On macOS, tick **Add to Apple Photos** to add the exported files to Photos as well. Type an **Album** name to put them in that album. The first time, macOS asks whether LightCraft may control Photos.
12. Click **OK**. The export shows its progress in the activity stack at the top left.

If you did not choose a folder, the window stays open and shows an error, so your choices are not lost.

## Reuse your settings

- To keep a set of choices, type a name next to **Save preset** and click **Save**. Pick it again later with **Preset** at the top of the window.
- **File** > **Export with Previous** repeats your last export. It tells you to use **Export…** first if you have not exported yet.

## Contact sheet

To make a PDF of thumbnails, select photos and choose **File** > **Contact Sheet PDF…**.
