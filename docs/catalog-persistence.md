# Catalog persistence: what is stored and when it is rewritten

The catalog is two files in the library folder (`crates/catalog/src/journal.rs`): `catalog.snap`, a JSON snapshot of
the whole catalog, and `catalog.log`, one JSON line per change made since. Every change is appended to the log and
fsynced before its command returns; now and then the log is compacted into a new snapshot. This page covers catalog
format 10: the stored form of records, the rule for when to compact, and the measurements behind both.

## The stored form

A photo's record states only what was set (`crates/catalog/src/stored.rs`):

- fields of the photo and of its metadata that are empty, zero or unset are left out;
- develop settings (the photo's, and those of its history steps, versions and import look) are stored as their
  difference from the defaults: `{}` for a photo nobody edited.

Reading did not change: a missing field reads as its default, so libraries of formats 1 to 9 load as they are and are
rewritten in format 10 when first opened. Builds before format 10 refuse a format 10 library.

The files an upgrade replaces are kept: see "Going back to an older version" below.

`catalog.query` and the other commands still return every field. That public form of a photo is also what a Local
record's fingerprint and the preview-cache key are made from, so neither changed.

Three rules keep what is left out safe:

- **A value is left out only where that reads back the same.** Which values that holds for is worked out from the
  types themselves when the program starts, not from a list: not the rendering process (a missing process means the
  first one), and not single fields of a value that is read all or nothing (the crop rectangle).
- **Every difference is read back and compared before it is written.** Settings that would not survive are stored in
  full, as before.
- **The library says what a missing value reads as.** `catalog.snap` states it (`"defaults"`: the settings of a
  record with nothing stated, about 2.8 kB once per library), and that covers the log beside it. Every setting is
  stated, also the few whose own JSON is left out at their default (the white balance scale, the HDR section, the
  camera's local tone, Denoise on or off); only the rendering process is not, since a record without one is on the first process in every build. So what a photo's
  record means does not depend on the defaults of the LightCraft that reads it:
  - a build with the same defaults reads the records as they are, at no cost;
  - a build that only has settings the library never heard of does too (they take that build's default), and so
    does one that lacks settings the library knows (it would not read them whatever it read them over);
  - a build whose defaults differ reads every record over the library's own, then rewrites the library with its own
    before anything is added to it (`LoadReport::other_defaults`). If that can't be written, the library is refused
    and left as it is.

  What a missing value reads as in the current build is also pinned by a test
  (`crates/catalog/src/develop_defaults.json`), so a changed default is seen when it is made. A setting that is
  removed or renamed needs a loader of its own.

## Going back to an older version

Opening a library with a newer LightCraft rewrites it in that version's format, and older versions then refuse it
(safely: they say so and change nothing). So that this is not a one-way step, the upgrade first keeps the files the
library was, byte for byte, beside it:

- `catalog.snap.v<format>-<seq>` and `catalog.log.v<format>-<seq>`, where `<format>` is the format they are in and
  `<seq>` the number of the last change the library held when it was upgraded (a library that had no log, or only a
  log, has only the one file). `Journal::open` reports their names (`LoadReport::kept`) and logs them.
- The kept files stay unless the library is known to be in its old format still: once the upgrade is written,
  nothing that fails after it takes them away.
- A library that can't be kept, or whose upgrade can't be written (a full disk, a folder that can't be written to),
  is not opened where working in it as it is would not be safe: one that states no defaults for develop settings
  (formats before 9), or other ones than this version's. The error says why, and the catalog is not changed (a torn
  last record of its log is still cut off, as on any open). A library that states this version's defaults opens as
  it is, and the next open tries again.
- A library whose log was damaged is upgraded like any other. What it was is then its snapshot, kept under the name
  above, and its log, kept as `catalog.log.damaged-<seq>`: the snapshot alone is the library without the changes
  that log held.
- Nothing removes the kept files. They are as large as the library was; LightCraft says so once when it has upgraded
  a library (and `library.info` lists it under `settingsWarnings`). Delete them once the older version is no longer
  wanted. The browser build keeps none (it always runs the newest version).

**To go back**, with LightCraft closed: move `catalog.snap` and `catalog.log` away, then rename the kept pair to
`catalog.snap` and `catalog.log`. The older version opens the library as it was when it was upgraded; changes made
since, in the newer version, are in the files that were moved away and are not carried back.

## When the log is compacted

A snapshot rewrites the whole catalog, so its cost grows with the library. `SnapshotPolicy`:

| When | Rule |
|---|---|
| While working | The log is long (2000 records or 16 MB) **and** has grown to a quarter of the snapshot's size; or it has reached 256 MB. |
| On quit, snapshot up to 64 MB | Whatever log there is. |
| On quit, larger snapshot | Only when the log is long (2000 records or 16 MB). |

Before format 10 the log was compacted every 2000 records or 16 MB, and on every quit. For a small library nothing
changes. For a large one, saving follows what changed and not the library's size, and a log is still never left
longer than 2000 records from one session to the next, so what a damaged record in it can cost is what it was.

## Measurements

`cargo run --release -p lightcraft-catalog --example persist_bench -- <scratch dir> <photos…>`, on an Apple M1 Max
(internal SSD), 2026-10-10; "before" is main at `b70ae174`. Three kinds of library:

- **Browsed raws** (the default): 90 % Local browse records of raws on their import defaults, 10 % library photos with
  one edit and one history step.
- **JPEG library** (`PERSIST_BENCH_MIX=library`): imported JPEGs nobody edited, each with a title.
- **Edited raws** (`PERSIST_BENCH_MIX=edited`): every photo edited, with 20 history steps.

### Size of a record

| Library | Before | After |
|---|---|---|
| Browsed raws | 3,688 B | 585 B |
| JPEG library | 3,403 B | 412 B |
| Edited raws, 20 history steps | 56,513 B | 3,414 B |

### Importing (one change per photo, compacting whenever it is due)

| Library | Photos | Compactions before | Snapshots written before | Time before | Compactions after | Snapshots written after | Time after |
|---|---|---|---|---|---|---|---|
| Browsed raws | 10,000 | 5 | 0.11 GB | 0.6 s | 5 | 0.02 GB | 0.4 s |
| Browsed raws | 100,000 | 50 | 9.4 GB | 17.7 s | 15 | 0.25 GB | 3.1 s |
| Browsed raws | 300,000 | 150 | 83.6 GB | 136 s | 21 | 0.90 GB | 8.9 s |
| JPEG library | 100,000 | 50 | 8.7 GB | 15.3 s | 16 | 0.21 GB | 1.9 s |
| JPEG library | 300,000 | 150 | 77.1 GB | 122 s | 21 | 0.59 GB | 5.1 s |
| Edited raws | 20,000 | 40 | 23.2 GB | 40.0 s | 8 | 0.27 GB | 14.3 s |

Before, the snapshots written grow with the square of the library (75 times the final snapshot at 300,000 photos);
after, they stay at 3 to 5 times the final snapshot.

### One compaction, and opening the library

| Library | Photos | Snapshot before | Snapshot after | Compaction before | Compaction after | Open before | Open after |
|---|---|---|---|---|---|---|---|
| Browsed raws | 100,000 | 369 MB | 58.5 MB | 572 ms | 214 ms | 1,222 ms | 255 ms |
| Browsed raws | 300,000 | 1,107 MB | 176 MB | 1,659 ms | 586 ms | 3,698 ms | 758 ms |
| JPEG library | 100,000 | 340 MB | 41.2 MB | 495 ms | 88 ms | 1,134 ms | 183 ms |
| JPEG library | 300,000 | 1,021 MB | 124 MB | 1,480 ms | 195 ms | 3,415 ms | 550 ms |
| Edited raws | 20,000 | 1,130 MB | 68.3 MB | 1,809 ms | 2,797 ms | 3,860 ms | 443 ms |

Compaction is the synchronous time; natively it runs on a worker thread and blocked the caller for 10 to 25 ms in
every run above, before and after. Appending one change took 4.1 to 4.9 ms (median, with its fsync) in every run.

**Where it costs more.** Settings that differ from the defaults are read back before they are written, which takes
more processor time than writing them in full: about 1.6 times per snapshot when every photo has 20 history steps
(the last row). That snapshot is 17 times smaller and written 5 times less often during the import above, and on a
worker thread the whole compaction went from 4.6 s to 2.9 s; it is slower only where the snapshot is written
synchronously (the browser build, and quitting).

### Larger libraries (after only)

An import at these sizes was not run before the change: at 300,000 photos it already wrote 77 to 84 GB.

| Library | Photos | Snapshot | Import | Snapshots written | Compaction | Caller blocked | Open |
|---|---|---|---|---|---|---|---|
| Browsed raws | 1,000,000 | 589 MB | 27.9 s | 2.6 GB | 1.9 s | 33 ms | 2.6 s |
| JPEG library | 1,000,000 | 414 MB | 16.1 s | 2.0 GB | 0.56 s | 37 ms | 1.8 s |
| JPEG library | 3,500,000 | 1,454 MB | 55.8 s | 6.7 GB | 1.9 s | 72 ms | 6.4 s |

Replaying a log when the library is opened cost 0.3 to 1.1 ms per 1000 records up to 300,000 photos (200,000 rating
changes: 51 to 217 ms).

**What this does not change.** The whole catalog is still held in memory and still written as one file. The 3.5
million photo run peaked at 34 GB of memory (the benchmark holds two copies of the catalog while it checks the
reopened one), and replaying 200,000 changes took 2.8 s there. A library of that size wants the snapshot split into
segments, so a compaction rewrites only what changed and opening can use every core.
