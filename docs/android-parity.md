# Android tablet status

The ARM64 release APK is built from the Android host in `apps/lightcraft-android` and packages the shared LightCraft engine and egui UI.

Implemented and exercised:

- SAF folder/file picking with persisted URI permissions, recursive discovery, and bounded stream materialization.
- JPEG/PNG and RAW/DNG import through the shared source pipeline, non-destructive edits, catalog persistence, export, save-as, and Android sharing.
- Touch/stylus viewer gestures for swipe navigation, long-press original preview, and tool-drag conflict handling.
- Tablet toolbar actions, fullscreen, lifecycle/memory callbacks, and the loopback debug inspection endpoint.
- ARM64 release packaging with SDK 29 minimum and target SDK 35.

The APK is a testable implementation slice, not a claim that every desktop workflow has reached parity. Physical tablet validation is still required for stylus pressure, vendor-specific SAF providers, large RAW performance, rotation/insets, and GPU differences. The Android catalog currently reports a lock warning on some app-private filesystems; edits and export continue to work, but that storage behavior should be hardened before a production release.
