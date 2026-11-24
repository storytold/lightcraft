## Fixes

- **Linux AppImage:** the app registers a launcher entry and its icon for itself on first start, so the Wayland and X11 taskbars show its icon instead of the generic one. Entries from a package or a launcher tool are left alone.
