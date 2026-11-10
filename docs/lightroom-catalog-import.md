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
next to: the catalog merges into it without asking when it has no network sources. Network sources
always get a checkbox review before import, including with Always merge.

**Scripts and agents.** The app command `file.importLightroom {path?, into?, name?, location?, networkShares?}` (control channel,
`lightcraft-cli mcp --connect`) chooses without the window: `into: "open"` merges, `into: "new"` creates the library
`name` (default: the catalog's name) in the folder `location` (default: the open library's parent folder; a full
path), opens it and imports. Without `into` it first returns `{running: true, kind: "sources", path}` while discovering sources.
Poll `library.importLightroom {status: true}` (or `{wait: true}`) for `{asking: true, path, into}` when
the window opens, or for the automatic merge result when no network sources require review. A catalog path that doesn't exist is
refused before a new library is made. The engine command `library.importLightroom` always imports into the library
open now.

A Lightroom Classic catalog backup is imported the same way, as the `.zip` Lightroom writes
(`Backups/YYYY-MM-DD HHMM/<name>.zip`; see *Catalog backups* below).

Equivalent commands:

```text
lightcraft-cli run --library "LightCraft Library" library.inspectLightroom path="Lightroom Catalog.lrcat"
lightcraft-cli run --library "LightCraft Library" library.importLightroom path="Lightroom Catalog.lrcat"
```

The import itself (`library.importLightroom`, `lightcraft-cli run`) adds records to the currently open LightCraft
library; only the desktop window above can make a new library for it first. The desktop result stays open until dismissed and
shows the destination library, the number of records added, missing originals and warnings.
**Show in Library** opens All Photos, clears filters and selects an imported record (not one left in Recently
Deleted; the button is left out when every record is there). Errors stay
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

Records marked `VIDEO` in the source catalog appear in the **Videos** type filter, including
offline originals. Reimport also repairs the classification of videos imported as images by
older builds, preserving their IDs, edits and album/stack membership. This does not add video
playback, frame previews or video discovery through folder synchronization.

Before changing records, the importer attempts to save source image records, decoded XMP,
verbatim develop settings, history, snapshots and collection content to compressed
`Interop/lightroom-import-N.lca` recovery archives in the LightCraft library. Archiving is
best-effort: a size limit or write failure produces a warning and does not prevent photo import.
Unchanged source data reuses its existing archive. Opaque SQL BLOBs are skipped. Each archive is
capped at 32 MiB (uncompressed and on disk), with at most eight archives and 128 MiB total
retained. Oldest managed archives are retired; legacy `.json` archives remain untouched.
`Interop/lightroom-index.json` records imported identities. Source catalogs and originals are
never overwritten.

Native import reads referenced database pages on demand (at most 64 KiB per page), rather than
loading the whole `.lrcat`. Database files are capped at 16 GiB and 16,777,216 pages; the WAL
remains capped at 1 GiB. Tables still have a 256 MiB encoded-payload budget and one million rows,
individual records are capped at 64 MiB, and XMP packets at 16 MiB. These limits can still reject
large catalogs with dense tables; file size alone no longer determines the memory allocation.
The open file and source path are checked again after all tables are read. Close Lightroom before
importing: metadata checks detect ordinary concurrent changes, but are not SQLite snapshot isolation.

The in-tree, read-only Rust SQLite B-tree reader follows SQLite's public file-format
specification; it introduces no dependency or C runtime.
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
membership, and so do saved prints, books, slideshows and web galleries (with the photos they hold). A LightCraft
album nests only inside a folder (a Lightroom collection set). A collection or creation Lightroom keeps elsewhere,
such as a print saved inside a plain collection, or one whose parent is system-only or gone from the catalog, joins the
nearest collection set above it, or the top level, and the result names it under warnings (issues #2165, #2532).
Quick Collection and unsaved creations are system-only and not imported; publish collections live in tables the
importer doesn't read. A collection the catalog lists twice imports once; a different collection with the id of an
earlier one is left out with a warning naming both, and the photos listed under that id go to the first. Archived
history and snapshots are source data, not native LightCraft history yet.
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
by default only when they are on this computer or on the network share the catalog itself is on
(`crates/engine/src/doc_path.rs`). An original on another computer (`\\server\share\…`,
`//server/…`, `file://server/…`), a device path, or one whose folder or file name in the catalog is
an absolute path is not looked at: the import lists it as missing with a warning and leaves the
photo and its virtual copies out of the library. The implicit catalog-share grant applies only when
the command omits `networkShares`. In the interactive import, explicitly select each share whose
originals should be imported, including the catalog's own share; an empty selection grants none.
A path that is not a path on this computer (a Windows drive path read on macOS or
Linux) is not looked at either and stays catalogued as missing.

**Local catalog, NAS originals.** After choosing the `.lrcat` or a Lightroom backup `.zip`, LightCraft reads its recorded root
folders in the background and lists **Network sources in this catalog** in the import window.
Each distinct UNC server/share (for example `\\nas\photos` or `\\archive\old-photos`) gets a checkbox;
folders and ASCII case/separator variants on the same share are grouped, using the same comparison
as path authorization. Non-ASCII spellings remain distinct rather than broadening a grant with
Unicode case folding. An explicitly unchecked UNC root cannot be imported through its
catalog-relative fallback, even when Windows treats its spelling as an alias of a selected share.
No folder picker is needed. Long lists render only visible rows; scrolling preserves selections.
Select the sources to import, then confirm the import. All discovered sources start unchecked;
unchecked network sources are not imported, including the catalog's own share. The catalog,
LightCraft library and original photos stay at their existing locations.

Example views using synthetic catalogs: [local sources only](images/lightcraft-import-local-sources.png)
and [two network sources with one selected](images/lightcraft-import-network-sources.png).

Discovery reads only the catalog and its committed WAL. It does not contact servers to probe
originals, but a catalog on a share requires contacting that catalog's server to read the catalog
and WAL. For a backup, discovery temporarily extracts its catalog using the same size, CRC and
cancellation checks as import, then removes the extraction before asking for consent. Import
extracts it again after confirmation. Relative sources refer to the original catalog location,
not the temporary folder. The network review also appears with **Always merge** or an in-memory library;
that preference still skips the destination question for catalogs without network sources. Cancel
grants nothing. Selections apply only to this import and are not remembered on the next one.

The same explicit option is available to `file.importLightroom`, `library.importLightroom` and
`library.inspectLightroom`: `networkShares: ["\\\\nas\\photos"]` in JSON. At most 32 UNC folder paths
are accepted, validated before catalog reads or library creation. The resolver checks the full server
and share names, not a path prefix: selecting `\\nas\photos` does not permit `\\nas\photos-private` or
another server. Device paths, non-file URLs, invalid paths and remote `file://host/…` URLs remain refused.
UNC originals work on Windows; on other platforms, an explicitly selected Windows UNC path stays
as an offline record without contacting its server. Relink it to the NAS's locally mounted path. An allowed but
offline original stays catalogued as missing for relinking. An explicit empty array allows no UNC shares,
including the catalog's own share. Without the option, the original restriction remains: a catalog must not make the app contact a server the user has not chosen.

## Catalog backups

Lightroom Classic backs a catalog up as `<catalog folder>/Backups/YYYY-MM-DD HHMM/<name>.zip`: one `.lrcat` (plus,
since Classic 12, a small `<name>.lrcat-data/` folder the import does not need). Someone who has removed Lightroom
and its catalog may have nothing else, so `library.importLightroom` / `library.inspectLightroom` and File ▸ Import
Lightroom Catalog… take the zip directly (issue #2592, `crates/engine/src/lightroom_backup.rs`):

- The `.lrcat` entry is inflated by streaming (stored or deflated, ZIP64 included) into a private folder,
  `Interop/lc-lrbackup-<pid>-<time>/` in the open library (the system's temporary folder when the library is not
  on disk), checked against the zip's size and CRC-32, read like any catalog, and removed after the read, on an
  error and on cancel alike. A folder left by a crash is removed by the next backup import once it is a day old.
  The zip is only read. A zip with no `.lrcat`, more than one, a damaged or cut-short entry, or a catalog over
  16 GiB is refused with a message. A known space shortage is refused before extraction; later write errors are reported.
- The import's `source` is the zip. Paths the catalog records relative to itself (`relativePathFromCatalog`) are
  taken from where the catalog was: two folders above a dated backup folder, else next to the zip.
- Open Library given `Backups/` without its catalog, or a dated folder in it, makes no library there and offers the
  import: of the dated folder's zip, of the only backup in `Backups/`, or, when `Backups/` holds several, of the zip
  the person picks (the file dialog asks).
- Collections are matched across imports by the catalog's own id (`Adobe_storeProviderID` in
  `Adobe_variablesTable`), which every backup of the catalog keeps, so importing a backup of a catalog already
  imported adds no collection twice. Collections imported by earlier releases, keyed on the catalog's path, are
  still found by that key. Each key records the id it was written for, so two catalogs with different ids that sat
  at the same path keep their own albums. A catalog without `Adobe_storeProviderID` (every Lightroom Classic
  catalog checked, from Lightroom 7 to Classic 13, has one; only a damaged or foreign file lacks it) is known by its
  path alone. If that path is already associated with a known catalog ID, the importer refuses the ambiguous
  match rather than silently combining different catalogs. Unowned legacy path keys can still match. Photos are matched as
  before, by Lightroom's image id and path: a backup whose photos were at another place (an older backup from
  before a move) adds them as other records.

Checked on 2026-10-22 on a real catalog (2,740 photos, 96 collections) and its four backups from Lightroom 7
(2018) to Classic 13 (2023), 88 MB zips holding 138 MB catalogs: each zip imports the same photos and collections
as its unpacked catalog in about 2.4 s on an Apple M4, leaves no file behind and is not changed.

## Historical local validation

Validated in a local Linux environment after upstream reconciliation: 47 catalog-focused tests, nine pipeline spot tests, the exact sparse-catalog regression, and all eight `cargo xtask ci` stages passed. The full gate recorded 3,166 passing test executions, zero failures, and 28 ignored tests. An original synthetic sparse catalog with a required image-table page beyond 1 GiB also passed headless application import and selection; its expected missing-original placeholder retained its five-star rating. This snapshot check does not establish native-window coverage or dense-catalog performance.

### Source-review limits and filesystem redirects

Source discovery lists all distinct recorded shares within the SQLite reader's table limits;
the limit of 32 selected shares applies to the import options, not discovery. A catalog with
more shares can still import local sources alone or a selected subset. Cancelling discovery is
checked between WAL chunks, root-table B-tree/overflow pages and rows; an OS read already in
flight must return before cancellation can be observed. If another dialog was opened during
discovery, its unconfirmed input is preserved and the import asks the user to retry.

On Windows, guarded reads of UNC originals and sidecars open relative to directory handles, one
validated component at a time. Every component is opened without following its final reparse
point; metadata is read from that handle and a reparse point is refused before proceeding.
Ancestors stay open during the walk, and metadata, probing and file bytes use the opened
object rather than reopening a previously checked path. Native remote-symlink errors are
refused; there is no fallback that retries the path through Win32 or evaluates a link target.
Preview and native byte-loading paths use the same entry point after import. Region-orientation
rereads and Synchronize Folder sidecar reads also revalidate their opens; a prior lookup or cached
folder listing does not authorize a later read. Synchronize Folder reads sidecars through one
checked handle, rejects an already oversized file before buffering it, and rejects content beyond 16 MiB while reading, even if a writer grows the file
after metadata was inspected. Image/preview loaders retain one original handle
for metadata and bytes, including whole-file preview fallback. Source discovery itself still
performs no original-file I/O.

This protection covers the import, preview, region-orientation and Synchronize Folder read
paths described above. Saving a sidecar is a separate operation: its read of an existing
sidecar uses `std::fs::read`, not the checked-handle path. Sidecar-save reads and writes are
not covered by this import guard.

The grant is to the **logical SMB share**, not to a physical directory on the server or a fixed
IP address. The selected server and Windows' SMB transport remain trusted to serve that share:
server-side mounts, storage layout and transport/DFS referrals are not controlled by the
catalog importer. The importer does not evaluate catalog-driven links to another share.
A server that itself serves different data under an authorized name is outside this boundary,
just as it is for an ordinary file read. This distinction also applies to metadata-only checks.

The Windows implementation uses the safe, handle-relative API of `cap-primitives` (pinned to
4.0.3), with `FILE_FLAG_OPEN_REPARSE_POINT` and no delete sharing during traversal. No unsafe
code or operating-system policy changes are added to LightCraft. Tests use synthetic local and
loopback-SMB fixtures; junction tests need no elevation, while symbolic-link tests explicitly
report a skip if Windows denies link creation.

The import-start response includes `networkShares`, the policy received by the import job
(`null` for the default document-share policy, an array for an explicit selection).
