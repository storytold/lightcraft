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
| macOS `.dmg` (the app, with the CLI link and the craft-fonts faces inside) | `packaging/macos/package.sh` | release.yml (`arm64`, `x64` and `universal`, from one build of each architecture) | released builds are ad-hoc signed only (no Apple developer certificate, not notarized); signing/notarization run only if the secrets are set |
| Windows `.msi` | `packaging/windows/package.ps1`, `app.wxs.in` | release.yml (x64), windows-arm64.yml (installs and uninstalls) | released builds are unsigned; Authenticode signing via `sign.ps1` runs only if signing secrets are set |
| FreeBSD `.tar.gz` | `packaging/freebsd/package.sh` | release.yml / freebsd.yml | `--dry-run` works on any OS (packaging-lint) |
| Web (static) | `packaging/web/package.sh` | release.yml | hosting notes in `packaging/web/HOSTING.md` |

Versions are `vYY.WW.B`: ISO week-year, ISO week, and the release's number within that week (`v26.43.7`), the
same string in tags, file names and package metadata, see [Release versions](versioning.md).

Desktop integration (Linux, FreeBSD): `<app_id>.desktop`, MIME types for the catalog and preset extensions, and
AppStream metadata (`<app_id>.metainfo.xml.in`), the PTP udev rule, which describes the seven modules and the publish, Immich,
tethering, Edit In, actions and plug-in features. `packaging-lint.yml` validates the templates
(`desktop-file-validate`, `appstreamcli`, Flatpak manifest parity). `cargo xtask install` installs locally under
`~/.local` without a package.

### One program for the app and the CLI

Every desktop package carries one build of the engine: the app's executable, which is also `<cli>`. The CLI is a
library (`apps/cli`, `lcc_cli::main_with`) that the app's `main` runs before anything of the app (no log file, no
window, no GPU), when the program is started under the CLI's name (`<cli>`, `<cli>.exe`, case aside) or with `--cli`
as its first argument. Before this, the CLI was a second program of about the app's size (231 MB next to 239 MB on
Linux x86_64, 26.46.3).

| Platform | `<cli>` is | Made by |
|---|---|---|
| Linux tarball, `.deb`, `.rpm`, AppImage | a relative link `usr/bin/<cli>` → `<binary>` | `stage_binaries` (`packaging/env.sh`); `nfpm.yaml.in` (a `symlink` entry) |
| Flatpak | a link `/app/bin/<cli>` → `<binary>` | both `flatpak/<app_id>*.yml.in` manifests |
| FreeBSD | a relative link `bin/<cli>` → `<binary>` | `stage_binaries` |
| macOS | a relative link `Contents/MacOS/<cli>` → the bundle's executable, covered by the bundle's signature (`codesign --verify --strict --deep` accepts it) | `packaging/macos/package.sh` |
| Windows `.msi` and portable zip | `<cli>.exe`, a console launcher of about half a megabyte that starts `<binary>.exe --cli …` beside it and passes on its standard input, output, error and exit code; the app runs in a job that ends it with the launcher, so a Ctrl+C or a closed console stops both (programs the app started, such as an external editor, keep running) | `apps/cli-launcher`, `crates/winjob`, `packaging/windows/package.ps1` |
| Nix | a link to the wrapped `<binary>` (the wrapper keeps the name it was started by) | `nix/package.nix` |

- **Windows needs the launcher** because the app is a GUI-subsystem program: a terminal doesn't wait for it and its
  output has no console. The launcher is a console program (`package.ps1` checks both PE subsystems), and the app
  it starts inherits the launcher's standard handles. Ctrl+C in the terminal reaches only the launcher, so the app
  runs in a kill-on-close job object (`crates/winjob`) and ends with it, however the launcher ends. Programs the
  app starts stay out of the job and keep running. Its tests run only on Windows and have not been run yet.
- **The name decides.** A link of the user's own must keep the name `<cli>`: a link named otherwise starts the app.
  The macOS manual's `ln -sf …/Contents/MacOS/<cli> ~/.local/bin/<cli>` keeps it.
- **Headless machines:** the program hard-links only the C library, `libm`, `libgcc_s` and the loader; X11, Wayland,
  xkbcommon, EGL, GL and Vulkan are opened at run time, and only by the app's window. Started as `<cli>` it runs
  in a bare Ubuntu base chroot with none of them installed (checked 2026-11-21, arm64).
- **Cost:** none measurable. Started as `<cli>` it runs `--version` in 1.25 ms against 1.21 ms for the old separate
  CLI, and `commands --json` in 28.2 ms for both, with about 1 MB more peak memory (arm64 Linux, stripped release
  builds, median of 50 runs).
- **Updates:** the CLI marks its process (`lcc_updater::mark_command_line_tool`), so inside the macOS bundle it
  still never installs an update over the app, though the executable it runs is the app's.
- `cargo xtask install` and `cargo xtask package` still build and copy the two programs separately; the `app-cli`
  binary stays for development, the tests and xtask.

### Symbols

The Linux, FreeBSD and macOS packages strip their binaries (`strip` in each platform's `package.sh`). On macOS
that happens after `lipo` and before signing, since changing a signed binary breaks its signature, and it leaves
only the symbols dynamic linking needs: about 40 MB less per binary and architecture (measured on 26.47.1 for
Apple silicon; about 11 MB off the compressed disk image). Nothing looks its own symbols up at run time. Windows
needs nothing: MSVC builds keep symbols in a separate `.pdb`, which isn't shipped.

What's lost is function names in backtraces taken on users' machines: a `RUST_BACKTRACE=1` run of a packaged
binary shows none. A panic's message and file:line are compiled into the program, not read from symbols, so the
app's crash log (`<binary>-panics.log`, written by `lcc_engine::guard::install_hook`) still records them.

## Gaps

Nothing here is built or tested by this review beyond `cargo xtask package --render-only` and
`appstreamcli validate` (one pre-existing warning: the placeholder `developer` id in `brand.toml`).

1. **Flatpak permissions (decided 2026-10-11).** Both manifests grant `--share=network`: Map tiles and geocoding,
   Immich, publish services, SFTP web upload, IPP printing, PTP/IP tethering and model downloads need it, and each
   is opt-in inside the app. For USB tethering they grant `--device=all`. That is broad (every device node, not
   only cameras); `--device=usb` (Flatpak >= 1.15.11) would be narrower, but the CI builder (Ubuntu 24.04,
   Flatpak 1.14) rejects it. Switch to `--device=usb` once the builder and users' Flatpak are new enough.
   Users who want less run `flatpak override --user --nodevice=all --unshare=network <app_id>`.
2. **Flatpak filesystem access.** Only `xdg-pictures` is granted; a tethered-capture folder or publish destination
   elsewhere works only when picked through the portal. Folders typed by hand or remembered from a previous
   session may not be reachable.
3. **Tethering udev rule.** `packaging/linux/70-{app_id}-ptp.rules.in` (USB still-image class 06/01/01,
   `TAG+="uaccess"`) is installed to `/usr/lib/udev/rules.d/` by the `.deb`/`.rpm` packages and the tarball tree.
   AppImage and source users copy it by hand ([tethering.md](tethering.md) → Linux); the Flatpak cannot install
   udev rules, so Flatpak users need the rule from the host too. FreeBSD (devd) and Windows (WinUSB driver) have no
   equivalent shipped yet.
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
