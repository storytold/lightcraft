# Capture One catalog import

File → Import Capture One Catalog… brings a Capture One catalog's library into LightCraft: its photos and their
variants, ratings, colour tags, keywords, IPTC fields and the albums, projects and groups they are organised in.
Develop settings (adjustments, crops, masks) are not imported yet. Capture One is not required, and the catalog is
only read (issue #4250).

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
read (below), and adjustments. Capture One's own collections (All Images, Recent Imports, Recent Captures, Trash,
the folder tree) are not albums: LightCraft has its own. Photos a project or group lists because they are in its
albums are only in those albums. The result window says all of this in the app's language; the command line and
agents get the same warnings in English, and as data (`notices`) in the import's report.

**Orange and pink.** Capture One has seven colour tags, LightCraft five labels. An orange or pink photo gets no
label, but the colour isn't lost: its name stays with the photo, is written to its XMP (`xmp:Label`) while the
photo has no label of its own, and the keyword finds the photo. Setting a label in LightCraft replaces the kept
colour (Edit → Undo brings it back); importing the catalog again brings it back too.

**Smart albums.** A smart album whose every condition has a LightCraft smart album rule comes in as a smart album
with those rules: rating, colour tag, keywords, any text, capture date (also its year or month), orientation,
camera make or model ("contains"), lens, serial number, ISO, aperture, shutter speed, focal length, exposure
compensation, has GPS, title, description, creator, copyright, location, city, state and country. Orange and pink
are found by their keyword; "is not" on keywords and dates becomes a "none of" group. Conditions switched off in
Capture One are left out. Where LightCraft has no rule for a condition (file size, pixel width or height, adjusted,
annotated, flash mode, the camera owner, the IPTC fields LightCraft has no field for, …), the smart album comes in
as a plain album holding the photos its rule selects in the catalog at the time of the import, and the import's
result names it and the fields. Capture One's "is" on keywords tells upper and lower case apart and LightCraft's
doesn't, so such a condition stays a rule only while the catalog has its keyword in one spelling. Orientation is
taken as Capture One shows the photo, turned by the file's orientation or by hand. A rule that tests something the catalog import doesn't read (Live Comments), or
that can't be read at all, leaves the smart album out, with a warning. Capture One's rule is kept with the album's
record in the recovery archive. Text is compared ignoring case; "contains" with several words is matched as typed,
so such a condition counts as one LightCraft has no rule for.

**Other IPTC fields** (headline, ISO country code, job identifier, instructions, source, provider, categories,
genre, scene, contact fields, description writer) have no field in LightCraft yet. They are kept with each photo's
record in the import's recovery archive in the library (`Interop/`), like a Lightroom catalog's source data.

**Camera stand-ins.** For a file without a camera Capture One records the file format instead (make "Joint
Photographic Experts Group", model "Jpeg File"), and for a lens it can't name a description such as "-- mm f/--".
These are not taken as a camera or a lens. GPS, which Capture One writes as text in degrees and minutes with the
hemisphere, is read as decimal degrees.

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
cleanly. A catalog Capture One merely has open (a `writelock` file in the package) is read, with a warning that
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
