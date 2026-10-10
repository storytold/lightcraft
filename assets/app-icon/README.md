# App icon

Every file here is generated from `brand.toml`'s `icon_svg` (`brand/icon.svg`) by `packaging/icons.sh`.
Do not edit them by hand: change the SVG (or point `icon_svg` at another one) and rerun the script.

| File | What |
|---|---|
| `app-1024.png` | 1024 px render (store listings, docs) |
| `app-macos-512.png` | runtime window/Dock icon on macOS (embedded by `apps/app/src/main.rs`); Apple's grid, with a transparent margin |
| `app.icns` | macOS bundle icon (`CFBundleIconFile`) |
| `app.ico` | Windows icon, 16-256 px, embedded in the executable by `apps/app/build.rs` |
| `hicolor/<n>x<n>/apps/app.png` | Linux icon theme, 16-512 px; the 256 px one is also the runtime window icon on Windows and Linux |
| `hicolor/scalable/apps/app.svg` | Linux scalable icon (a copy of the source SVG) |

Packaging installs the hicolor icons under the brand's app id (`packaging/env.sh` `install_icons`).

## Regenerate

`packaging/icons.sh` needs `resvg` (or `rsvg-convert`); the `.icns` needs `iconutil` (macOS) or Python Pillow,
and the `.ico` is packed by `cargo xtask ico`. Every output is committed.
