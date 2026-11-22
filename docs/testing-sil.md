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
| `sil_workflow` | import a folder → cull (rate, flag, reject, label, filters) → develop (Auto, sliders, WB picker, crop, radial mask, preset) → copy/paste and Synchronize → virtual copy, history back and forth → albums and a smart collection → keywords and metadata → export JPEG, TIFF and Original + Settings → XMP write, change, read back → print render → slideshow export (PDF in the foreground, JPEG slides in the background, an unknown format refused) → reopen |
| `sil_raw_workflow` | corpus raws (skipped without a corpus): import, Auto, sliders, WB, export JPEG and DNG, XMP, reopen |
| `sil_monkey` | seeded random user actions (about 8 % with hostile arguments: bogus ids, out-of-range and huge numbers, control characters), with a reopen every 100 steps and at the end; it stops making virtual copies once the library holds 64 photos (`MONKEY_MAX_PHOTOS`), so a long run's library, and with it each step and its checks, stays inside the per-step timeout |

Engine-level scenarios (no child process; `cargo test -p lcc-engine --test it roundtrip`), in
`crates/engine/tests/it/roundtrip.rs`:

| Test | What it does |
|---|---|
| `import_develop_export_reimport_keeps_settings_metadata_and_sane_pixels` | procedural JPEG → import → metadata, rating, exposure/contrast → full-size JPEG export (unedited and edited) → XMP sidecar → fresh library re-imports the original (settings, rating, title, keywords back) and the export (title, keywords, creator back); pixels: decodable, same size, not blank or clipped, the brighter edit exports brighter (tolerances, no goldens) |
| `exporting_a_photo_whose_original_is_gone_is_an_error_not_a_panic` | the original vanishes before export: a per-photo error or batch error, never a panic |

`cargo xtask sil` also runs the sync merge properties (`crates/sync/src/proptests.rs`, no app process):
merges of the group, of collections and of last-writer-wins values are commutative, associative and idempotent;
copies that saw the same writes converge in any order; and no merge or sync session lets a device that doesn't own
a collection take, fetch, add to or be told the photos of more than its owner granted. `cargo xtask sil sync` runs
only these; `PROPTEST_CASES=5000` makes a deeper run, and a failure prints its minimal case.

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
- **Slideshow export:** the PDF starts with `%PDF`, there is a decodable JPEG per slide, the background export's
  activity row drains, the status has no error and no `.part` file is left behind.
- **XMP:** settings read back from a sidecar equal those written.
- **No stuck jobs:** the activity stack empties.

## Running it

```sh
cargo xtask sil                      # all scenarios (a few seconds once built)
cargo xtask sil monkey --seed 42 --steps 2000
SIL_CORPUS=/path/to/raws cargo xtask sil raw
```

`cargo test --workspace` (and so `cargo xtask ci`) runs it too, with a fresh random monkey seed each time. The seed
is printed at the start, so a red run can be reproduced. Environment variables: `SIL_SEED`, `SIL_STEPS` (default 200; every run draws a new seed, so the PR gate keeps the budget short and runs add up. Use `--steps 2000` for a deep run),
`SIL_CORPUS`, `SIL_OUT` (where run folders go; default the temp dir), `SIL_KEEP=1` (keep a passing run's folder).

The nightly corpus job ([`ci.md`](ci.md#nightly)) runs the full `cargo xtask ci`, so `sil_raw_workflow` runs there
on the pinned raw corpus instead of skipping; its skip, like every other, counts against `ci/skip-allowlist.toml`.

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

## Hostile-input tables (below SIL)

Malformed files are caught at the lowest level, next to the decoder, not here. Raw containers with a compressed
decoder of their own (CR3 lossless and C-RAW, Sony ARW6, GoPro VC-5, Fujifilm compressed RAF in Bayer and X-Trans,
Nikon High Efficiency) share one table in `crates/raw/src/vendor/container_robust.rs`: each row builds a valid seed
with the crate's procedural test encoders, then runs truncations across the whole seed (tail included) and seeded mutations of it (`lcc-fuzzkit`)
through the decoder with two budgets. The row must finish inside a generous deadline (a hang fails instead of
stalling CI), and a successful decode of a mutation may not produce more samples than the row's cap (a header
claim trusted without the data behind it). DNG and the vendor TIFF variants stay in `crates/raw/tests/it/robust.rs`
and `vendor_robust.rs`.

`LCC_FUZZ_ITERS=N` raises the mutation count of every `lcc-fuzzkit` harness for a longer local run, e.g.
`LCC_FUZZ_ITERS=30000 cargo test -p lcc-raw --lib container_robust` (the deadlines scale with it). An input that
fails is printed with its harness name and iteration; add it to the row's `regressions` so every run replays it.
