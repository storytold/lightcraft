# Lightroom Classic catalog import

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

File → Import Lightroom Catalog… opens `.lrcat` directly. Lightroom, Python, DNG conversion
and a C SQLite runtime are not required. The source database is read-only, including committed
`-wal` pages; a catalog that changes during the read is rejected. The app owns subsequent edits.

Equivalent commands:

```text
<cli> run --library "<app> Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
<cli> run --library "<app> Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

Original files are referenced in place. Ratings, picks/rejects, color labels, XMP metadata,
hierarchical keywords, regular collections/collection sets, virtual copies and mapped develop
settings are imported. Missing originals remain catalogued for relinking. Existing
edits are preserved by default; `updateExisting=true` explicitly replaces them. Persistent import
identity prevents reimport from duplicating virtual copies and collections. One undo step reverses
catalog changes; any saved recovery archives remain available.

Before changing records, the importer attempts to save source image records, decoded XMP,
verbatim develop settings, history, snapshots and collection content to compressed
`Interop/lightroom-import-N.lca` recovery archives in the app's library. Archiving is
best-effort: a size limit or write failure produces a warning and does not prevent photo import.
Unchanged source data reuses its existing archive. Opaque SQL BLOBs are skipped. Each archive is
capped at 32 MiB (uncompressed and on disk), with at most eight archives and 128 MiB total
retained. Oldest managed archives are retired; legacy `.json` archives remain untouched.
`Interop/lightroom-index.json` records imported identities. Source catalogs and originals are
never overwritten.

Native import is bounded to a 1 GiB database/WAL, 16 MiB XMP packets and one million rows per
table. The in-tree, read-only Rust SQLite B-tree reader follows SQLite’s public file-format
specification; it introduces no dependency or C runtime. Table payloads are capped at 256 MiB.
Optional history, keyword and collection tables that exceed limits are skipped with warnings;
required photo tables still report errors. Unrelated unsupported schemas are ignored. A needed
WITHOUT ROWID or virtual table produces an explicit error, handled as a warning when optional.

Develop settings reuse the existing XMP/preset mapper. Supported sliders, curves and supported
mask structures remain editable, but this is approximate rendering: camera profiles, Adobe AI
models, some masking/retouch fields and process-version algorithms are not reproduced. Unmapped
fields are reported. Lightroom's `ProcessVersion` is not mapped: newly imported photos are on
the app's latest rendering process, and photos already in the library keep theirs
([process-versions.md](process-versions.md)). When a recovery archive is saved, it retains source settings/history/snapshots
and original smart-collection rules. Smart collections become regular albums with current
membership. Archived history and snapshots are source data, not native history yet.
A Custom white balance on a raw the app develops relative to its as-shot look (ARW, NEF, RW2,
RAF, CR3, CR2, PEF, SRW; Lightroom names them all `RAW`) can't be taken as Kelvin: the catalog keeps no
as-shot white for Custom photos, so the photo stays As Shot and `Temperature, Tint` are reported
(issue #510; a DNG keeps the Kelvin value, and an XMP packet with `crs:AsShotTemperature` is
shifted by the same mired difference, see [xmp-interop.md](xmp-interop.md)).
Lightroom's `-999999` deferred-adjustment sentinel is omitted from both catalog and XMP mappings;
it is reported, included in any saved archive, and never clamped into a real slider value. Deferred Adobe Auto Tone
is not evaluated by the importer; the app's Auto control remains available after migration.

Native desktop inspection/import runs on a cancellable background worker with progress. Only
prepared catalog operations commit on the owner thread; archive index writing stays on a worker.
Late results from a closed or switched library are rejected, including reopening the same path.
Ordinary edits during preparation remain intact. CLI commands complete synchronously using the
same preparation/commit pipeline; browser builds reject native catalog import.

Archive version 1 begins with the 12 bytes `LC-LRARCH\0\x01Z`, followed by zlib-compressed JSON.
Recovery readers must cap decompression at 32 MiB. JSON retains source identities, photo paths,
settings, decoded XMP, keywords, history, snapshots, collection definitions and membership.

The importer uses shared Rust code on macOS, Windows and Linux. If originals move between
computers or volumes, use the app's missing-photo relinking tools to update their paths.
