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

### Who can start a release

Repository settings (Settings ▸ Environments and Settings ▸ Rules), not code:

- The `release` environment, which holds every signing secret, is limited to the `release` branch: a run on any
  other branch that names `environment: release` is refused before it gets a secret.
- The `release` branch ruleset (**release: PR + approval**) forbids deleting it and force pushes, and takes changes
  only through a pull request approved by someone other than its author; a push after the approval needs a new one.
  So a release takes two people. The repository admin (emmertex) may bypass it, and so may an agent working with
  that account (`gh pr merge --admin`).
- [`.github/CODEOWNERS`](../.github/CODEOWNERS) names the owners of `.github/workflows/`, `.github/scripts/`, `packaging/` and
  `xtask/` (the code the signing jobs hand secrets to), and main's ruleset requires their review, so nobody can
  change what runs with the secrets alone. Caveat: the platform builds compile the workspace with the secrets set,
  so a `build.rs`, proc-macro or dependency could still read them; the PR into `release` is the check for that (the admin bypass applies here too).

`only` limits a dry run to some platforms, comma-separated: `macos`, `windows`, `linux`, `flatpak`, `android`,
`web`, `riscv64`, `freebsd` (`-f only=android`). A real run refuses it, since a release builds every platform.

How a real run flows:

1. **version** resolves the version and creates the draft release `v<version>` (or reuses an existing draft
   for that tag, deleting its old assets and retargeting it at this commit). It stops if `v<version>` is
   already published.
2. Each **platform job** (macOS, Windows x64/ARM64, Linux, Flatpak, web, and riscv64/FreeBSD when enabled)
   uploads its files to that draft with `gh release upload --clobber` as soon as it finishes, and its
   `sha256sum` output as a tiny `sums-<platform>` artifact. Flatpak fetches the Linux tarball from the draft.
   Files never round-trip through Actions artifacts, which are very slow from the self-hosted runners.
3. **release** (after every platform, whether it built or not) writes `SHA256SUMS.txt` for the files on the
   draft from those checksum artifacts, signs it (`SHA256SUMS.txt.sig`, see [Update signing key](#update-signing-key)),
   names failed platforms in the notes and the run summary, and uploads `SHA256SUMS.txt`, its signature and
   `latest.json`. The release stays a **draft**; publishing is a separate manual step.

## Prepare and start a release

1. **Land what is ready.** Pull requests, intake rounds included
   ([`docs/upstream-pr-intake.md`](upstream-pr-intake.md)), merge when they are reviewed and green, in any order; a
   release takes whatever `main` holds when its changelog PR merges. Branches must be up to date with `main`, so
   each merge leaves the other open PRs behind: merging several in a row means bringing each up to date
   (`gh pr update-branch N`, or a merge of `main` into it) and waiting for its checks (about 6 minutes) before the
   next. Merge a waiting intake PR early: it is the one most likely to need a merge of `main` by hand.
2. Gather the changelog on `main`, in a PR: `cargo xtask notes --release <version>` writes
   `docs/changelog/<version>.md` from the entries in `docs/changelog/unreleased/` (adding to the file if it was
   already started) and deletes them; `--dry-run` shows it first. Edit it as usual. Gathering on `main` keeps the
   entries from being gathered again for the next release.
   - If `main` moves before this PR merges, merge `main` into it and run `notes --release <version>` again, so the
     entries merged meanwhile are in this release's notes too.
   - Name the upstream pull requests that landed since the last release (the intake PRs' adopted rows) in a
     `## Upstream` section, one line per change ending `(upstream #N, <author>)`. Don't link them: the upstream
     repository's name fails the brand check.
   - **Check the manual against the changelog** in the same PR: every feature in `docs/changelog/<version>.md` has its
     page or line in `docs/manual-site/` / `docs/manual/`, nothing there describes removed behaviour, and the install
     and signing statements (macOS is not signed or notarized unless that changed) still hold.
3. Set the workspace version in `Cargo.toml` (for example, with `cargo xtask version set 1.2.0`), in the same PR.
   For a manual workflow run, the version input can override the workspace version for that run.
   Use a semantic version such as `1.2.0` or `1.2.0-rc.1`; versions containing a hyphen are
   marked as prereleases.
4. Dispatch CI on the commit (Actions → CI → Run workflow: the full test run on all three OSes) and wait for it to
   pass (about 2½ hours); a push to `release` doesn't run CI again. A workflow can only be dispatched on a branch of
   this repository, so when the changelog PR comes from a fork, dispatch on `main` once it has merged. Then bring
   that commit to `release`: a PR into `release` whose head is that commit (merging it gives exactly its tree),
   a push, or a dispatch of the workflow on `release` (a dispatch on another branch is a dry run; see below).
   The merge or push starts the release.
5. **Set the release notes while it builds.** The version job creates the draft with GitHub's generated list of pull
   requests, and the release job copies the draft's notes into `latest.json`, the "What's New" of the in-app
   update dialog. So once the draft exists, and before the platform jobs finish, replace them with the changelog:
   `gh release edit v<version> --notes-file docs/changelog/<version>.md`. Set later, the feed keeps the generated
   list: download `latest.json`, set its `notes` and `releases[0].notes` to the changelog, and upload it again
   with `gh release upload v<version> latest.json --clobber` (it isn't in `SHA256SUMS.txt`, so the signature
   still holds; `releases/latest/download/` serves the old file for a minute or so).
6. Check the workflow run. On success, the workflow creates or updates a **draft**
   GitHub Release named `<app> v<version>`, targeted at the commit that triggered the run.
   Review the draft and its `SHA256SUMS.txt`, then publish it in GitHub Releases when ready. The job log's
   *Sign the checksums* step says `SHA256SUMS.txt.sig verified`; if it warns that the list is not signed,
   the app will not install this release by itself (see [Update signing key](#update-signing-key)).
   [Check a draft release](#check-a-draft-release) works with any two small files from the draft (the web zip and
   the AppImage's `.zsync`, say) beside the checksum list and its signature.

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

The feed says *what* is newer; it does not decide what gets installed. Its `sha256` fields and GitHub's
`digest` come from the same place as the files, so the app checks every download against the release's
`SHA256SUMS.txt` instead, and only once that list carries a valid signature from the key pinned in
`brand.toml` ([Update signing key](#update-signing-key)).

Prerelease tags get a manifest too (with `prerelease: true`): the app only offers one when
"Include prereleases" is ticked, so a release candidate never reaches someone on a stable build.

## Builds and artifacts

Release jobs build macOS (Apple silicon, Intel and universal), Windows x64/ARM64, Linux x86_64 (aarch64 paused),
and the WASM web app. The platform scripts in [`packaging/`](../packaging/) write their outputs to
`dist/release/` (the full list of files is the README's Downloads section):

- macOS: three app DMGs from one build of each architecture: `-macos-arm64.dmg`, `-macos-x64.dmg` and
  `-macos-universal.dmg` (both builds). The CLI and the craft-fonts faces are inside the app. The
  universal image stays while installed copies need it: builds before issue #1228 update only through it
  (they take any `-macos-universal`, `-macos-aarch64` or `-macos-x86_64` file, so the per-architecture
  images must never use those two slugs). Dropping it is the maintainer's call and its own change.
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
- Android (on unless the repository variable `RELEASE_ANDROID` is `false`): an APK per ABI
  (`<binary>-<version>-android-arm64-v8a.apk`, `…-android-x86_64.apk`) and one Play Store bundle
  (`…-android-arm64-v8a-x86_64.aab`), cross-compiled on gpu0 (see [android.md](android.md)).

Only the macOS, Windows, Android and draft-release jobs use the `release` environment (not on a dry run). The
others sign nothing.

### Where the release jobs run

All on the maintainer's self-hosted runners (docs/ci.md, *Where the jobs run*):

| Job | Runner | Notes |
|---|---|---|
| version, web, draft release | gpu0 / gpu0-b | `nice -n 19`; jobs fetch the GitHub CLI into the job's temp dir (`.github/scripts/install-gh.sh`) where the runner has none |
| Linux x86_64 | gpu0, `ubuntu:22.04` container | Rust, registry and target dir in Docker volumes |
| Flatpak x86_64 | gpu0, `ubuntu:24.04` container, `--privileged` | Flathub runtime/SDK kept in a Docker volume |
| macOS | mac0 | `nice -n 19`; signing certificate in a temporary keychain in `$RUNNER_TEMP`, deleted (and taken off the search list) at the end of the job, stale ones removed at the start; the login keychain is never modified |
| Windows x64 | win0 | BelowNormal priority; .NET SDK and WiX in the runner tool cache |
| Windows ARM64 | SNAPDRAGON | native ARM64 build (win0 has no ARM64 MSVC tools) |
| Android | gpu0 / gpu0-b | both ABIs in one cargo run; one shared build cache (`~/.cache/lcc-release-android`) and sccache |

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
`b4ce3ee090ea4b06791e9501ea2f8e6f746d5cd3` (keep the pins in `packaging/fonts.sh` and
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

- **Update signing:** `UPDATE_SIGNING_KEY`, the private key the in-app updater's checksum list is signed
  with (next section). Without it the release builds, warns, and the app links to it instead of installing it.

Linux and web artifacts are not code-signed by this workflow. Each platform job computes the SHA-256
of its own files; the release job gathers them into `SHA256SUMS.txt` for every file on the draft and
attaches it.

## Update signing key

The in-app updater installs a release only when its `SHA256SUMS.txt` carries a valid ed25519 signature
(`SHA256SUMS.txt.sig`, base64, over the file's exact bytes) from a public key pinned in `brand.toml`
(`[updates] public_keys`), and the downloaded file matches the SHA-256 that signed list records. Anyone who
can change a release's files can also change its checksums; they cannot sign them. The file must also
carry the offered version in its name (`<binary>-<version>-<os>-<arch>…`, as packaging names every
artifact), so an older release's signed list and file, served under a newer tag, don't install as it:
keep that naming. A missing list or signature, a signature by another key, a list that doesn't name the
file, a file named for another version, or a file that doesn't match means no install, with a sentence
saying why; the release page link still works. A build whose
`brand.toml` has no key (a fork that hasn't made one, a dev build) checks for updates and links to the
release, but never downloads or installs one.

The release job signs with `cargo xtask update-key sign` (`.github/scripts/sign-checksums.sh`, after the
*Checksums* step) and then checks the signature against `brand.toml` with `cargo xtask update-key verify`,
before anything is uploaded: a secret that doesn't match a pinned key fails the job. Without the secret it
warns and uploads no `.sig`.

The tool is built in a step of its own, *Build the signing tool (no secrets)*, with `UPDATE_SIGNING_KEY`
and `GH_TOKEN` unset and into fresh folders under `$RUNNER_TEMP` (its own `CARGO_HOME` and target folder,
no compiler cache); the job's checkout keeps no token in `.git/config` (`persist-credentials: false`). The
signing step then runs that built program and compiles nothing. So the code a build runs (build scripts,
proc-macros) never runs while the key is in the environment, and no shared build cache goes into the
tool. That is all it protects against: see *Runner trust*. Run by hand, the script builds the tool with
`cargo xtask` as usual.

### Runner trust

The signing key is only as safe as the runner that holds it. The release job runs on the self-hosted
runner, which also builds other people's pull requests as the same user. Such a job can still reach the
signing step through:

- a `.cargo/config.toml` in any parent folder of the checkout (including `~/.cargo/config.toml` when the
  runner's work folder is under `$HOME`): a fresh `CARGO_HOME` doesn't stop cargo reading parent folders;
- the shared toolchain: `~/.rustup` and `~/.cargo/bin/cargo`.

The stronger option is to run the `release` job on a GitHub-hosted runner: it needs only `gh`, `jq` and
an xtask build, none of the self-hosted runner's tools. That is the maintainer's decision; until then,
treat anyone who can run code on that runner as able to reach the key.

### Create the key pair (once)

1. On your own machine: `cargo xtask update-key new --out <a file outside the repository>`. It refuses a
   path inside the repository and an existing file, and creates the file readable by you only. Keep that
   file somewhere safe (a password manager): a lost key is handled like a leaked one (see *Rotate the
   key*), and costs every installed build one update by hand.
2. Paste the file's whole contents (one line, `update-signing-secret:…`) into a new secret,
   `UPDATE_SIGNING_KEY`, in the `release` environment (Settings ▸ Environments ▸ release ▸ Add secret).
3. Put the **public** key the command printed into `brand.toml` and commit it:
   ```toml
   [updates]
   public_keys = ["<the printed key>"]
   ```
   Never put the private key there: `update-key verify` and the updater's tests refuse it. Builds from that
   commit on check signatures; earlier builds install the way they always did.

### Check a draft release

The *Sign the checksums* step of the release job logs `SHA256SUMS.txt.sig verified`. To check by hand,
download `SHA256SUMS.txt`, `SHA256SUMS.txt.sig` and one or more installers from the draft into an empty
folder and run `cargo xtask update-key verify <folder>`: it checks the signature against the `brand.toml`
of the commit you have checked out (check out the release's tag or commit first, so that is the key the
release's builds carry) and every listed file present in the folder against its SHA-256.

### Rotate the key

An installed app trusts only the keys it was built with, so a key change ships in a release signed with
the old key:

1. `cargo xtask update-key new --out <new file outside the repository>`.
2. Add the new public key to `brand.toml` beside the old one, `public_keys = ["<new>", "<old>"]`, and keep
   the **old** private key in `UPDATE_SIGNING_KEY`. Release as usual: this release is signed with the old
   key (which every installed build trusts) and carries the new one.
3. Once that release is out, and users have had time to take it, replace `UPDATE_SIGNING_KEY` with the new
   private key, and remove the old public key from `brand.toml`. Releases from then on are signed with the
   new key.

A build from before step 2 trusts only the old key: if it skips every release signed with the old key, it
can't install the first one signed with the new key, and its user updates once by hand from the release
page (the dialog says why). So keep signing with the old key for a few releases before
step 3.

**A leaked or lost key** can't go through step 2: a leaked one must not sign again, and a lost one can't.
Make a new key at once (step 1), put only the new public key in `brand.toml`
(`public_keys = ["<new>"]`) and the new private key in `UPDATE_SIGNING_KEY`, and release. Builds from
before then can't verify that release: each updates once by hand from the release page (the dialog says
so and links to it), and installs automatically again from there. Say so in the release notes.
