## New features

- **Folder polling** reacts to the system's change notifications (FSEvents, ReadDirectoryChangesW, inotify), with the interval poll kept as the fallback (gap 37, #123).
- **One copy at a time:** a second launch hands its files and links to the running app and exits; `<PREFIX>_NEW_INSTANCE` or any option starts a separate copy.
- **Links** (`<scheme>://sync?path=…`) are registered by the Windows installer and the Linux desktop entry (not yet on macOS).
