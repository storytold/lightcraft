---
title: Settings and privacy
description: Where settings live, and what goes online.
sidebar:
  order: 7
---

## Settings
- **Edit ▸ Settings…**. Tabs: General, Import, Folders, Performance, Display, Interface, Slideshow, Faces, Profiles, Develop, AI Denoise, AI Models, Generative Fill, Shortcuts, Controls, Connections, Sync. Every page and group is described in [Settings](../../reference/settings/); the **?** beside a group opens the manual at it.
- **Interface ▸ Expanded sliders** (off by default) lets sliders that support it reach past Lightroom's range; today only a mask's **Hue** (±180° instead of about ±34°). Expanded values are not XMP compliant: Lightroom gets them clamped. Turning it off narrows the slider but leaves edits already past the range as they are.
- App settings are in a `Chromaforge` folder: `~/.config/Chromaforge` (Linux), `~/Library/Application Support/Chromaforge` (macOS), `%APPDATA%\Chromaforge` (Windows). A portable copy keeps them beside the program.
- Logs are in a `logs` folder there, never in the library.
- The **library** is a separate folder you choose; see [Backup and sharing](../../sync/backup-and-sharing/).
- Immich keys are kept in the system keychain or an encrypted file, not in settings.
- Anyone who can read your settings folder can drive the app if you enable the control port, so enable it only when needed.

## Privacy
- **No** analytics, telemetry, crash reporting, accounts or ads. Your photos and catalog stay on your device.
- Connections are made only for features you use:
  - update check (off at start-up by default; **Help ▸ Check for Updates…**)
  - map tiles while the Map module is open
  - online place lookup, only if you set it and tick the box allowing positions to be sent
  - downloads you start: offline place list, lens profiles, AI models
  - your own Immich or SFTP server, and paired devices on your network
  - plug-ins, only for hosts you grant
- None carries an identifier for you or your device.
- Exports and shares go only where you send them. The full policy is `PRIVACY.md` in the repository.
