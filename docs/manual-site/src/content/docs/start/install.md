---
title: Install
description: Get Chromaforge running.
---

- Download from the project's **Releases** page.
- **macOS**: open the `.dmg`, drag the app to Applications. The app is not signed with an Apple developer certificate, so macOS warns first (see below). The [command-line tool](../../automate/cli/) is inside the app (see below).
- **Windows**: run the installer, or unpack the portable zip. The files are not code-signed, so SmartScreen may warn: choose **More info ▸ Run anyway**.
- **Linux**: AppImage, `.deb`, `.rpm`, Flatpak or a tarball.
- **Android**: install from [Google Play](https://play.google.com/store/apps/details?id=<app_id>), or sideload the `.apk` from Releases. Play runs a little behind Releases because of Google's review process.
- **Web**: open the browser build; it runs locally, nothing is uploaded.
- Build from source: `cargo xtask install`.
- Updates: the app checks for new releases and tells you.

## macOS: "unknown app" or "may be malware"

The app is not signed with a paid Apple developer account, so Gatekeeper blocks it the first time. If you trust it:

1. Open **System Settings** from the Apple menu.
2. Click **Privacy & Security** in the sidebar.
3. Scroll to the **Security** section.
4. Find the message that the app was blocked and click **Open Anyway**.
5. Enter your administrator password or use Touch ID, then click **Open**.

- Steps vary a little between macOS versions.
- On the newest macOS, if it still won't open, remove the quarantine flag in Terminal, then open it again:

```bash
xattr -c path/to/file.dmg
```

- Search "macos gatekeeper allow app" for Apple's own background.

## macOS: the command-line tool

`<cli>` comes inside the app, so it is installed and updated with it:

```bash
/Applications/<app>.app/Contents/MacOS/<cli> --version
```

- To type just `<cli>` in Terminal, link it into a folder on your `PATH` once. The link follows the app, so updates reach it:

```bash
mkdir -p ~/.local/bin
ln -sf /Applications/<app>.app/Contents/MacOS/<cli> ~/.local/bin/<cli>
```

- If `~/.local/bin` isn't on your `PATH` yet (`echo $PATH` doesn't list it), add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshrc` and open a new Terminal window. Or link it into `/usr/local/bin` instead, with `sudo ln -sf …`.
- Open the app once first and allow it (above). Until then, macOS stops the tool: Terminal prints `Killed: 9` and a dialog says it can't be opened. Once the app is allowed, the tool inside it runs too, directly and through the link.
- Moved the app out of Applications? Use its new place in the link.
- Earlier releases had the tool as a separate `.zip`. If you copied that `<cli>` somewhere on your `PATH`, delete it, so the one inside the app runs (`which <cli>` shows which one does).
