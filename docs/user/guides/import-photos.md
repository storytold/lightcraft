---
title: Import photos
description: Bring photos from a folder or card into your LightCraft library, in place or as copies.
order: 10
verified: 0.7.1
---

Use **File** > **Import Photos…** to pick files, or **File** > **Import from Folder…** to scan a whole folder. Both open the **Import Photos** window, where you choose what to bring in and how.

## Steps

1. Open the **File** menu and choose **Import Photos…** or **Import from Folder…**, then pick the files or the folder.
2. Look at the thumbnails. Every photo that can be imported has a checkbox. Click a photo to check or uncheck it. Shift-click sets a range. **Check All** and **Uncheck All** change every photo at once.
3. Photos already in your library are marked **Duplicate** or **In library** and start unchecked. A file LightCraft cannot read is marked **Unreadable**. A photo you deleted that is still in **Recently Deleted** is marked **Will be restored**. **In Recently Deleted** sets what happens to those: **Restore them** (the default) brings the photos back with their edits, ratings and albums, **Import as new** deletes the old record first, and **Leave them** skips them.
4. Under **Transfer**, choose how the files get into the library:
   - **Add in place** keeps the files where they are. The library only points to them. Nothing is copied or moved.
   - **Copy** copies the files into the library's `Originals` folder, or into a folder you pick with **Choose…** next to **Copy to**.
   - **Move** does the same, then removes each original from its source once the copy is verified and catalogued.
5. With **Copy** or **Move**, set **Folders** to sort the copies **By day (YYYY/YYYY-MM-DD)**, **By month (YYYY/YYYY-MM)**, **Into one folder**, or with a **Custom template…**. Use **Rename** to give the files new names, for example `{date}_{seq:3}`. Leave it empty to keep the names. With **Copy**, **Copy as DNG** converts raw files to DNG.
6. Optional: set **Album** to add the photos to an existing album or a new one, **Preset** to apply a preset to every photo, and **Keywords** (separated by commas).
7. Click the button at the bottom. It shows the number of checked photos, for example **Import 24 Photos**. With **Move** it reads **Move 24 Photos**.

The import runs in the background, with a progress row in the activity stack at the top left, and you can keep working while it does. The whole import is one undo step.

## Good to know

- Importing a folder scans it for photos. It does not add the folder to **Local**. To work in a folder without importing, use **Local** > **Browse Folder…**.
- Folder names and `{date}` use the capture time. A photo without one uses today's date.
- Raw formats LightCraft reads include DNG, Canon CR2 and CR3, Sony ARW, Nikon NEF, Fujifilm RAF, Panasonic RW2, Leica RWL, Pentax PEF and Olympus ORF. Some variants open as **Preview only**. See [A raw photo opens as "Preview only"](../troubleshooting/preview-only-raw.md).

## Next

[Rate, flag and label photos](cull-photos.md) to sort out the keepers.
