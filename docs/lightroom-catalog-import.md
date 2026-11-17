# Lightroom Classic catalog import

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

File → Import Lightroom or Capture One Catalog… opens `.lrcat` directly. Lightroom, Python, DNG conversion
and a C SQLite runtime are not required. The source database is read-only, including committed
`-wal` pages; a catalog that changes during the read is rejected when the change is
observable. Close Lightroom and every other catalog writer before inspection or import:
the metadata, header and companion checks are best-effort change detection, not a
transactional SQLite snapshot. The app owns subsequent edits.

The same command also opens a Capture One catalog (`.cocatalogdb`, see *Capture One catalogs* below); the file
dialog offers both kinds together and each on its own.

File → Open Catalog… given a Lightroom catalog folder (or an `.lrdata` / `.lrcat-data` folder, or the `.lrcat`)
doesn't make a library of the app there: it offers this import instead, with the catalog found
(`crates/engine/src/lightroom_folder.rs`, issue #891), and then asks where the catalog goes as described next
(unless the question is turned off, below).
`<cli> --library` refuses those places too.

**Where the catalog goes.** Before the import starts, the desktop app asks where the catalog goes (issue #2672),
unless Settings says to always merge (*Turning the question off*, below):

- **Merge into the open library "<name>" (N photos)**: the catalog's photos, collections and edits join the ones
  already in the library open now (the library's name is its folder's name).
- **Create a new library**: named after the catalog file (`Travel.lrcat` → "Travel", or "Travel 2" when a
  "Travel" folder is already there) and placed next to the open library (in its parent folder); both can be
  changed (Choose… picks the folder). The app creates and opens it as File → New Catalog… does (the folder
  must be new or empty; it is refused while another import runs), then imports the catalog into it. The library
  left behind stays in File → Open Recent Catalog.
- **Cancel** does nothing.

The window opens on *Merge* when the open library has no photos (there is nothing to merge with) and on *Create a
new library* when it has some.

**Turning the question off.** Settings → General → Library → *When importing a Lightroom or Capture One catalog*: *Ask where to
import it* (the default) or *Always merge into the open library* (how imports always worked before). The window's
*Don't ask again (always merge into the open library)* checkbox, offered with *Merge*, picks the second; the window
says where to turn the question back on. The choice is an app setting (`ui.json`, `settings.lightroomImport`:
`"ask"` or `"alwaysMerge"`); a settings file without it asks.

A session with no library on disk (in memory, e.g. after Continue Without Saving) has nothing to put a new library
next to: the catalog merges into it without asking, as before.

**Scripts and agents.** The app command `file.importLightroom {path?, into?, name?, location?}` (control channel,
`<cli> mcp --connect`) chooses without the window: `into: "open"` merges, `into: "new"` creates the library
`name` (default: the catalog's name) in the folder `location` (default: the open library's parent folder; a full
path), opens it and imports. Without `into` it shows the window (and returns `{asking: true, path, into}`, `into`
being the choice the window opens with) or merges when the setting says so. A catalog path that doesn't exist is
refused before a new library is made. The engine command `library.importLightroom` always imports into the library
open now.

Equivalent commands:

```text
<cli> run --library "<app> Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
<cli> run --library "<app> Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

The import itself (`library.importLightroom`, `<cli> run`) adds records to the currently open
library; only the desktop window above can make a new library for it first. The desktop result stays open until dismissed and
shows the destination library, the number of records added, missing originals and warnings.
**Show in Library** opens All Photos, clears filters and selects an imported record. Errors stay
readable in the same result window. If another dialog is open, the result waits until that dialog closes. The result
is also available through `library.importLightroom {"status":true}` during that session.

Original files are referenced in place. Ratings, picks/rejects, color labels, XMP metadata,
hierarchical keywords, regular collections/collection sets, smart collections, saved prints, folder stacks,
virtual copies and mapped develop settings are imported. An image's caption is taken from Lightroom's own IPTC table (`AgLibraryIPTC`) when it has one
there, since the XMP packet Lightroom caches for the image can lack it. Missing originals remain catalogued for relinking, count as added records and are reported
separately from failures reading files that are present. Known-missing originals are not probed
again. Previously browsed Local records join All Photos even when their originals are offline;
their promotion is counted once and undone together with the catalog import.
An image whose file, folder or root folder record is gone from the catalog, whose reference to one is not a whole
number (missing, NULL, text or a fraction: it names no record, not record 0), or whose original path
is empty (Lightroom leaves these behind and ignores them), is skipped together with its virtual
copies instead of failing the whole import. The result lists them as data (`skipped`: each
image id with its reason, `missingFile`, `missingFolder`, `missingRoot`, `incompletePath` or
`masterSkipped`), also in `library.inspectLightroom`, and the desktop result window says how many in the UI language.
Virtual copies count as added records too; reimport reports zero additions when all
records already exist. Existing
edits are preserved by default; `updateExisting=true` explicitly replaces them. Settings read from an
original's own XMP (embedded or a sidecar) while the import adds it, or while it was browsed, are not
existing edits: a photo the import adds, or a Local one unchanged since browsing, takes the
catalog's settings, history and snapshots, and a setting the catalog leaves out (Lightroom omits
some at zero) starts from the camera's defaults. Any change after browsing, a rating or keyword
included, makes a Local photo's settings count as existing. Persistent import
identity prevents reimport from duplicating virtual copies and collections. One undo step reverses
catalog changes; any saved recovery archives remain available. When the catalog's data can't be
applied, the import stops with that error and adds no photos: the records it had already made for the
catalog's originals are removed, and photos browsed in Local are Local again. The redo history is
cleared, as by any change.

Records marked `VIDEO` in the source catalog appear in the **Videos** type filter, including
offline originals. Reimport also repairs the classification of videos imported as images by
older builds, preserving their IDs, edits and album/stack membership. This does not add video
playback, frame previews or video discovery through folder synchronization.

Before changing records, the importer attempts to save source image records, decoded XMP,
verbatim develop settings, history, snapshots and collection content to compressed
`Interop/lightroom-import-N.lca` recovery archives in the app's library. Archiving is
best-effort: a size limit or write failure produces a warning and does not prevent photo import.
Unchanged source data reuses its existing archive. History-step and snapshot settings that Lightroom
Classic 13 and later store compressed are decoded into it; one that can't be decoded, or past 32 MiB
of them in one table, is left out with a warning (a row past that limit is marked
`_overDecodeBudget`). Other opaque SQL BLOBs are skipped. Each archive is capped at 32 MiB
(uncompressed and on disk); one past that with its history-step and snapshot settings is saved
without them, with a warning, and skipped only when still too big. At most eight archives and
128 MiB total are retained. Oldest managed archives are retired; legacy `.json` archives remain
untouched. When an archive is saved but cleanup afterwards fails (a temporary file or an old archive that
can't be removed), the report still names the archive and a separate warning names what was left
behind; the folder is synced once the archive is saved.
`Interop/lightroom-index.json` records imported identities. Source catalogs and originals are
never overwritten.

Native import reads MAIN database pages on demand; no whole-file size cutoff or MAIN-file read
allocation is used. Committed WAL validation is streamed into a compact page index; WAL remains
capped at 1 GiB and 4,000,000 frames. MAIN is structurally bounded to 16,777,216 pages (up to
1 TiB logical size with 64 KiB pages); each table B-tree walk is bounded to 4,000,000 visited
pages. A record is capped at 64 MiB and XMP packets at 16 MiB. The in-tree, read-only Rust SQLite
B-tree reader follows SQLite’s public file-format specification; it introduces no dependency or C
runtime. Tables are streamed to the importer one row at a time, so memory follows what the
importer keeps (mapped columns, never BLOBs), not the table's size: the large develop settings,
history, snapshot and metadata tables have no row or payload cap. Tables the importer keeps whole
(folders, files, images, keywords, collections, stacks) are capped at 1,000,000 rows and 256 MiB of
payload each. These bounds limit work and decoded data: a catalog within the page-count cap may
still exceed a per-table or record limit. Inspection and import can be cancelled between pages.
The open file and source path are checked again after all tables are read. Close Lightroom before
importing: metadata checks detect ordinary concurrent changes, but are not SQLite snapshot isolation.
Develop settings that can't be read fail the import with a clear error (otherwise every photo
would arrive unedited). Optional history, snapshot, metadata, keyword and collection tables that
can't be read are skipped with warnings, unless the read failed because the catalog file itself
failed or changed, which fails the import; required photo tables still report errors. A develop
settings, metadata, keyword, history, snapshot or collection member record whose `image` is not a whole
number (missing, NULL, text or a fraction) belongs to no photo: it is left out, with one warning per
table; a collection member stays in the recovery archive but joins no album. A record whose `image` is
a whole number naming no photo in the catalog is still dropped without a warning. Unrelated
unsupported schemas are ignored. A needed
WITHOUT ROWID or virtual table produces an explicit error, handled as a warning when optional.

A capture time of a year or a month alone (`1983`, `1983-04`: Lightroom keeps a scanned print's date that way) is
imported as stated. A capture time that isn't a date (including an impossible one such as 2026-02-30) is left out
with a warning naming the image, which imports without it; the catalog's value stays in the import archive. A record
an earlier version stored with such a time keeps it, with a warning, rather than take Lightroom's (undoing the import
could not restore it), and a new virtual copy of it doesn't inherit it.

Develop settings reuse the existing XMP/preset mapper. Supported sliders, curves and supported
mask structures remain editable, but this is approximate rendering: Adobe AI
models, some masking/retouch fields and process-version algorithms are not reproduced. Unmapped
fields are reported. The profile the person picked (the `Look` table's `Name` when there is one, else `CameraProfile`) selects the camera's Lightroom-matched calibrated look of that
name (`profile.camera = "cal:<slug>"`, see [xmp-interop.md](xmp-interop.md)); a camera without one
keeps its default colour, and a photo already on an imported `.dcp` keeps it. Lightroom's `ProcessVersion` is not mapped: newly imported photos are on
the app's latest rendering process, and photos already in the library keep theirs
([process-versions.md](process-versions.md)). When a recovery archive is saved, it retains source settings/history/snapshots
and original smart-collection rules. Smart collections become smart albums when every rule has an exact equivalent: rating, pick
flag, color label, has-adjustments, ISO, focal length and aperture comparisons; text rules on
keywords, filename, camera, lens, title, caption, creator, copyright, location fields and file
format (contains all / any / none, begins/ends with, is, empty); capture date and edit date
(`captureTime` / `touchTime`: is, after, before, in range, in / not in the last N hours, days,
weeks, months or years; is today, yesterday, in this week (Monday to Sunday), month or year); collection-name rules (contains all / any, begins/ends with, is), which
become "in album" rules for the collections imported with it whose names match (resolved at import:
a collection made later isn't picked up by name); and nested match-all / any /
none groups. Any other rule (metadata status, …) keeps the collection as a
regular album with its current membership, and the report names the rule. On reimport an album keeps
its kind: an existing smart album keeps its current (possibly edited) rules, and a regular album
made by an earlier import stays regular and gains new members, with a warning (delete it and
reimport to get a smart album). The report counts albums by their resulting kind:
`smartAlbums`, `smartAsStatic`, `smartKept` (existing smart albums left as they were) and `stacks`. Folder stacks become stacks in Lightroom's order
and collapsed state; photos already in a stack keep it, so reimport doesn't duplicate them.

A Chromaforge album nests only inside a folder (a Lightroom collection set), but Lightroom also
keeps a saved print, book, slideshow or web gallery (and, in some catalogs, a collection) inside a
plain or smart collection, and a parent can be system-only (Quick Collection, an unsaved creation:
not imported) or gone from the catalog. Such an album joins the nearest collection set above it, or
the top level, and the result names it under warnings; books, slideshows and web galleries import as
regular albums with the photos they hold. On reimport an album keeps the place the user gave it.

A saved print (`creationId` `com.adobe.ag.print`) becomes a Saved Print: the album holds the
collection's photos and the print layout, and Print ▸ Saved Prints (or a double-click in Collections)
opens it. Its layout is the Lua table in `AgLibraryCollectionContent` (`owningModule`
`com.adobe.ag.print`): the page size and margins (points), the layout style, and the Print Job.
A Custom Package keeps every cell where Lightroom placed it (`bounds`, points from the page's top
left, in catalog order, back to front) with the photo it names; a cell whose photo isn't imported
stays empty. A Picture Package keeps its cells and repeats each photo in them; a Single Image /
Contact Sheet keeps its rows, columns, gaps and margins (cells fill the margins, so a fixed cell
size Lightroom kept smaller is reported). Zoom to Fill, Rotate to Fit, the stroke (a package's
Inner Stroke, a grid's Stroke Border), page background, page numbers, page info, crop marks / cut
guides, destination (printer or JPEG), resolution, print sharpening, media, rendering intent, Print
Adjustment, 16-bit output and draft mode carry over. The identity plate, watermark, a package's
photo border and a named printer profile have no equivalent here and are listed under warnings; the
original layout stays in the recovery archive. A layout that can't be read (or a saved print the
catalog keeps no layout for) imports as a plain album with the reason. Reimport gives an earlier
import's plain album of a saved print its layout once; a Saved Print keeps its layout, so changes
made since stay. The report counts them as `savedPrints`. An unsaved print is system-only and not
imported. A collection the catalog lists twice imports once; a different collection with
the id of an earlier one is left out with a warning naming both, and the photos listed under that
id go to the first. A collection member whose `collection` is not a whole
number (missing, NULL, text or a fraction) joins no album, with one warning; it does not join collection 0.

Lightroom's develop history (`Adobe_libraryImageDevelopHistoryStep`) becomes the photo's History,
oldest first, each step named as in Lightroom and mapped like the current settings (so a step shows
what the app can reproduce of it). When the newest step differs from the photo's settings, a final
"Lightroom catalog" entry holds them, so History ends where the photo is. Steps that can't be read
are skipped with a warning; at most the newest 200 are kept. Lightroom 6 stores each step's
settings as text, Lightroom Classic 13 and later compressed (a 4-byte big-endian length, then zlib,
as for XMP); both are read, and a compressed one that is damaged is skipped with a warning naming
the step. The report counts them as
`historySteps`. A photo whose edits are preserved (reimport without `updateExisting`) keeps its
History. Lightroom snapshots (`Adobe_libraryImageDevelopSnapshot`) become the photo's Versions, in
Lightroom's order and under their names, mapped the same way; unreadable ones are skipped with a
warning and the report counts them as `snapshots`. Of the 113 flat `crs:` develop fields in Adobe's
public XMP documentation, 105 map (test `develop_field_coverage_over_public_crs_fields`); the other
eight have no equivalent and are reported: `AutoWhiteVersion`, `LensProfileSetup`, `UprightPreview`,
`UprightCenterMode`, `UprightFocalLength35mm`, the pre-2012 `Defringe`, `HDRMaxValue` and `SDRBlend`.
A Custom white balance on a raw the app develops relative to its as-shot look (ARW, NEF, RW2,
RAF, CR3, CR2, PEF, SRW; Lightroom names them all `RAW`) can't be taken as Kelvin on its own. A Custom
photo's develop settings keep no as-shot white, but its history does: the last
`Adobe_libraryImageDevelopHistoryStep` that says As Shot with a `Temperature` and `Tint` gives the white
the edit is the same mired shift from (read row by row, none kept, so a history table of any size gives it; a
read that fails warns and keeps the whites found so far). With no such step the photo stays As Shot and
`Temperature, Tint` are reported
(issue #510, upstream #3906; a DNG keeps the Kelvin value, and an XMP packet with `crs:AsShotTemperature` is
shifted by the same mired difference, see [xmp-interop.md](xmp-interop.md)). A raw whose camera has
measured colour matrices (`lcc_raw::spectral`: ILCE-7M3, D850, EOS 5D Mark III…), or whose white-balance
presets give a Kelvin scale (`lcc_raw::kelvin_presets`: ILCA-99M2, ILCA-77M2…), edits in Kelvin
and keeps the Kelvin value too (issue #730) — when its file was found and probed, since the camera is
read from the file, not from the Lightroom catalog; a missing file is treated as a relative-scale raw
until Reload reads it.
Lightroom's `-999999` deferred-adjustment sentinel is omitted from both catalog and XMP mappings;
it is reported, included in any saved archive, and never clamped into a real slider value. Deferred Adobe Auto Tone
is not evaluated by the importer; the app's Auto control remains available after migration.

New photo records are moved into staged operations; only masters needed for virtual copies
retain a second owned record while the import commits.
Applying catalog operations collects affected source-cache IDs before consuming the operations,
avoiding another full operation copy. Full-strength develop patches reuse their JSON without
serializing an unused copy of the current settings.

Native desktop inspection/import runs on a cancellable background worker with progress. Only
prepared catalog operations commit on the owner thread; archive index writing stays on a worker.
Late results from a closed or switched library are rejected, including reopening the same path.
Ordinary edits during preparation remain intact. CLI commands complete synchronously using the
same preparation/commit pipeline; browser builds reject native catalog import.

Archive version 1 begins with the 12 bytes `LC-LRARCH\0\x01Z`, followed by zlib-compressed JSON.
Recovery readers must cap decompression at 32 MiB. JSON retains source identities, photo paths,
settings, decoded XMP, keywords, history, snapshots, collection definitions and membership.

The importer uses shared Rust code on macOS, Windows and Linux. When a root folder's recorded
path isn't a folder on this computer (a catalog copied from another machine), the importer uses
the path relative to the catalog that Lightroom records for root folders on the catalog's volume
(`relativePathFromCatalog`), when that is a folder, and reports the remapping as a warning. Copying
a project folder with its catalog inside it therefore keeps its originals linked. Other moved
originals stay missing; use the app's missing-photo relinking tools to update their paths.

Original and root folder paths are resolved with the catalog as the document
(`crates/engine/src/doc_path.rs`). Originals on this computer and on network drives and shares are looked at
wherever the catalog is: a mapped drive or mounted share is an ordinary path, and a UNC path (`\server\share\…`,
`file://server/…`) is followed on Windows even when the catalog is on another drive or share. (Upstream follows a
share only when the catalog itself is on it; Chromaforge does not, see the README.) A device path, or an original whose
folder or file name in the catalog is an absolute path, is not looked at: the import lists it as missing with a
warning and leaves the photo and its virtual copies out of the library. A path that is not a path on this computer
(a Windows drive or UNC path read on macOS or Linux) is not looked at either and stays catalogued as missing.

## Capture One catalogs

A Capture One catalog keeps its library in `<name>.cocatalogdb`, an SQLite database, which the same read-only
reader opens (`crates/engine/src/capture_one_catalog.rs`) into the same import model, so the destination window,
one undo step, reimport without duplicates and the result window are shared. Checked against catalogs written by
Capture One 16.9.0 on Windows (catalog format 160906).

- **Photos:** one per variant (`ZVARIANT`); the first variant of an image is the photo, later ones become virtual
  copies ("Variant 2", …). Originals inside the catalog are found relative to the folder holding the database,
  referenced ones by `ZPATHLOCATION` (`ZWINROOT` / `ZMACROOT` + `ZRELATIVEPATH`). Images in Capture One's trash
  are left out, with a warning, and so is any image whose original is a file type the app doesn't read (a Nikon
  `.NEFX`, say): it is named in the warnings and its file is never added.
- **Offline originals** (a drive that isn't connected, a file that is gone) take the capture time (`ZEXP_DATE`, the
  camera's clock, no offset), camera make, model and lens from the catalog; a file that is there keeps its own.
  Capture One's stand-ins for a file without a camera ("Tiff File", "Jpeg File") are no camera.
- **Ratings and colour tags** (`ZVARIANTMETADATA`): red, yellow, green, blue and purple become labels; orange and
  pink have no label here and come in without one, with a warning.
- **Keywords:** `ZCONTENT_KEYWORDS` lists each keyword as its full path with an order suffix
  (`Animals|Birds|Owls||4`) and the parents of an assigned keyword as entries of their own; the parents are
  dropped, as the path implies them.
- **IPTC fields** become an XMP packet merged like a sidecar.
- **Albums** keep their projects and groups (a group is a `VirtualFolderCollection`, as are Recent Imports and
  Recent Captures; the folders the catalog itself names are system folders and stay out, with the albums under
  them). Smart albums are not imported yet (a warning names each one).
- **Develop settings** (`crates/engine/src/capture_one_develop.rs`): a variant's layers in `ZVARIANTLAYER`, told
  apart by `Z_ENT` (`ZENTITIES` names them): the *adjustment* layer holds what the user set (NULL where untouched),
  *style* layers what applied styles set, the *default* layer the camera's defaults. A setting is the variant's when
  the adjustment layer (else a style) sets it to something other than the default. Imported so far, on top of the
  photo's camera defaults:
  - **Exposure** (`ZEXPOSURE`, EV).
  - **Orientation and straighten** (`ZROTATION`): the whole turn in degrees, clockwise as shown, including the
    camera's own orientation (the default layer's value, which the file's orientation already shows). The quarter
    turns beyond the camera's become the photo's orientation, the rest (±45°) the straighten angle.
  - **Crop** (`ZCROP` `cx;cy;w;h`): the centre in pixels of the stored image, the size in pixels of the turned frame;
    its aspect (`ZCROPASPECTRATIONAME`, `16x9`, `1x1 (Square)`; Original and Unconstrained stay free) turned to the
    crop's shape. **Flips** (`ZFLIP` 1 horizontal, 2 vertical).
  - **White balance** (`ZWHITEBALANCE`, gains on the raw channels): read as the app reads a raw's own gains (through
    the camera's matrices when it has them, else as a raw without colour tags), and applied as Capture One's change
    from as shot on the app's own as-shot white, so a white left as shot stays exactly as shot. A raw edited on the
    relative scale (no matrices for its camera) gets the same change through that scale, which on those cameras
    only approximates Capture One's (the result window says so, with the photo count); a rendered file's white
    balance is not imported.
  - The ICC profile choice is left out on purpose: photos get the app's camera colour.

  Every other setting a photo uses is named in the result window, by Capture One tool (Brightness, Levels, High
  Dynamic Range, LCC, Local adjustment layers, …, with how many photos use it), and per photo in the report's
  `unmapped`. A photo is dated at Capture One's last settings change (`ZSETTINGSMODIFICATIONDATE`).

  Checked against Capture One's own exports of the same raws (`<cli> compare`, aligned, 1000 px renders; 12 photos
  for geometry, 8 for white balance): straightened and turned photos (−18.9° to +2.4°, camera portraits at +2° and
  +13.9°, user quarter and half turns) aligned at 0.968–1.003 scale, 10 of 12 with no shift and 2 within 6 px of
  512. White balance is an approximation. With the app's standard colour (no camera profile installed) it lowered
  ΔE00 to the export from 14.2 to 7.3 on a raw with a Kelvin scale (a camera with measured matrices), and on raws
  edited on the relative scale from 6.9–10.6 to 5.1–7.6 (a camera without an entry yet, 4 photos) and from
  16.3–21.6 to 11.2–20.1 (another, 3 photos). With a camera profile fitted to Capture One's colours, the same 4
  photos went from 5.6–10.4 to 1.9–5.0. The rest of the difference includes settings not imported yet and the
  camera colour. A camera that gets an entry in the camera database moves to the Kelvin scale; the import reads
  the same database, so it follows.
- The import's activity row, messages, result window, destination window and undo step say Capture One.

## Historical local validation

Validated in a local Linux environment after upstream reconciliation: 47 catalog-focused tests, nine pipeline spot tests, the exact sparse-catalog regression, and all eight `cargo xtask ci` stages passed. The full gate recorded 3,166 passing test executions, zero failures, and 28 ignored tests. An original synthetic sparse catalog with a required image-table page beyond 1 GiB also passed headless application import and selection; its expected missing-original placeholder retained its five-star rating. This snapshot check does not establish native-window coverage or dense-catalog performance.
