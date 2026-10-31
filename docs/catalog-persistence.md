# Catalog persistence

The catalog is saved in the library folder by `crates/catalog/src/journal.rs`:

- `catalog.log`: one JSON line per change. Every change is appended and fsynced before its command returns.
- `catalog.snap`: the snapshot the log is compacted into now and then. It holds everything but the photos (albums,
  keywords, folders, counters) and lists the segment files.
- `segments/<first id>-<seq>.json`: the **segments**, each a run of photos in id order, as JSON. The numbers in a
  name say which photos it starts at and at which change it was written; what tells LightCraft which photos a segment
  holds is the list in `catalog.snap`.

This page covers catalog formats 10 (what a record states) and 11 (the photos in segments), and what happens to a
library's files when a newer version opens it.

## The stored form

A photo's record states only what was set (`crates/catalog/src/stored.rs`):

- fields of the photo and of its metadata that are empty, zero or unset are left out;
- develop settings (the photo's, and those of its history steps, versions and import look) are stored as their
  difference from the defaults: `{}` for a photo nobody edited.

Reading did not change: a missing field reads as its default, so libraries of formats 1 to 9 load as they are and are
rewritten in format 10 when first opened. Builds before format 10 refuse a format 10 library. A new library whose
`catalog.snap` can't be written (a full disk, a read-only volume) is not opened either, since what was added to its log
would leave out defaults that nothing states.

The files an upgrade replaces are kept: see "Going back to an older version" below.

`catalog.query` and the other commands still return every field. That public form of a photo is also what a Local
record's fingerprint and the preview-cache key are made from, so neither changed.

Three rules keep what is left out safe:

- **A value is left out only where that reads back the same.** Which values that holds for is worked out from the
  types themselves on first use, not from a list: not the rendering process (a missing process means the
  first one), and not single fields of a value that is read all or nothing (the crop rectangle).
- **Every difference is read back and compared before it is written.** Settings that would not survive are stored in
  full, as before.
- **The library says what a missing value reads as.** `catalog.snap` states it (`"defaults"`: the settings of a
  record with nothing stated, about 2.8 kB once per library), and that covers the log beside it. Every setting is
  stated, also the few whose own JSON is left out at their default (the white balance scale, the HDR section, the
  camera's local tone, Denoise on or off); only the rendering process is not, since a record without one is on the
  first process in every build. So what a photo's record means does not depend on the defaults of the LightCraft that
  reads it:
  - a build with the same defaults reads the records as they are, at no cost;
  - a build that lacks settings the library knows reads it as it is (it would not read them whatever it read them
    over), and one that has settings the library never heard of reads them as its own default, then rewrites the
    library to state them before a record that leaves them out is added (`LoadReport::defaults_added`). If that rewrite
    can't be written (a full disk, a read-only volume) the library still opens, and its records state develop settings
    whole, as formats before 10 did, until a snapshot (a compaction, or closing) succeeds; then compact records start.
    A crash at any point leaves the old files whole, or a header that states every setting before any compact record;
  - a build whose defaults differ reads every record over the library's own, then rewrites the library with its own
    before anything is added to it (`LoadReport::other_defaults`). If that rewrite can't be written, the library is
    refused: its files still state the library's own defaults, or the new snapshot was written and only the log was not
    emptied (its records are then skipped as stale), so the next open reads it the same way. No compact record follows a
    header that doesn't state what it relies on.

  What a missing value reads as in the current build is also pinned by a test
  (`crates/catalog/src/develop_defaults.json`), so a changed default is seen when it is made. A setting that is
  removed or renamed needs a loader of its own.

## Going back to an older version

Opening a library with a newer LightCraft rewrites it in that version's format, and older versions then refuse it
(safely: they say so and change nothing). So that this is not a one-way step, the upgrade first keeps the files the
library was, byte for byte, beside it:

- `catalog.snap.v<format>-<seq>` and `catalog.log.v<format>-<seq>`, where `<format>` is the format they are in and
  `<seq>` the number of the last change the library held when it was upgraded (or, where the upgrade was left for a
  later snapshot, when the library was opened, or when changes were last saved in the old format; a library that had no log, or only a log, has only the one file). `Journal::open` reports their names (`LoadReport::kept`) and logs them.
- The files kept at open stay unless the library is known to be in its old format still, because the upgrade at open
  did not happen (then they keep nothing, and are removed again): once the upgrade is written, nothing that fails
  after it takes them away. The files a later snapshot keeps (below) stay too, even if that snapshot then fails and
  the library is still in its old format.
- A library that can't be kept, or whose upgrade can't be written (a full disk, a folder that can't be written to),
  is not opened where working in it as it is would not be safe: one in a format before 10 (builds of those formats
  read a setting a record leaves out as their own default), or one that states other defaults than this version's.
  The error says why, and the catalog is not changed (a torn last record of its log is still cut off, as on any
  open). If what is on disk after a failed write can't be read back to say whether the upgrade was written, the
  library is refused too (its way back stays).
  A library in format 10 or later, which states its defaults, opens as it is (if it lacks a setting this version has,
  records are written whole until the snapshot that upgrades it, which keeps its files first): a later snapshot, during
  the session or when it is closed, upgrades it, keeps the files first and is not written when they can't be kept (a
  change that could not be appended to the log is still saved when the upgrade can't be done, whether the files can't
  be kept or the new snapshot can't be written: in the format on disk, in one file, so nothing is lost and nothing is
  upgraded without its way back; the files kept before then go, since they are not the way back from what is on disk
  now), and the next open tries again. The files that snapshot keeps are the library as it was when it opened (the
  snapshot, and the log up to where it ended then; where changes were saved in the old format meanwhile, that
  one-file snapshot, with no log): what was changed since is in the files that replace them, not in the kept ones. A snapshot that fails and is tried again keeps them once. When a snapshot during the session does
  that, LightCraft tells where the files are kept, as it does at open (a snapshot when the library is closed, or
  switched away from in Settings, has no one left to tell: the files are in the folder, named as above). Until then,
  changes go on being appended to the library as it is, so that they can be saved at all where there is no room for a
  copy of its files. (If the session ends without that snapshot, the next open keeps the log as it is then, which
  holds those changes, and tells so; the files kept earlier stay beside it.)
- A library whose log was damaged is upgraded like any other. What it was is then its snapshot, kept under the name
  above, and its log, kept as `catalog.log.damaged-<seq>`: the snapshot alone is the library without the changes
  that log held. If the snapshot can't be kept, the library is refused and left as it was; if the damaged log can't
  be replaced after the upgrade was written, it is refused too, and its way back (the kept snapshot and
  `catalog.log.damaged-<seq>`) stays (changes made in it would be appended after the damage, where the next open
  would not read them).
- Once the upgrade is written, nothing removes the kept files. They are as large as the library was; LightCraft says so once when it has upgraded
  a library (and `library.info` lists it under `settingsWarnings`). Delete them once the older version is no longer
  wanted. The browser build keeps none (it always runs the newest version).

**To go back**, with LightCraft closed: move `catalog.snap`, `catalog.log` **and the `segments` folder** away, then
give each kept file its own name back: `catalog.snap.v<format>-<seq>` becomes `catalog.snap`, and the kept log
(`catalog.log.v<format>-<seq>`, or `catalog.log.damaged-<seq>` where the log was damaged) becomes `catalog.log`. A
library that had only one of them (no log, or no snapshot) has only that one to rename. The older version opens the
library as it was when it was upgraded (where the upgrade came from a later snapshot: as it was when it was opened, or when
changes were last saved in the old format, so the changes made between that and the upgrade are not carried back
either); changes made since, in the newer version,
are in what was moved away and are not carried back. The `segments` folder goes with them because it holds the newer library's photos: left in place
it only takes room.

Only a library in one file (formats up to 10) is kept this way. One already in segments would need its segments kept
too; no upgrade from such a format exists yet, and a backup is the way back from a future one.

## Segments (format 11)

A library in a folder on disk keeps its photos in segment files (`crates/catalog/src/segments.rs`), and opening it
reads them on several threads.

- **Size.** A segment holds up to 4096 photos (`SnapshotPolicy::segment_photos`: a setting, not part of the format, so
  a library written with one size opens with another). One that grows past twice that is cut into pieces; one left
  without photos is dropped. A small library is one segment. The size follows the number of photos, not the size of
  their records: a library of heavily edited photos has larger segments (sizing by bytes is a later change).
- **What a compaction writes.** Every segment, each under a name no snapshot lists and each fsynced, then
  `catalog.snap` is replaced atomically, which is the commit, then the files it no longer lists are removed. A crash
  at any point leaves a snapshot with all its segments. (Rewriting only the segments whose photos changed is a later
  change.) A crash can leave segment files nothing lists. They are never read and never written over, and they are
  removed when the library is next opened by a program that holds its lock (to a second program, the segments the
  first is writing for its next snapshot look the same). Only segment files are ever removed: not other files in the
  folder, and not the files of a one-file `catalog.snap` put back over a newer folder, which may be all there is of
  the newer catalog (they go when the library, by then in segments, is next opened).
- **Checks on open.** Each segment's size, CRC-32, photo count and range of ids, and the total number of photos, must
  be what `catalog.snap` states. A damaged or missing segment fails the open with an error naming the file, and
  nothing is modified. Segment files without a `catalog.snap` are refused, not opened as a new library, unless the
  log holds every change from the first (a library whose first snapshot never finished).
- **Upgrade.** A one-file library (formats up to 10) is split when first opened: the segments are written, read back
  and compared with the photos that were loaded, and only then does the new `catalog.snap` replace the old one. If
  they don't read back the same, the one file stays: a format 10 library opens from it as before (and the next open,
  a later compaction and quitting try again, keeping the files first (once, until changes are saved in the old format), see "Going back to an older
  version"),
  an older one is not opened.
- **Stores without segments.** The browser build keeps the photos in `catalog.snap` itself (its storage holds a fixed
  set of files). Either layout is read wherever it is found; a desktop library folder is not a browser library.

## Measurements: segments (format 11)

`cargo run --release -p lightcraft-catalog --example persist_bench -- <scratch dir> 300000`, one run of each on an
Apple M1 Max (internal SSD) on 2026-10-25, with other builds running on the machine (a load average of 20 to 50), so read
the figures as rough. The catalog is 300,000 photos in the benchmark's default mix (90 % Local browse records of raws,
10 % library photos with an edit), 180 MB in one file. "Before" is the parent of this change, which writes that one
file; "after" writes segments.

| | Before (one file) | After (segments) |
|---|---|---|
| Open (load the snapshot, replay the log) | 259 ms | 78 ms |
| Compaction of 2000 records, synchronous (median of 3) | 840 ms | 448 ms |
| Compaction in the background, the worker's time (3 runs) | 922 to 1,072 ms | 752 to 767 ms |
| Caller blocked by a background compaction (median) | 15.4 ms | 13.9 ms |
| Appending one change, with its fsync (median) | 4.3 ms | 4.1 ms |
| An append during a background compaction: median and p95 (3 runs) | 4.2 to 4.5 ms, 7.7 to 7.9 ms | 5.6 to 7.4 ms, 17.5 to 20.0 ms |

Every compaction writes every segment in this change, on several threads, so a compaction of a catalog this size is
quicker than writing one file, and opening is about three times quicker. Appends made while a background compaction runs
are slower, because the segments are made durable on several threads at once while the log is. Only this one mix
and size were measured.

**What this does not change.** The whole catalog is held in memory, as before.
