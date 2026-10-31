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

A compaction used to rewrite the whole catalog. A library in a folder on disk keeps its photos in segment files
(`crates/catalog/src/segments.rs`), and a compaction writes only the segments whose photos changed, so its cost follows
what changed, and opening a library reads its segments on several threads.

- **Which segments changed** is not worked out from the changes logged. The journal keeps the photos as the segments
  on disk hold them (shared pointers, not copies) and compares: a photo is unchanged while the catalog still points
  to the same one, and changing a photo copies it. So a segment is rewritten exactly when one of its photos was
  changed, added or removed, whatever did it. Where nothing is known to be on disk (a library just upgraded from one
  file, a snapshot thread that did not start or died, a library read over other defaults than this build's) every
  segment is written.
- **Size.** A segment holds up to 4096 photos and about 2 MB (measured: "Segment size" below): where records are
  large (edited photos with their history), fewer photos, down to 64 (`SnapshotPolicy::segment_photos`,
  `segment_bytes`: settings, not part of the format, so a library written with one size opens with another; the
  record size of photos in no segment yet is taken from the snapshot there is). One that changes is cut into pieces
  once it holds more than twice that; one left without photos is dropped. Photos added after a full last segment
  start new ones, so an import leaves the existing segments alone. A small library is one segment.
- **Commit.** New segments are written (several at once, each fsynced) under names no snapshot lists, then
  `catalog.snap` is replaced atomically, which is the commit, then the files it no longer lists are removed. A crash
  at any point leaves a snapshot with all its segments. A crash can leave segment files nothing lists. They are never read and never written over, and they are
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

## Backups

File ▸ Back Up Library (`library.backup`) makes a folder named after the time (UTC) that holds `catalog.snap`, the
segments it lists, `catalog.log` and the presets, view and preferences files: the catalog, not the photos or the
caches (`crates/catalog/src/backup.rs`). It goes to `Backups` in the library folder, to the folder set for the library
(Settings ▸ General ▸ Backups, or `library.backupPreferences`), or to the one given with the command. A folder
given for backups may not be one of the library's own (inside the library folder only `Backups` is for backups: the
others are the catalog's, and a restore moves them as such), nor a backup or a folder inside one. The app makes it in the background and says in a
toast where it went or why there is none; the command itself returns when the backup is made (`background: true` for
the other).

- **A library can be backed up without being asked.** Two choices of their own, saved with the library and off
  until made (`library.backupPreferences`: `every`, `onClose`; Settings ▸ General ▸ Backups). While it is open, the
  app backs it up in the background once a day, a week or a month (thirty days), counted from the last backup made
  to the folder its backups go to, whoever made it (`lastBackup`, the session clock's time, kept in the library's
  preferences); a backup by itself that fails, or can't be started, says so and is tried again an hour later. Backed up
  "there" is meant: when backups go to another folder the library has not been backed up, and when the folder they
  were in is gone, the next look finds that out, forgets the last backup and makes one.
  When LightCraft quits, it asks first or backs up at once (a backup that runs already is the one it waits for), and
  quits when the backup is made; one that fails asks whether to quit anyway. A restore keeps the schedule, like the
  folder: both are the library's choices now, not the backup's from then. Only the app a
  person has open does either: headless sessions (snapshots, agents, the command line) back up when they are told to.
- **A library keeps its backups forever, unless told for how long** (`library.backupPreferences`, `keepDays`;
  Settings ▸ General ▸ Backups ▸ Keep backups: 1, 3, 6 months or a year, a month being thirty days). After a backup is
  made to the folder the library's backups go to, its backups older than that are removed, each as a whole folder:
  - only after a backup that was made, never that backup, and never one of the library's newest three, however old
    they are. Age is told by clocks, and one backup made while the clock was wrong must not cost the others: a
    clock that jumped ahead costs the backups beyond those three, and no more.
  - only what is certainly an old backup of this library. A backup says in its `backup.json` which library it is of
    (`library`, the library folder, and `libraryId`, the id a library is given at its first backup and keeps in its
    preferences) and when it was made (`made`). It is left alone when either is another's or missing (another
    library's backup in the same folder; one of a library that was in this place before and was deleted; a folder
    that is no backup), when it no longer has the name it was made under (a backup set aside under a name of your
    own is yours to keep), and when the time it was made is yet to come or is no time at all: such a backup is
    neither removed nor counted among the newest. A library that was moved does not remove the backups made of
    it where it was.
  - by the time the backup says it was made, not by file dates, which change when a drive is copied.
  - not while the library is being restored: a restore reads a backup, so none is made or removed until it is over.
  - only a backup that holds nothing but the files it lists: one that something else was put into since (notes,
    an export) stays with everything in it, and the backup says it could not be removed and why. A link in the
    folder that leads to a backup kept elsewhere is not followed.

  A library restored into another folder (`library.restore` with `to`) is another library from then on, with an
  id of its own from its first backup. What the rule can't tell apart is a library folder copied by hand to the
  same path on another computer (or a cloned drive mounted at the same place) while both copies back up to one
  shared folder with a time set: they have the same id and the same path, so each counts the other's backups as
  its own. Give such copies folders of their own for backups. And a library put back from a backup of another
  library, or from one made before libraries had ids, no longer removes the backups made of it before that: the
  folder then grows until they are removed by hand.

  The backup says which older ones it removed and which it could not (`removed`, `notRemoved`), and so does the
  app's note that the library was backed up.
  `library.pruneBackups` says which backups are that old without removing any (`remove: true` removes them). A
  removal that is cut short leaves a `<name>.partial` folder, which is no backup and is cleared away by a later
  backup once it is an hour old; a backup of which nothing could be removed has its name back.
- **What restores replaced goes by the same rule.** Each restore keeps the whole catalog it replaced in
  `Replaced/<the time>` in the library folder, and nothing else ever removes those. A library that keeps its backups
  for a time removes them with its old backups, after a backup is made to its own folder: the folders of `Replaced`
  whose names say they are older than that, never the newest three of them (a folder named `… (not opened)`, which
  holds a backup that did not open and not what the library was, goes by its age and is not one of the three).
  Only a folder that is as the restore left it goes. One that was renamed has no age to go by; one that holds anything
  a restore did not put there stays, which covers a folder that was opened as a library since (that leaves its
  lock), anything you put into it, and also what the system adds when a folder is looked at (`.DS_Store`,
  `Thumbs.db`): the rule can't tell those from yours. Nothing goes while a restore is cut short. What it can't tell
  is a file you put there under a name the catalog uses (`presets.json`, a segment's name): that goes with the
  folder.
  The backup says what went (`replacedRemoved`, `replacedNotRemoved`), and so does the app's note. Before any is
  removed, `library.pruneBackups` says which would go (`replaced.outdated`) and which are as old but stay, with why
  (`replaced.kept`), so a folder that never goes is no riddle. A library that keeps its backups forever keeps all of
  it. A removal that was cut short leaves `<name>.partial`, which goes with the next; one of which nothing could be
  removed has its name back.
- **For more than that, use a backup tool.** Each backup is an ordinary folder that stands on its own, so tools made
  for keeping snapshots (`restic`, `rustic` and the like) can take the folder backups go to, with their own rules
  for what to keep and where. Point them at finished backups, not at the library folder while LightCraft has it
  open: the catalog's files change as you work.
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
  added since (not what Finder or Explorer write into a folder somebody looked at: `.DS_Store`, `Thumbs.db`,
  `desktop.ini`, `._…`).

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
  backup of it, to go back. A library that keeps its backups for a time removes these folders with its old backups
  (see above); in any other, delete the ones you no longer want. The files an
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

One million photos (414 MB of catalog, 123 segments of up to 8,192 photos, as measured before segments were cut
to 4,096 photos and about 2 MB, so a backup now copies less after the same edits; Apple M1 Max): the first backup took 1.0 s; one after 1000
edits in one place copied 3.4 MB and shared 122 files, in about 0.15 s (most of it reading the files it shares);
checking a backup took 64 ms. Three backups together took 398 MB on disk, where the library takes 395 MB.

## Measurements: only the changed segments are written (format 11)

`cargo run --release -p lightcraft-catalog --example persist_bench -- <scratch dir> 300000`, one run of each on an
Apple M1 Max (internal SSD) on 2026-10-25, with other builds running on the machine (a load average of 15 to 40), so read
the figures as rough. The catalog is the benchmark's default mix of 300,000 photos (90 % Local browse records of raws,
10 % library photos with an edit), 180 MB. "Before" is the parent of this change, which writes every segment each time;
"after" writes the ones whose photos changed.

| | Before (every segment) | After (changed segments) |
|---|---|---|
| Compaction of 2000 records (changes to one photo), synchronous (median of 3) | 545 ms | 48 ms |
| The same in the background, the worker's time (3 runs) | 785 to 929 ms | 102 to 111 ms |
| An append during a background compaction: median and p95 (3 runs) | 5.8 to 9.2 ms, 24.8 to 30.6 ms | 4.4 to 5.8 ms, 8.7 to 9.3 ms |
| Compaction after 1000 changes in one place (the first 1000 photos) | 180.3 MB written, 421 ms | 2.5 MB written, 59 ms |
| Compaction after 6000 changes all over the library (every 50th photo) | 180.3 MB written, 497 ms | 180.3 MB written, 554 ms |
| Open | 81 ms | 77 ms |
| Appending one change, with its fsync (median) | 4.2 ms | 4.1 ms |

Changes in one place write about what the segments there hold, and the time follows. Changes that touch every segment
write all of them, as before, and the 11 % more time in the one run above is within what these runs vary by here; the
extra cost is looking through the catalog for what changed. Only this one mix and size were measured for this change.

**What this does not change.** The whole catalog is still held in memory.

### Segment size

How large a segment is is a setting (`SnapshotPolicy::segment_photos`, `segment_bytes`), not part of the format: a
library written with one size opens with another, and its segments are cut to the new size as they are next
rewritten. These runs were made on this change before it was split into parts (the same segment writer and the same
changed-segments rule), and were not repeated for the parts. They varied the number of photos per segment with no size in bytes (same machine; a one-off harness
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
the final one). A segment holds at most the photos per segment of its row, so a library of 1 million photos needs at
least 1,000,000 ÷ that many segments (245 at 4,096): the counts below are the ones the harness reported and are lower
than that for every row, so they do not describe a library of exactly 1 million photos; the harness
was not kept, and they were not remeasured. Read the columns as how the sizes compare with each other, not as counts
for a library of that size (the 3.5 million table above counts as expected):

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
photos per segment up. So segments are about 2 MB and at most 4096 photos, which keeps what scattered changes rewrite
and what a backup after them copies small, and makes a save that touches every segment (rare, and in the
background) about 1.4 times as slow where records are small. The size is not
adjusted to how a library is used: it already follows the size of the records, the measurements are flat around it,
and every change of size would rewrite segments that backups share.
