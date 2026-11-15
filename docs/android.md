# Android

The Android app is being built in stages (plan: Android build in CI, then peer-to-peer catalog sync). This page
covers what exists today.

## Status

| Stage | State |
|---|---|
| Shared crates build for `aarch64-linux-android` (`cargo xtask android check`, in `ci --fast`) and `x86_64-linux-android` (the full pre-release run) | ✅ |
| App host (`apps/android`, NativeActivity; JNI in `crates/android-host`) | 🟡 runs on the emulator: import via the system picker, Develop, library kept across restarts; status-bar insets not handled yet |
| APK / AAB packaging (`cargo xtask android package`), release job (on by default; `RELEASE_ANDROID=false` turns it off) | ✅ arm64-v8a and x86_64 APKs, one AAB with both (arm64 APK 75 MB) |
| Play Store upload | not started (needs the Play developer account) |
| Peer-to-peer catalog sync between desktop and Android | 🟡 pairing and device sync of the catalog or chosen collections (Settings ▸ Sync, `crates/peer`, `crates/sync`; [sync.md](sync.md)); not yet tested between real devices |
| Modern layout, phone- and tablet-sized (`crates/ui-egui/src/modern.rs`, on by default on Android, `view.modernLayout` elsewhere) | 🟡 photo-first Develop (tap for tools, single-tool drag adjust in groups Basic (white balance + tone), Colour (presence), Tone (parametric curve), Detail, Optics, Effects (vignette, grain); hold for the original, Edit panel beside / under the photo), slim Library bar, swipe scrolling with momentum and long-press multi-select, Back (`touch.back`, predictive back), cutout and system-bar insets, a pinned or floating histogram (`touch.histogram`), touch Settings and Export sheets; Settings ▸ General ▸ Layout switches Touch / Desktop on every platform (desktop default Desktop, mouse works); verified on Waydroid, not yet on a phone or tablet; not yet: pinch in adjust mode, tool-specific touch UIs for Crop / Masks |

## Design

- **NativeActivity, not GameActivity.** android-activity's GameActivity backend compiles C++ from the Android Games
  SDK; NativeActivity is pure Rust on our side. The Java side is one small `NativeActivity` subclass
  (`packaging/android`) that runs the system pickers and opens links.
- **Folders.** The host calls `lcc_brand::set_platform_dirs` before anything else. Settings go in the app's internal
  data folder, and the library in its external data folder (`Android/data/<android_package>/files/<library name>`), which
  the Files app can reach and which is removed with the app. No environment variables are set (`set_var` is unsafe
  and racy once the activity's threads run).
- **Pickers.** The shared UI's asynchronous `Services::picker` is answered by the Storage Access Framework. The
  activity answers with paths (the real path where it can read a document in place, else a copy in the app's
  folders; see [Storage](#storage)) through a native callback that it
  registers at start-up, so no symbol depends on the package name. For a save, it watches the path it handed out
  and copies the written file to the chosen document.
- **Picked folders stay linked.** A folder read in place sees new files by itself. For one that has to be copied,
  the activity keeps the read grant (`takePersistableUriPermission`, up to 16 folders) and rescans it every 4 s
  while the app is in front, copying files that appeared since into its copy (shown in the activity stack): never overwriting or deleting, skipping hidden files (the camera's
  `.pending-…` shots), and only once a file's size held between two scans, under a hidden name renamed into place.
  So a studio capture session watching the copy of `DCIM/Camera` imports each new shot, with no photo permission.
  Deleting the copy, or revoking the grant, unlinks the folder. Not built: a `MediaStore` observer behind
  `READ_MEDIA_IMAGES`, and scanning while the app is in the background (gap 34).
- **Opening, sharing and dropping photos from other apps.** The activity takes `VIEW` intents for `content://`
  images and `SEND` / `SEND_MULTIPLE` for `image/*` (raw files arrive as `image/x-…` types, so `image/*` covers
  them; `application/octet-stream` would offer the app for every file). It is `singleTask`, so a share while the
  app runs arrives in `onNewIntent`. Drag and drop from another app (split screen) goes through a drag listener on
  the window, with `requestDragAndDropPermissions` held until the copy is done. In every case the documents are
  staged like picked files ([Storage](#storage): real path or a reused copy in `Incoming/Picked`) off the UI thread and the paths go to `nativeOpened` (registered with
  `nativePicked`). The native side queues them and the next frame hands them to the shared UI as dropped files, so
  they take the desktop's drop path: photos open the import review, presets import as presets. A share that arrives
  before the native side is up waits in the activity until `Host::new` calls `hostReady()`. Copy failures are logged
  and shown as a toast.
- **HEIC/HEIF photos** (what iPhones shoot) open like on the desktop: the same pure-Rust decoder
  (`lcc-heif`), on through the Android app's default `heif` feature, so the import review, the camera roll
  (MediaStore files HEIC shots as images) and the pickers (`image/heif`) all take them. Distributors who must
  avoid HEVC (patents) build with `--no-default-features`, as on the desktop.
- **Peer discovery** is multicast DNS service discovery (RFC 6762 / 6763, `crates/peer`), the same as Bonjour, so
  a future iOS build can join. Android's Wi-Fi drops multicast unless an app holds a `WifiManager.MulticastLock`:
  the activity acquires one in `onCreate` (permission `CHANGE_WIFI_MULTICAST_STATE`) and releases it in `onDestroy`.
- **On-screen keyboard.** NativeActivity's own soft-input support has no input connection, so predictions, composed
  text and most non-Latin input never arrive. The activity adds a 1×1 transparent `View` over the native content
  (a plain view, not an `EditText`, so no system cursor or selection handles). It takes focus while the app wants
  text; its `InputConnection` keeps no text and forwards `commitText`, `setComposingText`, `finishComposingText`,
  `deleteSurroundingText`, Enter and Backspace to `nativeIme` (registered at start-up beside `nativePicked`). The
  native side queues them (bounded) and wakes the UI; the next frame's `raw_input_hook` turns them into egui text,
  IME pre-edit/commit and key events (`lcc_ui_egui::soft_keyboard`). After each frame the app checks whether a text
  field wants input (egui's IME output) and calls the activity's `setKeyboard(show)` only when that changes.
- **System font fallback.** At start-up the app reads a few faces from `/system/fonts` (Noto Sans CJK or
  DroidSansFallback, `NotoSans<Script>`) for scripts craft-fonts didn't supply, and lists them after every other
  face (`theme::set_system_fallbacks`). At most 12 faces, 32 MB each and 64 MB together; each is parsed before
  egui sees it, and a face that can't be read is logged and skipped. Nothing is bundled.
- **Settings** are written within seconds of a change (`apps/android/src/prefs.rs`), because Android can stop a
  background app without calling `on_exit`.

## Storage

The Storage Access Framework hands out `content://` documents, but the engine reads files by path. The activity
(`MainActivity`, Storage section) answers each pick with paths:

- **In place.** A folder or file on the device's own storage (the external storage provider's `primary:<path>`)
  is answered with its real path under `Environment.getExternalStorageDirectory()` when the app can read it. Then
  nothing is copied, and Library ▸ Browse, folder scans and Synchronize work on the real folder. Reading a folder
  in place needs **"All files access"** (`MANAGE_EXTERNAL_STORAGE`, Android 11+) or, on Android 9–10,
  `READ_EXTERNAL_STORAGE` (with `requestLegacyExternalStorage` for 10), and only the GitHub build declares them
  (see *Two builds* below). Under scoped storage a folder lists without it, but shows only the files the app made,
  so its photos would be missing; without it the folder is copied (below). A single picked file is read in place
  when the photo permission lets the app read it.
- **Two builds.** `cargo xtask android package --store github|play` (`xtask/src/apk.rs`, `Store`): the APKs are the
  GitHub build by default; the bundle (`--aab`) is always the Play build. The Play build's manifest has no
  `MANAGE_EXTERNAL_STORAGE` and no `requestLegacyExternalStorage`, because Play allows All files access only for
  file managers, backup and antivirus apps. The activity reads its own declared permissions
  (`allFilesAccessDeclared`, through the package manager): without All files access it never asks for it (not at
  the first start, not from Settings, where `Services::storage_access` is `None` and the Device storage row is
  hidden), and `hasAllFilesAccess` is false, so picked folders are always copied (with the persisted tree URI) and
  the camera roll goes through MediaStore. Picked folders therefore don't sync in place in the Play build; picking
  the same folder again copies only what changed. `--store play` builds Play-variant APKs for testing by
  sideloading.
- **Camera roll.** `file.addCameraRoll` (first-start question in `crates/ui-egui/src/camera_roll.rs`, kept as
  `cameraRollAsked` in `ui.json`; Settings ▸ Folders ▸ Camera roll ▸ Import… asks again) calls
  `Host::camera_roll`. The activity asks for the photo permission when it lacks it (`READ_MEDIA_IMAGES` on 13+,
  `READ_EXTERNAL_STORAGE` before; on 14+ also `READ_MEDIA_VISUAL_USER_SELECTED`, so *Allow selected photos*
  works: the roll then lists only the chosen photos, and importing again asks again so the selection can change),
  queries MediaStore for `DCIM/Camera/…`, and answers through
  `nativeCameraRoll` with real paths where it can read them, else copies in `Incoming/Camera Roll` (reused
  next time). The paths open the import review. A refused permission or an unreadable roll comes back as an
  error the UI shows. Raws: MediaStore files DNG, CR2, NEF, ARW and most other raws as images, so they are listed;
  files it doesn't (CR3 on most devices) are looked up in its Files table by extension, which only All files access
  can see on Android 11+. In the Play build such raws are left out of the camera roll; pick their folder instead
  (copied).
- **Watching.** Adding the roll for keeps — the first-start question's Add Camera Roll, or Settings ▸ Folders ▸
  Camera roll ▸ Import… — sets `cameraRollWatch` in `ui.json`: the roll is watched (issue #1024). The watch lists
  it again through the same service when the library opens, when the app comes to the front and every five
  minutes (`camera_roll::watch_tick`, folder polling's cadence), and hands the paths the library doesn't have
  (a photo in Recently Deleted counts as had) to an import in place, without the review and with the library's
  import settings; the activity stack shows the import, and the toast when it is done says how many new photos
  the roll brought. Nothing found says nothing. While an import, a scan, a waiting review or a folder
  synchronize runs, nothing is listed — a listing made then is dropped, and the next one picks the news up.
  Settings ▸ Folders ▸ Camera roll has a *Watch for new photos* checkbox to turn the watch off and on
  (`camera_roll::set_watch`); declining the first-start question never enables it.
- **Asking.** In the GitHub build the activity asks once, at the first start (remembered in its `storage`
  SharedPreferences): the system's All files access page on 11+, the runtime permission before. The Play build
  never asks. `app.allFilesAccess` (Settings ▸ Folders ▸
  Device storage ▸ Change…) asks again; `Services::storage_access` gives the shared UI the state and the question.
- **Copies.** Anything else (cloud providers, SD cards and USB drives, which are other volumes) is copied into
  `<external data>/Incoming`: single files into `Incoming/Picked`, folders into `Incoming/<folder name>`. The
  document's URI is remembered with its copy's path, so picking the same document again reuses the copy (when the
  size still matches) or overwrites it, and picking the same folder again copies only files whose size changed. A
  document whose name has no extension gets one from its MIME type (raw types first), so the importer accepts it.
- **Progress.** A copy that copies at least one file reports through JNI (`nativeCopyProgress`, `nativeCopyEnded`)
  to `apps/android/src/copies.rs`, which holds an `import` row ("Copying picked files") in the activity stack. The
  row's ✕ answers the copy loop's next report with "stop" (checked between files and every 8 MB).
- **Cleanup.** At start, off the UI thread, `apps/android/src/staging.rs` deletes files under `Incoming` that no
  library photo refers to (keeping a kept file's sidecars, files younger than a day, and never following links or
  leaving the folder), then the folders left empty.
- **Import ▸ Copy of a staged file.** When the import dialog's mode is Copy for files that were staged, the file is
  copied a second time into the library's folders. The staged copy then has no photo and is deleted by the next
  cleanup a day later; use Add for picked files to keep one copy.

## Toolchain

- Rust targets: `rustup target add aarch64-linux-android x86_64-linux-android` (x86_64 for the emulator and
  Chromebooks; releases ship both).
- Android NDK r28c (minimum API 28, Android 9). `cargo xtask android check` finds it through `ANDROID_NDK_HOME`,
  `ANDROID_NDK_ROOT`, or the newest `~/Android/android-ndk-*`. CI installs the pinned, sha256-verified NDK with
  `.github/scripts/install-android-ndk.sh` (run it locally as well; it prints the `ANDROID_NDK_HOME` to export).
- Without the target or the NDK the check is skipped with a message. In CI (`CI` set) a missing toolchain fails.

The NDK's clang is needed for one C dependency, `ring` (used by SFTP publishing through `russh`). Everything else
in the graph is pure Rust.

## What is off on Android

- **Delete from Disk**: the `trash` crate has no Android backend, so `Session::trash` stays `None` (as on the web).
- **USB tethering**: `nusb` can't list devices on Android, where USB access needs a device handle from Java. Tether
  reports it as unsupported.
- **Updates**: the in-app updater has no Android target. Updates come from the Play Store or the GitHub release.
- **Sound out** (`audio-out` feature): stays off, because `cpal` would pull in the C++ `oboe` library.

## Rules for shared code

Android is `unix`. Code meant for Linux and the BSDs names them (`target_os = "linux"`, …), never
`all(unix, not(target_os = "macos"))`. `cargo xtask android check` catches mistakes.

## Building the APK

```sh
.github/scripts/install-android-ndk.sh   # prints ANDROID_NDK_HOME to export
.github/scripts/install-android-sdk.sh   # JDK 17+ and SDK platform 36 / build-tools 36 (prints JAVA_HOME, ANDROID_HOME)
rustup target add aarch64-linux-android  # and x86_64-linux-android for the emulator
cargo xtask android package [--abi arm64] [--abi x86_64] [--split] [--aab] [--store github|play] [--version-code N]
adb install -r target/android/<binary>-<version>-android-arm64-v8a.apk
```

There is no Gradle. The activity is one Java file on the platform's `NativeActivity` (no AndroidX), so xtask
drives the SDK tools itself (`xtask/src/apk.rs`):
1. `javac` and `d8` for the activity, rendered from `packaging/android/*.in` with the `brand.toml` values (the
   package is `android_package` from `brand.toml`: provisional, and permanent once the app is on the Play Store).
2. `aapt2` for the manifest and launcher icon.
3. The native library from `cargo build --profile dist`, every `--abi` in one cargo run (one `--target` each), so
   the host-side build is shared and all cores stay busy. One APK holds every ABI, or with `--split` one APK per
   ABI. The bundle always holds them all, and the Play Store gives each device only its own.
4. `zipalign -P 16` (16 KB pages, which Play requires) and `apksigner`.
5. With `--aab`, the same parts go into a Play Store bundle through `bundletool` (downloaded once, pinned by sha256)
   and are signed with `jarsigner`. The bundle always gets the Play manifest (no All files access); the APKs get
   the GitHub one unless `--store play` (Storage ▸ *Two builds*).

The version code is derived from the workspace version: `X.Y.Z` → `X·10⁶ + Y·10⁴ + Z·100 + 50`, and a prerelease
`-rcN` (or `-betaN`) uses `N` (01–49) in place of `50`; 51–99 are reserved for re-uploads of the same release. Releases are now numbered `YY.WW.B` (ISO year, week, release that week, e.g. `26.43.4` → 26 430 450), which
the formula already fits; see [Release versions](versioning.md).

`--version-code N` replaces the derived code (1 to 2 100 000 000, Play's limit). Every upload to Google Play, even
one it rejects or one later discarded, uses up its version code for good, so a second upload of the same version
needs a new one: take the next free code in the release's reserved 51–99 range (`26.43.7` uploaded as 26430750 →
`--version-code 26430751`). 26.43.5 predates this scheme (its release code was 26430599) and was re-uploaded as
26430600, the `00` slot the formula never produces.

**Signing.** Set `ANDROID_KEYSTORE` (a keystore file), `ANDROID_KEYSTORE_PASSWORD`, and optionally
`ANDROID_KEY_ALIAS` (default `upload`) and `ANDROID_KEY_PASSWORD`. Without them a debug key in
`<target>/android/debug.keystore` signs the build, and a warning says so. A debug-signed build installs for testing,
but the Play Store refuses it.

## Releases

`release.yml` has an `android` job, on unless the repository variable `RELEASE_ANDROID` is `false`. On gpu0 it
installs the pinned NDK, SDK and JDK (cached between runs), cross-compiles arm64-v8a and x86_64 in one cargo run,
and uploads an APK per ABI and one AAB to the draft release. Its build cache (`~/.cache/lcc-release-android`) is
shared by both gpu0 runner instances and backed by sccache. To try it without releasing, dispatch the Release
workflow as a dry run with `only: android`. Its secrets,
in the `release` environment, are all optional: `ANDROID_KEYSTORE_BASE64` (the upload keystore, base64),
`ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`. The in-app updater ignores `.apk` and
`.aab` files.

Without those secrets the release is signed with the debug key kept in that build cache, so successive releases
share it and update each other. Android refuses an update signed with a different key: when the release key is
added, or if the cache is wiped, people uninstall the old build first. Set up the upload key before Android
builds are announced. Create it with `keytool -genkeypair -v -keystore upload.jks -alias upload -keyalg RSA
-keysize 4096 -validity 10000`, keep it safe, and add it base64-encoded as `ANDROID_KEYSTORE_BASE64`.

