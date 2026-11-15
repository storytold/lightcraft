### Immich albums as collections, one photo across devices and Immich

- Device sync now names a photo by the SHA-1 of its original, the same checksum Immich uses. A device that holds
  the file and a device that holds only an Immich link see the same photo, never two.
- A device that hasn't hashed a photo yet still matches it with the other devices; synced catalogs move to the new
  names on their own, keeping every edit.
- A photo that arrives from another device is looked up on your Immich server first, so an original Immich
  already has is linked instead of uploaded again.
- Importing a file you already have as an Immich link-only photo no longer makes a second photo: the Immich photo
  takes the file and keeps its edits.
- **Show Immich albums as collections** (Settings ▸ Connections) lists every Immich album as a collection in the
  Immich set. On each device choose None, Previews or Full Quality in Settings ▸ Sync; photos added on any device
  go to the Immich album, uploaded only if Immich doesn't have them yet.
- **Share with synced devices** sends an Immich connection, key included, to your other devices over their
  encrypted link; off, the connection and its albums stay on this device.
- **Sync this device's camera roll** (Settings ▸ Sync) puts new photos into the collection "Camera Roll - ‹device›",
  without duplicates when the Immich app uploads them too.
- The Immich "Collections ↔ albums" name-matching rule is gone: albums now show as collections (accounts that
  used the rule are switched over).
