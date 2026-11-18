# Agents, performance and feature status

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

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
  to the running app with screenshots, clicks and gestures. See [docs/mcp.md](../mcp.md).

  ```sh
  cargo build --release -p lcc-cli
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
  <cli> run --import ~/Pictures/shoot library.selectAll export.contactSheet path=Contact.pdf paper=a4 columns=3 rows=4 captions=true
  ```

  `export.contactSheet` also accepts explicit `ids`, `landscape=true`, and `paper=letter`. It replaces an existing
  output only after the entire PDF succeeds. Limits: 1,000 photos, 100 pages, 256 MiB; captions are rasterized
  with the same font coverage as watermarks (CJK requires craft-fonts), with long names shortened in the middle.
  For printer profiles, driver options and picture packages, use the Print module.
- **Every widget is addressable** (`ui.widgets`) and clickable by name, so agents operate the real UI, not a
  side door.
- The screenshots in this README were produced end to end by the [`docs/showcase/`](../showcase/) scripts.
  Protocol reference: [docs/control-protocol.md](../control-protocol.md).


## Fast, native, private

- **Pure Rust, no C.** Our own RAW decoders (DNG, Canon CR2/CR3, Sony ARW, Nikon NEF, Fujifilm RAF incl. X-Trans,
  Panasonic RW2 / Leica RWL, Pentax PEF, Olympus ORF), our own colour science, our own pipeline. JPEG, PNG, TIFF, WebP,
  PSD composites, JPEG XL and HEIC/HEIF (iPhone photos) open today (HEVC stays a build-time choice: the `heif`
  feature is on by default and can be turned off).
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
- **Online only when you ask.** The app reaches the network only for something you turn on or start: update
  checks (Help ▸ Check for Updates…, or at start-up once you tick "Check for updates when the app starts", which is
  off by default), model and lens-profile downloads, map tiles and place-name searches (geocoding), Immich,
  publishing (SFTP), network printing (IPP), Wi-Fi tethering (PTP/IP to the camera), remote denoise and AI-mask
  (SAM 3) workers on a server you name, and plug-ins you grant network access to. The control port listens on this
  computer only, and Edit in PhotoCraft talks to PhotoCraft on this computer. An update check asks GitHub for this
  project's release list; GitHub sees your IP address, the time and a User-Agent of the app's name and version, and
  nothing else is sent.

## Feature status

The app is young and moving fast. **Where we honestly stand** (details in the [roadmap](../../ROADMAP.md#where-we-stand)):

- **By feature count we're at ~90%** of Lightroom (core features 96%, important ones 90%; 522 of 649 rows done and 82
  partial), tracked row by row in
  [docs/parity-checklist.md](../parity-checklist.md).
- **As a day-to-day Lightroom replacement we're nearer 60–70%.** It's great for JPEG/DNG and most Nikon / Sony /
  older-Canon raws on one machine.
- **The biggest gaps:**
  - **camera colour calibration:** Sony, Nikon, Panasonic, Fujifilm and Canon CR3 raws have guarded estimates from their camera JPEGs, with built-in profiles for twenty bodies (among them ILCA-99M2, ILCE-7CR, ILCE-7M4, ILCE-7RM5, X-H2S, X-T4, Nikon Z 6II, Z 8, D5300, D750, D780, D90, Panasonic DMC-GM5), matrices fitted to measured spectral sensitivities for 52 more (Profile ▸ Calibrated camera colour), dcraw's matrices for about 550 more models, and your own `.dcp` camera profiles import; colour-chart and Lightroom-matched calibration tools exist, but a measured calibration database of our own is still missing;
  - **per-model raw verification:** 596 of 1,446 models (41%) are verified on real files, so about 7 in 8 photographers shoot a verified body; other bodies are unverified, see [docs/raw-parity.md](../raw-parity.md);
  - **AI quality:** Subject / Background (BiRefNet) and Object (SAM 3) masks are optional downloads; sky is still heuristic, denoise quality is unverified;
  - **network libraries:** a preview reads the raw's header and its embedded JPEG only (56.5 MB → 5.9 MB, 10 s → 1 s on the test share), but the *developed* picture still needs the sensor data over the wire; see [docs/raw-preview-reads.md](../raw-preview-reads.md);
  - **HDR display covers the loupe only** (zoomed tiles, Before/After and the second window are still SDR); **video is new** (global edits only).
- **What's next:** see [where we're going](../../ROADMAP.md#where-were-going).

| Area | Status |
|---|---|
| Library: albums, folders, smart albums, stacks (incl. auto-stack), virtual copies, ratings, flags, labels, filter bar, search, sort, grids, filmstrip | ✅ |
| Culling: Compare (synced zoom) and Survey views, auto-advance, instant previews | ✅ |
| Light, Color, Effects (vignette styles), Levels, Tone Curve (+ refine saturation, targeted adjustment), Color Mixer (+ targeted), Point Color, Color Grading, Calibration, B&W | ✅ |
| Histogram (RGB, luminance, R/G/B, chroma; linear or log; drag a tonal zone) and scopes (Waveform, RGB Parade, Vectorscope), pinned above the Edit panel | ✅ |
| Masking: brush (A/B presets, density), linear, radial, luminance/colour range, per-mask feather and Curve, add/subtract/intersect; AI Subject / Background (BiRefNet) and Object (SAM 3), optional downloads | ✅ |
| Crop, straighten tool + auto straighten, flip, rotate, aspect ratios, overlays | ✅ |
| Profiles (Color, Neutral, Vivid, Landscape, Portrait, Monochrome: our own looks), presets, versions, history, copy/paste/sync settings and metadata, Auto Sync, per-photo Auto | ✅ |
| Camera colour: DNG files use their own matrices; import your own `.dcp` profiles | ✅ DNG, `.dcp` import · 🟡 built-in profiles for eleven bodies, spectral matrices for 52 more, dcraw matrices for ~550; chart / Lightroom calibration tools; measured calibration database missing |
| Native macOS menu bar (generated from the command registry), control channel + every widget addressable, headless UI snapshots | ✅ |
| RAW: DNG, CR2, CR3 (lossless CRX Bayer and version 0x100/0x200 C-RAW), ARW, NEF (uncompressed + lossless/lossy compressed), Fujifilm RAF (uncompressed + lossless/lossy compressed, Bayer + X-Trans), Panasonic RW2 / Leica RWL / Panasonic RAW (every raw format, DMC-LX1 to DC-S1RM2), Pentax PEF, Olympus ORF (uncompressed + compressed), Samsung SRW (uncompressed + compressed), CR3 tiled C-RAW, Minolta MRW, Sony ARW v1, packed NEF, SuperCCD RAF, Canon CRW, Epson ERF, Mamiya MEF; AVIF import; embedded previews for every format incl. CR3; 596 models verified | 🟡 · per-model verification continues |
| Detail: sharpening, luminance + colour noise reduction | ✅ · AI Denoise, Super Resolution ⬜ |
| Remove (content-aware fill) / Heal / Clone spots (auto source), Visualize Spots, Red Eye and Pet Eye (auto pupil detection, catchlight) | ✅ · spot pin editing 🚧 |
| Export: JPEG / PNG / TIFF / WebP / AVIF / DNG / original, sizing, file-size limit, output sharpening, naming templates, batch, metadata policy, text or image watermark | ✅ |
| HDR: HDR editing with a headroom limit, SDR rendition, Visualize HDR; export as ISO 21496-1 gain map JPEG, PQ or HLG AVIF, or 32-bit float TIFF; HDR display on macOS (EDR), Windows (HDR) and Linux (Wayland) | ✅ · HDR display of zoomed tiles and Before/After 🚧 |
| Colour-managed display: previews follow the monitor profile on macOS, Linux (X11 / colord) and Windows (WCS) | ✅ |
| Library persistence (crash-safe op log + snapshots, background compaction, failed saves reported), disk thumbnail cache | ✅ |
| Import: Add in place / Copy / Move, rename and folder templates, devices, duplicate detection, watched folders; Local folder browsing | ✅ |
| Output: Print (printer ICC, intent, BPC, Print Adjustment, 16-bit, `ipps://`, driver options), Book (PDF, pair kerning), Slideshow (PDF/JPEG/video with music), Web (HTML galleries, SFTP), publish services with grid sections | ✅ |
| MCP server (headless or live app, persistent libraries), CLI, control channel | ✅ |
| XMP sidecars (read/write, auto-write), reading `crs:` develop settings, preset files (`.lcpreset`, XMP presets) | ✅ |
| Optics (distortion, vignetting, auto + manual CA, defringe, lens corrections embedded in DNG files and Panasonic / Leica RW2 / RWL distortion data), Geometry (transforms, Constrain Crop), Upright (Auto/Level/Vertical/Full/Guided) | ✅ · lensfun-format lens database set up or downloaded from Settings ▸ Lens profiles, with a lens picker for manual or misnamed lenses 🟡 · lens profiles of our own ⬜ |
| Photo Merge: HDR (auto-align, deghost), Panorama (spherical/cylindrical/perspective, boundary warp, auto crop), HDR Panorama → DNG | ✅ |
| GPU pipeline (wgpu compute, CPU-exact within 1/255), CPU fallback on device limits / errors | ✅ · WebGPU in the browser 🚧 |
| Video: MP4/MOV/MKV/WebM/TS/MXF decode (H.264, HEVC, VP9, AV1, ProRes, DNxHR, APV, MPEG-2), playback, trim, poster frame, Capture Frame, global grading, H.264 / ProRes export with audio | 🟡 · local adjustments and hardware decode on video ⬜ |
| AI: segmentation masks (BiRefNet, SAM 3), Bayer AI denoise, faces (whole networks on the GPU when faster, batched) | 🟡 · super resolution ⬜ (see [roadmap](../../ROADMAP.md#where-were-going)) |
| Web build (same UI in the browser via WASM): persistent library in OPFS/IndexedDB, Web Worker rendering, export downloads | ✅ · WebGPU, Safari/Firefox testing 🚧 |

<sub>✅ works today · 🚧 in progress · ⬜ not started</sub>
