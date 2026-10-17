# Releasing the app

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<PREFIX>`, `<settings_dir>`, …) are the values set in
> [`brand.toml`](../brand.toml); see the README.

The release pipeline is [`.github/workflows/release.yml`](../.github/workflows/release.yml).
It runs on every push to the `release` branch. A maintainer can also dispatch it manually, with
an optional version override such as `1.2.0-rc.1`. A manual dispatch is a **dry run** unless
`dry_run` is unticked (`gh workflow run release.yml --ref <branch> -f dry_run=true`, the default): every
platform builds unsigned, no draft is created and nothing is uploaded, so nothing on GitHub's Releases page or
its tags changes. With `dry_run=false` only the `release` branch may run it: the version job refuses any other
branch, and the release environment's branch restrictions still apply to the signing jobs.

How a real run flows:

1. **version** resolves the version and creates the draft release `v<version>` (or reuses an existing draft
   for that tag, deleting its old assets and retargeting it at this commit). It stops if `v<version>` is
   already published.
2. Each **platform job** (macOS, Windows x64/ARM64, Linux, Flatpak, web, and riscv64/FreeBSD when enabled)
   uploads its files to that draft with `gh release upload --clobber` as soon as it finishes, and its
   `sha256sum` output as a tiny `sums-<platform>` artifact. Flatpak fetches the Linux tarball from the draft.
   Files never round-trip through Actions artifacts, which are very slow from the self-hosted runners.
3. **release** (after every platform, whether it built or not) writes `SHA256SUMS.txt` for the files on the
   draft from those checksum artifacts, names failed platforms in the notes and the run summary, and uploads
   `SHA256SUMS.txt` and `latest.json`. The release stays a **draft**; publishing is a separate manual step.

## Prepare and start a release

1. Gather the changelog on `main`, in a PR: `cargo xtask notes --release <version>` writes
   `docs/changelog/<version>.md` from the entries in `docs/changelog/unreleased/` (adding to the file if it was
   already started) and deletes them; `--dry-run` shows it first. Edit it as usual. Gathering on `main` keeps the
   entries from being gathered again for the next release. Then merge the release-ready changes into `release`.
2. Set the workspace version in `Cargo.toml` (for example, with `cargo xtask version set 1.2.0`).
   For a manual workflow run, the version input can override the workspace version for that run.
   Use a semantic version such as `1.2.0` or `1.2.0-rc.1`; versions containing a hyphen are
   marked as prereleases.
3. Push to `release`, or dispatch the workflow on `release` (a dispatch on another branch is a dry run; see below).
4. Check the workflow run. On success, the workflow creates or updates a **draft**
   GitHub Release named `<app> v<version>`, targeted at the commit that triggered the run.
   Review the draft and its `SHA256SUMS.txt`, then publish it in GitHub Releases when ready.

The workflow replaces assets when it updates an existing draft (the version job clears them first). It stops rather than overwriting a
release that has already been published, so bump the version before producing another release.

## The update feed

Each release carries a `latest.json` beside its artifacts. It is what Help ▸ Check for Updates… reads
(the in-app updater, [`crates/updater`](../crates/updater)): the version, the date, and one entry per
artifact with its file name, download URL, size and SHA-256. The app reads
`https://github.com/<owner>/<repo>/releases/latest/download/latest.json` first and falls back to GitHub's
Releases API, so the check keeps working when the API is rate-limited. Whatever the feed lists, the app
only takes files from `https://github.com/<owner>/<repo>/releases/download/` (the `repository` in
`brand.toml`) and only over https; a fork gets its own feed by setting its own `repository`. No command
or setting points the app at another feed (a test can, through the Rust API).

The `release` job builds the manifest from the draft's asset list (names and sizes) and the checksums the
platform jobs computed, so only what is actually attached to the release is offered. It is uploaded with the rest of the assets and
replaced on every run of the draft. Nothing else has to be maintained by hand: a new platform is offered as
soon as its artifact name carries the OS and architecture slugs the pipeline already uses
(`<binary>-<version>-<os>-<arch>…`).

Prerelease tags get a manifest too (with `prerelease: true`): the app only offers one when
"Include prereleases" is ticked, so a release candidate never reaches someone on a stable build.

## Builds and artifacts

Release jobs build macOS universal, Windows x64/ARM64, Linux x86_64 (aarch64 paused),
and the WASM web app. The platform scripts in [`packaging/`](../packaging/) write their outputs to
`dist/release/` (the full list of files is the README's Downloads section):

- macOS: app DMG and CLI ZIP.
- Windows: MSI installer and portable ZIP for each architecture.
- Linux: AppImage (with its `.AppImage.zsync`), `.deb`, `.rpm`, and `.tar.gz` for each
  architecture (`packaging/linux/package.sh`). Each AppImage embeds
  `gh-releases-zsync|<owner>|<repo>|latest|<binary>-*-linux-<arch>.AppImage.zsync`, so
  AppImageUpdate fetches only the changed blocks from the latest published (non-pre-) release.
- Linux riscv64 (job disabled since 0.8.0; run only when the repository variable `RELEASE_RISCV64` is `true`): `.tar.gz` only, cross-compiled on Ubuntu 24.04 against its riscv64 ports
  libraries (so glibc 2.39 or newer), with the CLI smoke-tested under QEMU (`CROSS_*` variables
  of `packaging/linux/package.sh`).
- Flatpak: a single-file `.flatpak` bundle for each architecture, repackaged from that
  architecture's Linux tarball (`packaging/linux/flatpak-bundle.sh` with
  `packaging/linux/flatpak/<app_id>.bundle.yml`; no Rust build). The from-source
  manifest `<app_id>.yml` is for Flathub; packaging-lint keeps their runtime and
  `finish-args` identical.
- FreeBSD (job disabled since 0.8.0; run only when the repository variable `RELEASE_FREEBSD` is `true`, and `freebsd.yml` is disabled): `<binary>-<version>-freebsd-x86_64.tar.gz`, a `/usr/local`-style tree built in a
  FreeBSD 14.3 VM (`packaging/freebsd/package.sh`, the same packages as `freebsd.yml`). Install
  with `tar -xzf <file> --strip-components 1 -C /usr/local`.
- Web: `<binary>-web-<version>.zip`.

Only the macOS, Windows and draft-release jobs use the `release` environment (not on a dry run). The
others sign nothing.

### Where the release jobs run

All on the maintainer's self-hosted runners (docs/ci.md, *Where the jobs run*):

| Job | Runner | Notes |
|---|---|---|
| version, web, draft release | gpu0 / gpu0-b | `nice -n 19`; jobs fetch the GitHub CLI into the job's temp dir (`.github/scripts/install-gh.sh`) where the runner has none |
| Linux x86_64 | gpu0, `ubuntu:22.04` container | Rust, registry and target dir in Docker volumes |
| Flatpak x86_64 | gpu0, `ubuntu:24.04` container, `--privileged` | Flathub runtime/SDK kept in a Docker volume |
| macOS universal | mac0 | `nice -n 19`; signing certificate in a temporary keychain in `$RUNNER_TEMP`, deleted (and taken off the search list) at the end of the job, stale ones removed at the start; the login keychain is never modified |
| Windows x64 | win0 | BelowNormal priority; .NET SDK and WiX in the runner tool cache |
| Windows ARM64 | SNAPDRAGON | native ARM64 build (win0 has no ARM64 MSVC tools) |

The Linux builds run in an Ubuntu 22.04 container and target glibc 2.35 or newer. AppImages and binaries may
also require system libraries for the windowing stack; the `.deb` and `.rpm` packages declare
their runtime dependencies.

Release builds fetch the pinned [`storytold/craft-fonts`](https://github.com/storytold/craft-fonts)
revision and require it (`CRAFT_FONTS_REQUIRED=1`). Keep that pin deliberate when updating the
workflow, and bump it (in `release.yml`, five jobs, and `freebsd.yml`) whenever craft-fonts adds a face a
shipped language needs: a stale pin still builds, it just ships tofu (issue #319: v0.4.0 pinned a revision
from before Noto Sans CJK SC, so Simplified Chinese had no glyphs). Before a release, check the pin against
craft-fonts' `fonts/manifest.txt` and run `CRAFT_FONTS_DIR=../craft-fonts cargo test -p lcc-ui-egui i18n`,
whose glyph-coverage test checks the characters in available faces. `CRAFT_FONTS_REQUIRED=1` now also makes
the native build fail if the pinned manifest lacks Noto Sans CJK SC Regular (`Hans`) or BIZ UDPGothic Regular;
the web build requires BIZ UDPGothic Regular and packages Noto Sans CJK SC as a separate asset.

Local macOS, Linux and Windows packaging scripts automatically use the sibling `../craft-fonts`
checkout. If it is absent, they clone it at revision
`3d9ab3a132ed0eff457e424226a435a901da30d3` (keep the pins in `packaging/fonts.sh` and
`packaging/windows/fonts.ps1` in sync with the release workflow). Existing checkouts are left untouched.
Set `CRAFT_FONTS_DIR` to use another
checkout; relative paths resolve from the workspace root. The script requires the Chinese and Japanese
UI faces and includes their OFL licences in the package. Download failures or incomplete checkouts
stop packaging with an error. `--skip-build` (`-SkipBuild` on Windows) packages existing binaries without downloading fonts or
changing what they embed; pass the same `CRAFT_FONTS_DIR` used to compile them so their licences ship.

## Signing credentials

All credentials are stored as secrets in the GitHub `release` environment. Signing is optional:
without platform signing credentials, packaging continues with unsigned artifacts and warnings.
macOS notarization is a separate optional step; when its credentials are absent, a build with a
signing identity is signed but not notarized. Configure only the platform credentials you need:

- **macOS signing:** `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, and `KEYCHAIN_PASSWORD`.
  **Notarization** additionally needs all of `APPLE_ID`, `APPLE_PASSWORD` (an app-specific
  password), and `APPLE_TEAM_ID`.
- **Windows signing:** either `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD`, or Azure
  Trusted Signing credentials: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
  `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT`, and `AZURE_CERT_PROFILE`.

Linux and web artifacts are not code-signed by this workflow. Each platform job computes the SHA-256
of its own files; the release job gathers them into `SHA256SUMS.txt` for every file on the draft and
attaches it.
