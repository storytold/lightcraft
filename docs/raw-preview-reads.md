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
  the JPEG. A preview colour space of **Adobe RGB** is read from the planned buffer and handed on
  (`PreviewRead::adobe_rgb`), and the quick decode applies it as `decode_raw_preview` does (issue
  #1440: a Sony α1 writes Adobe RGB previews, which until then sent every one of them to the
  whole-file read — 70 MB for an 8 MB JPEG); a container that says sRGB needs nothing, since that is
  what the decode assumes anyway. The file's own EXIF orientation, when the preview JPEG carries none
  of its own and the bounded header cannot see it, still declines and the whole file keeps the
  preview;
- a format the planner cannot describe — NEF, ORF and PEF probe from samples, CRW/MRW/X3F are found
  by scanning — a plan that does not fit the buffer or wants more than 60 % of the file (the bound the
  import path's own partial read uses), or a window that does not decode all answer `None`, and the
  loader reads the file whole exactly as before: the plan can cost bytes but never the picture.

Measured over the CC0 corpus (111 raws, `cargo xtask corpus --download`): **52 files take the plan
route**, reading a median **7.8 %** of their bytes (0.9 %–27.7 %) — by format RW2 3.3 %, Leica RWL
4.1 %, ARW 7.5 %, CR3 8.4 %, DNG 9.7 %, CR2 14.3 % — while the 21 RAFs stay on the header route
(median 4.5 %). The remaining 38 read whole: NEF, NRW, ORF, PEF and Samsung SRW because the planner
does not describe them (the first four say so; an SRW plans to the whole file, and the 60 % budget
then keeps the whole-file read), and the ARWs, CR2s, CR3s and DNGs that declared Adobe RGB (these take
the plan route since #1440; the counts here are from before) or whose plan wants more than that budget. Across the whole corpus the plan handed on **the same preview as
the whole file, every time** — and wrote it down: `cargo test -p lightcraft-raw --release --test
sparse_corpus -- --nocapture` prints these numbers. `LIGHTCRAFT_PROFILE=1` says which route answered:

```
[profile] preview …/arw-sony-a7rm4a-compressed.arw: read 688128 of 61832192 bytes (plan, window 483441)
```

### Reading only the preview that covers the display

A display of `max_edge` pixels decodes the smallest embedded preview that covers it
([below](#the-smallest-jpeg-that-covers-the-box)): a 512 px grid thumbnail or content-index picture of a Sony α1
ARW uses its 1616 × 1080 JPEG, not the 8640 × 5760 one. The planned read reads only that one:
`sparse::preview_plan_covering(bytes, loaded, min_edge)` reads the header ranges with every large preview
skipped but its first 64 KiB (the strips of preview IFDs, and the `JPEGInterchangeFormat` blobs where a Sony
ARW keeps its full-size JPEG), then the head of every candidate up to its frame header, which is what the
choice looks at, and then only the preview it takes. A CR3 or a RAF carries one preview and gets
`preview_plan`. Measured on real files, with the bytes the planned read took and its pixels compared with
the whole-file path's:

| File | at 512 px (content scan) | at 2048 px (face scan) |
|---|---|---|
| Sony α1 ARW, 74.1 MB, Adobe RGB | 0.75 MB (1.0 %) | 7.8 MB (10.5 %) |
| Sony A7S III ARW, 25.5 MB | 0.48 MB (1.9 %) | 1.16 MB (4.5 %) |

The same check over 17 more files of that library (9 ARWs from five bodies, 8 DNGs: drone, panorama and
merged HDR files), gave the whole-file path's pixels for every one, reading 0.4 % to 11.3 % of each file.

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
- Whether the embedded JPEG can actually be read is only known when the quick job runs, so
  `Session::quick_thumb_job` carries the real thumbnail job as its last resort: a raw whose preview
  the loader can't read renders the raw instead of leaving the cell empty (issue #1448).
- The thumbnail key still changes with the settings hash, so editing a photo re-renders its
  thumbnail through the ordinary background path.

Measured on the real folder: idle for a minute, the app read 12 previews (**26.9 MB**) and decoded
**zero** raws for thumbnails, against 638 MB of whole-file reads for the same 12 photos before. The
render queue stayed empty.

## The smallest JPEG that covers the box

Many TIFF-based raws keep more than one embedded JPEG. A Sony A7R VI ARW holds a 1616 × 1080 one
(0.3 MB) beside the 9984 × 6656 one (5.6 MB), and Nikon NEF and Panasonic RW2 files have mid-sized
copies too. The loupe stand-in and the grid thumbnail (`embedded_preview_srgb`) decode the smallest
JPEG whose long edge is at least the size asked for (`lightcraft_raw::embedded_preview_covering`).
They take the largest one when no other JPEG is big enough. The plan route picks the same way
(`preview_window` takes the size asked for), so a raw read through the plan gets the JPEG the whole
file would give, and reads only that one ([above](#reading-only-the-preview-that-covers-the-display)). A smaller JPEG only counts as a copy of the picture when its aspect ratio is within
1 % of the largest one's and it has the same number of colour components. That rules out letterboxed
thumbnails of another shape and the grey masks an iPhone ProRAW DNG keeps at the picture's shape.
Sources for editing (an unsupported raw's JPEG, `load_embedded_preview`) and the camera-look fit still
use the largest JPEG, so their geometry and colour do not depend on the size level.

## Tests

- `crates/raw/src/preview.rs`: the header-only range matches `embedded_preview` byte for byte; a
  clamped prefix, a padded extent, a late pointer, a lying pointer and a non-JPEG window are all
  covered. `a_stand_in_takes_the_smallest_covering_jpeg_of_the_same_shape` covers the picker: a
  smaller JPEG that covers the box is taken, the largest when none does, and a thumbnail of another
  shape or a grey mask never is.
- `crates/engine/src/files.rs`: on a synthetic RAF with a 200 KB tail, the quick path and the
  whole-file path produce identical pixels, and the read stays under half the file. On a synthetic
  TIFF/DNG whose preview only its tags locate — the plan path — the same identity holds and the CFA
  strip is not read; the same structure without the tag that makes it a raw declines instead of
  guessing, and the caller keeps reading it whole. So does a file whose own orientation only the
  whole-file path can show. One that states Adobe RGB takes the plan route and shows the same pixels
  as the whole-file path (the space changes them: decoded as sRGB they differ), and one that states
  sRGB stays on the fast path. A synthetic DNG with a full-size and a half-size JPEG gets, on the plan
  path, the half-size one for a box it covers and the full-size one otherwise, with the same pixels as
  the whole-file path (`preview_loading_plans_the_smallest_covering_jpeg`).
- `crates/raw/src/sparse.rs`: on a raw with a full-size JPEG beside a small one, `preview_plan_covering`
  reads the small one and not the big one for 512 px and the big one past the small one's size, and the
  buffer gives `embedded_preview_covering`'s whole-file answer with the skipped bytes `0` and `0xFF`; a
  candidate whose frame header sits past the kept 64 KiB is read up to it before the choice.
- `crates/engine/src/files.rs`: the content scan's loader reads a fixture's small preview and not its big
  one, with the whole-file path's pixels; asked for more, it reads the big one; a file the planner cannot
  describe is read whole.
- `crates/raw/tests/it/sparse_corpus.rs`: `preview_plan_covering` at 512 and 2048 px must hand on the whole
  file's choice for every corpus file it answers for. `preview_plan` is asked for **every** corpus file, not only
  the ones whose header plan is `Ready`, and the preview it yields must equal the whole file's with
  the skipped bytes `0` and `0xFF`. The test prints what each format reads.
- `crates/engine/tests/it/preview_read.rs` (opt-in, `#[ignore]`): the same measurement on a **real**
  file, typically on a share:

  ```sh
  LC_PREVIEW_RAW="/mnt/nas/…/DSCF0001.RAF" \
    cargo test --release -p lightcraft-engine --test it preview_read:: -- --ignored --nocapture
  ```

  No raw fixture is committed (media never is, see `AGENTS.md`).

A network share is still slower than a local disk — the preview itself is 5.6 MB — and the sensor
data still has to come over the wire for the *developed* picture, which is a separate path
(`fs_pair_loader`, the full decode). This change removes the preview's share of it, nothing more.
