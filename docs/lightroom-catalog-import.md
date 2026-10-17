# Lightroom Classic catalog import

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

File → Import Lightroom Catalog… opens `.lrcat` directly. Lightroom, Python, DNG conversion
and a C SQLite runtime are not required. The source database is read-only, including committed
`-wal` pages; a catalog that changes during the read is rejected when the change is
observable. Close Lightroom and every other catalog writer before inspection or import:
the metadata, header and companion checks are best-effort change detection, not a
transactional SQLite snapshot. The app owns subsequent edits.

File → Open Library… given a Lightroom catalog folder (or an `.lrdata` / `.lrcat-data` folder, or the `.lrcat`)
doesn't make a library of the app there: it offers this import instead, with the catalog found
(`crates/engine/src/lightroom_folder.rs`, issue #891). `<cli> --library` refuses those places too.

Equivalent commands:

```text
<cli> run --library "<app> Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
<cli> run --library "<app> Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

Import adds records to the currently open library; it does not create a separate
folder named after the Lightroom catalog. The desktop result stays open until dismissed and
shows the destination library, the number of records added, missing originals and warnings.
**Show in Library** opens All Photos, clears filters and selects an imported record. Errors stay
readable in the same result window. If another dialog is open, the result waits until that dialog closes. The result
is also available through `library.importLightroom {"status":true}` during that session.

Original files are referenced in place. Ratings, picks/rejects, color labels, XMP metadata,
hierarchical keywords, regular collections/collection sets, smart collections, folder stacks,
virtual copies and mapped develop settings are imported. Missing originals remain catalogued for relinking, count as added records and are reported
separately from failures reading files that are present. Known-missing originals are not probed
again. Previously browsed Local records join All Photos even when their originals are offline;
their promotion is counted once and undone together with the catalog import.
Virtual copies count as added records too; reimport reports zero additions when all
records already exist. Existing
edits are preserved by default; `updateExisting=true` explicitly replaces them. Persistent import
identity prevents reimport from duplicating virtual copies and collections. One undo step reverses
catalog changes; any saved recovery archives remain available.

Records marked `VIDEO` in the source catalog appear in the **Videos** type filter, including
offline originals. Reimport also repairs the classification of videos imported as images by
older builds, preserving their IDs, edits and album/stack membership. This does not add video
playback, frame previews or video discovery through folder synchronization.

Before changing records, the importer attempts to save source image records, decoded XMP,
verbatim develop settings, history, snapshots and collection content to compressed
`Interop/lightroom-import-N.lca` recovery archives in the app's library. Archiving is
best-effort: a size limit or write failure produces a warning and does not prevent photo import.
Unchanged source data reuses its existing archive. Opaque SQL BLOBs are skipped. Each archive is
capped at 32 MiB (uncompressed and on disk), with at most eight archives and 128 MiB total
retained. Oldest managed archives are retired; legacy `.json` archives remain untouched.
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
Develop settings that can't be read fail the import with a clear error (otherwise every photo
would arrive unedited). Optional history, snapshot, metadata, keyword and collection tables that
can't be read are skipped with warnings, unless the read failed because the catalog file itself
failed or changed, which fails the import; required photo tables still report errors. Unrelated
unsupported schemas are ignored. A needed
WITHOUT ROWID or virtual table produces an explicit error, handled as a warning when optional.

Develop settings reuse the existing XMP/preset mapper. Supported sliders, curves and supported
mask structures remain editable, but this is approximate rendering: camera profiles, Adobe AI
models, some masking/retouch fields and process-version algorithms are not reproduced. Unmapped
fields are reported. Lightroom's `ProcessVersion` is not mapped: newly imported photos are on
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

Lightroom's develop history (`Adobe_libraryImageDevelopHistoryStep`) becomes the photo's History,
oldest first, each step named as in Lightroom and mapped like the current settings (so a step shows
what the app can reproduce of it). When the newest step differs from the photo's settings, a final
"Lightroom catalog" entry holds them, so History ends where the photo is. Steps that can't be read
are skipped with a warning; at most the newest 200 are kept. The report counts them as
`historySteps`. A photo whose edits are preserved (reimport without `updateExisting`) keeps its
History. Lightroom snapshots (`Adobe_libraryImageDevelopSnapshot`) become the photo's Versions, in
Lightroom's order and under their names, mapped the same way; unreadable ones are skipped with a
warning and the report counts them as `snapshots`. Of the 113 flat `crs:` develop fields in Adobe's
public XMP documentation, 105 map (test `develop_field_coverage_over_public_crs_fields`); the other
eight have no equivalent and are reported: `AutoWhiteVersion`, `LensProfileSetup`, `UprightPreview`,
`UprightCenterMode`, `UprightFocalLength35mm`, the pre-2012 `Defringe`, `HDRMaxValue` and `SDRBlend`.
A Custom white balance on a raw the app develops relative to its as-shot look (ARW, NEF, RW2,
RAF, CR3, CR2, PEF, SRW; Lightroom names them all `RAW`) can't be taken as Kelvin: the catalog keeps no
as-shot white for Custom photos, so the photo stays As Shot and `Temperature, Tint` are reported
(issue #510; a DNG keeps the Kelvin value, and an XMP packet with `crs:AsShotTemperature` is
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
