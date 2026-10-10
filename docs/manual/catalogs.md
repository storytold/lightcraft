# Catalogs

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

A **library** is a folder. Inside it:

| Path | What |
|---|---|
| `<name>.<catalog_ext>` | the catalog entry file you open (File → Open Catalog…) |
| `catalog.redb`, `catalog.snap`, `catalog.log` | the catalog store: photos, edits, history, albums, keywords |
| `catalog.lock` | held while the catalog is open, so two app instances can't write it at once |
| `Originals/` | photos copied or moved in on import (`YYYY/YYYY-MM-DD/` by default) |
| `backups/` | catalog backups (default location) |
| `publish.json`, `tether.json`, `actions.json` | publish services, the studio session, saved actions |
| `Interop/` | Lightroom import recovery archives ([migration guide](migrating-from-lightroom.md)) |

Originals added in place stay where they are; the catalog references them. Edits are stored in the catalog and,
with File → Automatically Write Changes into XMP on, also in XMP sidecars that Lightroom, darktable and Immich can
read ([xmp-interop.md](../xmp-interop.md)). App-wide settings (keymap, language, window layout) live in the
`<settings_dir>` folder of your user configuration directory, not in the library.

## Opening and creating

File → New Catalog… creates an empty library folder; File → Open Catalog… (or File → Open Library…) opens another.
The last one opens at the next start. From the command line, `--library DIR` opens or creates one:

```sh
<cli> run --library ~/Pictures/Test catalog.info
```

## Backups and maintenance

Edit → Catalog Settings… sets the backup schedule (never, every exit, daily, weekly — the default — or monthly), the
backup folder, how many backups to keep, and whether to test integrity and optimize when backing up.

- File → Back Up Catalog: copy the catalog now and test the copy (`catalog.backup`).
- File → Test Catalog Integrity (`catalog.checkIntegrity`) and File → Optimize Catalog (`catalog.optimize`).
- File → Back Up Library… / Restore Library from Backup…: the whole library folder.

## Moving photos between catalogs

- File → Export as Catalog… (`catalog.export`): selected, visible or all photos with their albums and stacks into
  a new catalog folder, optionally with the originals and previews (for a laptop trip).
- File → Import from Another Catalog… (`catalog.import`): bring them back. Choose what wins for photos both catalogs
  have — keep, replace settings, replace metadata, or both — and preview the changes first. One undo step.

## Missing photos

Library → Find Missing Photos… lists photos whose originals are offline or moved; relink a folder and its photos
follow. Missing photos keep their edits and metadata in the catalog.

See [catalog.md](../catalog.md) for the storage format and scale benchmarks.
