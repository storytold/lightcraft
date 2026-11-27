---
title: Export photos
description: Save edited photos as JPEG, PNG, TIFF and other files with the size, folder and file names you choose.
order: 40
verified: 0.21.1
---

Select the photos, then choose **File** > **Export…**. Choose a format and destination, then click the export button, which says how many photos it will export, such as **Export 3 Photos**. Your originals are not changed.

## Steps

1. Select one or more photos in the grid.
2. Choose **File** > **Export…**. The **Export** window shows how many photos it will export.
3. Set **Format**: **JPEG**, **PNG**, **TIFF**, **PSD**, **WebP**, **AVIF**, **JPEG XL**, **DNG** or **Original files**. For **DNG**, set **DNG of** to **Raw data** to store the raw photo with embedded edit settings, or **Rendered photo** to bake the edits into the image. **Original files** copies the files unchanged, each with an XMP sidecar that holds its edits.
4. For **JPEG**, **AVIF** or lossy **JPEG XL**, set **Quality**. For **JPEG XL**, tick **Lossless** to keep every pixel exactly, at the cost of larger files. **Effort** trades time for file size, and **Progressive** makes the file show a coarse version first while it loads.
5. When size options are available, tick **Full size**, or clear it and choose a **Resize to** mode. **Width & Height** sets maximum width and height separately. **Dimensions** uses the larger value for the long edge and the smaller value for the short edge, regardless of orientation. Other modes are **Long Edge**, **Short Edge**, **Width**, **Height**, **Megapixels** and **Percentage**.
6. Set **Color space** (**sRGB**, **P3**, **Adobe RGB**, **ProPhoto** or **Rec.2020**) when the format offers a choice. **AVIF** uses **sRGB** for standard output. With **HDR output**, it uses **Rec.2020** with the selected **HDR curve** (**PQ** or **HLG**). For standard output, lossy **JPEG XL** writes **Adobe RGB** and **ProPhoto** as **Rec.2020**. Tick **Lossless** to keep them.
7. Optional: set **Sharpen** for **Screen**, **Matte** or **Glossy** output, set **Metadata** to **All**, **No camera**, **Copyright** or **None**, and tick **Remove location info** or **Watermark**.
8. For **TIFF**, set **Compression** to **ZIP** (the default), **LZW** or **None**. **LZW** is usually larger and slower than **ZIP**, and at 16 bit it can be larger than **None**. Use it only if other software needs LZW.
9. For **Specific folder**, use **Choose…** next to **Folder**. On desktop, **Same folder as original photo** exports each photo to its own original’s folder. **Subfolder** adds a folder inside each destination folder.
10. Set **File name** to a template such as `{name}-{seq}`. Click **Tags** to see the tags you can insert.
11. Set **If file exists** to **Add number**, **Overwrite** or **Skip**.
12. On macOS, tick **Add to Apple Photos** to add the exported files to Photos as well. Type an **Album** name to put them in that album. The first time, macOS asks whether LightCraft may control Photos.
13. Click the export button, which says how many photos it will export, such as **Export 3 Photos**. The export shows its progress in the activity stack at the top left.

With **Specific folder** selected, a blank **Folder** keeps the window open and shows an error.

## Reuse your settings

- To keep a set of choices, type a name next to **Save preset** and click **Save**. Pick it again later with **Preset** at the top of the window.
- **File** > **Export with Previous** repeats your last export. It tells you to use **Export…** first if you have not exported yet.

## Contact sheet

To make a PDF of thumbnails, select photos and choose **File** > **Contact Sheet PDF…**.
