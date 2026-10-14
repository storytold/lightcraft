# Reading a raw's preview without reading the raw

A grid thumbnail and a loupe preview of an unedited raw both show the camera's embedded JPEG, not a
decode of the sensor data. That JPEG lives near the *start* of the file, but the rest of the file is
the sensor data — for a 24 MP Fuji RAF, **50 of 56 MB**. Until this change the preview loader called
`std::fs::read` on the whole file and threw the sensor data away, so every preview paid for bytes
nobody looked at.

That is free on a local SSD and expensive on a network share, which is where photographers keep
libraries. Measured on one real file (`DSCF9492.RAF`, 56.5 MB) over a GVFS/SMB share on Wi-Fi:

| | read | time |
|---|---|---|
| whole file | 59 277 632 B (100 %) | ~10 s |
| header + preview | **5 857 257 B (9.9 %)** | **0.96 s** |

## How it works

1. `read_prefix` reads the first `PREVIEW_PREFIX` (4 KiB); every format whose preview is locatable
   from the header keeps its pointer well inside that.
2. For Fuji RAF the header's two big-endian `u32`s at **84** (offset) and **88** (length) are the
   same fields `lightcraft_raw`'s RAF reader uses. When they declare more than the prefix holds, the
   prefix is re-read once at exactly that extent: `lightcraft_raw::bounded_preview` reports the range
   its buffer covers (`slice` clamps to `b.len()`), so a short prefix yields a *plausible but
   truncated* range — a real 4 KiB prefix reports 3 948 bytes where the file holds 5.6 MB.
3. `embedded_preview_from_range` checks the window really starts with a JPEG SOI and trims it at the
   last EOI marker, exactly as `embedded_preview` trims the whole-file answer.
4. When the prefix already covers the range — the usual case, since it was grown to it — the window
   is a slice of the prefix. Reading it again would double a 5.6 MB preview.

Formats whose preview pointer is not in the header — a Canon CR3's boxes, a TIFF raw whose preview
tag sits in an IFD the prefix cuts short — are planned instead (next section but one), and what the
planner cannot describe keeps the whole-file path, so nothing regresses: `preview_window` returns
`None` and the loader reads the file as before. A pointer that lies, a short read, a plan that does
not fit or a file that changes underneath us ends in the same fallback — the user's preview is never
lost to a damaged header.

`LIGHTCRAFT_PROFILE=1` prints what each preview cost:

```
[profile] preview …/DSCF9492.RAF: read 5857257 of 59277632 bytes (header 5857257, window 5857109)
```

## What the header cannot point at: the plan

A Canon CR3 keeps its preview in a JPEG track, and a TIFF raw (CR2, ARW, DNG, RW2, 3FR …) can keep
its preview tag in an IFD the header route's prefix cuts short. Neither has a field step 2 above can
read, so `preview_window` asks `lightcraft_raw::sparse::preview_plan` — the module the
Lightroom-catalog import probes with — which ranges `embedded_preview` reads, and reads exactly
those:

- the buffer is the file's length with only the planned ranges filled in; every other byte stays zero
  and its pages are never written. Its cost is the bytes read — on macOS a freed buffer of that size
  may stay in the allocator's cache instead of going back to the OS (measured on a 38k-photo import,
  [#832](https://github.com/storytold/lightcraft/issues/832)) — and the loader's memory gate counts
  the whole file either way;
- the module's own guarantee is what this path needs, and its corpus test asserts it: with the
  skipped bytes `0` and again `0xFF`, what a partly loaded buffer yields is what the whole file
  yields. A second corpus pass now asks `preview_plan` for **every** file, including the ones whose
  header probe reads samples and whose header plan is `Whole`, which is where that plan was untested
  before;
- the quick path must be able to show what the whole-file path would. Two container facts are not in
  the JPEG, so a file that states one where the bounded header cannot see it declines and the whole
  file keeps the preview: a preview colour space of **Adobe RGB** (the whole-file path applies it
  through `decode_raw_preview`; a JPEG-only decode has the JPEG's own space, else sRGB — a container
  that says sRGB does not decline, since that is what the decode assumes anyway), and the file's own
  EXIF orientation when the preview JPEG carries none of its own;
- a format the planner cannot describe — NEF, ORF and PEF probe from samples, CRW/MRW/X3F are found
  by scanning — a plan that does not fit the buffer or wants more than 60 % of the file (the bound the
  import path's own partial read uses), or a window that does not decode all answer `None`, and the
  loader reads the file whole exactly as before: the plan can cost bytes but never the picture.

Measured over the CC0 corpus (111 raws, `cargo xtask corpus --download`): **52 files take the plan
route**, reading a median **7.8 %** of their bytes (0.9 %–27.7 %) — by format RW2 3.3 %, Leica RWL
4.1 %, ARW 7.5 %, CR3 8.4 %, DNG 9.7 %, CR2 14.3 % — while the 21 RAFs stay on the header route
(median 4.5 %). The remaining 38 read whole: NEF, NRW, ORF, PEF and Samsung SRW because the planner
does not describe them (the first four say so; an SRW plans to the whole file, and the 60 % budget
then keeps the whole-file read), and the ARWs, CR2s, CR3s and DNGs that declare Adobe RGB or whose
plan wants more than that budget. Across the whole corpus the plan handed on **the same preview as
the whole file, every time** — and wrote it down: `cargo test -p lightcraft-raw --release --test
sparse_corpus -- --nocapture` prints these numbers. `LIGHTCRAFT_PROFILE=1` says which route answered:

```
[profile] preview …/arw-sony-a7rm4a-compressed.arw: read 688128 of 61832192 bytes (plan, window 483441)
```

A Canon CR3's preview is a JPEG track instead (issue #832 measured 3 MB of one R5 Mark II file,
~15 %), and Canon is the largest bucket in [gaps.md](gaps.md)'s ranking (33 % of photographers), so
before this change every unedited CR3 thumbnail paid for the whole file.

## The grid thumbnail is the embedded JPEG too

A 512 px grid thumbnail of an unedited raw now keeps the camera's own JPEG instead of rendering the
sensor behind it. The grid showed that JPEG first in any case and then queued a decode of the whole
raw to replace it — for a folder of 149 Fuji RAFs that is 8.3 GB of traffic, 45 s a photo at
1.2 MB/s, and a queue that delayed the render the user was actually waiting for (measured: the first
loupe render of a cold photo finished 174 s after the click, most of it spent behind thumbnails).

- `Session::embedded_thumb_is_final(photo)` says whether the photo is an unedited raw whose camera
  JPEG the viewer is showing (the same "unedited raw" gate as the preview path: `has_import_look`).
- `grid::skips_the_real_render(embedded_is_final, from_embedded)` is the decision, unit-tested:
  a **cached render** as the stand-in, an **edited photo** and a **raw with no embedded JPEG** all
  still render, because their thumbnail must show the pipeline's output rather than the camera's.
- The thumbnail key still changes with the settings hash, so editing a photo re-renders its
  thumbnail through the ordinary background path.

Measured on the real folder: idle for a minute, the app read 12 previews (**26.9 MB**) and decoded
**zero** raws for thumbnails, against 638 MB of whole-file reads for the same 12 photos before. The
render queue stayed empty.

## Tests

- `crates/raw/src/preview.rs`: the header-only range matches `embedded_preview` byte for byte; a
  clamped prefix, a padded extent, a late pointer, a lying pointer and a non-JPEG window are all
  covered.
- `crates/engine/src/files.rs`: on a synthetic RAF with a 200 KB tail, the quick path and the
  whole-file path produce identical pixels, and the read stays under half the file. On a synthetic
  TIFF/DNG whose preview only its tags locate — the plan path — the same identity holds and the CFA
  strip is not read; the same structure without the tag that makes it a raw declines instead of
  guessing, and the caller keeps reading it whole. So do a file whose own orientation or Adobe RGB
  colour space only the whole-file path can show, while one that states sRGB stays on the fast path.
- `crates/raw/tests/sparse_corpus.rs`: `preview_plan` is asked for **every** corpus file, not only
  the ones whose header plan is `Ready`, and the preview it yields must equal the whole file's with
  the skipped bytes `0` and `0xFF`. The test prints what each format reads.
- `crates/engine/tests/preview_read.rs` (opt-in, `#[ignore]`): the same measurement on a **real**
  file, typically on a share:

  ```sh
  LC_PREVIEW_RAW="/mnt/nas/…/DSCF0001.RAF" \
    cargo test --release -p lightcraft-engine --test preview_read -- --ignored --nocapture
  ```

  No raw fixture is committed (media never is, see `AGENTS.md`).

A network share is still slower than a local disk — the preview itself is 5.6 MB — and the sensor
data still has to come over the wire for the *developed* picture, which is a separate path
(`fs_pair_loader`, the full decode). This change removes the preview's share of it, nothing more.
