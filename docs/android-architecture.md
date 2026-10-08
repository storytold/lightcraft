# Android tablet architecture

Baseline: upstream eeb136b9eec179805307c4626596f0ccc074368b (2026-10-08).
Objective: preserve the actual desktop UI and editing semantics on Android 10+
ARM64 tablets, including offline RAW development and durable edits.

## Reuse

`ui-egui::LightcraftApp` owns the desktop panels, commands and gestures. `engine`
owns import, jobs, presets, history and export. `catalog` retains its durable
FsStore journal in internal app storage. `raw`, `codecs`, `develop`, `pipeline`,
`preview`, `geom`, `meta`, `color`, `raster` remain shared. No Kotlin image pipeline.
Existing file_loader, file_probe, preview_loader and file_bytes hooks provide
the source boundary; Availability has an injectable existence probe.

## Host and rendering

Use eframe 0.36.2 / winit Android NativeActivity. This is a supported eframe
backend and avoids GameActivity's C++ glue. A small Android host supplies SAF,
persisted grants, document destinations and sharing. Rust owns rendering and
application state. Vulkan draws the UI; shared GPU compute must retain CPU
fallback. UI surface lifecycle belongs to winit. Unsupported graphics hardware
must produce a clear startup error rather than imply CPU rendering draws a surface.

The Android entrypoint/VM bridge is isolated from pure safe application code.
Only that helper may contain justified FFI; shared crates retain their safety rules.
Desktop rfd and OS menu dependencies are not linked into the Android host.

## Storage

Persist content URIs as identities, never invent filesystem paths for them.
Folder discovery uses DocumentsContract through ACTION_OPEN_DOCUMENT_TREE and
persistable grants, including SD cards exposed by SAF. Recursive traversal is
bounded and cancellable. Original bytes are read only on demand through streams;
no directory-wide copy and no assumption of seekable descriptors or atomic rename.
App-private per-file staging is allowed for codecs requiring a path, with bounded
cache and leases protecting active reads. Catalog, thumbnails, settings, history,
and custom presets are internal. Provider errors must remain visible and offer
reauthorization. A missing file never deletes edits.

Exports render with the shared engine on workers to private staging, then write
through a selected document/tree. Original photos must never be overwritten by
default. Destination errors do not count as successful exports. Sharing grants a
temporary read URI; export collision policy is explicit.

## Compatibility and validation

The upstream local `plan/` and per-crate README files are absent from this clone.
Use AGENTS.md, ROADMAP.md, docs/parity.md, docs/web.md and source as evidence.
CR3/compressed ORF and other upstream preview-only formats remain labelled as such.
SAM model distribution and camera calibration gaps remain upstream limitations.
Path-based rename/move/XMP/external-editor commands need a platform capability
boundary; visible controls alone do not establish Android parity.

Build both signed debug and local-release ARM64 APKs. Test source identities,
discovery, revoked permissions, export failures, persistence, gestures and cache
eviction; run existing desktop/web quality gates. Device/emulator edit-export and
matching desktop/tablet screenshots are required before claiming runtime parity.
An absent ADB device is recorded as unverified, never as a successful test.
