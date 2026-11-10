# Nikon High Efficiency NEF through Adobe DNG Converter

Nikon's High Efficiency (HE / HE★) NEFs store the sensor data with intoPIX TicoRAW, a licensed codec with no public
description. LightCraft can't decode it, so these files open from the camera's embedded JPEG ("Preview only"; issues
#193, #1334, #2169). Lossless compressed NEFs from the same cameras decode as usual.

Adobe's free DNG Converter has a licence for the codec. When it is installed, LightCraft can run it as a separate
program and develop the DNG it writes (issue #3921). LightCraft does not bundle, link or read any part of the
converter. It only starts the program the user installed, with Adobe's documented command-line options.

## How it works

- A photo qualifies when it is a NEF whose preview-only reason is the High Efficiency one
  (`lightcraft_raw::is_nef_high_efficiency`) and the catalog has its content hash. A virtual copy is not converted
  and shows no Convert button: select its original photo.
- LightCraft runs `<converter> -c -p0 -d <temp folder> -o <name>.dng <file.nef>`: a lossless compressed DNG with no
  embedded preview, written into a temporary folder inside the cache. One batch converts up to four files at once.
  In the app, a Convert click in the Preview only notice waits until the batch that runs is done; a
  `photo.convertHighEfficiency` request starts its own batch for the photos that are not converting already. A
  conversion that runs longer than the timeout (120 s by
  default) is stopped.
- The DNG must be a DNG that LightCraft decodes (at most 256 MB) before it moves into the cache. A failed, stopped
  or timed-out conversion leaves nothing behind, and the photo keeps its embedded preview. Temporary folders left by a
  process that ended during a conversion are removed when the next conversion starts.
- The cache is the operating system's cache folder: `~/Library/Caches/LightCraft/dng-cache` on macOS,
  `%LOCALAPPDATA%\LightCraft\dng-cache` on Windows, `$XDG_CACHE_HOME/lightcraft/dng-cache` (or
  `~/.cache/lightcraft/dng-cache`) elsewhere; `$LIGHTCRAFT_DNG_CACHE` overrides it. A file's name holds a hash of the
  photo's content hash (the hash of the whole NEF that import already computes), so a moved or renamed NEF keeps its
  DNG, a changed one is converted again, and finding a DNG never reads the NEF. Every library on the computer shares
  the cache.
- After a conversion the photo is read again as Reload from Disk does (one undo step). Its size, as-shot white
  balance and lens data come from the DNG, it is no longer preview only, and the catalog marks it as developed from
  its DNG (`Photo::converted_dng`, stored in the existing `preview_checked` field, so the catalog format is
  unchanged). Only a photo marked that way is developed from the DNG; undoing the reload puts it back on its embedded
  preview. Its render and thumbnail keys change, so nothing rendered from the embedded JPEG is shown for it, and its
  smart preview is removed and made again when needed.
- The NEF stays the photo's file: metadata, XMP sidecars, the content hash and moves or renames all use the NEF. The
  NEF is never written.
- Edits made while the photo showed its embedded JPEG are kept, but they may look different on the raw data.
- A DNG takes about 1.7 × the NEF's space. Deleting the cache (Settings ▸ Performance, `library.clearDngCache`) puts
  the photos back on their embedded preview.

## Settings and commands

| Where | What |
|---|---|
| Settings ▸ Performance ▸ Nikon High Efficiency NEF | **Convert with Adobe DNG Converter when opened** (off by default), the converter's location (empty: found in the default place), whether it was found, the cache folder, and the converted DNGs with a Delete button |
| Preview only notice (Edit, Info) | **Convert with Adobe DNG Converter** when the converter is installed; "Converting with Adobe DNG Converter…" or "Waiting for Adobe DNG Converter…" while it runs; the reason when it failed; a hint to install it when it is missing |
| `app.dngConverter` | `{auto?, path?, timeoutSeconds?}` → the settings, `found` (the program that will run), `defaultPaths`, `cacheDir`, `cachedFiles`, `cachedBytes` |
| `photo.convertHighEfficiency` | `{ids?, wait?}`: converts the targeted HE NEFs and reads them again. In the app it runs in the background (activity row `dngConvert`, cancellable) unless `wait: true` |
| `library.clearDngCache` | `{reload?}`: deletes the converted DNGs (the cache every library shares). For the converted photos of the open library only, it deletes their smart previews (except for a photo whose NEF is offline, which keeps its smart preview) and reads them again. Other libraries keep their smart previews and their converted marks; they show the embedded preview again after Reload from Disk → `deletedFiles`, `freedBytes`, `failedFiles` (DNGs that could not be deleted), `converted`, `reloaded`. With `reload: false` nothing is read again: the catalog still marks those photos as converted until the caller runs `photo.reload` on the `converted` ids (the app does that in the background) |

The settings belong to one process. The app applies its saved settings at start. A headless `lightcraft-cli`
command or MCP server starts with the defaults (automatic conversion off, the converter looked for in its default
place) and needs its own `app.dngConverter` call to use another path.

Default locations: macOS `/Applications/Adobe DNG Converter.app/Contents/MacOS/Adobe DNG Converter` (also under
`~/Applications`), Windows `C:\Program Files\Adobe\Adobe DNG Converter\Adobe DNG Converter.exe`. On macOS the setting
also takes the `.app` itself. Adobe makes no Linux version; there is no default location there, but the setting can
name any program that takes the same options.

Automatic conversion is off by default: the converter is another vendor's program, it uses disk space, and the user
should choose to run it. When it is on, a photo is converted when it is opened in Edit, once a session (not when it
is only selected in the grid).

## Limits

- The DNG carries the colour data Adobe's converter writes (its matrices and, when present, its profile). LightCraft
  reads them from the user's file at run time, as for any DNG; none of it enters this repository.
- Photo Merge, Denoise and Convert to DNG still read the NEF itself, so they see only its embedded preview.
- The cache has no size limit yet; Delete empties it.
- On macOS each conversion can show the converter's icon in the Dock while it runs. If the converter waits in a window
  of its own (an update or licence prompt), the conversion stops at the timeout: open the converter once by itself.
- There is no conversion during import yet.
