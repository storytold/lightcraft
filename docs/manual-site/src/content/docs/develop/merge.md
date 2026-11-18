---
title: Photo merge
description: HDR, panorama, HDR panorama and focus stack.
sidebar:
  order: 17
---

- **Photo ▸ Photo Merge**: select the photos, then **HDR…** (`Ctrl+H`), **Panorama…** (`Ctrl+M`), **HDR Panorama…** or **Focus Stack…** (beyond Lightroom).
- **Dialog** previews at a small size and updates when an option changes; click the preview to zoom, drag to pan.
- **HDR**: Auto Align, Auto Tone, Deghost amount. Result `<name>-HDR.dng`. HDR is partial: local shifts such as parallax are only handled in part.
- **Panorama**: Spherical, Cylindrical, Perspective or Auto projection; **Boundary Warp**, **Fill Edges**, **Auto Crop** (set as the crop, so it stays adjustable). Result `<name>-Pano.dng`.
- **HDR Panorama**: brackets grouped by EXIF, merged one set at a time.
- **Focus Stack**: photos focused at different distances become one sharp DNG (partial).
- **HDR for Each Bracket Set** merges many brackets in one go. **Merge with last settings** skips the dialog.
- Runs in the background: a row in the activity stack with a stop button. The merged DNG is imported and selected.
- **Pixel Shift** (Nikon Z 8, 16-shot series): select all the frames of one series and choose **Photo ▸ Photo Merge ▸ Pixel Shift**. The frames are merged on the raw mosaics into one image with full colour at twice the resolution, written next to frame 01 as `<name>-PixelShift.dng` and stacked on its frames. A selection that is not one complete series is refused with the reason (shown on the window), and so is a series whose result file already exists. Choosing another merge kind on a complete series asks whether to merge it as Pixel Shift instead. **Photo ▸ Photo Merge ▸ Delete Merge Sources** moves the merged photo's frames other than frame 01 to Recently Deleted. There is no registration: a series shot on an unsteady tripod shows it as softness at fine detail.
