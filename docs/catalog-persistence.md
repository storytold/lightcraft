# Catalog persistence: when the log is compacted

The catalog is two files in the library folder (`crates/catalog/src/journal.rs`): `catalog.snap`, a JSON snapshot of
the whole catalog, and `catalog.log`, one JSON line per change made since. Every change is appended to the log and
fsynced before its command returns; now and then the log is compacted into a new snapshot. This page covers when that
happens and what it costs.

## When the log is compacted

A snapshot rewrites the whole catalog, so its cost grows with the library. `SnapshotPolicy`:

| When | Rule |
|---|---|
| While working | The log is long (2000 records or 16 MB) **and** has grown to a quarter of the snapshot's size; or it has reached 256 MB. |
| On quit, snapshot up to 64 MB | Whatever log there is. |
| On quit, larger snapshot | Only when the log is long (2000 records or 16 MB). |

Before, the log was compacted every 2000 records or 16 MB, and on every quit. For a small library nothing changes. For
a large one, saving follows what changed and not the library's size, and after a quit the log is still never longer
than 2000 records or 16 MB, so what a damaged record in it can cost is what it was. A session that ends without a quit
(a crash, a power cut) leaves the log as long as it had grown, up to a quarter of the snapshot or 256 MB; a damaged
record in it costs the records after it, which are kept as `catalog.log.damaged-<seq>`.

Writing a few times the final snapshot during an import holds up to a snapshot of 1 GB (four times the 256 MB cap).
Beyond it the cap is what makes a log due, and the snapshot is rewritten once per 256 MB of log.

## Measurements

`cargo run --release -p lightcraft-catalog --example persist_bench -- <scratch dir> <photos…>`, on an Apple M1 Max
(internal SSD), 2026-10-23, with the machine busy. "Before" is the same benchmark with the old rule (`SnapshotPolicy`
set to compact every 2000 records or 16 MB, and on every quit). The library is 90 % Local browse records of raws on
their import defaults and 10 % library photos with one edit and one history step; a record is 3.7 kB.

### Importing (one change per photo, compacting whenever it is due)

| Photos | Compactions before | Snapshots written before | Time before | Compactions after | Snapshots written after | Time after |
|---|---|---|---|---|---|---|
| 10,000 | 5 | 0.11 GB | 1.7 s | 5 | 0.11 GB | 1.9 s |
| 100,000 | 50 | 9.4 GB | 38.1 s | 15 | 2.1 GB | 16.7 s |

Before, the snapshots written grow with the square of the library (25 times the final snapshot at 100,000 photos);
after, they stay at about 6 times the final snapshot. Appending one change took 4.5 to 5.0 ms (median, with its fsync)
in every run, and the time the caller was blocked by a background compaction stayed at 10 to 25 ms.

**What this does not change.** The whole catalog is still held in memory and still written as one file; a compaction
still takes as long as the snapshot is large (1.5 s on a worker thread at 100,000 photos).
