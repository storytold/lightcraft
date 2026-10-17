# Getting started

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

## Install

Release builds are an AppImage, `.deb`, `.rpm` and `.tar.gz` for Linux, a `.dmg` for macOS, an `.msi` for Windows,
a FreeBSD package and a static web build. [packaging.md](../packaging.md) lists what each contains and its status.
From source:

```sh
cargo xtask run                       # build and start the desktop app under its brand name
cargo xtask install                   # release build into ~/.local: binaries, man page, completions, desktop entry
```

Optional: clone [craft-fonts](https://github.com/storytold/craft-fonts) next to the repository and build with
`CRAFT_FONTS_DIR=../craft-fonts`, so Chinese and Japanese text has glyphs.

## First launch

`<binary>` opens (or creates) your library, `<library_default>` under your Pictures folder. A library is a folder
holding the catalog (`<name>.<catalog_ext>` and its store), previews and backups. Originals stay where they are
unless you copy them on import. File → New Catalog… and File → Open Catalog… switch libraries
([catalogs.md](catalogs.md)).

To try things without your own photos, use the procedural demo library: `<cli> run --demo …`,
`<cli> snapshot --demo` and `<cli> mcp --demo` all start with it.

## First import

File → Import Photos… (`Cmd+Shift+I`) picks files, folders or a card; choose to add them in place or copy them, a
destination and file naming, and a develop preset, metadata and keywords to apply. Coming from Lightroom Classic?
File → Import Lightroom Catalog… reads your `.lrcat` directly ([migration guide](migrating-from-lightroom.md)).

## Develop and export

Select a photo and press `D` for Develop. Edits are non-destructive and stored in the catalog (and in XMP sidecars
when File → Automatically Write Changes into XMP is on). File → Export… writes JPEG, PNG, TIFF, WebP, AVIF, DNG or
the original; the command line uses the same encoder:

```sh
<cli> render photo.ARW -o photo.jpg --set light.exposure=0.5 --size 2048
```

## Modules and panels

Library `Cmd+Alt+1`, Develop `D`, Map `Cmd+Alt+3`, Book `Cmd+Alt+4`, Slideshow `Cmd+Alt+5`, Print `Cmd+Alt+6`,
Web `Cmd+Alt+7`. `F5`–`F8` show the module picker, filmstrip and side panels; `Tab` hides the side panels,
`Shift+Tab` everything. See [modules.md](modules.md) and [keyboard-shortcuts.md](keyboard-shortcuts.md).

## Everything is a command

Every menu item, slider and dialog action is a command with an id (`photo.rate`, `develop.set`, `publish.run` …):

- `<cli> commands` lists them with their parameters, `<cli> controls` lists the develop sliders.
- `<cli> run --library DIR cmd key=value …` runs them headlessly; `<binary> --control 7980` plus
  `<cli> run --connect …` drives the running app ([control-protocol.md](../control-protocol.md)).
- `<cli> mcp` exposes them to AI agents ([mcp.md](../mcp.md)).
