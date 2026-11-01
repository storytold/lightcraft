---
title: Which cameras and file types does LightCraft open?
description: The raw and image formats LightCraft reads, what happens with raws it cannot decode yet, and how to open Lightroom catalogs.
order: 20
verified: 0.7.1
---

LightCraft reads raw files from DNG, Canon CR2 and CR3, Sony ARW, Nikon NEF, Fujifilm RAF (including X-Trans), Panasonic RW2, Leica RWL, Pentax PEF and Olympus ORF cameras. It also opens JPEG, PNG, TIFF, WebP, PSD composites and JPEG XL.

## What if my camera is not decoded yet?

A raw that LightCraft cannot decode opens from the camera's embedded JPEG instead, marked **Preview only**. Compressed Olympus raws, some Canon CR3 variants and Nikon High Efficiency NEF files work this way. See [A raw photo opens as "Preview only"](../troubleshooting/preview-only-raw.md).

How closely the starting look matches your camera's own JPEG varies by model. See [Raw photos look dull or washed out](../troubleshooting/raw-photos-look-dull.md). The project's [raw support table](https://github.com/storytold/lightcraft/blob/main/docs/raw-parity.md) lists what is known per camera.

## Can I open my Lightroom photos?

Yes. LightCraft imports Lightroom Classic catalogs. See [Move over from Lightroom](../guides/import-lightroom-catalog.md).

## What can I export?

JPEG, PNG, TIFF, PSD, WebP, AVIF, JPEG XL and DNG, or the original files with XMP sidecars. See [Export photos](../guides/export-photos.md).
