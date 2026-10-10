# Troubleshooting

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

## Where things are

| What | Where |
|---|---|
| App settings, keymap, window layout | `<settings_dir>` in your configuration folder: `~/.config/<settings_dir>` (Linux, or `$XDG_CONFIG_HOME`), `~/Library/Application Support/<settings_dir>` (macOS), `%APPDATA%\<settings_dir>` (Windows) |
| Log file | `logs/<binary>.log` in that folder; the two previous runs are kept as `<binary>.1.log` and `.2.log`, so a crashed run's log survives the next start |
| Your library | the folder shown by `<cli> run --library DIR catalog.info` (default `<library_default>` under Pictures) |
| Catalog backups | `backups/` in the library, or the folder set in Edit → Catalog Settings… |

Attach the log to a bug report. Set `<PREFIX>_LOG=debug` for more detail, or `RUST_LOG` for per-module levels
(`warn,dac_pipeline=trace`).

## The app shows an error instead of doing something

That's by design: a damaged file, a bad argument or a full disk produces an error you can act on, never a crash,
and your edits are kept. If a command fails from a script, `<cli> run … --keep-going` continues past it and the
JSON line for each command says why it failed. If the app does crash, please report it with the log: that is a bug.

## Blank, black or slow previews

- GPU problems: start with `<PREFIX>_GPU=0` (CPU only) to confirm, or pick a backend with
  `<PREFIX>_GPU_BACKEND=vulkan|metal|dx12|gl` (`off` also disables the GPU). Settings has a GPU rendering switch too.
- `<PREFIX>_PROFILE=1` prints per-stage pipeline timings to standard error.
- Library → Previews → Clear Preview Cache rebuilds thumbnails and previews.

## A raw file looks wrong or doesn't open

Colour for most raw formats is fitted from the camera's own embedded JPEG; a file without one, or a model that has
not been verified, can look muted. Check the camera's row in [parity.md](../parity.md)
(`LR-IMP-CAMERA-COVERAGE`, `LR-PROF-CAMERACOLOR`) and open an issue with the camera model; a CC0 sample file helps
most. `<cli> calibrate` can fit a profile for your camera from your own raws and their JPEGs.

## Photos are missing

Library → Find Missing Photos… lists photos whose originals can't be found (a disconnected drive, a renamed folder)
and relinks them. Their edits and metadata are safe in the catalog meanwhile.

## The catalog won't open

- "This library is already open in …": another running program holds the library's lock (the message names the
  process and computer). Close it there. A `catalog.lock` file left by a crash doesn't block anything: the
  operating system releases the lock when the process ends.
- File → Test Catalog Integrity checks the store; File → Restore Library from Backup… or opening a copy from
  `backups/` gets you back to the last good state.

## Chinese or Japanese text shows boxes

The build was made without craft-fonts. Release builds always include it; for source builds set
`CRAFT_FONTS_DIR` (see [getting-started.md](getting-started.md)).

## Tethered capture doesn't import

The session watches a folder; a shot is imported once its size stops changing between two scans. Check
`tether.status` (the folder, `active`), and that the camera software saves to that folder. Existing shots are
skipped when the session starts unless you turn that off ([tethering.md](tethering.md)).

## Network features

Map tiles, reverse geocoding, model downloads, Immich, SFTP upload and IPP printing are the only things that use
the network, and each asks or is configured explicitly. Behind a proxy or offline, the rest of the app works.
