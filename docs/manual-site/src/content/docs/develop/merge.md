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
- Runs in the background: a row in the activity stack with a stop button. The merged DNG is imported and selected; started while viewing an album, it is added to that album too (not to a smart album).
