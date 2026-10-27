# LightCraft app icon

A **lynx**, frontal head-and-shoulders portrait with an alert gaze, in the style of an engraving: the
Crafting Apps "owl template" (full-bleed colour field, tight portrait, the figure in paper with ink line
work and a crisp ink contour, no roundel or border).

## Palette (exactly three colours)

| Colour | Hex | Used for |
|---|---|---|
| Ink | `#0b0b0c` | line work and the contour |
| Paper | `#efe9dc` | the figure (the lynx) |
| LightCraft amber | `#f2a516` | the full-bleed field (the app colour) |

## Geometry

`viewBox="0 0 512 512"`, a rounded-square tile with `rx=112`, clipped; the colour fills the whole tile.
macOS files (`.icns`, `lightcraft-macos-512.png`) add Apple's transparent margin (an 824/1024 body);
Windows and Linux files use the full-bleed tile so the lynx reads at 16-48 px.

iOS and Android mask a square themselves, so their files drop the tile's clip. The traced figure stops
6.5 units short of the right and bottom edges and follows the tile's rounded corner, so:

- **iOS/iPadOS:** a 1024 px opaque square with the art scaled 1.03 from the top left, so the body runs
  past the system's corner mask. Dark appearance: the figure alone, on the dark field iOS draws.
  Tinted: a grayscale paper figure on black, tinted by luminance.
- **Android:** an adaptive icon. The background is the amber colour, the 108 dp foreground is the
  figure at 0.148 dp per unit centred on the head (266, 176): the ear tufts stay inside the 72 dp
  circle mask, the right edge sits at the viewport's edge, and the body runs off the bottom.
  Monochrome (themed icons): the paper figure, with the line work cut out of it.

## Provenance

The owner's original drawing, made in ArtCraft (2880 px, keyed to the three colours), vectorised with
craftrules `assets/logo-options/_tools/vectorize_tile.py`. The source drawing stays in craftrules at
`assets/app-icons/lightcraft/source.png`; `lightcraft.svg` here is the canonical artwork. Licence:
`LICENSE.txt` (MIT OR Apache-2.0, like the repo).

## Files

| File | What |
|---|---|
| `lightcraft.svg` | canonical vector, traced at 2048 px (about 770 KB) |
| `lightcraft-small.svg` | lighter vector, traced at 1024 px (about 360 KB) |
| `lightcraft-1024.png` | 1024 px render (store listings, docs) |
| `lightcraft-macos-512.png` | runtime window/Dock icon on macOS (embedded by `apps/lightcraft/src/main.rs`) |
| `lightcraft.icns` | macOS bundle icon (`CFBundleIconFile`) |
| `lightcraft.ico` | Windows icon, 16-256 px, embedded in `lightcraft.exe` by `apps/lightcraft/build.rs` |
| `hicolor/<n>x<n>/apps/ai.storyteller.lightcraft.png` | Linux icon theme, 16-512 px; the 256 px one is also the runtime window icon on Windows and Linux |
| `hicolor/scalable/apps/ai.storyteller.lightcraft.svg` | Linux scalable icon (the small SVG) |
| `ios/Assets.xcassets/AppIcon.appiconset/` | iOS/iPadOS app icon (default, dark, tinted), compiled into the bundle by `actool` in `apps/lightcraft-mobile/scripts/ios-sim.sh` |
| `android/res/` | Android adaptive launcher icon (`mipmap-anydpi-v26/ic_launcher.xml`, foreground and monochrome PNGs per density, background colour), linked by `apps/lightcraft-mobile/scripts/android-apk.sh` |

The app id is `ai.storyteller.lightcraft`: the Wayland app id, the `.desktop` file
(`packaging/linux/ai.storyteller.lightcraft.desktop`, `Icon=ai.storyteller.lightcraft`) and the hicolor icon
name. To install on Linux, copy `hicolor/` into `/usr/share/icons/hicolor/` (or `~/.local/share/icons/hicolor/`)
and the `.desktop` file into `applications/`.

## Regenerate

Edit `lightcraft.svg` (or `lightcraft-small.svg`), then run `packaging/icons.sh` (needs `resvg`; `iconutil`
on macOS for the `.icns`; the `.ico` is packed by `cargo xtask ico`). Every output is committed. The
`Contents.json` files and the Android XML are written by hand; the script only renders the PNGs.
