# Catalogs

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

A **catalog** is a folder. Inside it:

| Path | What |
|---|---|
| `<name>.<catalog_ext>` | the catalog entry file inside the catalog (File → Libraries → Open Catalog… takes the folder or this file) |
| `catalog.redb`, `catalog.snap`, `catalog.log` | the catalog store: photos, edits, history, collections, keywords |
| `catalog.lock` | held while the catalog is open, so two app instances can't write it at once |
| `Originals/` | photos copied or moved in on import (`YYYY/YYYY-MM-DD/` by default) |
| `backups/` | catalog backups (default location) |
| `publish.json`, `tether.json`, `actions.json` | publish services, the studio session, saved actions |
| `Interop/` | Lightroom import recovery archives ([migration guide](migrating-from-lightroom.md)) |

Originals added in place stay where they are; the catalog references them. Edits are stored in the catalog and,
with File → Automatically Write Changes into XMP on, also in XMP sidecars that Lightroom, darktable and Immich can
read ([xmp-interop.md](../xmp-interop.md)). App-wide settings (keymap, language, window layout) live in the
`<settings_dir>` folder of your user configuration directory, not in the catalog.

## Opening and creating

File → Libraries → New Catalog… creates an empty catalog folder; File → Libraries → Open Catalog… opens another, and
the catalogs you opened last are listed below them. Settings → General → At launch opens the last one at the next
start, or asks which one (*Ask me*); holding Alt (Option on a Mac) as the window appears asks once. The catalog opens
after the window appears, behind a small *Opening your catalog* window — after an update, the catalog's format
upgrade happens there first, with the bytes of its safety copy as progress, so a long wait says what it is doing
(a catalog that can't be opened ends at the usual window offering Try Again). From the command line, `--library DIR` opens or creates one:

```sh
<cli> run --library ~/Pictures/Test catalog.info
```

## Backups and maintenance

Edit → Catalog Settings… sets the backup schedule (never, every exit, daily, weekly — the default — or monthly), the
backup folder, how many backups to keep, and whether to test integrity and optimize when backing up.

- File → Libraries → Back Up Catalog: copy the catalog now and test the copy (`catalog.backup`).
- File → Libraries → Test Catalog Integrity (`catalog.checkIntegrity`) and Optimize Catalog (`library.compact`);
  Catalog Settings… → Optimise (`catalog.optimize`) rewrites the store and rebuilds its indexes.
- In the browser version, File → Libraries → Back Up Catalog… / Restore Catalog from Backup… download and restore
  the whole catalog.

## Moving photos between catalogs

- File → Libraries → Export as Catalog… (`catalog.export`): selected, visible or all photos with their collections and
  stacks into a new catalog folder, optionally with the originals and previews (for a laptop trip).
- File → More Imports → Import from Another Catalog… (`catalog.import`): bring them back. Choose what wins for photos both catalogs
  have — keep, replace settings, replace metadata, or both — and preview the changes first. One undo step.

## Missing photos

Library → Find Missing Photos… lists photos whose originals are offline or moved; relink a folder and its photos
follow. Missing photos keep their edits and metadata in the catalog.

## Duplicates

Library → Find Duplicates… groups exact copies (from content hashes) and, with Find Similar Copies, resized or
re-compressed ones; raw + JPEG pairs are offered as stacks. Remove sends marked copies to Recently Deleted; see
[duplicates.md](../duplicates.md). The catalog format is now 17: an older build refuses a catalog this one has opened.

See [catalog.md](../catalog.md) for the storage format and scale benchmarks.
