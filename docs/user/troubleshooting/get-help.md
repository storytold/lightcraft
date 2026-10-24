---
title: Get help with a problem
description: What to include when you ask for help with a LightCraft bug, and where to post it.
order: 30
verified: 0.7.1
---

Open an issue in the [issue tracker](https://github.com/storytold/lightcraft/issues), or ask in [Discord](https://discord.gg/artcraft). Include your version, your system, what you did, and what you expected.

## What to include

1. **Your version.** Choose **Help** > **About LightCraft** (on macOS, **LightCraft** > **About LightCraft**).
2. **Your system.** For example Windows 11, 64-bit. **Help** > **System Info…** shows details about your computer.
3. **The steps.** What you clicked, in order, and what happened.
4. **The camera and file type**, if the problem is with a photo.
5. **A sample file**, if a raw or other photo renders wrong. Maintainers ask for the original file, not an export. A link to a shared folder works. Uploading raws to [raw.pixls.us](https://raw.pixls.us) also helps, because every file there becomes a permanent test sample for that camera.
6. **A screenshot** of LightCraft next to the camera JPEG or the other app, for colour problems.
7. **The log file.** Choose **Help** > **Open Log Folder** and attach `lightcraft.log`.

## Where the log is

LightCraft writes its log to `logs/lightcraft.log` in its settings folder. On Windows that is `%APPDATA%\LightCraft\logs\`. Each start moves the previous log to `lightcraft.1.log`, and that one to `lightcraft.2.log`, so the log of a run that crashed survives the next start.

## A more detailed log

When thumbnails or previews don't appear, appear slowly, or keep the computer busy, a log of every picture LightCraft draws helps. Start LightCraft with the environment variable `RUST_LOG` set to `warn,lightcraft*=info,lightcraft_ui_egui::render=debug`. On Windows, in PowerShell:

```powershell
$env:RUST_LOG = "warn,lightcraft*=info,lightcraft_ui_egui::render=debug"; & "C:\Program Files\LightCraft\lightcraft.exe"
```

The log then has a line for each picture LightCraft asks for, with its file, and a line for how it ended: drawn, or dropped and why (for example, scrolled out of view, or replaced by a newer request). A file that can't be read is still a warning. So the log shows whether a file was asked for once or many times. `LIGHTCRAFT_LOG=debug` instead turns on the detailed log of all of LightCraft.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft).
