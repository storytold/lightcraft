---
title: Command line
description: The <cli> program runs commands, renders and exports without a window.
sidebar:
  order: 2
---

- **Start**: `<cli> --help` lists everything; `<cli> --version` prints the version.
- **`<cli> run`**: run a chain of commands and print one JSON line per command. A word without `=` starts the next command; `key=value` values are JSON when they parse.
- **`<cli> render`**: develop one file and export it (see [Headless](../headless/)).
- **`<cli> run-action NAME`**: play a saved [action](../actions/) on the selection or on `ids`.
- **`<cli> sync-folder FOLDER`** and **`open-url URL`**: synchronise a library folder, or follow a `<app>` link.
- **`<cli> mcp`**: the [MCP server](../mcp/).
- **`<cli> merge hdr|panorama|hdr-panorama|hdr-batch|focus FILES…`**: Photo Merge without a window.
- **`<cli> quality-scan [FOLDER]`**: the [Image Quality Scanner](../../library/quality-scanner/) without a window: scores the library (or a folder) and prints the summary as JSON. `--accepted`, `--rejected`, `--apply-flags`, `--rescan` and `--weights` set what the dialog sets; `--connect` scans in the running app. `run photo.qualityScan …` does the same from a script.
- **`<cli> commands` / `controls`**: the command and Develop-control registries (`--json` for scripts).
- **`<cli> compare REFERENCE TEST`**: how far two renders of one photo are apart (CIEDE2000 by tone band and hue).
- **Library choice**: `--demo` (procedural demo photos), `--library DIR` (a saved library; `@app` is the one the desktop app opens), `--import PATH`, or `--connect` to drive the running app.
- **Calibration tools** for contributors: `calibrate`, `calibrate-chart`, `calibrate-lightroom`, `camdb`; `denoise-serve` runs AI Denoise tiles for other computers.

```sh
<cli> run --import ~/Photos/a.dng develop.set control=light.exposure value=0.7 \
    develop.auto app.export path=/tmp/a.jpg longEdge=1600
<cli> run --library ~/lib --import ~/Photos/shoot library.info
<cli> run --demo --script steps.jsonl --keep-going
```

- Exit status is non-zero when a command fails; `--keep-going` continues after a failure.
- A library is open in one program at a time. While the app has it open, use `--connect` instead of `--library`.
