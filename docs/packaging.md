# Packaging

> Names in angle brackets (`<app>`, `<binary>`, `<cli>`, `<app_id>`) are the values set in
> [`brand.toml`](../brand.toml). Release procedure and signing: [releasing.md](releasing.md).

## What exists

`cargo xtask package` renders every packaging template from `brand.toml` into `target/package/` and, without
`--render-only`, builds release binaries under their brand names with a man page and bash/zsh/fish completions for
`<cli>` (generated from `<cli> --help`) and the icon set. The platform scripts then build the installers; the
`release.yml` workflow runs each on its own platform.

| Format | Script | Built by CI | Status / notes |
|---|---|---|---|
| AppImage (+ `.zsync`) | `packaging/linux/package.sh` | release.yml (Ubuntu 22.04, glibc 2.35) | runs `--version` and checks update info in CI |
| `.deb`, `.rpm` | `packaging/linux/package.sh` + `nfpm.yaml.in` | release.yml | needs `nfpm`; runtime deps (Vulkan/EGL, X11/Wayland) as depends/recommends |
| `.tar.gz` (FHS tree) | `packaging/linux/package.sh` | release.yml | input for the Flatpak bundle |
| Flatpak bundle | `packaging/linux/flatpak-bundle.sh`, `flatpak/<app_id>.yml.in` (source build), `.bundle.yml.in` (from the tarball) | release.yml | installed and smoke-tested in CI; not on Flathub |
| macOS `.dmg` + CLI `.zip` | `packaging/macos/package.sh` | release.yml (universal) | ad-hoc signed unless signing secrets are set; notarization optional |
| Windows `.msi` | `packaging/windows/package.ps1`, `app.wxs.in` | release.yml (x64), windows-arm64.yml (installs and uninstalls) | Authenticode signing via `sign.ps1` when secrets are set |
| FreeBSD `.tar.gz` | `packaging/freebsd/package.sh` | release.yml / freebsd.yml | `--dry-run` works on any OS (packaging-lint) |
| Web (static) | `packaging/web/package.sh` | release.yml | hosting notes in `packaging/web/HOSTING.md` |

Desktop integration (Linux, FreeBSD): `<app_id>.desktop`, MIME types for the catalog and preset extensions, and
AppStream metadata (`<app_id>.metainfo.xml.in`), which describes the seven modules and the publish, Immich,
tethering, Edit In, actions and plug-in features. `packaging-lint.yml` validates the templates
(`desktop-file-validate`, `appstreamcli`, Flatpak manifest parity). `cargo xtask install` installs locally under
`~/.local` without a package.

## Gaps

Nothing here is built or tested by this review beyond `cargo xtask package --render-only` and
`appstreamcli validate` (one pre-existing warning: the placeholder `developer` id in `brand.toml`).

1. **Flatpak has no network permission.** The manifests deliberately omit `--share=network` (it was only needed for
   the optional SAM 3 download). The Map module's tiles and geocoding, Immich, publish services, SFTP web upload,
   IPP printing and model downloads now all need it, so in the Flatpak they fail until the user runs
   `flatpak override --user --share=network <app_id>`. Decide whether to grant it (and update both manifests,
   which `packaging-lint` keeps identical) or document the override in the app's error messages.
2. **Flatpak filesystem access.** Only `xdg-pictures` is granted; a tethered-capture folder or publish destination
   elsewhere works only when picked through the portal. Folders typed by hand or remembered from a previous
   session may not be reachable.
3. **Tethering udev rule: TODO.** Native PTP/USB camera control doesn't exist yet (studio capture watches a folder),
   so no udev rule is shipped. When the PTP agent lands, add `packaging/linux/<NN>-<binary>-ptp.rules` (camera
   USB class `06`, `TAG+="uaccess"`), install it in `package.sh` (deb/rpm: `/usr/lib/udev/rules.d/`), document it
   for AppImage/tarball users, and add `--device=all` (or a USB portal) to the Flatpak.
4. **Not on any store or repository:** no Flathub submission, no APT/RPM repository, no Homebrew cask, no winget
   manifest, no FreeBSD port (only a tarball). The `homepage`/`repository` values in `brand.toml` are placeholders,
   which AppStream URLs and the AppImage update info inherit.
5. **No Snap**, no Arch `PKGBUILD` (the Nix flake is community-maintained and not in CI).
6. **Installers are not tested end to end here:** macOS signing/notarization and Windows signing depend on CI
   secrets; only the Windows arm64 MSI and the Flatpak/AppImage have install smoke tests.
7. **Fonts:** releases embed craft-fonts (`CRAFT_FONTS_REQUIRED=1`); local `package.sh` builds without
   `CRAFT_FONTS_DIR` ship without CJK glyphs and don't warn.
8. **Docs:** the user manual (`docs/manual/`) is not installed by the packages; `share/doc/<binary>` holds the README and
   licences. Consider shipping it, or pointing Help → `<app>` Help at a hosted copy.
