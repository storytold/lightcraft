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

Formats whose preview pointer is not in the header (CR3 scans boxes; a TIFF pointer tag can sit in
an IFD the prefix cuts short; CRW/MRW/X3F are found by scanning) keep the whole-file path, so
nothing regresses: `preview_window` returns `None` and the loader reads the file as before. A pointer
that lies, a short read or a file that changes underneath us ends in the same fallback — the user's
preview is never lost to a damaged header.

`LIGHTCRAFT_PROFILE=1` prints what each preview cost:

```
[profile] preview …/DSCF9492.RAF: read 5857257 of 59277632 bytes (header 5857257, window 5857109)
```

## Tests

- `crates/raw/src/preview.rs`: the header-only range matches `embedded_preview` byte for byte; a
  clamped prefix, a padded extent, a late pointer, a lying pointer and a non-JPEG window are all
  covered.
- `crates/engine/src/files.rs`: on a synthetic RAF with a 200 KB tail, the quick path and the
  whole-file path produce identical pixels, and the read stays under half the file.
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
