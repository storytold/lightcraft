<p align="center">
  <img alt="" src="brand/logo.svg" width="120">
</p>

<h1 align="center">Your photos. Your pixels. Your machine.</h1>

<p align="center">
  <b>Photo library and raw development; an open-source, clean-room reimplementation of Adobe Lightroom, rebuilt in pure Rust.</b><br>
  Native on macOS, Windows and Linux. In the browser via WebAssembly. Drivable end to end by AI agents over MCP.
</p>

> [!WARNING]
> **Please don't contribute yet.** This fork was published on 2026-10-10 and its initial planning and restructuring
> are still under way, so pull requests and issues filed now are likely to conflict with work in progress. Please
> wait about 18 hours, until 2026-10-11, before contributing.

> [!NOTE]
> The product name, binary names, settings folder and environment-variable prefix are set in one place,
> [`brand.toml`](brand.toml). This README calls the product "the app"; `<app>`, `<binary>`, `<cli>`, `<mcp_server>`
> and `<settings_dir>` stand for `display_name`, `binary`, `cli_binary`, `mcp_server` and `settings_dir`, and
> `<PREFIX>` for `env_prefix`, from that file.

<p align="center">
  <a href="#edit-like-you-mean-it">Editing</a> ·
  <a href="#color-grading-the-cinematic-way">Color grading</a> ·
  <a href="#before--after">Before &amp; after</a> ·
  <a href="#masking-that-goes-where-you-point">Masking</a> ·
  <a href="#presets-profiles--the-color-mixer">Presets</a> ·
  <a href="#organize-everything">Library</a> ·
  <a href="#built-for-agents">Agents &amp; MCP</a> ·
  <a href="#fast-native-private">Performance</a> ·
  <a href="#feature-status">Status</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="ROADMAP.md">Roadmap</a> ·
  <a href="#downloads">Downloads</a>
</p>

<br>

## Edit like you mean it

The app is a complete darkroom in a single native app. Every adjustment is **non-destructive**, so your originals
are never touched. Every slider renders through a **scene-referred, wide-gamut, 32-bit float pipeline**: highlights
roll off like film, shadows open up without halos, and colour stays clean from capture to export.

<table>
<tr>
<td width="50%" valign="top">

### ☀️ Light
**Exposure, Contrast, Highlights, Shadows, Whites, Blacks**, with edge-aware local tone mapping (a guided filter on
log-luminance). Pulling −100 Highlights recovers a blown sky without the grey halos you'd get from a naive curve.

### 🎨 Color
**White balance** by temperature and tint (Kelvin for raw, relative for JPEG) with presets, Auto and a
click-to-neutralise **eyedropper**. **Vibrance** that protects skin tones, **Saturation**, an 8-band **Color Mixer**
(hue / saturation / luminance) and 3-way **Color Grading** wheels with blending and balance. All of it is computed in
OkLCh, a modern perceptual colour space.

</td>
<td width="50%" valign="top">

### ✨ Effects
**Texture** for fine detail, **Clarity** for mid-tone punch, **Dehaze** (dark-channel prior with guided refinement;
push it negative to add atmosphere), post-crop **Vignette** with highlight priority, roundness and feather, and
resolution-independent film **Grain** with size and roughness.

### 📈 Tone Curve
Parametric region curve with movable splits **plus** point curves for RGB, Red, Green and Blue. Curves are monotone by
construction, so you never get an accidental tone inversion.

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/curve-tetons.jpg" alt="The Tone Curve open under the Light panel, with a gentle S-curve on the RGB point curve applied to The Tetons and the Snake River" width="100%">
  <br>
  <sub>A gentle S on the RGB point curve, right under the Light sliders. <i>Ansel Adams, 1942 (public domain).</i></sub>
</p>

<br>

## Color grading, the cinematic way

Split-tone shadows, midtones and highlights independently with drag-anywhere colour wheels. Below, Dorothea Lange's
*Migrant Mother* gets a warm, print-like tone (highlights at 42°, shadows at 28°) in two drags.

<p align="center">
  <img src="docs/images/grading-migrant-mother.jpg" alt="Color Grading wheels for midtones, shadows and highlights next to Dorothea Lange's Migrant Mother, toned warm like a print" width="100%">
  <br>
  <sub>Shadows, midtones and highlights wheels in the Color panel. <i>Dorothea Lange, "Migrant Mother" (1936). Public domain, Library of Congress.</i></sub>
</p>

<br>

## Before & after

Hold <kbd>\\</kbd> to peek at the original, press <kbd>Y</kbd> for side by side, or <kbd>Shift</kbd>+<kbd>Y</kbd> for a
split view. Every image below is a real screenshot of the app, captured automatically by an agent through the
[control channel](#built-for-agents).

<table>
<tr>
<td width="50%"><img src="docs/images/ba-tetons.jpg" alt="Side-by-side before and after of The Tetons and the Snake River: the after has deeper clouds and more open shadows along the river"><br><sub><b>The Tetons and the Snake River.</b> Highlights −45, Shadows +38, Clarity +28, Dehaze +18. <i>Ansel Adams, 1942 (public domain).</i></sub></td>
<td width="50%"><img src="docs/images/ba-migrant-mother.jpg" alt="Side-by-side before and after of Migrant Mother: the after is warmer, with lifted shadows"><br><sub><b>Migrant Mother.</b> Shadows +42, Texture +18, split-toned grade, vignette. <i>Dorothea Lange, 1936 (public domain).</i></sub></td>
</tr>
<tr>
<td width="50%"><img src="docs/images/ba-earthrise.jpg" alt="Side-by-side before and after of Earthrise: the Earth over the lunar horizon, the after slightly warmer and richer"><br><sub><b>Earthrise.</b> Dehaze +22, Highlights −30, warmer white balance, Vibrance +22. <i>NASA / Bill Anders, Apollo 8, 1968 (public domain).</i></sub></td>
<td width="50%"><img src="docs/images/ba-blue-marble.jpg" alt="Side-by-side before and after of The Blue Marble: the after has deeper blacks and firmer cloud detail"><br><sub><b>The Blue Marble.</b> Highlights −38, Blacks −20, Dehaze +15, Vibrance +30. <i>NASA, Apollo 17, 1972 (public domain).</i></sub></td>
</tr>
</table>

<br>

## Masking that goes where you point

Paint with a **Brush** (size, feather, flow, density, erase), drop **Linear** and **Radial Gradients** with draggable
pins, or select by **Luminance Range**, **Color Range**, **Sky**, **Subject** and **Background**. Combine components
with **Add / Subtract / Intersect**, invert any of them, and dial in 15 local adjustments per mask (Temp, Tint,
Exposure, Contrast, Highlights, Shadows, Whites, Blacks, Texture, Clarity, Dehaze, Hue, Saturation, Sharpness, Noise)
plus an overall Amount.

<p align="center">
  <img src="docs/images/masking.jpg" alt="Masking panel with a linear Sky mask and a radial Sun glow mask; the radial gradient is drawn as a red overlay around the sun on a lake scene" width="100%">
  <br>
  <sub>A radial "Sun glow" mask (Temp +40, Exposure +0.50) layered over a linear "Sky" mask. <i>Photo from the app's procedurally generated demo library.</i></sub>
</p>

<br>

## Presets, profiles & the Color Mixer

Eighteen hand-built presets ship in the box (*Golden Hour, Teal & Orange, Faded Matte, Selenium Tone, Crisp
Landscape* and more), each with an **Amount** slider from 0 to 200 %. Save your own from any group of settings, mark
favourites, and copy, paste or sync edits across a whole selection with exactly the groups you choose.

<p align="center">
  <img src="docs/images/presets-mixer.jpg" alt="Presets column grouped into B&amp;W, Color, Film, Landscape, Portrait and Style, beside the Color panel with the 8-band Color Mixer open on purple" width="100%">
  <br>
  <sub>The Presets column next to the 8-band Color Mixer. <i>Photo from the demo library.</i></sub>
</p>

<table>
<tr>
<td width="50%" valign="top">

### ✂️ Crop, straighten & geometry
Free or locked aspect ratios (1:1, 4:5, 5:7, 2:3, 4:3, 16:9, 16:10, original), drag-to-rotate straightening that always
keeps the largest crop inside the image, thirds / grid / golden-ratio overlays, flips and 90° rotations, plus
manual Vertical, Horizontal, Rotate, Aspect, Scale and Offset transforms.

</td>
<td width="50%" valign="top">

### ⚫️ Black & White
One click (<kbd>V</kbd>) to monochrome, with an 8-band **B&W Mix** that lets you darken skies or make foliage glow.
Then tone it with Color Grading for selenium, sepia or split-tone prints.

</td>
</tr>
<tr>
<td width="50%"><img src="docs/images/crop-tetons.jpg" alt="Crop tool on The Tetons and the Snake River: a 16 by 9 crop rotated slightly, with a rule-of-thirds overlay and the Geometry sliders on the right"><br><sub>A 16:9 crop straightened +2.5° with the thirds overlay. <i>Ansel Adams, 1942 (public domain).</i></sub></td>
<td width="50%"><img src="docs/images/bw-split.jpg" alt="Split before and after view of sand dunes: color on the left, black and white with grain and vignette on the right"><br><sub>Split view: colour original on the left, B&W with Clarity, Vignette and Grain on the right. <i>Demo library.</i></sub></td>
</tr>
</table>

<br>

## Organize everything

A library that stays out of your way: **All Photos**, **Recently Added**, **Picks**, **By Date**, **Albums** nested in
**Folders**, and **Recently Deleted**. Rate with <kbd>0</kbd>–<kbd>5</kbd>, flag with <kbd>P</kbd> / <kbd>X</kbd> /
<kbd>U</kbd>, colour-label with <kbd>6</kbd>–<kbd>9</kbd>. Search understands fields:
`rating:>3 flag:pick iso:>800 camera:x2 date:2026-04 keyword:mountains`. Every view sorts by capture date,
import date, edit date, name, rating, size or at random (a stable shuffle; View → Sort → Reshuffle for a new one). The justified **Photo Grid** and **Square Grid** views are virtualized,
so they stay smooth whether you have forty photos or forty thousand.

<table>
<tr>
<td width="50%"><img src="docs/images/grid-demo.jpg" alt="Justified Photo Grid of 24 demo photos with ratings and flags, and a sidebar of nested albums under a Travel 2026 folder"><br><sub>Photo Grid with albums nested in a folder, ratings and flags. <i>Demo library.</i></sub></td>
<td width="50%"><img src="docs/images/grid-pd.jpg" alt="Square Grid showing the four public-domain showcase photos with star ratings and pick flags"><br><sub>Square Grid of the four public-domain showcase photos.</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/images/info-earthrise.jpg" alt="Info panel for Earthrise showing file name, 2400 by 2400 JPEG dimensions, rating, title field and camera metadata"><br><sub>The Info panel: file, dimensions, rating, title, caption, copyright and camera metadata. <i>NASA / Bill Anders, "Earthrise", Apollo 8, 1968 (public domain).</i></sub></td>
</tr>
</table>

<br>

## Built for agents

Every menu item, slider, brush stroke, crop handle and keystroke in the app is a **command** with a stable id and
JSON parameters. The UI, the keyboard, the CLI, a JSON-lines control channel and an **MCP server** all dispatch
through the same entry point. An agent can cull a shoot, develop it, mask a sky and export it, and *see* the result.

```sh
<binary> --control 7980 ~/Pictures/trip
```
```jsonc
{"method": "engine.execute", "params": {"command": "photo.flag",  "params": {"flag": "pick"}}}
{"method": "engine.execute", "params": {"command": "develop.set", "params": {"values": {"light.highlights": -45, "light.shadows": 38}}}}
{"method": "engine.execute", "params": {"command": "mask.add",    "params": {"kind": "radial", "center": [0.62, 0.4], "rx": 0.2, "ry": 0.14}}}
{"method": "ui.clickWidget",   "params": {"id": "slider:effects.clarity"}}       // drive any widget by name
{"method": "ui.pointer",       "params": {"events": [{"kind":"down","x":0.2,"y":0.3}, {"kind":"up","x":0.4,"y":0.3}]}}
{"method": "ui.screenshot",    "params": {"path": "after.png"}}
```

- **Command registry.** `engine.commands` lists the available commands; `develop.controls` lists every slider's
  range, default and current value.
- **MCP server.** `<cli> mcp` exposes the command registry to Claude (or any MCP client), alongside
  helpers for import, query, develop, mask, render (returned as an image) and export. It runs headless, or attached
  to the running app with screenshots, clicks and gestures. See [docs/mcp.md](docs/mcp.md).

  ```sh
  cargo build --release -p dac-cli
  claude mcp add <mcp_server> -- "$PWD/target/release/app-cli" mcp ~/Pictures/shoot          # headless
  claude mcp add <mcp_server>-app -- "$PWD/target/release/app-cli" mcp --connect 127.0.0.1:7980  # live app
  ```
- **Scriptable CLI:** `<cli> run --import in.dng develop.set control=light.exposure value=0.7 app.export
  path=out.jpg longEdge=2048` runs any chain of commands (headless, on a saved library, or against the running app)
  and prints one JSON result per command; `<cli> render in.dng -o out.jpg --set light.exposure=0.7 --preset …`.
- **Undo for everything**, including agent actions: a slider drag (or a scripted burst of updates) is one undo step.
- **Contact sheets (native app / CLI):** select photos, then **File → Contact Sheet PDF…**. Choose A4 or Letter, landscape,
  rows/columns and filename captions; save a paginated PDF and print it from your PDF viewer. Exports use the
  current edits, fit each photo without further cropping, and render at 150 dpi in sRGB. The background export
  supports cancellation and protects originals and XMP sidecars. From the CLI:

  ```sh
  dac-cli run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  `export.contactSheet` also accepts explicit `ids`, `landscape=true`, and `paper=letter`. It replaces an existing
  output only after the entire PDF succeeds. Limits: 1,000 photos, 100 pages, 256 MiB; captions are rasterized
  with the same font coverage as watermarks (CJK requires craft-fonts), with long names shortened in the middle.
  This is a contact-sheet workflow,
  not a printer-driver or custom picture-package layout editor.
- **Every widget is addressable** (`ui.widgets`) and clickable by name, so agents operate the real UI, not a
  side door.
- The screenshots in this README were produced end to end by the [`docs/showcase/`](docs/showcase/) scripts.
  Protocol reference: [docs/control-protocol.md](docs/control-protocol.md).

<br>

## Fast, native, private

- **Pure Rust, no C.** Our own RAW decoders (DNG, Canon CR2/CR3, Sony ARW, Nikon NEF, Fujifilm RAF incl. X-Trans,
  Panasonic RW2 / Leica RWL, Pentax PEF, Olympus ORF), our own colour science, our own pipeline. JPEG, PNG, TIFF, WebP,
  PSD composites and JPEG XL open today; HEIC/HEIF photos too with `--features heif` (HEVC is a build-time choice,
  as in PhotoCraft).
- **Scene-referred & wide-gamut.** Linear Rec.2020 float internally, Bradford-adapted white balance, gamut mapping
  instead of clipping, a filmic shoulder for raw and pixel-exact pass-through for JPEGs you haven't touched.
- **Resolution-independent edits.** Radii and brush sizes are relative to the image, so a 400 px preview, your
  5K display and a 60 MP export look the same.
- **GPU-accelerated, CPU-exact.** The whole develop pipeline runs as wgpu compute kernels (Metal / Vulkan / DX12),
  checked against the CPU pipeline to within 1/255. On a 24 MP raw (Apple M4 Pro): a slider update re-renders in
  ~4 ms, a cold 2.5 MP loupe in ~30 ms, and a full-size export in ~0.3 s including a parallel JPEG encode.
  Without a GPU the same pipeline runs on all CPU cores, redoing only the stages a slider affects. Batch exports
  render several photos side by side, so one's decode and encode overlap another's render.
- **Instant culling.** Opening a raw shows its embedded camera preview or cached render within ~0.1 s while the
  full render follows (~0.2–0.5 s for 24 MP). The next and previous photos are prepared in the background, so stepping
  through a shoot takes ~50 ms per photo.
- **Background rendering.** A worker pool renders the loupe, before/after and every visible thumbnail off the UI
  thread: drafts during drags, full quality on release.
- **Local-first.** No account, no cloud, no telemetry, no subscription. Your catalog is an append-only log of
  human-readable operations you can diff, back up or replay.

<br>

## Feature status

The app is young and moving fast. **Where we honestly stand** (details in the [roadmap](ROADMAP.md#where-we-stand)):

- **By feature count we're at ~79%** of Lightroom (core features 98%), tracked row by row in
  [docs/parity.md](docs/parity.md).
- **As a day-to-day Lightroom replacement we're nearer 60–70%.** It's great for JPEG/DNG and most Nikon / Sony /
  older-Canon raws on one machine.
- **The biggest gaps:**
  - **camera colour calibration:** Sony, Nikon, Panasonic, Fujifilm and Canon CR3 raws have guarded estimates from their camera JPEGs, with built-in ILCE-7CR, ILCE-7M4, X-H2S and X-T4 profiles; measured calibration is missing, and other raws or rejected fits retain a neutral matrix;
  - **compressed Olympus raws and unsupported CR3 variants:** these use embedded JPEG previews when present. Fujifilm lossless/lossy compressed RAF now decodes sensor data; [verification and existing-library reload instructions](docs/raf-compression.md);
  - **AI masks and denoise:** subject and sky selection are classical heuristics;
  - **on-screen HDR display, video and the Classic Print / Book / Map modules.**
- **What's next:** see [where we're going](ROADMAP.md#where-were-going).

| Area | Status |
|---|---|
| Library: albums, folders, smart albums, stacks (incl. auto-stack), virtual copies, ratings, flags, labels, filter bar, search, sort, grids, filmstrip | ✅ |
| Culling: Compare (synced zoom) and Survey views, auto-advance, instant previews | ✅ |
| Light, Color, Effects (vignette styles), Tone Curve (+ refine saturation, targeted adjustment), Color Mixer (+ targeted), Point Color, Color Grading, Calibration, B&W | ✅ |
| Masking: brush, linear, radial, luminance/colour range, add/subtract/intersect | ✅ (AI subject/sky use classical heuristics for now) |
| Crop, straighten tool + auto straighten, flip, rotate, aspect ratios, overlays | ✅ |
| Profiles (Color, Neutral, Vivid, Landscape, Portrait, Monochrome: our own looks), presets, versions, history, copy/paste/sync settings | ✅ |
| Camera colour: DNG files use their own matrices | ✅ DNG · 🟡 own Sony/Fujifilm profiles; measured calibration database missing |
| Native macOS menu bar (generated from the command registry), control channel + every widget addressable, headless UI snapshots | ✅ |
| RAW: DNG, CR2, CR3 (lossless CRX Bayer and version 0x100/0x200 C-RAW), ARW, NEF (uncompressed + lossless/lossy compressed), Fujifilm RAF (uncompressed + lossless/lossy compressed, Bayer + X-Trans), Panasonic RW2 / Leica RWL / Panasonic RAW (every raw format, DMC-LX1 to DC-S1RM2), Pentax PEF, Olympus ORF (uncompressed), Samsung SRW (uncompressed); embedded previews for every format incl. CR3 | 🟡 · CR3, compressed ORF and compressed SRW decode ⬜ |
| Detail: sharpening, luminance + colour noise reduction | ✅ · AI Denoise, Super Resolution ⬜ |
| Remove / Heal / Clone spots (auto source), Visualize Spots, Red Eye and Pet Eye (auto pupil detection, catchlight) | ✅ · content-aware fill, spot pin editing 🚧 |
| Export: JPEG / PNG / TIFF / WebP / AVIF / DNG / original, sizing, file-size limit, output sharpening, naming templates, batch, metadata policy, text or image watermark | ✅ |
| HDR: HDR editing with a headroom limit, SDR rendition, Visualize HDR; export as ISO 21496-1 gain map JPEG, PQ AVIF or 32-bit float TIFF | ✅ · HDR display ⬜ |
| Library persistence (crash-safe op log + snapshots, background compaction, failed saves reported), disk thumbnail cache | ✅ |
| Import: Add in place / Copy / Move, rename and folder templates, devices, duplicate detection, watched folders; Local folder browsing | ✅ |
| MCP server (headless or live app, persistent libraries), CLI, control channel | ✅ |
| XMP sidecars (read/write, auto-write), reading `crs:` develop settings, preset files (`.lcpreset`, XMP presets) | ✅ |
| Optics (distortion, vignetting, auto + manual CA, defringe, lens corrections embedded in DNG files and Panasonic / Leica RW2 / RWL distortion data), Geometry (transforms, Constrain Crop), Upright (Auto/Level/Vertical/Full/Guided) | ✅ · camera lens profiles (our own) ⬜ |
| Photo Merge: HDR (auto-align, deghost), Panorama (spherical/cylindrical/perspective, boundary warp, auto crop), HDR Panorama → DNG | ✅ |
| GPU pipeline (wgpu compute, CPU-exact within 1/255), CPU fallback on device limits / errors | ✅ · WebGPU in the browser 🚧 |
| AI: segmentation masks, AI denoise, super resolution, faces; video | ⬜ (see [roadmap](ROADMAP.md#where-were-going)) |
| Web build (same UI in the browser via WASM): persistent library in OPFS/IndexedDB, Web Worker rendering, export downloads | ✅ · WebGPU, Safari/Firefox testing 🚧 |

<sub>✅ works today · 🚧 in progress · ⬜ not started</sub>

<br>

## Quick start

```sh
git clone https://example.invalid/repo && cd repo
cargo xtask brand check                                 # the product name lives only in brand.toml
cargo xtask run                                         # build and open your library (~/Pictures/<app> Library; a new one starts with demo photos)
cargo xtask run -- ~/Pictures/trip                      # import your photos (folders are scanned, duplicates skipped)
cargo xtask run -- --memory                             # a throwaway in-memory demo session (writes nothing)
cargo xtask run -- --control 7980                       # with the automation channel
cargo xtask web --serve                                 # the same app in the browser: http://127.0.0.1:8080/
cargo run --release -p dac-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask package                                     # installers and archives under the brand binary names
cargo xtask install                                     # install the app and CLI for the current user
cargo xtask ci                                          # fmt, clippy, tests, layering, wasm checks
```

Inside `target/` the binaries are always called `app` and `app-cli`; `cargo xtask run`, `package` and `install`
give them the brand names from `brand.toml`. `cargo run -p dac-app` / `-p dac-cli` also work for development.

CI defaults to line-table debug information and one build job per 1.5 GB of RAM
available when it starts (at most one per CPU; 4 if memory can't be read), so an
8 GB machine with 6 GB free builds with 4, and a busy machine with little free
builds with fewer. Test threads follow, capped at 4. Full-debuginfo linkers, one
per CPU, otherwise exhaust RAM before any test runs. GPU coverage is unchanged. Explicit
`CARGO_PROFILE_DEV_DEBUG`, `CARGO_BUILD_JOBS` and `RUST_TEST_THREADS` settings
override these defaults. Ordinary development commands keep their own settings.

**Chinese and Japanese text** need the shared font repo, an optional build input (official releases always include it):

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p dac-app
```

Without it the app builds and runs the same, but Chinese and Japanese text have no glyphs. Fonts are never committed to this
repo; see [craftrules `standards/fonts.md`](https://github.com/storytold/craftrules/blob/main/standards/fonts.md).

**Interface language:** **Edit → Language** (English, 简体中文, 繁體中文（台灣）, 日本語, Português (Brasil), Español, Deutsch, Русский, Français) or **Settings → General →
Language**; the choice applies immediately and persists. See [docs/localization.md](docs/localization.md).

**Logs:** the desktop app writes its log to standard error and to `logs/<binary>.log` in its settings folder
(Linux `$XDG_CONFIG_HOME/<settings_dir>/logs/`, by default `~/.config/<settings_dir>/logs/`; macOS
`~/Library/Application Support/<settings_dir>/logs/`; Windows `%APPDATA%\<settings_dir>\logs\`), never in the library. A
launch from a desktop menu or the Dock has no terminal, so attach this file to a bug report (**Help ▸ Open Log
Folder** shows it in the file manager). Each start moves the
previous log to `<binary>.1.log` and that one to `<binary>.2.log`, so the log of a run that crashed survives the
next start; the file stops growing at 16 MiB, `--version` and `--help` write none, and runs with
`<PREFIX>_NO_PREFS` log to standard error only. By default the app's own crates log at `info` and everything else
at `warn`. `<PREFIX>_LOG=info` or `debug` works as before (that level for the app's own crates, warnings and
errors from the rest; any other value: warnings and errors only) and wins over `RUST_LOG`, which otherwise replaces
the default with env_logger-style directives such as `RUST_LOG=debug` or `RUST_LOG=warn,dac_pipeline=trace` (a
directive ending in `*` covers every target starting with it, as in `dac*=debug`). Panics are recorded there
too, and still in `<binary>-panics.log` in the temp folder. The logger is `apps/app/src/logging.rs`.

The web build needs the `wasm32-unknown-unknown` target and the matching `wasm-bindgen` CLI
(`cargo xtask web` prints the exact install command); see [docs/web.md](docs/web.md).

**Nix** builds the desktop app and `<cli>` (the Nix build always includes the craft-fonts input, so Japanese
text has glyphs):

```sh
nix run <flake-url>                    # the desktop app
nix build <flake-url>                  # → ./result/bin/{<binary>,<cli>}
nix develop <flake-url>                # rust toolchain + native deps + fonts
```

In a flake configuration (NixOS, home-manager, nix-darwin):

```nix
# flake.nix
inputs.app.url = "<flake-url>";
# optional: build against your own nixpkgs instead of the one the app pins
# inputs.app.inputs.nixpkgs.follows = "nixpkgs";

# then, in a NixOS or home-manager module (where `inputs` is in scope):
nixpkgs.overlays = [ inputs.app.overlays.default ];   # makes `pkgs.<binary>` available
environment.systemPackages = [ pkgs.<binary> ];           # home-manager: home.packages = [ pkgs.<binary> ];
```

`nix build` installs the same desktop file, hicolor icons and AppStream metadata as the .deb/.rpm, and runs
`cargo test --workspace` as its check phase (skip it with `pkgs.<binary>.overrideAttrs { doCheck = false; }`).

**Keyboard:** <kbd>G</kbd> grid · <kbd>D</kbd> detail · <kbd>E</kbd> edit · <kbd>C</kbd> crop · <kbd>M</kbd> masking ·
<kbd>Shift</kbd>+<kbd>P</kbd> presets · <kbd>\\</kbd> original · <kbd>Y</kbd> before/after · <kbd>Z</kbd> zoom ·
<kbd>J</kbd> clipping · <kbd>⌘Z</kbd> undo · <kbd>⌘/</kbd> all shortcuts.

## How it's built

An engine-first Cargo workspace of small, tested crates with enforced layering (`cargo xtask layers`): `geom`,
`color`, `raster`, `tiff` → `raw`, `codecs`, `meta`, `develop` → `pipeline` → `catalog` → `engine` → `ui-egui`. The
egui frontend is one swappable crate; nothing below it knows a UI exists. `mcp` and the apps (`app`,
`<cli>`) sit on top of `engine`.

## Contributing

Humans and agents follow the same rules, so read [AGENTS.md](AGENTS.md) first. The short version:

- **Clean-room.** Never read Adobe binaries or GPL raw/photo code (darktable, RawTherapee, LibRaw, rawspeed, dcraw…);
  work from public specs and black-box observation.
- **No Adobe assets, ever:** no icons, screenshots, presets, profiles, LUTs or fonts from Adobe products. Every
  image, icon and font in the repo is original, public domain, Creative Commons, OFL or permissively licensed, and has
  an entry in [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md) added in the same commit. New fonts go to
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not here.
- **Pure Rust**, enforced crate layering, everything is a command, and `cargo xtask ci` green before every commit
  (one task id per commit).
- **Never crash.** Non-test code returns errors instead of panicking: no `unwrap()`, `expect()`, `panic!` or
  `unsafe`, checked indexing on anything derived from input, and a regression test with every crash fix. Details in
  [AGENTS.md](AGENTS.md#never-crash-outranks-feature-work).

<br>

## Downloads

Release builds are listed on the project's releases page. `<ver>` in the file names is the version number,
`<binary>`/`<cli>` are the brand binary names, and `SHA256SUMS.txt` lists a checksum for every file. Build them yourself
with `cargo xtask package`.

### Windows

| Build | Installer | Portable |
|---|---|---|
| x64 (64-bit Intel/AMD) | `<binary>-<ver>-windows-x64.msi` | `<binary>-<ver>-windows-x64-portable.zip` |
| arm64 (Snapdragon and other ARM PCs) | `<binary>-<ver>-windows-arm64.msi` | `<binary>-<ver>-windows-arm64-portable.zip` |
| x86 (32-bit) | `<binary>-<ver>-windows-x86.msi` | `<binary>-<ver>-windows-x86-portable.zip` |

Installers and executables are code-signed. The installer asks where to install (`C:\Program Files\LightCraft` by
default; upgrades stay in the folder you chose) and ends on a page confirming LightCraft was installed, with an
option to start it. For unattended installs: `msiexec /i lightcraft-<ver>-windows-x64.msi /qn INSTALLFOLDER="D:\Apps\LightCraft\"`.

### macOS

| Build | File | Notes |
|---|---|---|
| App, universal (Apple silicon + Intel) | `<binary>-<ver>-macos-universal.dmg` | Signed and notarized |
| Command-line tool, universal | `<cli>-<ver>-macos-universal.zip` | Signed and notarized |

### Linux

| Format | x86_64 | aarch64 (ARM64) | Notes |
|---|---|---|---|
| AppImage | `<binary>-<ver>-linux-x86_64.AppImage` | `<binary>-<ver>-linux-aarch64.AppImage` | Runs anywhere; updates itself with [AppImageUpdate](https://github.com/AppImageCommunity/AppImageUpdate) (`.zsync` files) |
| Flatpak | `<binary>-<ver>-linux-x86_64.flatpak` | `<binary>-<ver>-linux-aarch64.flatpak` | Sandboxed; `flatpak install --user <file>` |
| Debian/Ubuntu | `<binary>-<ver>-linux-x86_64.deb` | `<binary>-<ver>-linux-aarch64.deb` | |
| Fedora/RHEL/openSUSE | `<binary>-<ver>-linux-x86_64.rpm` | `<binary>-<ver>-linux-aarch64.rpm` | |
| Tarball | `<binary>-<ver>-linux-x86_64.tar.gz` | `<binary>-<ver>-linux-aarch64.tar.gz` | Unpack anywhere |

### FreeBSD

| Build | File |
|---|---|
| x86_64 | `<binary>-<ver>-freebsd-x86_64.tar.gz` |

### Web (WebAssembly)

| Build | File | Notes |
|---|---|---|
| Static site | `<binary>-web-<ver>.zip` | Runs in a modern browser; host it on any static server |

<br>

## License and credits

The app is licensed under the [MIT License](LICENSE), except `crates/segment` and `crates/fetch`, which are
Apache-2.0 only. It is a fork of an upstream open-source project; the upstream copyright notices and required
third-party notices are in [LICENSE](LICENSE) and [NOTICE](NOTICE).

Bundled fonts, icons, images and other assets keep their own open licenses; each one is listed
with its author, source and license in [assets/ATTRIBUTION.md](assets/ATTRIBUTION.md).

Showcase photographs are public-domain works, used via Wikimedia Commons: Ansel Adams, *The Tetons and the Snake River*
(1942, U.S. National Archives); Dorothea Lange, *Migrant Mother* (1936, Library of Congress); Bill Anders / NASA,
*Earthrise* (1968); NASA, *The Blue Marble* (1972). The demo library is procedurally generated by the app. UI font:
Inter (SIL OFL 1.1). Builds made with [craft-fonts](https://github.com/storytold/craft-fonts) (all official releases)
also embed its Chinese and Japanese fonts (Noto Sans CJK SC, BIZ UDPGothic, BIZ UDMincho, Shippori Mincho; SIL OFL 1.1), listed in its
[ATTRIBUTION.md](https://github.com/storytold/craft-fonts/blob/main/ATTRIBUTION.md). All icons are original.

Forks and modified versions must remove them.

<sub>Adobe, Photoshop, Illustrator, Premiere Pro, Lightroom, Acrobat, After Effects and InDesign are trademarks or registered trademarks of Adobe Inc. in the United States and/or other countries. The app is an independent, open-source project and is not affiliated with, sponsored by or endorsed by Adobe Inc.; these names are used only to describe the workflows it is compatible with.</sub>


