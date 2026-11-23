## Release builds and CI

- **One program for the app and the CLI:** the desktop packages carry one build of the engine instead of two. The CLI
  is a link to the app's executable on macOS (inside the bundle), Linux (tarball, `.deb`, `.rpm`, AppImage, Flatpak),
  FreeBSD and Nix, and a small console launcher on Windows; started under the CLI's name (or with `--cli` first) the
  app's executable runs the CLI and nothing of the app. Installations are about 170 to 250 MB smaller per
  architecture, and the CLI's commands, output and exit codes are unchanged.
