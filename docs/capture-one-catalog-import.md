# Capture One catalog import

File → Import Capture One Catalog… brings a Capture One catalog's library into LightCraft: its photos and their
variants, ratings, colour tags, keywords, IPTC fields and the albums, projects and groups they are organised in.
Crops, turns, flips and lens corrections come in too (issue #4596); other develop settings (exposure, colour,
masks) are not imported yet. Capture One is not required, and the catalog is only read (issue #4250).

Capture One is a trademark of Capture One A/S. LightCraft is not affiliated with Capture One; the name is used only
to say which files can be imported.

## What can be imported

A Capture One **catalog** is a folder holding a SQLite database, `<Name>.cocatalogdb`, next to `Originals/`,
`Adjustments/` and `Cache/`. On macOS the folder is a package, `<Name>.cocatalog`, that Finder shows as one file.
The file dialog takes the package, or the `.cocatalogdb` inside a catalog folder (on Windows, where the catalog is a
plain folder) or inside a catalog backup, the dated folder Capture One writes (File → Backup Catalog…) with only the
database in it. The commands below also take a folder: its database is the one named after the folder, else its
only `.cocatalogdb`; a folder with several is refused with their names. `.cocatalogdb.backup`, which Capture One leaves after upgrading a catalog, is read when it is chosen, never
picked from a folder.

Catalogs last written by Capture One 16.3 (database schema 1621) have been read, one of them first made with
Capture One 21 and upgraded since; catalogs still in an older schema have not been seen yet. The reader goes by the
tables and columns it needs rather than by version number: a catalog that lacks one is refused with a
message naming what is missing and which Capture One version last wrote it, so newer catalogs import as long as
they keep these.

Not read: Capture One **sessions** (`.cosessiondb` and the `.cos` files next to the images), catalogs older than
Capture One 21, and EIP packages.

## What comes in

| Capture One | LightCraft |
|---|---|
| An image's first variant | a photo |
| Further variants | virtual copies of it, named "Variant 2", "Variant 3", … |
| Originals inside the catalog (`Originals/`) and referenced elsewhere | the photo's file, where it is |
| Star rating | rating |
| Colour tags red, yellow, green, blue, purple | the colour label of the same colour |
| Colour tags orange and pink | no label (see below), the keyword `Capture One\|Orange` / `Capture One\|Pink` |
| Keywords (`Parent\|Child`) | keywords, without the parents Capture One lists beside a keyword |
| The keyword list, also keywords no photo carries | the Keyword List, with "include on export" |
| Title, description, creator, copyright, usage terms, location, city, state, country | the same fields |
| Make, model, serial, lens, ISO, aperture, shutter, focal length, exposure compensation, GPS | what the file says; the catalog fills what it leaves empty |
| Capture time | the file's; the catalog's (the camera's clock) for an original that isn't there |
| Albums | albums |
| Projects and groups | album folders, with their albums inside |
| Smart albums | smart albums with the same rules; else albums with the photos the rule selects (see below) |

Left out, and said so in the import's result: images in Capture One's trash, smart albums whose rule can't be
read (below), and adjustments other than crop, rotation, flip and lens corrections (below). Capture One's own collections (All Images, Recent Imports, Recent Captures, Trash,
the folder tree) are not albums: LightCraft has its own. Photos a project or group lists because they are in its
albums are only in those albums. The result window says all of this in the app's language; the command line and
agents get the same warnings in English, and as data (`notices`) in the import's report.

**Orange and pink.** Capture One has seven colour tags, LightCraft five labels. An orange or pink photo gets no
label, but the colour isn't lost: its name stays with the photo, is written to its XMP (`xmp:Label`) while the
photo has no label of its own, and the keyword finds the photo. Setting a label in LightCraft replaces the kept
colour (Edit → Undo brings it back); importing the catalog again brings it back too.

**Smart albums.** A smart album whose every condition has a LightCraft smart album rule comes in as a smart album
with those rules: rating, colour tag, keywords, any text, capture date (also its year or month), orientation,
lens, serial number, ISO, aperture, shutter speed, focal length, exposure compensation, has GPS, title,
description, creator, copyright, location, city, state and country. Orange and pink are found by their keyword;
"is not" on keywords and dates becomes a "none of" group. Conditions switched off in Capture One are left out.

Where LightCraft has no rule that means the same (file size, pixel width or height, adjusted, annotated, flash
mode, the camera owner, the camera's make or model, the IPTC fields LightCraft has no field for, …), the smart
album comes in as a plain album holding the photos its rule selects in the catalog at the time of the import, and
the import's result names it and the fields. The camera's make or model is one of these because LightCraft's camera
is both together. Capture One's "is" on keywords tells upper and lower case apart and LightCraft's doesn't, so such
a condition stays a rule only while the catalog and the library have its keyword in one spelling. Orientation is
taken as Capture One shows the photo, turned by the file's orientation or by hand; LightCraft turns a photo only as
its file says, so an orientation condition stays a rule only while no photo of the catalog was turned by hand. A
catalog that doesn't say how its photos are turned leaves a smart album on orientation out, with a warning.
Aperture, focal length and exposure compensation are compared as typed (5.6 is 5.6). Text is compared ignoring
case; "contains" with several words is matched as typed, so such a condition counts as one LightCraft has no rule
for.

A rule that tests something the catalog import doesn't read (Live Comments, a field this version of Capture One's
catalog doesn't have), or that can't be read at all, leaves the smart album out, with a warning. Every smart
album's rule, also one left out, is kept in the recovery archive: as read, or as written when it can't be read.

**Other IPTC fields** (headline, ISO country code, job identifier, instructions, source, provider, categories,
genre, scene, contact fields, description writer) have no field in LightCraft yet. They are kept with each photo's
record in the import's recovery archive in the library (`Interop/`), like a Lightroom catalog's source data.

**Camera stand-ins.** For a file without a camera Capture One records the file format instead (make "Joint
Photographic Experts Group", model "Jpeg File"), and for a lens it can't name a description such as "-- mm f/--".
These are not taken as a camera or a lens. GPS, which Capture One writes as text in degrees and minutes with the
hemisphere, is read as decimal degrees.

## Crop, rotation, flip and lens corrections

Only what the user changed in Capture One comes in; what Capture One does by itself stands for what LightCraft does
by itself. Each variant comes in on its own, not as a copy of its master's edits.

| Capture One | LightCraft |
|---|---|
| Rotation (`ZROTATION`, degrees, clockwise, including the file's orientation) | the quarter turns beyond the file's own as the photo's rotation (Rotate Left / Right), the rest as the crop's straighten angle |
| Flip (`ZFLIP`: 1 left ↔ right, 2 top ↔ bottom, after the rotation) | Flip Horizontal / Vertical |
| Crop (`ZCROP`: centre on the stored image, size as shown) | the crop, on the same part of the photo; the ratio's name as the locked aspect |
| A straightened photo without a crop of its own | the largest rectangle of the photo's shape inside it, as Capture One shows it |
| Lens profile and distortion amount | Enable Profile Corrections with LightCraft's own profile (from the file or the lens database), and the amount when it isn't 100; the generic profile switches it off |
| Chromatic aberration switched on or off | Remove Chromatic Aberration |
| Keystone tilts (`TILTX`, `TILTY`) and aspect, without a crop of the user's own | Transform's Vertical, Horizontal, Rotate and Aspect, fitted to Capture One's result, with Scale and a crop that show the part of the photo Capture One shows |

A photo that brings any of these changes gets its lens correction as Capture One showed it: the crop was drawn on
the corrected photo, and LightCraft switches a correction on by itself later only for a photo that is still as
imported. A profile LightCraft doesn't have yet (no lens database, an offline original) applies once it finds one;
the import says how many photos wait for one. Capture One's working area after a lens correction that hides the
empty edges is smaller than the photo; LightCraft's correction fills its frame, as a Fujifilm camera's does, so the
crop is taken relative to that area. With another lens database profile the crop can sit a few pixels off.

Capture One's keystone tilt turns a virtual camera about an axis of the stored image (focal length: the keystone's
focal value times the image diagonal; the tangent of the angle: the slider value / 100 times that focal value); its
aspect makes the photo as shown wider (or narrower) by 1 + value / 100; and it shows the largest rectangle of the
photo's shape centred on the corrected photo's bounding box. LightCraft's Transform turns its camera with a fixed
focal length, so the import fits Vertical, Horizontal, Rotate and Aspect to bring a grid on the photo where Capture
One shows it, up to a scale and a shift: within 1 % of the photo's width, 1.5 % for a strong tilt both ways at once.
A keystone skew shears the photo along the stored image's axes by the tangent of half its value; LightCraft's
Transform has no shear, so it is left out.

Left out, and named in the import's result with the number of photos: a keystone skew, a keystone tilt together
with a crop of the user's own (with a keystone the crop is stored at another scale, not measured yet), a crop
reaching past the photo (it is kept on the photo), distortion correction with the generic profile, a lens profile
chosen by hand (LightCraft takes the lens from the file), distorted areas shown, sharpness falloff, diffraction
correction, light falloff, purple fringing, shift lens movement and LCC.

Checked against Capture One's own TIFF exports of a test catalog made for this: every turn, flip, straighten and
crop above, also on a portrait phone photo and on Fujifilm raws Capture One corrects, shows the same part of the
photo (correlation 0.97 to 1.0 between the exports); keystone tilts and aspect on photos 0.99 to 1.0, and on a
measuring chart made for this (a grid, where any misplacement shows) as close as the unedited chart itself.

## Where the catalog goes

As for a Lightroom catalog, the desktop app first asks whether the catalog merges into the open library or goes
into a new library, named after the catalog (see [Lightroom catalog import](lightroom-catalog-import.md#where-the-catalog-goes)).
The setting *When importing a Lightroom or Capture One catalog* turns the question off for both. The import runs
in the activity stack, can be cancelled while the catalog is read, is one undo step ("Import Capture One
Catalog"), and ends in a result window with the counts and warnings.

Importing the same catalog again adds nothing twice: photos are recognised by their variant's id, albums by the
catalog's id (`ZDOCUMENTUUID`), which a catalog and its backups share. Ratings, labels, metadata and keywords are
taken from the catalog again; photos edited in LightCraft keep their edits.

## Reading a catalog safely

Nothing is ever written to the catalog. Capture One keeps a rollback journal next to the database while it writes:
a catalog with a journal that holds a transaction (or a write-ahead log) is refused until Capture One has closed it
cleanly, and so is one whose journal or log is there but can't be read. A catalog Capture One merely has open (a `writelock` file in the package) is read, with a warning that
changes Capture One hasn't saved yet are not included. A database that changes while it is read is refused.

The database is read with LightCraft's own read-only SQLite page reader. Every value is treated as untrusted: a
damaged or crafted catalog gives an error or a warning, never a crash. A path that points outside this computer, is
a device or names a file LightCraft doesn't read never becomes a photo record.

## Scripts and agents

```
lightcraft-cli run --library "LightCraft Library" library.inspectLightroom path="Photos.cocatalog"
lightcraft-cli run --library "LightCraft Library" library.importLightroom path="Photos.cocatalog"
```

The engine commands of the Lightroom import take a Capture One catalog too; the app command
`file.importCaptureOne {path?, into?, name?, location?}` works as `file.importLightroom` does. The browser build
can't import catalogs.

## How it was built

The reader (`crates/engine/src/capture_one_catalog.rs` and its modules) was written from catalogs Capture One wrote,
read as data, and from public descriptions of the format. Smart album rules (`capture_one_catalog/smart.rs`)
were learned from smart albums made for this in Capture One 16.3 and compared with its rule dialog. No Capture One program files, resources or profiles were
looked at. It writes its result in the Lightroom import's model, so the import itself is shared.

Tests build small synthetic catalogs; no real catalog or photo is in the repository. A real catalog can be read in
an opt-in test:

```
LIGHTCRAFT_C1_CATALOG="/path/to/Photos.cocatalog" cargo test -p lightcraft-engine --lib capture_one_catalog::tests::a_real_catalog_reads -- --nocapture
LIGHTCRAFT_C1_CATALOG="/path/to/Photos.cocatalog" cargo test -p lightcraft-ui-egui --lib capture_one_import::a_real_catalog
```
