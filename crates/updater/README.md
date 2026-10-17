# lcc-updater

The app's auto updater (L0, native only): it finds a newer release, downloads the file for this
machine, and stages installing it. There is no update service behind it — the release feed is a file
the release pipeline publishes, plus GitHub's own Releases API as a fallback.

```rust
// what is newer than this build?
let opts = lcc_updater::Options::default();          // brand repository, this version, this platform
if let Some(update) = lcc_updater::check(&opts, None, None)? {
    println!("{} → {}", update.current, update.version);   // 0.7.0 → 0.8.0
    println!("{} ({})", update.asset.name, update.action); // the file, and what installing it does
    // fetch it (resumable, size- and SHA-256-checked) into the cache folder
    let got = lcc_updater::download(&update, &lcc_updater::client()?, dir, None, None)?;
    // say what would happen, then do it (installs over the running installation)
    let plan = lcc_updater::plan(update.asset.kind, &got.path, &update.version.to_text());
    let outcome = lcc_updater::install(update.asset.kind, &got, &update.version.to_text())?;
    if outcome.exit { /* quit: the installer or the replacement script takes over */ }
}
```

- **The feed.** `latest.json` on the newest release (`…/releases/latest/download/latest.json`),
  else `GET /repos/{owner}/{repo}/releases` (no token needed for a public repository; a 403 is the
  rate limit, reported as such), else a URL a Rust caller names (`Options::manifest_url`: a mirror
  or a test; no command or setting reaches it). Both shapes parse into `Release`; the newest
  **version** wins, not the feed's order.
- **Where files may come from.** Only `https://github.com/<owner>/<repo>/releases/download/…` of the
  brand's `REPOSITORY` (`release_download_prefix`); a file the feed lists anywhere else is passed
  over, and a feed, file or checksum list on plain `http://` is refused. Tests trust one local
  origin through `Options::extra_origin`.
- **Versions** are SemVer 2.0.0 (`v1.2.3`, `1.2.3-rc.1`); a tag that is not a version (`nightly`) is
  not a release of ours. A prerelease is only offered when asked for, and a release outranks its own
  prereleases.
- **The right file** is picked from the artifact names the pipeline already uses
  (`<binary>-<version>-<os>-<arch>…`): the MSI on Windows, the DMG on macOS, the AppImage on Linux
  x86_64/aarch64, and a tarball (or the portable zip) as a manual install anywhere else. An
  installer always wins over an archive; the `SHA256SUMS.txt` asset is read for the hash, never
  installed.
- **Downloads** resume from a `.part` file, are capped, and must match the size the feed states and
  the SHA-256 the release publishes (the size alone when it publishes none) before they are renamed
  into place. A file that fails its check is deleted. A file already there is re-hashed, not trusted
  by name; a hostile asset name (`../../…`) is reduced to its last path segment.
- **Installing** is `install::plan` (what will happen, in words, before anything does) and
  `install::install`: `msiexec` runs the Windows installer; on macOS the disk image is mounted
  read-only, its `<display name>.app` (by that name, never a link) is copied beside the running
  bundle, checked against the release's version and swapped in with two renames that are undone if
  anything fails, then started again once the app has quit (in-process: no Terminal, no script); a
  program that isn't inside an `.app` is pointed to the release page instead. A Linux script waits
  (up to 30 s) for the app to quit, keeps the running AppImage as `.old` (hard link, else copy),
  renames the new one over it in one atomic step and starts it, and anything else is revealed in the
  file manager for a manual unpack. The Linux script is written with `O_EXCL` into a fresh 0700
  folder (under `$XDG_RUNTIME_DIR`, else the download folder) and deletes itself when done; if the
  app doesn't quit in time or a step fails, the original stays, the staged file is deleted, nothing
  starts, and the reason is left for the next start to log (`clean_up_after_update`, which also
  deletes the `.old` beside `$APPIMAGE`). Every path ends with the app running again or with a
  sentence saying what to do.
- **Never a panic.** No network, a rate-limited API, a release without a file for this machine, a
  truncated download, a full disk, an installer that refuses: all are `UpdateError` values a dialog
  can show, and all leave the current installation untouched.

## Sources

Own design, from the documented [GitHub Releases API](https://docs.github.com/rest/releases) and
the layout of release assets; version precedence follows [Semantic Versioning
2.0.0](https://semver.org/spec/v2.0.0.html). The shape of the flow (check → download → install and
restart, with a manifest beside the release) follows Tauri's updater as a design reference, not its
code.

Tests use local HTTP servers for the feed, the checksum list and the file: they make no internet
requests. The end-to-end path (feed → download → staged file) is covered in `lcc-engine`.

The macOS install is after craft-launcher's in-process disk image install
([storytold/craft-launcher](https://github.com/storytold/craft-launcher), Apache-2.0), written anew
from the documented options of `hdiutil(1)`, `ditto(1)`, `plutil(1)` and `open(1)`. Its tests make
their own small disk images (`hdiutil create`) with fake bundles in a temp folder.

## Testing the macOS install by hand

A real install of a real build, from a scratch copy and with no feed involved (never on
`/Applications`, never with the user's settings or library). Build once, package it twice under two
versions, install the newer image over the older bundle. `$V` is the workspace version
(`Cargo.toml`): the binary is the same in both, so only `Info.plist` tells the two apart, and the
relaunched app reports the workspace version either way. Run it from the repository root, in bash
or zsh:

```sh
export CARGO_TARGET_DIR="$PWD/target/macos-hand-test" S="$TMPDIR/updater-test"
V="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n 1)"                  # workspace version
APP="$(sed -n 's/^display_name *= *"\([^"]*\)".*/\1/p' brand.toml | head -n 1).app"  # bundle name
unset MACOS_SIGN_IDENTITY APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID   # ad-hoc signing, no notarization
mkdir -p "$S/old" "$S/cfg" "$S/lib" "$S/m"
# no update check in the relaunched app (the test refuses to run without this)
echo '{"version": 1, "updates": {"checkAtStartup": false}}' > "$S/cfg/updates.json"
DIST="$S/a" PACKAGE_VERSION=0.0.1 packaging/macos/package.sh --arch aarch64
DIST="$S/b" PACKAGE_VERSION="$V" packaging/macos/package.sh --arch aarch64 --skip-build
hdiutil attach -readonly -nobrowse -mountpoint "$S/m" "$S"/a/*.dmg
ditto "$S/m/$APP" "$S/old/$APP" && hdiutil detach "$S/m"
UPDATER_TEST_DMG="$(ls "$S"/b/*.dmg)" UPDATER_TEST_BUNDLE="$S/old/$APP" \
  UPDATER_TEST_VERSION="$V" UPDATER_TEST_CONFIG_DIR="$S/cfg" UPDATER_TEST_LIBRARY="$S/lib" \
  cargo test -p lcc-updater --lib a_real_build_is_replaced_in_place -- --ignored --nocapture
```

The ignored test refuses to run unless `$S/cfg/updates.json` turns the start-up check off. It then
installs, and starts the new bundle on the scratch settings and library: unlike the app's own
relaunch (a plain `open -n` of the bundle once the app has quit), it passes them with
`open --env <PREFIX>_CONFIG_DIR=… --args --library …`, because an app started by `open` doesn't
inherit the shell's environment.

Afterwards the scratch bundle's `Info.plist` names `$V`, no `.partial` or `.old` is left beside
it, and the app runs on the scratch settings and library. Then:

```sh
pgrep -fl "$S/old/"                  # the relaunched app runs from the scratch bundle
# look at its window (empty scratch library), then quit it with ⌘Q
pgrep -fl "$S/old/" || echo quit     # nothing runs from it any more
hdiutil info | grep -F "$S" || echo "nothing mounted"
rm -r "$S"
```
