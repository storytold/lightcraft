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
(`warn,lcc_pipeline=trace`).

## The app shows an error instead of doing something

That's by design: a damaged file, a bad argument or a full disk produces an error you can act on, never a crash,
and your edits are kept. If a command fails from a script, `<cli> run … --keep-going` continues past it and the
JSON line for each command says why it failed. If the app does crash, please report it with the log: that is a bug.

## Blank, black or slow previews

- GPU problems: start with `<PREFIX>_GPU=0` (CPU only) to confirm, or pick a backend with
  `<PREFIX>_GPU_BACKEND=vulkan|metal|dx12|gl` (`off` also disables the GPU). Settings has a GPU rendering switch too.
- The window stopped drawing and the app ended with "its window stopped drawing: the graphics driver lost the …
  renderer's GPU device": the library and settings were saved first. From the next start the window uses the next
  renderer (Windows: Vulkan → DX12 → GL → OpenGL) and keeps trying the lost one last; `<PREFIX>_GPU_BACKEND=dx12`
  (or `vulkan`, `gl`, `glow`) still forces one. Deleting `window-lost` in the settings folder forgets the loss.
- `<PREFIX>_PROFILE=1` prints per-stage pipeline timings to standard error.
- Library → Previews → Clear Preview Cache rebuilds thumbnails and previews.
- A very large library on a network share or hard disk opens much faster since 26.46.2; culling stays responsive while previews are prepared in the background, and a raw's first picture is its camera JPEG.

## A raw file looks wrong or doesn't open

Colour for most raw formats is fitted from the camera's own embedded JPEG; a file without one, or a model that has
not been verified, can look muted. Check the camera's row in [parity-checklist.md](../parity-checklist.md)
(`LR-IMP-CAMERA-COVERAGE`, `LR-PROF-CAMERACOLOR`) and open an issue with the camera model; a CC0 sample file helps
most. `<cli> calibrate` can fit a profile for your camera from your own raws and their JPEGs.

## Photos are missing

Library → Find Missing Photos… lists photos whose originals can't be found (a disconnected drive, a renamed folder)
and relinks them. Their edits and metadata are safe in the catalog meanwhile. A folder that can't be read (permissions, a
disconnected share) shows the reason in the grid instead of an empty "No photos in this folder".

Library → Validate DNG Files checks the selected DNGs' raw data against their checksum and lists damaged files.

## The catalog won't open

- After an update the app first shows *Opening your catalog* while it makes and checks a safety copy of the
  catalog (the progress bar moves by the copy's bytes); on a large catalog this takes a little while and is
  normal, not a hang. A catalog that still can't be opened ends at the usual window offering Try Again.
- "This library is already open in …": another running program holds the library's lock (the message names the
  process and computer). Close it there. A `catalog.lock` file left by a crash doesn't block anything: the
  operating system releases the lock when the process ends.
- File → Test Catalog Integrity checks the store; File → Restore Catalog from Backup… or opening a copy from
  `backups/` gets you back to the last good state.

## Updating the app

Help → Check for Updates… reads the project's release feed and, when something newer exists, offers to
download it for this system and install it; the app restarts afterwards. Things that can go wrong:

- "Could not reach the release server" or "the release server answered 403": no internet, a proxy, or GitHub's
  rate limit. The check uses the release's `latest.json` first, which usually still works; try again later.
- "Release X has no download for this system": that release has no file for this platform (a platform added
  after it was published, or one we don't package). Get it from the project's releases page, which the dialog
  links to.
- Before anything is downloaded, the app checks that the release's checksum list (`SHA256SUMS.txt`) is
  signed with the project's update key; the download must then match the size the release states and the
  SHA-256 that signed list records. "The update could not be verified, so it was not installed: …" says
  which check failed (the list or its signature is missing, it was signed with another key, or it doesn't
  list the file); a file that fails the check is deleted and nothing is installed. Get that version from
  the releases page instead, and tell the project.
- "This build can't verify updates": the build was made without an update key (a fork or a build from
  source). It still tells you when a new version is out; download it from the releases page.
- An update installed through the Windows installer (`.msi`): the app quits, the installer runs and
  the app starts again when it is done. When the installer wants a Windows restart as part of it
  (rare), the app starts after that restart instead. Start-menu shortcuts — and taskbar entries
  pinned from them — keep working across updates.
- A Windows copy unpacked from the portable zip (with or without `portable.txt`) updates itself: the new zip is
  unpacked beside the program, its files replace the copy's own, and the new version starts once the app has quit.
  Only the zip's files are replaced; `portable.txt`, the portable data folder and anything else you put there stay.
  - If you cancel quitting (a task still running, say), the new version waits and starts when the window does
    close. A copy you start by hand meanwhile is already the new version.
  - The previous files are kept as `<file>.update-old` until the new version is up. If it doesn't come up (it
    stops before its window shows), its next start puts the previous files back and starts the previous version;
    the new version's files are then deleted as `<file>.update-failed`. Starting the app again while the new
    version is still coming up (a large library opening) doesn't count as a failure.
  - The updater keeps its record of an update in `.<binary>-update.json` beside the program, and leaves a small
    hidden `.<binary>-update.json.lock` there for good.
  - The restart doesn't carry over the command-line arguments the app was started with (`--library`, files to
    import), as after the MSI: the library last used opens.
  - A copy under Program Files, or in a folder it can't write to, gets the zip and its folder shown instead. A copy
    the MSI installed (Windows Installer is asked, for every product that has the app's component) always updates
    through the MSI.
- A tarball (Linux riscv64, FreeBSD) is downloaded and its folder shown: quit the app and unpack it over the
  installation yourself.
- "Check for updates when the app starts" is off until you turn it on (Settings → General, the welcome dialog
  of a first start, or the update dialog); Help → Check for Updates… always works by hand. The setting lives in
  the app's own settings folder (`updates.json`), not in a catalog. Runs that keep no settings (`--memory`,
  `<PREFIX>_NO_PREFS`) and headless sessions (`snapshot`, MCP) never check at start.

## Chinese or Japanese text shows boxes

The build was made without craft-fonts. Release builds always include it; for source builds set
`CRAFT_FONTS_DIR` (see [getting-started.md](getting-started.md)).

## Tethered capture doesn't import

The session watches a folder; a shot is imported once its size stops changing between two scans. Check
`tether.status` (the folder, `active`), and that the camera software saves to that folder. Existing shots are
skipped when the session starts unless you turn that off ([tethering.md](tethering.md)).

## Network features

Nothing reaches the network on its own: the update check runs once, a few seconds after start-up, only when
"Check for updates when the app starts" is ticked (off by default — see above). Map tiles, reverse geocoding,
model downloads, Immich, SFTP upload and IPP printing all use the network too, and each asks or is configured
explicitly. Behind a proxy or offline, the rest of the app works.
