# Catalog persistence: what is stored and when it is rewritten

The catalog is saved in the library folder by `crates/catalog/src/journal.rs`:

- `catalog.log`: one JSON line per change. Every change is appended and fsynced before its command returns.
- `catalog.snap`: the snapshot the log is compacted into now and then. It holds everything but the photos (albums,
  keywords, folders, counters) and lists the segment files.
- `segments/<first id>-<seq>.json`: the **segments**, each a run of photos in id order, as JSON. The numbers in a
  name say which photos it starts at and at which change it was written; what tells LightCraft which photos a segment
  holds is the list in `catalog.snap`.

This page covers catalog formats 10 (compact records) and 11 (segments): the stored form of records, the segments, the
rule for when to compact, and the measurements behind them.

## The stored form

A photo's record states only what was set (`crates/catalog/src/stored.rs`):

- fields of the photo and of its metadata that are empty, zero or unset are left out;
- develop settings (the photo's, and those of its history steps, versions and import look) are stored as their
  difference from the defaults: `{}` for a photo nobody edited.

Reading did not change: a missing field reads as its default, so libraries of formats 1 to 9 load as they are and are
rewritten in the current format when first opened. Earlier builds refuse a library in a newer format.

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

**To go back**, with LightCraft closed: move `catalog.snap`, `catalog.log` **and the `segments` folder** away, then
rename the kept pair to `catalog.snap` and `catalog.log`. The older version opens the library as it was when it was
upgraded; changes made since, in the newer version, are in what was moved away and are not carried back.

The `segments` folder must go with them. It holds the newer library's photos, and left in place it would be taken for
leftovers once a newer version has opened the library the older one wrote (when it next opens it), and removed.

Only a library in one file (formats up to 10) is kept this way. One already in segments would need its segments kept
too; no upgrade from such a format exists yet, and a backup (below) is the way back from a future one.

## Segments (format 11)

A compaction used to rewrite the whole catalog. Now it writes only the segments whose photos changed
(`crates/catalog/src/segments.rs`), so its cost follows what changed, and opening a library reads its segments on
several threads.

- **Which segments changed** is not worked out from the changes logged. The journal keeps the photos as the segments
  on disk hold them (shared pointers, not copies) and compares: a photo is unchanged while the catalog still points
  to the same one, and changing a photo copies it. So a segment is rewritten exactly when one of its photos was
  changed, added or removed, whatever did it.
- **Size.** A segment holds up to 4096 photos and about 2 MB (measured: "Segment size" below): where records are
  large (edited photos with their history), fewer photos, down to 64. One that changes is cut into pieces once it holds more than twice that; one
  left without photos is dropped. Photos added after a full last segment start new ones, so an import leaves the
  existing segments alone. A small library is one segment.
- **Commit.** New segments are written under names no snapshot lists (several at once, each fsynced), then
  `catalog.snap` is replaced atomically, which is the commit, then the files it no longer lists are removed. A crash
  at any point leaves a snapshot with all its segments; files nothing lists are removed when the library is next
  opened by a program that holds its lock.
- **Checks on open.** Each segment's size, CRC-32, photo count and range of ids, and the total number of photos, must
  be what `catalog.snap` states. A damaged or missing segment fails the open with an error naming the file, and
  nothing is modified. Segment files without a `catalog.snap` are refused, not opened as a new library.
- **Upgrade.** A one-file library (formats up to 10) is split when first opened: the segments are written, read back
  and compared with the photos that were loaded, and only then does the new `catalog.snap` replace the old one. If
  they don't read back the same, the one file stays: a format 10 library opens from it as before (and the next open
  tries again), an older one is not opened (see "Going back to an older version").
- **Stores without segments.** The browser build keeps the photos in `catalog.snap` itself (its storage holds a fixed
  set of files). Either layout is read wherever it is found; a desktop library folder is not a browser library.

## Backups

File ▸ Back Up Library (`library.backup`) makes a folder named after the time (UTC) that holds `catalog.snap`, the
segments it lists, `catalog.log` and the presets, view and preferences files: the catalog, not the photos or the
caches (`crates/catalog/src/backup.rs`). It goes to `Backups` in the library folder, to the folder set for the library
(Settings ▸ General ▸ Backups, or `library.backupPreferences`), or to the one given with the command. A folder
given for backups may not be one of the library's own (inside the library folder only `Backups` is for backups: the
others are the catalog's, and a restore moves them as such), nor a backup or a folder inside one. The app makes it in the background and says in a
toast where it went or why there is none; the command itself returns when the backup is made (`background: true` for
the other).

- **A backup takes every file the library has.** The library's own files beside the catalog are one list
  (`SettingsFile` in `crates/engine/src/library.rs`), which is what a backup takes along and what the browser build
  reads at start-up. The library's file store refuses to write a file that is not on it, so a new one can't be added
  without being backed up. Not in a backup: the files an upgrade kept (`catalog.snap.v…`), a damaged log set aside,
  previews and caches.
- **A backup holds a whole library, and is not worked in.** Its files are a library folder's, but LightCraft refuses
  to open a backup where it is: working in it would change it. To go back to it, or to look at it, restore it
  (`library.restore`, below).
- **Each backup shares with the one before it the files that didn't change.** A segment is never changed once
  written, so one of the same name, size and CRC in the previous backup is the same file, and is hard-linked instead
  of copied. Every backup still stands on its own: removing an older one, or the library, takes nothing from it.
  Where hard links aren't possible (another volume than the last backup's), files are copied. A file is shared only
  once it was read and still has its CRC, so damage in an older backup is not handed on: that file is copied afresh
  from the library.
- **Made while the library is in use.** The files are read as they are on disk: `catalog.snap`, the segments it
  lists, the log, then `catalog.snap` again. If it changed meanwhile (a compaction), the backup starts over, keeping
  what it has. Every segment is checked against the size and CRC the snapshot states; a library with a damaged or
  missing segment is not backed up, and the error names the file.
- **Nothing half-made counts.** A backup is written as `<name>.partial` and renamed when whole; one that fails or is
  stopped (its row in the activity stack) removes it. One cut short by the program ending stays as `<name>.partial`,
  is never taken for a backup, and is removed by a later backup to the same folder once it is an hour old.
- **A backup says what it holds.** Its marker, `backup.json`, lists every file with its size and CRC-32.
  `library.checkBackup` reads a backup against that list and names the first file that was edited, damaged, removed or
  added since.

### Restoring

File ▸ Restore Library from Backup… (`library.restore {dir}`) puts a backup back (`crates/catalog/src/restore.rs`): the open library becomes what the backup
holds (catalog, log, presets, view and preferences; the folder its backups go to stays as it is now), and the session
goes on in it, with no undo history, as after opening a library. With `to`, the backup is restored into that folder
instead, which is how to look at a backup without giving up the library; nothing is opened. `to` is a folder that
isn't there or is empty, or a library that nothing has open (it is replaced the same way); not a folder in the backup
or in the open library, and not a folder of other files. `library.backups` lists the backups of a folder, newest
first.

- **Only a sound backup is restored.** It is read against its marker first, and each file again as it is copied. One
  that was edited or damaged is refused, and so is one whose catalog or log a newer LightCraft wrote, with the library
  untouched. The backup is only read: its files are copied, never shared, because a library goes on changing, and
  nothing is made in its folder.
- **What the library was is kept.** The files a restore replaces, with every change made up to that moment, are moved
  to `Replaced/<the time (UTC)>` in the library folder. That is a library folder itself: open it, or restore from a
  backup of it, to go back. Nothing removes these folders yet; delete the ones you no longer want. The files an
  earlier upgrade kept (`catalog.snap.v…`, `catalog.log.v…`) are not part of this: they stay where they are, and are
  still the library as the older version left it, not the one restored.
- **All or nothing.** The backup is first copied into `restore.partial` in the library folder while the library is
  still as it was. Then a note of what is to be moved (`restore.json`) is written, and only then are files moved: the
  library's out, the snapshot first, and the backup's in, the snapshot last. If the library's files can't all be
  moved out, the ones that were are moved back and the restore is refused. A restore that stops after the note (the
  program ends) is completed when the library is next opened, which says so; one that stops before it leaves the
  library as it was. A library folder is never opened as some files of one catalog and some of another.
- **The note is not trusted.** It moves a catalog's files by plain name between the library folder, `Replaced` and
  `restore.partial`, which must be folders of the library's own, not links; never another folder, a lock file or a
  backup's marker. A note that can't be read or asks for anything else stops the library from opening, moves nothing,
  and says where the files are; with the note removed, the library opens as whatever is in place.
- **A restored catalog that doesn't open is taken back.** If the session can't open what it restored (a segment that
  has its checksum but is no catalog data), the library's files are moved back, the backup's go to
  `Replaced/<the time> (not opened)`, and the session goes on in the library as it was. With `to` nothing is opened,
  so that is found out only when the folder is.
- **Not while the library is at work.** A restore of the open library is refused while a backup or any other
  background task is running.
- **If it can neither go on nor go back** (the disk fails between two moves), the session changes and writes nothing
  more and says so; opening the library again completes the restore.
- **It takes the space of the catalog twice over**: the copy in the library, and what it replaced.

In the app the menu item asks for the backup's folder, and the backup is copied and checked on a worker thread while
the library stays in use (`background: true`): a row in the activity stack (kind `restore`) shows it, and its ✕ stops
it with nothing changed. Once the copy is made, its files are moved into place when the library is at rest (no slider
held, no backup or other background work), which takes the moment that moving files and opening the catalog take; a
change made while the backup was being copied is in what the restore replaced. A toast says where that went, or why
the library was not restored. While it waits its row reads "copied", and can still stop it.

- Opening a library before the backup is moved into place, another one or this one again, gives the restore up: the
  copy stops, and what was copied is removed.
- A copy left by a program that ended while it was copying (`restore.partial`, with no note) is cleared away when the
  library is next opened.
- `Replaced/<the time>` is named for when the restore was asked for, not for when the files were moved.
- `background` is for a session that goes on (the app, MCP, the control channel). Without it the command does all of
  it before it returns, which is what a script or a one-shot `lightcraft-cli run` needs: that program ends when the
  command returns, and a restore in the background ends with it, having changed nothing.
  `library.restoreStatus` reports a restore in the background and, asked until `running` is false, sees it through
  where no frame loop does.

A backup on the same disk as the library guards against a damaged catalog or a mistake, not against losing the disk:
set the library's backup folder to another drive for that.

One million photos (414 MB of catalog, 123 segments; Apple M1 Max): the first backup took 1.0 s; one after 1000
edits in one place copied 3.4 MB and shared 122 files, in about 0.15 s (most of it reading the files it shares);
checking a backup took 64 ms. Three backups together took 398 MB on disk, where the library takes 395 MB.

## When the log is compacted

`SnapshotPolicy`, where "what it would rewrite" is `catalog.snap`, the segments holding changed photos, and a sixteenth
of the catalog for looking through it (all of the catalog where there are no segments):

| When | Rule |
|---|---|
| While working | The log is long (2000 records or 16 MB) **and** has grown to a quarter of what a compaction would rewrite; or it has reached 256 MB. |
| On quit, rewriting up to 64 MB | Whatever log there is. |
| On quit, rewriting more | Only when the log is long (2000 records or 16 MB). |

Changes within a few segments are compacted every 2000 records however large the library; changes spread all over
it, and bulk imports into a very large one, less often. A log is never left longer than 2000 records from one session
to the next, so what a damaged record in it can cost is what it always was.

## Measurements: compact records and compaction by size (format 10)

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

**What this did not change.** The whole catalog was still written as one file, which format 11 changes.

## Measurements: segments (format 11)

Same benchmark and machine; "before" is format 10 (one file) at `f43b0fdc`. These tables were measured with segments
of up to 8192 photos and about 4 MB, the sizes before "Segment size" below halved them. "One place" is 1000 changes to the first
1000 photos; "all over" is a change to every 50th photo of the library, which touches every segment.

| Library | Photos | Open before | Open after | Compaction, one place before | after | Compaction, all over before | after |
|---|---|---|---|---|---|---|---|
| JPEG library | 100,000 | 175 ms | 59 ms | 81 ms (41 MB) | 49 ms (4.1 MB) | 88 ms | 129 ms |
| JPEG library | 300,000 | 532 ms | 147 ms | 187 ms (124 MB) | 48 ms (4.1 MB) | 191 ms | 283 ms |
| JPEG library | 1,000,000 | 1,767 ms | 518 ms | 570 ms (414 MB) | 58 ms (4.1 MB) | 577 ms | 886 ms |
| JPEG library | 3,500,000 | 6,129 ms | 1,714 ms | 2,009 ms (1,455 MB) | 75 ms (4.2 MB) | 2,060 ms | 3,360 ms |
| Browsed raws | 100,000 | 251 ms | 63 ms | 210 ms (58 MB) | 62 ms (4.7 MB) | 209 ms | 167 ms |
| Browsed raws | 300,000 | 741 ms | 165 ms | 575 ms (176 MB) | 63 ms (4.7 MB) | 581 ms | 363 ms |
| Edited raws | 20,000 | 399 ms | 132 ms | 2,742 ms (68 MB) | 326 ms (6.8 MB) | 2,731 ms | 731 ms |

Importing (one change per photo, compacting whenever it is due):

| Library | Photos | Snapshots written before | Time before | Snapshots written after | Time after |
|---|---|---|---|---|---|
| JPEG library | 100,000 | 0.25 GB | 1.9 s | 0.10 GB | 3.1 s |
| JPEG library | 300,000 | 0.72 GB | 5.2 s | 0.25 GB | 7.7 s |
| JPEG library | 1,000,000 | 2.4 GB | 16.6 s | 0.56 GB | 20.3 s |
| JPEG library | 3,500,000 | 8.2 GB | 57.5 s | 1.7 GB | 61.0 s |
| Browsed raws | 300,000 | 1.08 GB | 9.2 s | 0.33 GB | 10.6 s |
| Edited raws | 20,000 | 0.34 GB | 14.0 s | 0.07 GB | 6.4 s |

Appending one change (4.1 to 5.0 ms) and how long a background compaction blocks the caller (10 to 30 ms; 70 ms at
3.5 million photos) are as before.

**Where it costs more.**

- **Rewriting every segment at once** is slower than writing one file where records are small (JPEG libraries: 1.5
  times), because each segment file is made durable on its own. It happens when changes touch every segment, and once
  when a one-file library is split.
- **Importing** small records takes up to 1.6 times as long in this benchmark, which measures the catalog alone:
  compactions are smaller but more frequent, and each looks through the catalog for what changed. A real import also
  reads every file, which takes far longer than either.

**What this does not change.** The whole catalog is still held in memory (see below). Not measured: how long
replaying a long log takes at 3.5 million photos (the benchmark's figure there is dominated by memory pressure from
the copies of the catalog it holds).

### Segment size

How large a segment is is a setting (`SnapshotPolicy::segment_photos`, `segment_bytes`), not part of the format: a
library written with one size opens with another, and its segments are cut to the new size as they are next
rewritten. These runs varied the number of photos per segment with no size in bytes (same machine; a one-off harness
around `Journal`, not kept in the repository; one run per row, saves and opens the median of three; page cache warm).
"20 places" is 50 changes at each of 20 places spread over the library. The megabytes are what a save rewrites, which
is also what the next backup copies.

3.5 million unedited JPEGs (1,455 MB of catalog):

| Photos per segment | Segment size | Segments | Open | One place | 20 places | All over |
|---|---|---|---|---|---|---|
| 2,048 | 0.9 MB | 1,709 | 1,116 ms | 48 ms (1.0 MB) | 143 ms (17 MB) | 7.8 s |
| 4,096 | 1.7 MB | 855 | 975 ms | 56 ms (1.8 MB) | 138 ms (34 MB) | 4.7 s |
| 8,192 | 3.4 MB | 428 | 1,002 ms | 63 ms (3.4 MB) | 189 ms (68 MB) | 3.3 s |
| 16,384 | 6.8 MB | 214 | 998 ms | 75 ms (6.8 MB) | 290 ms (136 MB) | 2.6 s |
| 32,768 | 13.6 MB | 107 | 844 ms | 95 ms (13.5 MB) | 432 ms (272 MB) | 2.1 s |
| 65,536 | 26.9 MB | 54 | 864 ms | 146 ms (27.0 MB) | 803 ms (544 MB) | 2.0 s |

1 million unedited JPEGs (414 MB), with the import that made them ("written": all snapshots together, as a multiple of
the final one):

| Photos per segment | Segments | Import | Written | Open | One place | 20 places | All over |
|---|---|---|---|---|---|---|---|
| 1,024 | 922 | 29.5 s | 1.08 | 333 ms | 40 ms | 131 ms (9 MB) | 4.3 s |
| 2,048 | 442 | 24.7 s | 1.14 | 256 ms | 39 ms | 127 ms (21 MB) | 2.1 s |
| 4,096 | 207 | 23.7 s | 1.22 | 250 ms | 45 ms | 176 ms (44 MB) | 1.1 s |
| 8,192 | 91 | 22.9 s | 1.35 | 251 ms | 55 ms | 235 ms (102 MB) | 0.84 s |
| 16,384 | 49 | 20.4 s | 1.78 | 258 ms | 61 ms | 308 ms (182 MB) | 0.66 s |
| 65,536 | 14 | 19.7 s | 2.92 | 305 ms | 139 ms | 516 ms (413 MB) | 0.53 s |

50,000 edited raws with 20 history steps each (169 MB):

| Photos per segment | Segment size | Open | One place | 20 places | All over |
|---|---|---|---|---|---|
| 128 | 0.4 MB | 115 ms | 94 ms (3.5 MB) | 179 ms (12 MB) | 1.9 s |
| 256 | 0.9 MB | 116 ms | 122 ms (3.5 MB) | 218 ms (21 MB) | 1.3 s |
| 512 | 1.7 MB | 119 ms | 188 ms (3.5 MB) | 324 ms (38 MB) | 1.1 s |
| 1,024 | 6.7 MB | 127 ms | 340 ms (6.8 MB) | 1,172 ms (135 MB) | 1.1 s |
| 4,096 | 18.7 MB | 199 ms | 917 ms (20 MB) | 1,904 ms (169 MB) | 1.9 s |
| 8,192 | 28.1 MB | 286 ms | 1,598 ms (35 MB) | 2,844 ms (169 MB) | 2.8 s |

What a save of a few changes rewrites follows the size of a segment, and for edited photos so does its time. Changes
to every segment go the other way, since each file is made durable on its own. Opening is the same from about 2000
photos per segment up. So segments are about 2 MB and at most 4096 photos: half of what they were (8192 photos, 4
MB), which halves what scattered changes rewrite and what a backup after them copies, and makes a save that touches
every segment (rare, and in the background) about 1.4 times as slow where records are small. The size is not
adjusted to how a library is used: it already follows the size of the records, the measurements are flat around it,
and every change of size would rewrite segments that backups share.

## In memory: equal develop settings are held once

The whole catalog is held in memory. A photo holds its develop settings behind a shared pointer, and so do its
history steps, versions and import look; `crates/catalog/src/shared.rs` gives every settings value that enters a
catalog (read from a saved library, or set by a change) the value already held when an equal one is. Photos nobody
edited share the defaults, raws of one camera their import defaults, and a photo's last history step its settings.

- Settings saved with nothing stated (`{}`: every unedited photo) are not read at all when a library is opened: they
  are the one value held for them.
- Equal means the same down to the saved JSON: settings holding a negative zero (which `==` takes for zero, and the
  JSON, a photo's fingerprint and the preview-cache key don't) are never shared.
- Sharing can't carry one photo's edit to another: settings behind a shared pointer are replaced, never changed in
  place.

Bytes requested from the allocator per photo, the library opened from disk (300,000 photos; 20,000 edited ones),
before = format 8 without sharing (`394a4ea8`):

| Library | Before | After | At 3.5 million photos |
|---|---|---|---|
| JPEG library, unedited | 2,877 B | 1,373 B | 10.1 GB → 4.8 GB |
| Raws on their import defaults | 2,876 B | 1,372 B | 10.1 GB → 4.8 GB |
| Edited raws, 20 history steps each | 34,060 B | 32,556 B | (each step holds its own settings) |

Opening got faster where settings are not read (JPEG library: 518 → 387 ms at 1 million photos, 1,714 → 1,301 ms at
3.5 million), and the 3.5 million photo benchmark run peaked at 18.1 GB where it was 30.7 GB (it holds two to three
copies of the catalog). Where each photo's settings are read and found equal to another's (raws on their import
defaults), opening takes about a tenth longer (348 → 394 ms for 300,000); edited photos open as fast as before.

**What this does not change.** A photo's record still takes about 1.1 kB whatever it holds (room for every metadata
field, lens data, culling scores), and every history step of an edited photo holds a full settings value (1.5 kB):
both are where the rest of the memory is.
