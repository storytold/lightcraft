### AppImage shows its own icon in the taskbar

- On Linux, an AppImage now registers a launcher entry and its icon for itself on first start (in `~/.local/share`), so the taskbar and the window switcher on Wayland and X11 show the app's icon instead of a generic one. The entry follows the file when it is moved or renamed; one installed by a package or a launcher tool is left alone.
- Only a start from the AppImage itself registers the entry: a build or test run in a terminal that was itself started from an AppImage (which passes its own `APPIMAGE` on) writes nothing.
