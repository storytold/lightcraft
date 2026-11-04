---
title: Move over from Lightroom
description: Import a Lightroom Classic catalog into LightCraft, and learn what comes across and what does not.
order: 50
verified: 0.7.1
---

Choose **File** > **Import Lightroom Catalog…** and pick your `.lrcat` file. LightCraft reads it directly. You do not need Lightroom installed.

## Steps

1. Open the library you want the photos in. To start a new one, use **File** > **New Library…**.
2. Choose **File** > **Import Lightroom Catalog…**.
3. Pick your Lightroom Classic catalog, the file that ends in `.lrcat`. You can also give **File** > **Open Library…** a Lightroom catalog folder. LightCraft then offers this import instead of making a library there.
4. Wait for the import to finish. It runs in the background, with a progress row in the activity stack at the top left. A message reports how many photos were added.

If you change your mind, one **Edit** > **Undo** reverses the changes to the catalog.

## What comes across

- Ratings, picks and rejects, and colour labels
- Keywords, including hierarchical ones, and XMP metadata
- Collections and collection sets, which become albums
- Virtual copies
- Develop settings that LightCraft can map

Original files stay where they are, and LightCraft refers to them in place. Photos whose originals are missing stay in the library so you can relink them later. LightCraft never changes your Lightroom catalog or your originals.

## What does not match

- Develop settings are an approximation. Camera profiles, Adobe's AI models and some masking and retouch fields are not reproduced, and settings LightCraft cannot map are reported.
- Smart collections become regular albums that hold the photos they contain at the time of import.
- Lightroom's process version is not carried over. Imported photos use LightCraft's newest rendering.

## Good to know

- Before it changes anything, LightCraft tries to save your source settings, history and collections in a recovery archive in the library's `Interop` folder. This is best effort. If it fails, the photos are still imported with a warning.
- Importing the same catalog again does not duplicate virtual copies or collections. By default, edits you already made in LightCraft are kept.
- A big catalog can take a long time. In one report on an earlier version, 38,000 photos took about 25 minutes from a USB SSD.
- To stop the import, click ✕ on its row in the activity stack. This works while LightCraft reads the catalog. Once it starts adding the photos, it finishes.
- The browser version cannot import a catalog.
