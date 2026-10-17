# SIL testing (software in the loop)

> Names in angle brackets (`<app>`, `<cli>`, `<PREFIX>`) are the values set in [`brand.toml`](../brand.toml).

The SIL harness drives the **real app** through what a photographer does in Lightroom Classic, and checks
invariants after every step. It doesn't compare images or exact outputs: rendering quality is known to be imperfect
and is judged elsewhere. What it checks is that nothing crashes, hangs, loses work or contradicts itself.

## How it works

`apps/cli/tests/sil.rs` starts `<cli> snapshot --library DIR --script -` as a child process. That is the whole
desktop app (UI, engine, catalog, files on disk) running headless on the CPU, with no window, and answering
[control-protocol](control-protocol.md) requests one line at a time on stdin/stdout. No OS-level input is ever sent.
The library is a fresh temporary folder, the settings folder is isolated through `<PREFIX>_CONFIG_DIR`, and the
photos are procedural JPEGs from `lcc-scenes`, plus a few raws from `corpus/raw` when it exists. "Reopen" means
the process quits (EOF) and a new one opens the same library.

Scenarios (one `#[test]` each, run in parallel):

| Test | What it does |
|---|---|
| `sil_workflow` | import a folder → cull (rate, flag, reject, label, filters) → develop (Auto, sliders, WB picker, crop, radial mask, preset) → copy/paste and Synchronize → virtual copy, history back and forth → albums and a smart collection → keywords and metadata → export JPEG, TIFF and Original + Settings → XMP write, change, read back → print render → reopen |
| `sil_raw_workflow` | corpus raws (skipped without a corpus): import, Auto, sliders, WB, export JPEG and DNG, XMP, reopen |
| `sil_monkey` | seeded random user actions (about 8 % with hostile arguments: bogus ids, out-of-range and huge numbers, control characters), with reopen every 60 steps; it stops making virtual copies once the library holds 64 photos (`MONKEY_MAX_PHOTOS`), so a long run's library, and with it each step and its checks, stays inside the per-step timeout |

Invariants:

- **Still alive:** every request gets an answer within 180 s (otherwise it counts as a deadlock), the process never
  dies, and it exits by itself on EOF, with no `panicked at` on stderr and no signal.
- **Clean errors:** every reply has `ok`, and every failure carries a message that isn't a caught panic.
- **Consistent library** (after every step): the selection names only existing photos and the active photo is
  selected, the view's `visibleCount` matches the photos it lists, `library.info` agrees with the catalog, and no
  album holds more photos than the library.
- **Undo then redo** returns to the same state: every photo's catalog fields, its develop settings and the albums.
- **Reopen keeps everything:** the same fingerprint before and after quitting and reopening.
- **Export:** one file per photo, each non-empty and decodable by `lcc-codecs`. Original + Settings also writes an XMP.
- **XMP:** settings read back from a sidecar equal those written.
- **No stuck jobs:** the activity stack empties.

## Running it

```sh
cargo xtask sil                      # all scenarios (a few seconds once built)
cargo xtask sil monkey --seed 42 --steps 2000
SIL_CORPUS=/path/to/raws cargo xtask sil raw
```

`cargo test --workspace` (and so `cargo xtask ci`) runs it too, with a fresh random monkey seed each time. The seed
is printed at the start, so a red run can be reproduced. Environment variables: `SIL_SEED`, `SIL_STEPS` (default 400),
`SIL_CORPUS`, `SIL_OUT` (where run folders go; default the temp dir), `SIL_KEEP=1` (keep a passing run's folder).

## When it fails

The panic message names the broken invariant and shows the last steps. It also gives:

- `report.txt`: every step with its time, the request and the outcome;
- `replay.jsonl`: every request sent, in order (`# --- start` / `# --- quit` mark the reopens);
- the library folder and the shoot it imported, kept for inspection;
- for the monkey, `reproduce: SIL_SEED=… cargo xtask sil monkey`.

To replay by hand, take the part of `replay.jsonl` up to the first `# --- quit` and run
`<cli> snapshot --library NEW_DIR --script part.jsonl` (repeat with the next part on the same library to replay a
reopen). The import paths in the script point at the kept shoot folder. Shrink the script by deleting lines until
the failure goes away. The smallest version that still fails becomes the regression test, either as an engine unit
test or as a scenario step.

## Adding a scenario

Write a `#[test] fn sil_<name>()` in `apps/cli/tests/sil.rs`:

```rust
let mut s = Sil::new("name");
let (dir, n) = shoot(&s.root, 6, 0);   // 6 JPEGs, 0 corpus raws
s.start();
let ids = import(&mut s, &dir, n);
s.ok("photo.rate", json!({"rating": 4, "ids": [ids[0]]}));   // must succeed
s.cmd("develop.paste", json!({}));                            // may fail cleanly
s.request("ui.set", json!({"view": "detail"}));               // a non-command control method
s.consistent();
s.undo_redo_roundtrip();
s.reopen_keeps_everything();
s.finish();
```

Check behaviour the user would notice ("the album shows 2 photos", "the copy didn't change its original"), not
numbers that are simply what the code produces today. To teach the monkey a new action, add an arm to
`random_action`.
