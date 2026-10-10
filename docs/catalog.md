# Catalog storage and scale

> Names in angle brackets (`<app>`, `<catalog_ext>`, …) are the values set in [`brand.toml`](../brand.toml).

## Benchmark

`cargo xtask bench-catalog --photos 250000,1000000 [--backend v3|v4]` builds a synthetic library (camera, lens,
keywords, ratings, flags, labels, capture dates over 15 years; one photo in five edited with two History steps),
then reopens it **in a fresh process** so the peak RSS is the open's alone, and times the filter bar's queries on it
(all, rating ≥ 4, picks, camera, keyword, a year, free text; median of 3 runs, default sort by capture date).

Columns: *open* = load until the catalog is usable; *peak RSS* = the open process's high-water mark (Linux
`VmHWM`); *filter* = the slowest of the queries above; *snapshot* = one full compaction after the import; *import* =
photos applied and appended to the log per second, in batches of 1000; *disk* = the library folder after the
compaction.

### v3 (JSON snapshot + op log, everything in RAM) — baseline, 2026-10-10

Linux, 32 threads, NVMe.

| photos | open | peak RSS | filter (worst) | snapshot | import | disk |
|---|---|---|---|---|---|---|
| 250 000 | 3.9 s | 1 963 MB | 210 ms | 1.5 s | 138 k/s | 1.1 GB |
| 500 000 | 9.4 s | 3 926 MB | 1 157 ms | 3.1 s | 145 k/s | 2.2 GB |
| 1 000 000 | 16.5 s | 7 847 MB | 1 016 ms | 7.3 s | 94 k/s | 4.4 GB |

Where it goes: about 4.4 KB per photo on disk and 8 KB in RAM, most of it develop settings (every photo carries a
full `DevelopSettings`, edited ones three copies with their History). Opening parses the whole JSON snapshot on one
thread; every compaction rewrites all of it; every filter scans and sorts every photo.

### v4 (redb store + op log) — 2026-10-10

Same machine, but measured while other builds kept it busy (load average about 40 on 32 threads), so the times are
pessimistic; the parallel load and filter suffer most. *First snapshot* is the checkpoint that writes every photo
(an import of that size); *after 1000 edits* is the next one.

| photos | open | peak RSS | filter (worst) | first snapshot | after 1000 edits | import | disk |
|---|---|---|---|---|---|---|---|
| 250 000 | 0.37 s | 787 MB | 51 ms | 2.1 s | 82 ms | 124 k/s | 0.55 GB |
| 500 000 | 0.73 s | 1 294 MB | 110 ms | 4.1 s | 151 ms | 125 k/s | 1.1 GB |
| 1 000 000 | 1.6 s | 2 337 MB | 287 ms | 9.0 s | 333 ms | 128 k/s | 2.2 GB |

Earlier runs on a quieter machine: 500 000 photos filtered in 15–60 ms (except the unfiltered capture-date sort,
which dominates the worst case) and opened in 0.69–0.72 s with 1 050 MB peak RSS.

Migration v3 → v4 of 500 000 photos (`--backend v3 --migrate`, same load): 34 s on the first open (loading the JSON
snapshot is half of it), peak RSS that of a v3 open; the next open takes 1.0 s. It happens once.

Against the Phase 1 targets (500k photos: open < 2 s, filter < 100 ms, RSS < 2 GB): open and memory are met with room
to spare; filtering is met for every filtered view and is at the limit for the unfiltered sort of all 500k photos on a
loaded machine. 1M photos stays usable (1.6 s open, 0.3 s worst filter), but above 2 GB.

## Design (v4)

**Decision: redb.** A pure-Rust embedded key-value store (MIT OR Apache-2.0), ACID with copy-on-write B-trees and
checksummed pages, one file, no background threads. What decided it, from the v3 benchmark: the costs were parsing
one big JSON file on one thread, rewriting all of it on every compaction, and a full copy of the develop settings per
photo. A store with per-record writes and parallel decoding removes the first two; content-addressed settings remove
the third. redb gives the per-record transactions without a C dependency (SQLite) or a log-structured engine's
background compaction (fjall, sled), and its whole-store integrity check backs the backup's integrity test.

**What stays the same.** The `Catalog` / `Op` / `Journal` API: every change is still an op, applied in memory and
appended (fsynced) to `catalog.log` before a command returns; undo still uses inverse ops; the log is still the change
feed. The `Catalog` stays in memory (the engine reads `&Arc<Photo>`), so "lazy" loading is about what is loaded and how
fast (below), not about leaving photos on disk.

**What changes.** The snapshot is replaced by the store (`crates/catalog/src/db.rs`):

- a **checkpoint** writes, in one transaction, the photos whose `Arc` changed since the last one (an op that changes a
  photo replaces its `Arc`), the albums, stacks, remote links and previews entries that differ, and the `seq` of the
  last op it holds; the log is then reset. It runs on a worker thread like the JSON compaction did (the store travels
  with it). Loading = the store + the log records after its `seq`, with the same torn-tail / damage rules;
- **develop settings are content-addressed**: a photo record refers to its develop settings, History steps, Versions
  and import look by hash (`settings_ref.rs`); the `develop_settings` table holds each distinct value once, and loading
  shares one `Arc` per value (every unedited photo shares the defaults). This is most of the memory saved;
- loading decodes records on up to 16 threads, in batches (bounded extra memory);
- the store is opened for each load, checkpoint or read and closed after: a session never holds it, so one session
  per library stays `LibraryLock`'s job. A generation number catches a store another writer changed since it was
  loaded (it is then rewritten whole, not patched);
- **secondary indexes** (`folders`, `keywords`, `idx_camera`, `idx_captured_day`) serve lookups straight from the file
  (`CatalogDb::ids_where`), as do `read_photo` and `page_ids` (grid paging by id) — for tools and a future paging
  grid. Rating and flag have too few values for an index to beat a scan. The in-memory filter bar instead filters on
  several threads and sorts with keys held inline (`query.rs`), which is what keeps it under 100 ms;
- redb panics on some damaged files: every call into it is wrapped (`guarded`), so a damaged store is an error
  ("restore it from a backup"), never a crash (`tests_v4::damaged_or_missing_store_is_an_error_not_a_crash` flips and
  truncates bytes).

The web build (OPFS) and in-memory libraries keep the JSON snapshot (`Store::dir` is `None` for them); both formats
carry the same catalog (format version 4).

### Tables

| table | key → value |
|---|---|
| `meta` | `format`, `version`, `seq`, `generation`, `head` (id counters, colour label names, browse times) |
| `photos` | photo id → photo JSON (settings as references) |
| `develop_settings` | settings hash → settings JSON (develop, History, Versions, import look) |
| `collections` | album id → album (albums, smart albums, album folders) |
| `stacks` | stack id → stack |
| `previews` | photo id → previews index entry (size, 1:1, settings hash, built at) |
| `remote_identity` | (photo id, service, account) → link (remote id, remote checksum, remote updated at, last synced at, sync state) |
| `remote_identity_by_remote` | (service, account, remote id) → photo id |
| `folders`, `keywords`, `idx_camera`, `idx_captured_day` | value → photo ids |

### Format 4 additions

`Photo.sha1` (the original's SHA-1, as Immich identifies assets; `Op::SetSha1`, `Catalog::photos_with_sha1`),
`Photo.xmp` (an `XmpStamp`: what the photo's XMP-relevant state and the sidecar's mtime/size were at the last read or
write; `Photo::xmp_status` → in sync / changed in catalog / changed on disk / conflict; `Op::SetXmpStamp`), remote
links (`Op::SetRemote`, `Catalog::remote_of`, `Catalog::photo_of_remote`, `Catalog::link_remote_op`; one remote asset
links to one photo), and the previews index (`Op::SetPreview`, `Catalog::preview_entry`). Deleting a photo
permanently drops its links and preview entry in the same undo step.

## Catalog folder, migration, backup

A catalog is a folder (`crates/catalog/src/library.rs`):

```text
My Catalog/
  My Catalog.<catalog_ext>   entry point (JSON: magic, version, store)
  catalog.redb               the store
  catalog.log                the op log
  catalog.snap               stub that makes builds before v4 refuse the folder untouched
  catalog-settings.json      backup schedule, backup folder, backups kept, last backup
  backups/                   default backup folder; before-v4 <time>/ holds the migrated v3 files
```

- `library::create(parent, name)`, `library::open(path)` (entry point or folder), `library::resolve`,
  `RecentCatalogs` (Open Recent list, a startup default, the chooser when Alt is held or asked for).
- **Migration** happens in `Journal::open` for any folder library still in the JSON format: its `catalog.snap` and
  `catalog.log` are copied to `backups/before-v4 <time>/`, the library is loaded, the store is written to a temporary
  file and renamed into place, then the stub replaces the snapshot and the log is reset. A crash at any point leaves
  the v3 library (migrated again on the next open) or the finished v4 one.
- **Backup**: `Journal::backup(catalog, root, keep)` checkpoints, copies the store, log, stub, settings and entry point
  into `root/<time>/`, opens the copy and runs redb's integrity check on it, then prunes to the newest `keep`.
  `Journal::backup_if_due` applies `catalog-settings.json` (never / every exit / daily / weekly / monthly; integrity
  test before, optimise after). `Journal::check_integrity` (store pages, photo count, dangling album/stack
  references) and `Journal::optimize` (the store rewritten from the catalog: unused settings dropped, indexes rebuilt;
  then compacted).

## Export as catalog / import from another catalog

`crates/catalog/src/transfer.rs`: `export_catalog(catalog, ExportOptions { photos, include_originals,
include_previews }, parent, name)` writes a new catalog folder with those photos, the albums that hold them (only
them), every smart album, the album folders above, stacks (two or more exported members), remote links and optionally
the previews entries; with originals, the files are copied to `Originals/` and the photos point there.
`load_readonly(path)` reads another catalog without changing it; `plan_import(here, other)` is the change preview
(new photos, changed photos with whether settings and/or metadata differ, unchanged, new and extended albums);
`ImportPlan::ops(here, other, ConflictRule)` builds one undoable op (keep / replace settings / replace metadata /
both). Photos match by file path and virtual-copy name, so a photo whose original moved imports as a new photo.

## In the app

- **File ▸ New Catalog… / Open Catalog… / Open Recent Catalog ▸** (UI commands `catalog.new`, `catalog.open`,
  `catalog.openRecent`; `crates/ui-egui/src/catalog_ui.rs`). Open validates the folder or entry point
  (`library::resolve`), so a folder that is no catalog is refused instead of becoming an empty library. The recent
  list is `<settings_dir>/recent-catalogs.json` (`RecentCatalogs`); its `default` is opened at startup when no
  `--library` is given.
- **Catalog chooser** (`catalog.chooser`): recent catalogs, New…, Open…, "always show at startup"
  (`catalog.promptAtStartup`). It shows at startup when asked for, or when Alt is held while the window opens. The
  last catalog is opened first and the chooser switches from it (pure Rust has no portable way to read the keyboard
  before the window exists).
- **Backup**: engine commands `catalog.backup {dir?}`, `catalog.checkIntegrity`, `catalog.optimize` (File menu) and
  `catalog.backupIfDue`, which the desktop app runs on exit before the closing checkpoint.
- **Catalog Settings…** (Edit menu, `dialog.catalogSettings` → `catalog.settings`): backup schedule, folder, integrity
  test, optimise after backup; standard preview size, 1:1 discard, previews at import. The preview settings live in
  `catalog-settings.json` (`previews`), no longer in the library's `prefs.json` (read from there once, moved on the next
  save). Browser and in-memory libraries keep them in `prefs.json`.
- **File ▸ Export as Catalog…** (`catalog.export {parent, name, ids? | scope, originals, previews}`) and **Import from
  Another Catalog…** (`catalog.import {path, rule, preview}`): the dialog shows the change preview and the rule for
  changed photos; the import is one undo step.
- **Metadata vs XMP**: every XMP write (Save Metadata to File, auto-write) and read (Read Metadata from File, import
  of a photo with a sidecar) records an `XmpStamp` (journaled, not an undo step). `photo.xmpStatus` reports in sync /
  changed in catalog / changed on disk / conflict; the grid draws the badge; Read / Save Metadata ask first when they
  would overwrite changes on the other side (`confirmed: true` skips the question).
- **Previous Export** source (`library.source {kind: previousExport}`): the photos of the last export, kept with the
  view (`view.json`).

## Not done yet

- The grid still takes the full id list from `Catalog::query`; paging it through `CatalogDb::page_ids` (and leaving
  photo records on disk until shown) needs the engine to read photos through a cache instead of `&Arc<Photo>`.
- Migration of a 500k-photo v3 library takes tens of seconds and the RAM of a v3 open, once; no progress is reported.
