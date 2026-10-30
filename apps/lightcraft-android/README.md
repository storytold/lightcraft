# lightcraft-android

LightCraft on Android: a small `apps/` crate that hosts the existing egui UI in an
`android-activity` (GameActivity) window and supplies the three things the desktop shell gets from
the OS — a home directory, file dialogs and a phone-shaped layout. The engine, the UI, the theme and
the command table are unchanged.

## What the shell does

- **Entry point / storage** — builds as `libmain.so`, loaded with `System.loadLibrary`. `$HOME` is
  set to `AndroidApp::internal_data_path()` before the app starts, so LightCraft's *own* code points
  the library at `<files>/Pictures/LightCraft Library` and the preferences at `<files>/.config/lightcraft`.
- **File pickers** — `ACTION_OPEN_DOCUMENT` through the Activity; the picked photo is copied into
  `<filesDir>/import/` and imported with the app's own `library.import` command. Exports write to a
  scratch path and are published to the `ACTION_CREATE_DOCUMENT` destination afterwards.
- **Phone layout** — `phone_layout()` opens on the photo grid with the left and right panels closed
  and the filmstrip kept, and `TouchShell` raises hit targets to 40 pt after LightCraft installs its
  theme (its `Tokens` toolbar/filmstrip heights grow with it).
- **Command palette** — long-press the top 8% of the screen for 500 ms; the buttons send LightCraft's
  own command ids (`file.addPhotos`, `dialog.export`, `view.photoGrid`, …), so the palette is a
  finger-sized menu bar, not a second UI.
- **Fonts** — the `/system/fonts` CJK face is appended to every family, including the theme's named
  `semibold` family, after the theme's fonts are live (earlier, egui panics).
- **GPU** — the develop pipeline's secondary `wgpu` device is off by default (the settings switch
  turns it back on); the window itself is still drawn by eframe/wgpu.
- **Self-test** — a background thread builds a demo library, exports a JPEG through the engine's
  export path, decodes the result back and writes/reads it in app storage, so one logcat line proves
  scenes, catalog, develop, JPEG and sandbox I/O.

## Known gaps

- The system clipboard is not wired.
- The photo library is the app's private directory; there is no "use this folder as the library"
  tree picker (SAF trees are `content://`, the engine wants paths).
- Native macOS menus are not installed; the in-window menu bar plus the palette replace them.
- Launcher icons are the Android Studio template's placeholder art.

## Build

```bash
cargo ndk -t arm64-v8a -o apps/lightcraft-android/jniLibs build --release -p lightcraft-android
cd apps/lightcraft-android/android && ./gradlew assembleRelease
```

`jniLibs/` and the Gradle output are gitignored; the APK is not part of this change.
