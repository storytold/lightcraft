# Releasing LightCraft

The release pipeline is [`.github/workflows/release.yml`](../.github/workflows/release.yml).
It runs on every push to the `release` branch. A maintainer can also dispatch it manually, with
an optional version override such as `1.2.0-rc.1`; the release environment's branch restrictions
still apply.

## Prepare and start a release

1. Merge the release-ready changes into `release`.
2. Set the workspace version in `Cargo.toml` (for example, with `cargo xtask version set 1.2.0`).
   For a manual workflow run, the version input can override the workspace version for that run.
   Use a semantic version such as `1.2.0` or `1.2.0-rc.1`; versions containing a hyphen are
   marked as prereleases.
3. Push to `release`, or dispatch the workflow from a branch allowed by the `release` environment.
4. Check the workflow run and its artifacts. On success, the workflow creates or updates a **draft**
   GitHub Release named `LightCraft v<version>`, targeted at the commit that triggered the run.
   Review the draft and its `SHA256SUMS.txt`, then publish it in GitHub Releases when ready.

The workflow replaces assets when it updates an existing draft. It stops rather than overwriting a
release that has already been published, so bump the version before producing another release.

## Builds and artifacts

Release jobs build macOS universal, Windows x86/x64/ARM64, Linux x86_64/aarch64, and the WASM web
app. The platform scripts in [`packaging/`](../packaging/) write their outputs to `dist/release/`:

- macOS: app DMG and CLI ZIP.
- Windows: MSI installer and portable ZIP for each architecture.
- Linux: AppImage, `.deb`, `.rpm`, and `.tar.gz` for each architecture.
- Web: `lightcraft-web-<version>.zip`.

The Linux builds run on Ubuntu 22.04 and target glibc 2.35 or newer. AppImages and binaries may
also require system libraries for the windowing stack; the `.deb` and `.rpm` packages declare
their runtime dependencies.

Release builds fetch the pinned [`storytold/craft-fonts`](https://github.com/storytold/craft-fonts)
revision and require it (`CRAFT_FONTS_REQUIRED=1`). Keep that pin deliberate when updating the
workflow.

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

Linux and web artifacts are not code-signed by this workflow. The release job computes SHA-256
checksums for the complete artifact set and attaches the checksum file to the draft release.
