# Catalog persistence

The catalog is two files in the library folder (`crates/catalog/src/journal.rs`): `catalog.snap`, a JSON snapshot of
the whole catalog, and `catalog.log`, one JSON line per change made since. Every change is appended to the log and
fsynced before its command returns; now and then the log is compacted into a new snapshot. This page covers what a
record in those files states (catalog format 10) and what happens to a library's files when a newer version opens it.

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
  later snapshot, when the library was opened; a library that had no log, or only a log, has only the one file). `Journal::open` reports their names (`LoadReport::kept`) and logs them.
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
  the session or when it is closed, upgrades it, keeps the files first and is not written when they can't be kept, and
  the next open tries again. The files that snapshot keeps are the library as it was when it opened (the snapshot, and
  the log up to where it ended then): what was changed since is in the files that replace them, not in the kept ones.
  When a snapshot during the session does that, LightCraft tells where the files are kept, as it does at open (a
  snapshot when the library is closed, or switched away from in Settings, has no one left to tell: the files are in the
  folder, named as above). Until then, changes go on being appended to the library as it is, so that they can be saved
  at all where there is no room for a copy of its files. (If the session ends without that snapshot, the next open keeps
  the log as it is then, which holds those changes, and tells so; the files kept earlier stay beside it.)
- A library whose log was damaged is upgraded like any other. What it was is then its snapshot, kept under the name
  above, and its log, kept as `catalog.log.damaged-<seq>`: the snapshot alone is the library without the changes
  that log held. If the snapshot can't be kept, the library is refused and left as it was; if the damaged log can't
  be replaced after the upgrade was written, it is refused too, and its way back (the kept snapshot and
  `catalog.log.damaged-<seq>`) stays (changes made in it would be appended after the damage, where the next open
  would not read them).
- Nothing removes the kept files. They are as large as the library was; LightCraft says so once when it has upgraded
  a library (and `library.info` lists it under `settingsWarnings`). Delete them once the older version is no longer
  wanted. The browser build keeps none (it always runs the newest version).

**To go back**, with LightCraft closed: move `catalog.snap` and `catalog.log` away, then rename each kept file that exists
to `catalog.snap` and `catalog.log` (a library that had no log, or only a log, kept only the one file: rename only that
one, and leave the other name unused). Where the log was damaged, the kept log is `catalog.log.damaged-<seq>`: it takes
the place of `catalog.log` like any kept log (the older version reads it up to the damage and handles it as it did
before), beside the kept snapshot when there is one. The older version opens the library as it was when it was upgraded (where the upgrade came
from a later snapshot: as it was when it was opened, so the changes made between that and the upgrade are not carried
back either); changes made since, in the newer version, are in the files that were moved away.
