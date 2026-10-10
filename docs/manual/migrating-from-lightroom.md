# Migrating from Lightroom Classic

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

<app> reads a Lightroom Classic catalog (`.lrcat`) directly: Lightroom does not need to be installed, and the
catalog and your originals are opened read-only and never changed. You can keep using Lightroom while you try
<app>; re-importing later updates the same photos instead of duplicating them.

## Before you start

1. In Lightroom, let it finish any work, then quit it (a catalog that changes during the read is rejected).
2. Optional but useful: Metadata → Save Metadata to Files writes XMP sidecars, which other tools can read too.
3. Make sure the drives holding your originals are connected; missing originals are still catalogued, and you
   can relink them later (Library → Find Missing Photos…).

## Import

File → Import Lightroom Catalog… shows what will come across (`library.inspectLightroom`: photo and collection
counts, missing files, warnings) and then imports it in the background, with progress and Cancel. One undo step
reverses it. From the command line:

```sh
<cli> run --library "<library_default>" library.inspectLightroom path="Lightroom Catalog.lrcat"
<cli> run --library "<library_default>" library.importLightroom path="Lightroom Catalog.lrcat"
```

Photos you already edited in <app> keep their edits; `updateExisting=true` replaces them with Lightroom's.

## What comes across

| Lightroom | In <app> |
|---|---|
| Photos and folders | referenced in place (nothing is copied or moved) |
| Star ratings, picks / rejects, colour labels | yes |
| Titles, captions, copyright, creator and other XMP metadata | yes |
| Keywords, including hierarchies | yes |
| Collections and collection sets | albums and album sets |
| Smart collections | regular albums with their current photos; the rules are kept in the recovery archive |
| Virtual copies | virtual copies |
| Develop settings | mapped onto <app>'s controls (see below); unmapped fields are reported |
| Develop history and snapshots | kept in the recovery archive as source data, not as <app> history yet |

## What doesn't (yet)

- **Rendering is approximate.** Basic tone, colour, curves, HSL, grading, detail, effects, crop and the supported
  mask types stay editable, but Adobe camera profiles, Adobe AI features (Denoise, Select Subject results),
  lens-profile corrections from Adobe's database and Lightroom's process-version algorithms are not reproduced.
  Expect photos to look similar, not identical; [parity.md](../parity.md) (`LR-BEHAV-RENDER-FIDELITY`) tracks this.
- Lightroom's `ProcessVersion` is not mapped: imported photos use <app>'s latest process
  ([process-versions.md](../process-versions.md)).
- Custom white balance on non-DNG raws stays As Shot (the catalog has no as-shot white to convert from); see
  [lightroom-catalog-import.md](../lightroom-catalog-import.md).
- Deferred Auto Tone is not evaluated; run Auto (`Shift+A`) again if you want it.
- Not read from the catalog: stacks, face regions/people (read from XMP sidecars instead), publish services and their
  history, Book / Slideshow / Print / Web creations, the Map module's saved locations, and Lightroom's preview cache.
- Presets, templates and keyword lists live outside the catalog: bring develop presets in with File → Import
  Profiles & Presets… (Lightroom `.xmp` presets), keyword lists with File → Import Keywords…. Adobe's own
  profiles (`.dcp`) and lens profiles (`.lcp`) are never used.

## Safety

Before changing anything, the importer saves the source records, decoded XMP, develop settings, history, snapshots
and collection content to compressed recovery archives in the library's `Interop/` folder (best-effort, capped in
size). Imports are bounded: a 1 GiB database, one million rows per table. Details and limits:
[lightroom-catalog-import.md](../lightroom-catalog-import.md).

## Habits that carry over

- The same module layout and most of the same keys: `E`/`C`/`N` loupe, compare and survey, `D` for Develop, `P`/`X`/`U`,
  `0`–`5`, `6`–`9`, `R` crop, `Q` remove, `Shift+W` masking, `Cmd+'` virtual copy, `Cmd+Shift+E` external editor
  ([keyboard-shortcuts.md](keyboard-shortcuts.md)). Settings can switch to the Alternative keymap.
- XMP sidecars written by either app are read by the other ([xmp-interop.md](../xmp-interop.md)).
- Lightroom plug-ins (Lua) do not run; <app> plug-ins are WebAssembly ([plugins.md](../plugins.md)).
