# Lightroom Classic catalog import

File → Import Lightroom Catalog… opens `.lrcat` directly. Lightroom, Python, DNG conversion
and a C SQLite runtime are not required. The source database is read-only, including committed
`-wal` pages; a catalog that changes during the read is rejected. LightCraft owns subsequent edits.

File → Open Library… given a Lightroom catalog folder (or an `.lrdata` / `.lrcat-data` folder, or the `.lrcat`)
doesn't make a LightCraft library there: it offers this import instead, with the catalog found
(`crates/engine/src/lightroom_folder.rs`, issue #891). `lightcraft-cli --library` refuses those places too.

Equivalent commands:

```text
lightcraft-cli run --library "LightCraft Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
lightcraft-cli run --library "LightCraft Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

Import adds records to the currently open LightCraft library; it does not create a separate
folder named after the Lightroom catalog. The desktop result stays open until dismissed and
shows the destination library, the number of records added, missing originals and warnings.
**Show in Library** opens All Photos, clears filters and selects an imported record. Errors stay
readable in the same result window. If another dialog is open, the result waits until that dialog closes. The result
is also available through `library.importLightroom {"status":true}` during that session.

Original files are referenced in place. Ratings, picks/rejects, color labels, XMP metadata,
hierarchical keywords, regular collections/collection sets, virtual copies and mapped develop
settings are imported. Missing originals remain catalogued for relinking, count as added records and are reported
separately from failures reading files that are present. Known-missing originals are not probed
again. Previously browsed Local records join All Photos even when their originals are offline;
their promotion is counted once and undone together with the catalog import.
Virtual copies count as added records too; reimport reports zero additions when all
records already exist. Existing LightCraft
edits are preserved by default; `updateExisting=true` explicitly replaces them. Persistent import
identity prevents reimport from duplicating virtual copies and collections. One undo step reverses
catalog changes; any saved recovery archives remain available.

Before changing records, the importer attempts to save source image records, decoded XMP,
verbatim develop settings, history, snapshots and collection content to compressed
`Interop/lightroom-import-N.lca` recovery archives in the LightCraft library. Archiving is
best-effort: a size limit or write failure produces a warning and does not prevent photo import.
Unchanged source data reuses its existing archive. Opaque SQL BLOBs are skipped. Each archive is
capped at 32 MiB (uncompressed and on disk), with at most eight archives and 128 MiB total
retained. Oldest managed archives are retired; legacy `.json` archives remain untouched.
`Interop/lightroom-index.json` records imported identities. Source catalogs and originals are
never overwritten.

Each native `.lrcat` database and WAL file is independently limited to 1 GiB
(1,073,741,824 bytes). This bounds the current whole-file reader; referenced photo sizes and
photo counts are not part of that limit. XMP packets are capped at 16 MiB and each table at one million rows.
The in-tree, read-only Rust SQLite B-tree reader follows SQLite’s public file-format
specification; it introduces no dependency or C runtime. Table payloads are capped at 256 MiB.
Optional history, keyword and collection tables that exceed limits are skipped with warnings;
required photo tables still report errors. Unrelated unsupported schemas are ignored. A needed
WITHOUT ROWID or virtual table produces an explicit error, handled as a warning when optional.

Develop settings reuse the existing XMP/preset mapper. Supported sliders, curves and supported
mask structures remain editable, but this is approximate rendering: camera profiles, Adobe AI
models, some masking/retouch fields and process-version algorithms are not reproduced. Unmapped
fields are reported. Lightroom's `ProcessVersion` is not mapped: newly imported photos are on
LightCraft's latest rendering process, and photos already in the library keep theirs
([process-versions.md](process-versions.md)). When a recovery archive is saved, it retains source settings/history/snapshots
and original smart-collection rules. Smart collections become regular albums with current
membership. Archived history and snapshots are source data, not native LightCraft history yet.
A Custom white balance on a raw LightCraft develops relative to its as-shot look (ARW, NEF, RW2,
RAF, CR3, CR2, PEF, SRW; Lightroom names them all `RAW`) can't be taken as Kelvin: the catalog keeps no
as-shot white for Custom photos, so the photo stays As Shot and `Temperature, Tint` are reported
(issue #510; a DNG keeps the Kelvin value, and an XMP packet with `crs:AsShotTemperature` is
shifted by the same mired difference, see [xmp-interop.md](xmp-interop.md)). A raw whose camera has
measured colour matrices (`lightcraft_raw::spectral`: ILCE-7M3, D850, EOS 5D Mark III…), or whose white-balance
presets give a Kelvin scale (`lightcraft_raw::kelvin_presets`: ILCA-99M2, ILCA-77M2…), edits in Kelvin
and keeps the Kelvin value too (issue #730) — when its file was found and probed, since the camera is
read from the file, not from the Lightroom catalog; a missing file is treated as a relative-scale raw
until Reload reads it.
Lightroom's `-999999` deferred-adjustment sentinel is omitted from both catalog and XMP mappings;
it is reported, included in any saved archive, and never clamped into a real slider value. Deferred Adobe Auto Tone
is not evaluated by the importer; LightCraft's Auto control remains available after migration.

New photo records are moved into staged operations; only masters whose virtual copies are in the
same batch retain a second owned record while the batch commits. Applying catalog operations
collects affected source-cache IDs before consuming the operations, avoiding another full operation
copy. Full-strength develop patches reuse their JSON without serializing an unused copy of the
current settings.

Native desktop inspection/import runs on a cancellable background worker. The worker reads the
catalog, then prepares its photos in batches of 256 (masters ahead of their virtual copies): it
probes their originals and parses their XMP and maps their develop settings. Each batch is added on
the owner thread as it arrives, so photos appear while the rest is read; the progress counts each
original as it is read. Cancelling (✕ in the activity stack) keeps the photos added so far, without
collections, as one undo step; importing the catalog again continues where it stopped (their
identities are in the import index) and then adds the collections. Each batch is saved as it is added, so the
same holds after a crash or a forced quit: a master is found again by its path, a virtual copy
through the import index, which is written after each batch of virtual copies (they come after
every master, in the last batches). Late batches from a closed or
switched library are rejected, including reopening the same path. Ordinary edits during the import
remain intact (and, as with any batched import, fall into its undo step). The archive index is
written on a worker. CLI commands run the same pipeline synchronously; browser builds reject native
catalog import.

Originals are described from their headers: for DNG, CR3, CR2, ARW, RAF, RW2 and SRW files the
pixel data, nearly all of a raw file's bytes, is never read (`lightcraft_raw::sparse`; on the
raw.pixls.us corpus a CR3 or DNG is described from 2–3 % of its bytes, the result identical to
reading all of it). A raw LightCraft doesn't decode yet, shown from its embedded preview, is read
with that preview: a few megabytes of its tens. NEF, ORF, PEF and ARW6 files (their header probe looks at samples), JPEG and other rendered files and
files under 4 MiB are read whole, as a normal import reads them. On a 38,500-photo catalog with
1.1 TB of originals on a USB SSD (mostly EOS R5 Mark II CR3s, plus DNG, ORF and JPEG files) the
import took about 2 minutes instead of about half an hour. A raw described from its headers gets no content
hash. A later import of a copy of it from elsewhere still finds it by hashing it then (when at most 16
photos without a hash share the copy's size), and that hash is kept. Until a photo has a hash, Find
Missing Photos matches its file by name and size only, and Reload from Disk takes the same file as
unchanged (its size, dimensions and preview state decide: no undo step, previews kept). A
background pass filling in the missing hashes would lift both limits. Sidecars are looked for in a listing of each folder,
read once, rather than by asking the drive for each possible name (slow for files that aren't there
on exFAT and network drives).

Archive version 1 begins with the 12 bytes `LC-LRARCH\0\x01Z`, followed by zlib-compressed JSON.
Recovery readers must cap decompression at 32 MiB. JSON retains source identities, photo paths,
settings, decoded XMP, keywords, history, snapshots, collection definitions and membership.

The importer uses shared Rust code on macOS, Windows and Linux. When a root folder's recorded
path isn't a folder on this computer (a catalog copied from another machine), the importer uses
the path relative to the catalog that Lightroom records for root folders on the catalog's volume
(`relativePathFromCatalog`), when that is a folder, and reports the remapping as a warning. Copying
a project folder with its catalog inside it therefore keeps its originals linked. Other moved
originals stay missing; use LightCraft's missing-photo relinking tools to update their paths.

Original and root folder paths are resolved with the catalog as the document and are looked at
only when they are on this computer or on the network share the catalog itself is on
(`crates/engine/src/doc_path.rs`). An original on another computer (`\\server\share\…`,
`//server/…`, `file://server/…`), a device path, or one whose folder or file name in the catalog is
an absolute path is not looked at: the import lists it as missing with a warning and leaves the
photo and its virtual copies out of the library. To import originals on a share, open the catalog
from that share. A path that is not a path on this computer (a Windows drive path read on macOS or
Linux) is not looked at either and stays catalogued as missing.
