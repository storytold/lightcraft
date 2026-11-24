### Folders are checked as soon as they change, and the app runs once

- With folder polling on, the system now tells the app when a library folder changes (macOS, Windows, Linux), and the folder is checked a couple of seconds later; the check every few minutes stays for shares that send no notices.
- Opening the app again while it runs brings the running copy forward and hands it the photos, folders or link that were opened.
- `<scheme>://sync?path=…` links are registered with Windows and Linux desktops, so a script or another app can ask for a folder to be synchronized.
