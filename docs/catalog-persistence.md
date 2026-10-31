# Catalog persistence

The catalog is two files in the library folder (`crates/catalog/src/journal.rs`): `catalog.snap`, a JSON snapshot of
the whole catalog, and `catalog.log`, one JSON line per change made since. Every change is appended to the log and
fsynced before its command returns; now and then the log is compacted into a new snapshot. This page covers what
happens to a library's files when a newer version opens it.

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
- A library whose files can't be kept, or whose upgrade can't be written (a full disk, a folder that can't be written
  to), is not upgraded: it opens as it is, with no copy left lying about, and the next open tries again. If what is on
  disk after a failed write can't be read back to say whether the upgrade was written, the library is refused instead
  (its way back stays). A library left like that is upgraded by its next snapshot, during the session or when it is
  closed, and that snapshot keeps the files first too (and is not written when they can't be kept). When a snapshot
  during the session does that, LightCraft tells where the files are kept, as it does at open (a snapshot when the
  library is closed, or switched away from in Settings, has no one left to tell: the files are in the folder, named
  as above). The files kept are the library as it was when it opened (the snapshot, and the log up to where it ended
  then): what was changed since is in the files that replace them, not in the kept ones. (An earlier session of this
  version may have added changes while the library stayed in its old format: they are in the log it opened with, and
  an older version may refuse them.) Until then, changes
  go on being appended to the library as it is, so that they can be saved at all where there is no room for a copy of
  its files. (If the session ends without that snapshot, the next open keeps the log as it is then, which holds
  those changes, and tells so; the files kept earlier stay beside it.)
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
