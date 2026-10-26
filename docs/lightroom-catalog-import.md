# Lightroom Classic catalog import

File → Import Lightroom Catalog… opens `.lrcat` directly. Lightroom, Python, DNG conversion
and a C SQLite runtime are not required. The source database is read-only, including committed
`-wal` pages; a catalog that changes during the read is rejected. LightCraft owns subsequent edits.

File → Open Library… given a Lightroom catalog folder (or an `.lrdata` / `.lrcat-data` folder, or the `.lrcat`)
doesn't make a LightCraft library there: it offers this import instead, with the catalog found
(`crates/engine/src/lightroom_folder.rs`, issue #891), and then asks where the catalog goes as described next
(unless the question is turned off, below).
`lightcraft-cli --library` refuses those places too.

**Where the catalog goes.** Before the import starts, the desktop app asks where the catalog goes (issue #2672),
unless Settings says to always merge (*Turning the question off*, below):

- **Merge into the open library "<name>" (N photos)**: the catalog's photos, collections and edits join the ones
  already in the library open now (the library's name is its folder's name).
- **Create a new library**: named after the catalog file (`Travel.lrcat` → "Travel", or "Travel 2" when a
  "Travel" folder is already there) and placed next to the open library (in its parent folder); both can be
  changed (Choose… picks the folder). LightCraft creates and opens it as File → New Library… does (the folder
  must be new or empty; it is refused while another import runs), then imports the catalog into it. The library
  left behind stays in File → Open Recent Library.
- **Cancel** does nothing.

The window opens on *Merge* when the open library has no photos (there is nothing to merge with) and on *Create a
new library* when it has some.

**Turning the question off.** Settings → General → Library → *When importing a Lightroom catalog*: *Ask where to
import it* (the default) or *Always merge into the open library* (how imports always worked before). The window's
*Don't ask again (always merge into the open library)* checkbox, offered with *Merge*, picks the second; the window
says where to turn the question back on. The choice is an app setting (`ui.json`, `settings.lightroomImport`:
`"ask"` or `"alwaysMerge"`); a settings file without it asks.

A session with no library on disk (in memory, e.g. after Continue Without Saving) has nothing to put a new library
next to: the catalog merges into it without asking, as before.

**Scripts and agents.** The app command `file.importLightroom {path?, into?, name?, location?}` (control channel,
`lightcraft-cli mcp --connect`) chooses without the window: `into: "open"` merges, `into: "new"` creates the library
`name` (default: the catalog's name) in the folder `location` (default: the open library's parent folder; a full
path), opens it and imports. Without `into` it shows the window (and returns `{asking: true, path, into}`, `into`
being the choice the window opens with) or merges when the setting says so. A catalog path that doesn't exist is
refused before a new library is made. The engine command `library.importLightroom` always imports into the library
open now.

Equivalent commands:

```text
lightcraft-cli run --library "LightCraft Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
lightcraft-cli run --library "LightCraft Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

The import itself (`library.importLightroom`, `lightcraft-cli run`) adds records to the currently open LightCraft
library; only the desktop window above can make a new library for it first. The desktop result stays open until dismissed and
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
An image whose file, folder or root folder record is gone from the catalog, or whose original path
is empty (Lightroom leaves these behind and ignores them), is skipped together with its virtual
copies instead of failing the whole import. The result lists them as data (`skipped`: each
image id with its reason, `missingFile`, `missingFolder`, `missingRoot`, `incompletePath` or
`masterSkipped`), also in `library.inspectLightroom`, and the desktop result window says how many in the UI language.
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

## Limits

Native import reads referenced database pages on demand (at most 64 KiB per page), rather than
loading the whole `.lrcat`, and streams every table: each row is decoded, the fields the import
maps are taken from it, and the rest is dropped before the next row (issue #1234). Memory follows
what the import keeps (paths, image records, develop settings text, decoded XMP, keywords,
collections), not the size of the tables, so there is no per-table size or row cap any more.
What is bounded, and why:

- **Files**: database files up to 16 GiB and 16,777,216 pages; the WAL up to 4 GiB (it is read
  into memory to validate its checksums).
- **Records**: one record's payload up to 64 MiB, its overflow chain up to 1,048,576 pages with
  no repeated page; XMP packets up to 16 MiB decoded.
- **B-tree structure**: at most 64 levels deep, no page visited twice (cycles and shared pages are
  errors), cell pointers inside the page. Two whole-table sanity bounds replace the old 256 MiB /
  one million row caps: a table's rows can't carry more payload than the database has bytes, and
  can't number more than the database's bytes divided by five (the smallest a row can be). A
  well-formed table never reaches either; a hostile file whose cells overlap or share overflow
  chains stops there instead of walking without end.
- **History and snapshots** go to the recovery archive only (not to native history). They are
  streamed and kept up to 256 MiB together (estimated in memory); the archive holds at most
  32 MiB compressed and settings text compresses about 8:1, so more could not be archived anyway.
  Steps past the budget are counted in a warning; imported edits are not affected.

**Develop settings are never dropped quietly.** If the catalog has an `Adobe_imageDevelopSettings`
table that can't be read (a damaged catalog), the import still brings in the photos, ratings,
keywords and collections, keeps the settings read before the damaged part, and reports how many
photos arrived without their edits: `editsNotImported` in the inspect and import results, a first
warning that says so, and a longer-lasting import summary in the app that leads with it. Failing
the whole import instead would leave a user with a damaged catalog with nothing; after repairing
the catalog in Lightroom Classic (File > Optimize Catalog), importing it again with
`updateExisting=true` brings the edits in (without it, photos whose XMP already gave them an edit
keep it).

The open file and source path are checked again after all tables are read. Close Lightroom before
importing: metadata checks detect ordinary concurrent changes, but are not SQLite snapshot isolation.

The in-tree, read-only Rust SQLite B-tree reader follows SQLite's public file-format
specification; it introduces no dependency or C runtime.
Optional history, keyword and collection tables that can't be read are skipped with warnings
(rows read before the failure are kept); required photo tables still report errors. Unrelated
unsupported schemas are ignored. A needed WITHOUT ROWID or virtual table produces an explicit
error, handled as a warning when optional.

## How edits and folders map

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
originals stay missing; use LightCraft's missing-photo relinking tools to update their paths.

Original and root folder paths are resolved with the catalog as the document and are looked at
only when they are on this computer or on the network share the catalog itself is on
(`crates/engine/src/doc_path.rs`). An original on another computer (`\\server\share\…`,
`//server/…`, `file://server/…`), a device path, or one whose folder or file name in the catalog is
an absolute path is not looked at: the import lists it as missing with a warning and leaves the
photo and its virtual copies out of the library. To import originals on a share, open the catalog
from that share. A path that is not a path on this computer (a Windows drive path read on macOS or
Linux) is not looked at either and stays catalogued as missing.

## Historical local validation

Validated in a local Linux environment after upstream reconciliation: 47 catalog-focused tests, nine pipeline spot tests, the exact sparse-catalog regression, and all eight `cargo xtask ci` stages passed. The full gate recorded 3,166 passing test executions, zero failures, and 28 ignored tests. An original synthetic sparse catalog with a required image-table page beyond 1 GiB also passed headless application import and selection; its expected missing-original placeholder retained its five-star rating. This snapshot check does not establish native-window coverage or dense-catalog performance.
