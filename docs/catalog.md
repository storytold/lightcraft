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
