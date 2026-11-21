---
title: A raw photo opens as "Preview only"
description: Why a raw photo opens as Preview only, when LightCraft rechecks it, and how to retry manually.
order: 10
verified: 0.21.1
---

After updating LightCraft, open your library with its original files available. LightCraft checks older **Preview only** photos in the background. If it can now decode the raw, the notice goes away.

## Symptoms

- The **Edit** or **Info** panel shows **Preview only** and explains why LightCraft is using the preview.
- You updated LightCraft to a version that supports your camera, and the photo still looks the same.

## Fix

1. Reconnect any drive that holds the originals. If **Checking raw files** is running, let it finish.
2. In the grid, select the photos that still show **Preview only**.
3. Choose **Photo** > **Reload from Disk**. The right-click menu on a photo has the same entry.
4. Open a photo again. If **Preview only** is gone, LightCraft is reading the raw data. Your edits stay with the photo, though it may look different.

If the original is available and **Preview only** stays, your version may still lack support for that raw variant. Update to the [latest release](https://github.com/storytold/lightcraft/releases/latest) and try again, or report the file as described in [Get help with a problem](get-help.md).

## Why this happens

When LightCraft can't decode a raw on import, it can open a usable embedded preview instead. A camera JPEG preview keeps the camera's picture style and white balance. If an original was unavailable during the background check, LightCraft tries again the next time you open the library. **Reload from Disk** checks it immediately after you reconnect it.

## Still stuck?

Ask in [Discord](https://discord.gg/artcraft) or open an issue in the [issue tracker](https://github.com/storytold/lightcraft/issues).
