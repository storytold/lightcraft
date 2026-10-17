# Lens profiles from a lens database

Raws that carry no lens correction of their own (DNG `OpcodeList3`, or a camera correction a raw reader converts,
see [sony-lens-corrections.md](sony-lens-corrections.md) and [nikon-lens-corrections.md](nikon-lens-corrections.md)) can be corrected from a **lens database in the lensfun
XML format** that you supply. The app does not ship any lens database: lensfun's data is licensed CC-BY-SA 3.0 and
stays a separate download. Adobe LCP profiles are never used.

## Setup

**Settings ▸ Import ▸ Lens profiles** shows the database in use (its folder, how many lenses and cameras, when its
files were last updated) and offers:

- **Choose Folder…**: a folder of lensfun-format `*.xml` files, e.g. the `data/db` folder of
  <https://github.com/lensfun/lensfun> or the copy a Linux distribution installs under `/usr/share/lensfun/version_1/`.
- **Download** (**Update** once downloaded): fetches lensfun's database, only when you ask, from the project's source
  archive (`https://github.com/lensfun/lensfun/archive/master.tar.gz`, about 3 MB). Only its `data/db/*.xml` files are
  kept, in `lensfun/db` in the settings folder; the archive is size-capped, file names are checked so nothing is written
  outside that folder, and the old database stays until the new one has been read and holds lenses. The download
  shows in the activity stack and can be cancelled there; a failure (no network, a 404, a damaged archive) is shown
  in the section. The section names lensfun and its licence (CC-BY-SA 3.0).
- **Remove**: stop using the database (a downloaded one is deleted).

The folder is an app setting (`lens-profiles.json` in the settings folder), shared by every library, the CLI and the
MCP server. A change takes effect at once, without a restart: the database is reloaded and the photo open in Develop
is looked up again (other photos when they are next opened in Develop, or with `lens.db.refresh`).
`<PREFIX>_LENSFUN_DIR` still works and overrides the folder chosen in Settings (the section says so).

Develop ▸ Optics says why **Enable Lens Corrections** has nothing to apply to a raw without its own correction: no
lens database (a link to the Settings section), no lens named in the photo's metadata, or no profile for its lens.

Commands: `lens.db.status`, `lens.db.setDir {dir}`, `lens.db.download {wait?}`, `lens.db.downloadCancel`,
`lens.db.remove`, `lens.db.refresh {ids}`, `lens.db.photo {id, lens?}`, `lens.db.list {mount?, crop?, query?, limit?,
offset?}`; `app.settings {section: "lensProfiles"}` opens the section. Invalid files are skipped with a warning in the
log.

## Which correction applies

In this order, the first that applies wins:

1. the file's own correction (DNG opcodes, a camera correction a raw reader converts);
2. a lens given by the caller (a lens picker's choice: maker, model, and the focal length and aperture such files
   often lack, else the file's), found in the database by one of its names;
3. the automatic match by the lens name in the photo's metadata;
4. none.

A given lens the database lacks gives no correction; it does not fall back to the name match. `lens.db.photo`
reports which one applies: `embedded`, `chosen` (when called with `lens: {maker?, model, focal?, aperture?}`, or
just the model name), `found`, or why there is none (`noDatabase`, `notRaw`, `noLensName`, `notFound`). In code the
order is `lens_profiles::resolve_raw(raw, meta, given)` (`lookup_with` for the database part,
`Session::lens_match_with` for the status). Nothing stores a choice on the photo yet: the given lens is a parameter.

`lens.db.list` lists the database's lenses for a picker, sorted by maker and name: `mount` keeps lenses for that
camera mount (and those naming none), `crop` (the camera's crop factor) keeps lenses calibrated on that frame or a
larger one, and `query` keeps lenses whose maker or names contain every word. It returns at most `limit` lenses
(100 by default, never more than 500) from `offset`, with the `total` that match and whether there are `more`;
each lens has `maker`, `model` (the name to pass back as `lens.model`), `models`, `mounts`, `cropFactor`, the
calibrated `focal` range and which corrections it has (`distortion`, `vignetting`, `tca`). Malformed parameters
(non-numbers, negative or non-finite values, names over 256 characters) are errors, never a crash.

## What is applied

The lens is identified from EXIF: camera make / model (for the mount and crop factor; the 35 mm equivalent focal
length is used when the camera isn't listed) and the lens model string. Lens names match on their words; every
number (focal lengths, apertures) in the database name must appear in the EXIF name, and the lens maker must agree
when the file names one.

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
- No manual make / model picker yet; matching is automatic.
- Not yet validated against real photos and another renderer; the tests use synthetic profiles and grids.
