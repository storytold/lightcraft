# Lens profiles from a lens database

Raws that carry no lens correction of their own (DNG `OpcodeList3`, or a camera correction a raw reader converts,
see [sony-lens-corrections.md](sony-lens-corrections.md) and [nikon-lens-corrections.md](nikon-lens-corrections.md)) are corrected from a **lens database in the lensfun
XML format**. The app does not ship any lens database: lensfun's data is licensed CC BY-SA 3.0 and is downloaded at
run time, never bundled and never committed. Adobe LCP profiles are never used.

## Setup: one click

1. Nothing is downloaded without the user's click. On the first launch the desktop app asks once: "Download lens
   profiles? LightCraft can correct distortion and vignetting for many lenses using the open lens database
   (lensfun, about N MB)", with **Download** and **Not Now** (N is the pinned files' sizes added up). Not Now (or
   Esc) downloads nothing and the app doesn't ask again; Settings ▸ General ▸ Lens profiles has **Download Lens
   Database** and a switch that turns the offer back on. The answer is kept with the app's settings (`ui.json`,
   `lensDatabase`: `ask` / `accepted` / `declined`; a value it can't read asks again), and
   `lensfun_download::fetch_on_start` is the one place that decides. After a yes, a start that finds the database
   missing or from an older pin downloads it again (an interrupted download resumes).
   Download fetches lensfun's `data/db` (56 XML files, about 5 MB) into the app's data folder
   (`<config dir>/lensfun`, beside the face and denoise models; see `crates/engine/src/config.rs`), with a row in
   the activity stack (its ✕ stops it). The files are pinned to one lensfun commit by name,
   size and SHA-256 (`crates/engine/src/lensfun_download.rs`); a file that differs is thrown away. A later build with
   a newer pin replaces them once. The files are staged in `<config dir>/lensfun.download`, `SOURCE.txt` (the commit
   and the licence) is written there last, and the staged folder then replaces the old one as a whole: until it is
   in place the previous complete database is used, and if the swap fails it is put back. The app's folder is read
   only when it has `SOURCE.txt`, and a folder whose pinned files are missing or cut short is downloaded again.
   The CLI (`render`, `merge`, `snapshot`) and the headless MCP server read this folder too, but never ask and never
   fetch on their own; `lens.downloadDatabase` fetches it on request from any of them.
2. By default the files come from lensfun's GitHub repository at the pinned commit. `LIGHTCRAFT_LENSFUN_URL` lists
   other places serving the same files (base URLs, tried first), e.g. a copy on LightCraft's own server.
3. `LIGHTCRAFT_LENSFUN_DIR` points at a database of your own instead (lensfun's `data/db`, or a Linux
   distribution's `/usr/share/lensfun/version_1/`); then the app doesn't offer the download, and that folder is read
   even after `lens.downloadDatabase` fetches the app's own copy.

When the database arrives (or changes), every library raw without a profile whose lens it knows is read again in the
background and gets its profile; a photo still as imported gets the correction switched on, as import would, and an
edited one keeps its settings. When a download replaced the database's files, the raws whose profile came from the
old database are read again too (they get the new profile, or lose one it no longer gives), and so are raws whose
file named no lens at import. Ticking **Enable Lens Corrections** on a photo without a profile looks it up again
too. `library.refreshLensProfiles` (`all: true` for the wider pass), `lens.downloadDatabase` (which also selects the
downloaded folder) and `lens.databaseStatus` do the same for agents.

A database profile is pinned to the photo: every render (fresh decode, cached preview, thumbnail, export) uses the
profile the catalog holds, never one a decode looked up in a newer database, and the profile is part of the render
and thumbnail cache keys, so a changed profile never shows an old cached render. A file's own correction comes from
the file as before.

The Optics panel says which profile is loaded: "Profile: from the file", "Profile: <lens> (lens database)", or
"No lens profile found for <lens>". A profile is never copied between photos: presets, Copy / Paste, Sync and Auto
Sync carry Enable Lens Corrections and its amounts, and each photo keeps the profile of its own lens
(`crates/engine/src/tests/lens_refresh.rs`).

Files are read once per folder and again after a download. Invalid files are skipped with a warning in the log.

## What is applied

The lens is identified from EXIF: camera make / model (for the mount and crop factor; the 35 mm equivalent focal
length is used when the camera isn't listed) and the lens model string. Lens names match on their words; every
number (focal lengths, apertures) in the database name must appear in the EXIF name, and the lens maker must agree
when the file names one.

Some cameras record no lens model at all, only the lens specification: Nikon DSLRs write the lens's focal range and
maximum apertures in their maker note (tag `0x0084`) and no Exif `LensModel`, whatever lens is mounted. For such a
file the lens is looked up by a name built from the specification, `24-70mm f/2.8` (`18-55mm f/3.5-5.6`,
`85mm f/1.8`), and a profile is used only when one of its names is exactly that (ignoring case and punctuation).
The specification fits every maker's lens with that range (a Nikkor, a Sigma, a Tamron), so neither a partial match
nor a name with a maker in it ("Samyang 85mm f/1.4" for "85mm f/1.4") is taken; a database that should correct such
files names its profile that way, e.g. a lensfun entry with an extra `<model>24-70mm f/2.8</model>` for the one
24-70 f/2.8 the photographer owns. lensfun's own database has no such names, so it doesn't correct these files.

| Correction | Models | Interpolated over |
|---|---|---|
| Distortion | `poly3`, `poly5`, `ptlens` | focal length |
| Lateral CA (TCA) | `linear`, `poly3` | focal length |
| Vignetting | `pa` | focal length, aperture (in stops), focus distance (nearest calibrated, by 1/d; infinity when unknown) |

Fisheye and other non-rectilinear lenses get no distortion correction. The profile is converted by least squares
into the DNG `WarpRectilinear` / `FixVignetteRadial` polynomials, so it goes through the same path as embedded
corrections: **Enable Profile Corrections** and its Distortion / Vignetting amounts, the CPU and GPU geometry
stage (one resample), orientation handling and DNG export. A conversion whose radial error exceeds 0.001 of the
half diagonal, or that would fold the image, is not applied.

Normalisation follows lensfun's documentation: distortion and TCA radii are 1 at half the shorter side of the
calibration frame (its `cropfactor` and `aspect-ratio`), vignetting radii 1 at its corner; the optical centre is
the image centre (lensfun's `<center>` offsets are not read).

## Limits

- Only raws: rendered images (JPEG, HEIF, TIFF) are often already corrected in camera.
- The focus distance is not read from maker notes yet: vignetting uses the farthest calibrated distance.
- No manual make / model picker and no choice between sources (in the file, lens database, an own calibration)
  yet; matching is automatic, the file's own correction wins.
- Not yet validated against real photos and another renderer; the tests use synthetic profiles and grids.
