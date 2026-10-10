# Import and export

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

## Import

File → Import Photos… (`Cmd+Shift+I`) or Import from Folder… (`library.import`):

- **Add** references the files where they are; **Copy** copies them into the library's `Originals/` (or a destination
  you pick); **Move** copies, verifies the copy byte for byte, then removes the source and its sidecars. Failed,
  duplicate and unverified files keep their sources.
- **Organise** copies by date (`YYYY/YYYY-MM-DD`), month, flat, or a folder template such as `{date:%Y}/{date:%Y%m%d}`.
- **Rename** with tokens (`{name}`, `{seq:4}`, `{date:%Y%m%d}`, `{camera}`, `{lens}` …; `photo.renameTokens` explains
  each), optionally convert raws to DNG while copying.
- Apply a develop preset, a metadata preset, keywords, and add the photos to an album.
- Duplicates are detected and reported; a file in Recently Deleted can be skipped, restored with its edits, or
  imported fresh.

Other sources: File → Import Lightroom Catalog… ([migration guide](migrating-from-lightroom.md)), File → Import
from Immich… ([immich.md](../immich.md)), File → Import from Another Catalog… ([catalogs.md](catalogs.md)), and
tethered capture ([tethering.md](tethering.md)). File → Import Profiles & Presets… reads presets (including
Lightroom `.xmp` presets); Import Keywords… reads keyword lists.

Supported files: JPEG, PNG, TIFF, WebP, AVIF/HEIF, PSD, DNG and the common raw formats (Sony ARW, Nikon NEF, Canon
CR2/CR3, Fujifilm RAF, Panasonic RW2, Olympus ORF, Pentax PEF, Samsung SRW and more). Per-model status is in the
[parity tracker](../parity.md) (`LR-IMP-CAMERA-COVERAGE`).

## Export

File → Export… opens the export dialog; File → Export with Previous (`Cmd+Alt+Shift+E`) repeats the last one.

- Formats: JPEG, PNG, TIFF (8/16/32-bit, LZW/ZIP), WebP, AVIF, DNG, or the original file with an XMP sidecar.
  HDR edits can export as gain-map JPEGs or float TIFFs.
- Size by long/short edge, width/height, megapixels or percent; resolution in ppi; JPEG quality or a file size limit.
- Colour space sRGB, Display P3, Adobe RGB, ProPhoto RGB or Rec. 2020.
- Output sharpening (screen, matte, glossy), metadata (all, all except camera info, copyright only, none), location
  removal, file naming and subfolders, what to do with existing files.
- Watermarks: text (also vertical, for CJK) or a graphic, with size, opacity, anchor and inset.
- Save the settings as an export preset (`export.savePreset`); plug-ins can post-process exported files.
- File → Contact Sheet PDF… writes a quick contact sheet; the Print module does full layouts.

Export never overwrites an original or its sidecar (`export.checkTarget`).

## From the command line

The same encoder as the dialog:

```sh
<cli> render IMG_0001.CR3 -o out.jpg --set light.exposure=0.3 --preset <id> --size 2048 --quality 90
<cli> render in.ARW -o out.tif --opt colorSpace=proPhoto --opt bitDepth=16
<cli> run --library ~/Pictures/Lib library.import paths='["/media/card/DCIM"]' mode=copy \
    app.export dir=/tmp/out preset="<export preset name>"
```
