# Build from source and contribute

> Names in angle brackets are the values set in [`brand.toml`](../../brand.toml); see the [manual index](README.md).

## Build from source

Workspace builds, including the desktop, web and CLI applications, require Rust 1.95 or newer.

```sh
git clone https://example.invalid/repo && cd repo
cargo xtask brand check                                 # the product name lives only in brand.toml
cargo xtask run                                         # build and open your library (~/Pictures/<app> Library; a new one starts with demo photos)
cargo xtask run -- ~/Pictures/trip                      # import your photos (folders are scanned, duplicates skipped)
cargo xtask run -- --memory                             # a throwaway in-memory demo session (writes nothing)
cargo xtask run -- --control 7980                       # with the automation channel
cargo xtask web --serve                                 # the same app in the browser: http://127.0.0.1:8080/
cargo run --release -p lcc-cli -- render photo.jpg -o out.jpg --set light.exposure=0.5
cargo xtask package                                     # installers and archives under the brand binary names
cargo xtask install                                     # install the app and CLI for the current user
cargo xtask ci                                          # fmt, clippy, tests, layering, wasm checks
```

Inside `target/` the binaries are always called `app` and `app-cli`; `cargo xtask run`, `package` and `install`
give them the brand names from `brand.toml`. `cargo run -p lcc-app` / `-p lcc-cli` also work for development.

CI defaults to line-table debug information and one build job per 1.5 GB of RAM
available when it starts (at most one per CPU; 4 if memory can't be read), so an
8 GB machine with 6 GB free builds with 4, and a busy machine with little free
builds with fewer. Test threads follow, capped at 4. Full-debuginfo linkers, one
per CPU, otherwise exhaust RAM before any test runs. GPU coverage is unchanged. Explicit
`CARGO_PROFILE_DEV_DEBUG`, `CARGO_BUILD_JOBS` and `RUST_TEST_THREADS` settings
override these defaults. Ordinary development commands keep their own settings.

**Chinese and Japanese text** need the shared font repo, an optional build input (official releases always include it):

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
CRAFT_FONTS_DIR=../craft-fonts cargo run --release -p lcc-app
```

Without it the app builds and runs the same, but Chinese and Japanese text have no glyphs. Fonts are never committed to this
repo — the standard for what goes there lives with it: see the [craft-fonts README](https://github.com/storytold/craft-fonts#readme).

**Interface language:** **Edit → Language** (English, 简体中文, 繁體中文（台灣）, 日本語, Português (Brasil), Español, Deutsch, Русский, Français) or **Settings → General →
Language**; the choice applies immediately and persists. See [docs/localization.md](../localization.md).

**Logs:** the desktop app writes its log to standard error and to `logs/<binary>.log` in its settings folder
(Linux `$XDG_CONFIG_HOME/<settings_dir>/logs/`, by default `~/.config/<settings_dir>/logs/`; macOS
`~/Library/Application Support/<settings_dir>/logs/`; Windows `%APPDATA%\<settings_dir>\logs\`), never in the library. A
launch from a desktop menu or the Dock has no terminal, so attach this file to a bug report (**Help ▸ Open Log
Folder** shows it in the file manager). Each start moves the
previous log to `<binary>.1.log` and that one to `<binary>.2.log`, so the log of a run that crashed survives the
next start; the file stops growing at 16 MiB, `--version` and `--help` write none, and runs with
`<PREFIX>_NO_PREFS` log to standard error only. By default the app's own crates log at `info` and everything else
at `warn`. `<PREFIX>_LOG=info` or `debug` works as before (that level for the app's own crates, warnings and
errors from the rest; any other value: warnings and errors only) and wins over `RUST_LOG`, which otherwise replaces
the default with env_logger-style directives such as `RUST_LOG=debug` or `RUST_LOG=warn,lcc_pipeline=trace` (a
directive ending in `*` covers every target starting with it, as in `lcc*=debug`). Panics are recorded there
too, and in `<binary>-panics.log` beside the log files. The logger is `apps/app/src/logging.rs`.

The web build needs the `wasm32-unknown-unknown` target and the matching `wasm-bindgen` CLI
(`cargo xtask web` prints the exact install command); see [docs/web.md](../web.md).

**Nix** builds the desktop app and `<cli>` (the Nix build always includes the craft-fonts input, so Japanese
text has glyphs):

```sh
nix run <flake-url>                    # the desktop app
nix build <flake-url>                  # → ./result/bin/{<binary>,<cli>}
nix develop <flake-url>                # rust toolchain + native deps + fonts
```

In a flake configuration (NixOS, home-manager, nix-darwin):

```nix
# flake.nix
inputs.app.url = "<flake-url>";
# optional: build against your own nixpkgs instead of the one the app pins
# inputs.app.inputs.nixpkgs.follows = "nixpkgs";

# then, in a NixOS or home-manager module (where `inputs` is in scope):
nixpkgs.overlays = [ inputs.app.overlays.default ];   # makes `pkgs.<binary>` available
environment.systemPackages = [ pkgs.<binary> ];           # home-manager: home.packages = [ pkgs.<binary> ];
```

`nix build` installs the same desktop file, hicolor icons and AppStream metadata as the .deb/.rpm, and runs
`cargo test --workspace` as its check phase (skip it with `pkgs.<binary>.overrideAttrs { doCheck = false; }`).

## How it's built

An engine-first Cargo workspace of small, tested crates with enforced layering (`cargo xtask layers`): `geom`,
`color`, `raster`, `tiff` → `raw`, `codecs`, `meta`, `develop` → `pipeline` → `catalog` → `engine` → `ui-egui`. The
egui frontend is one swappable crate; nothing below it knows a UI exists. `mcp` and the apps (`app`,
`<cli>`) sit on top of `engine`.

## Contributing

Humans and agents follow the same rules, so read [AGENTS.md](../../AGENTS.md) first. The short version:

- **Clean-room.** Never read Adobe binaries or GPL / LGPL raw and photo code (darktable, RawTherapee, LibRaw,
  rawspeed…); work from public specs and black-box observation. One exception: dcraw's functions outside its
  RESTRICTED (Foveon) block, which its author releases free for all uses, may serve as a reference under the dcraw
  reference policy in [AGENTS.md](../../AGENTS.md#clean-room-sources-in-addition-to-the-non-negotiables).
- **No Adobe assets, ever:** no icons, screenshots, presets, profiles, LUTs or fonts from Adobe products. Every
  image, icon and font in the repo is original, public domain, Creative Commons, OFL or permissively licensed, and has
  an entry in [assets/ATTRIBUTION.md](../../assets/ATTRIBUTION.md) added in the same commit. New fonts go to
  [storytold/craft-fonts](https://github.com/storytold/craft-fonts), not here.
- **Pure Rust**, enforced crate layering, everything is a command, and `cargo xtask ci` green before every commit
  (one task id per commit).
- **Never crash.** Non-test code returns errors instead of panicking: no `unwrap()`, `expect()`, `panic!` or
  `unsafe`, checked indexing on anything derived from input, and a regression test with every crash fix. Details in
  [AGENTS.md](../../AGENTS.md#never-crash-outranks-feature-work).

To take something on, claim its issue with a comment and open a pull request with `Fixes #123` in the description;
[CONTRIBUTING.md](../../CONTRIBUTING.md) has the whole workflow. Questions, ideas or a bug you'd like to talk through
first? [Join the Discord]({{discord}}) and bring them to [the project's channel](https://discord.com/channels/1359579021108842617/1559361045468942346).
