---
title: Install Chromaforge
description: Download Chromaforge for Windows 11, macOS or Linux, install it, and open your photo library.
order: 10
verified: 0.7.1
---

Download the file for your system from the [latest release](https://github.com/emmertex/chromaforge/releases/latest) and install it. In the file names, `<version>` stands for the version number, for example `0.7.1`.

## Windows 11

1. Open the [latest release](https://github.com/emmertex/chromaforge/releases/latest).
2. Download `chromaforge-<version>-windows-x64.msi`. It fits most PCs (Intel and AMD, 64-bit).
3. Run the file. The installer asks where to install. The default is `C:\Program Files\Chromaforge`, and upgrades stay in the folder you chose.
4. At the end, choose the option to start Chromaforge.

For a Snapdragon or other Arm PC, download `chromaforge-<version>-windows-arm64.msi` instead. For a 32-bit Windows, download `chromaforge-<version>-windows-x86.msi`.

Upgrading from an installer older than 0.6.0 also keeps Chromaforge in the folder it was installed in. If that folder was deleted, the upgrade installs to `C:\Program Files\Chromaforge`.

To run it without installing, download `chromaforge-<version>-windows-x64-portable.zip`. Portable builds for `arm64` and `x86` are on the release page too.

## macOS

1. Download `chromaforge-<version>-macos-arm64.dmg` for an Apple silicon Mac, or `chromaforge-<version>-macos-x64.dmg` for an Intel Mac (Apple menu ▸ About This Mac says which: "Chip: Apple M…" or "Processor: … Intel"). `chromaforge-<version>-macos-universal.dmg` works on both and is twice the size. It isn't notarized, so the first time you open Chromaforge, macOS asks you to allow it (System Settings → Privacy & Security → Open Anyway).
2. Open the file and install Chromaforge.

The command-line tool, `chromaforge-cli`, is inside the app: `/Applications/Chromaforge.app/Contents/MacOS/chromaforge-cli`. It updates with the app. To run it from Terminal, link it onto your `PATH`: `mkdir -p ~/.local/bin && ln -sf /Applications/Chromaforge.app/Contents/MacOS/chromaforge-cli ~/.local/bin/chromaforge-cli`. If `~/.local/bin` isn't on your `PATH` yet, add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc`.

## Linux

The release has these files for `x86_64` and `aarch64` (Arm). Replace `x86_64` with `aarch64` on Arm.

| File                                         | Notes                                                    |
|----------------------------------------------|----------------------------------------------------------|
| `chromaforge-<version>-linux-x86_64.AppImage` | Runs anywhere.                                           |
| `chromaforge-<version>-linux-x86_64.flatpak`  | Sandboxed. Install with `flatpak install --user <file>`. |
| `chromaforge-<version>-linux-x86_64.deb`      | For Debian and Ubuntu.                                   |
| `chromaforge-<version>-linux-x86_64.rpm`      | For Fedora, RHEL and openSUSE.                           |
| `chromaforge-<version>-linux-x86_64.tar.gz`   | Unpack anywhere.                                         |

RISC-V and FreeBSD builds are on the release page too. On a Wayland desktop, Chromaforge runs through XWayland when it is available, because only X11 gives it file drag-and-drop.

## In a web browser

The release also has `chromaforge-web-<version>.zip`, a static site that runs Chromaforge in a modern browser. Host it on any static web server.

## First run

1. Start Chromaforge. It opens your library at `~/Pictures/Chromaforge Library`. A new library starts with demo photos.
2. To bring in your own photos, see [Import photos](../guides/import-photos.md).

## Still stuck?

Open an issue in the [issue tracker](https://github.com/emmertex/chromaforge/issues).
