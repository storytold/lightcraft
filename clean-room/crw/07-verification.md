# CRW decode verification (P4) — against LibRaw 0.22.2, zero tuning

Method: the frozen model of `06-crw-format.md` plus the frozen tables
(`logs/table_sel{0,1,2}.json`) were run by the local Python reference decoder over every
sample file, comparing **every sensor sample** (full values, low bits merged) against
`unprocessed_raw.exe -T` output. "EXACT" means zero mismatching samples over the whole
frame. The table extraction itself consumed these same files (known-plaintext learning),
so this is a consistency proof against the oracle, not a blind holdout; the cross-body
sharing (tables learned on one body, exact on others) is the strongest independent check.

Private files are the maintainer's own D60 photos (hashes in `01-plan.md` §2.2); only
their verdicts are recorded here, never content.

## Results

| File | Selector | Frame | Verdict |
|---|---|---|---|
| `crw-canon-eos-10d-a.crw` | 0 | 3152x2068: EXACT |  |
| `crw-canon-eos-10d-b.crw` | 2 | 3152x2068: EXACT |  |
| `crw-canon-eos-10d-c.crw` | 2 | 3152x2068: EXACT |  |
| `crw-canon-eos-300d-a.crw` | 0 | 3152x2068: EXACT |  |
| `crw-canon-eos-300d-b.crw` | 2 | 3152x2068: EXACT |  |
| `crw-canon-eos-d30.crw` | 1 | 2224x1456: EXACT |  |
| `crw-canon-eos-d60.crw` | 0 | 3152x2068: EXACT |  |
| `crw-canon-eos-digital-rebel.crw` | 0 | 3152x2068: EXACT |  |
| `crw-canon-powershot-g1.crw` | 1 | 2144x1560: EXACT |  |
| `crw-canon-powershot-g5.crw` | 1 | 2672x1968: 2685618 mismatches | first at 0 (row 0, col 0) |
| `crw-canon-powershot-g6.crw` | 0 | 3160x2344: EXACT |  |
| `private/CRW_0476.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0478.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0486.crw` | 0 | 3152x2068: EXACT |  |
| `private/CRW_0527.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0683.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0717.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0765.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0822.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_0879.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1011.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1153.crw` | 0 | 3152x2068: EXACT |  |
| `private/CRW_1277.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1514.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1646.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1740.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_1776.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_2035.crw` | 1 | 3152x2068: EXACT |  |
| `private/CRW_2219.crw` | 2 | 3152x2068: EXACT |  |

**28 of 29 files decode EXACT** — all 10 usable public CC0 files and all 18 private
D60 files; the PowerShot G5 is the known exception (`06-crw-format.md` §4).

## Notes

- The final 1–3 samples of each frame follow the stream-end rule (chains persist);
  every "EXACT" verdict includes them.
- PowerShot G5: see `06-crw-format.md` §4 — detected and excluded; embedded-JPEG
  fallback until resolved.
- The G1 (10-bit, no low-bits plane) verifies the low-bits presence rule; the G6
  (12-bit, different geometry, w not divisible by 64) verifies the row-reset and
  block rules on a second geometry.
