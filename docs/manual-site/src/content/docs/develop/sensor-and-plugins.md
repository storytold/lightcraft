---
title: Sensor and plug-ins
description: Hot pixels, dust and plug-in filters.
sidebar:
  order: 18
---

- **Hot, dead and stuck pixels** are fixed automatically on raw data, on by default; **Detail ▸ Sensor ▸ Fix hot and dead pixels** in the Edit panel turns it off. Each camera body keeps its own map; confirming a hot pixel rebuilds only that body's previews. **Photo ▸ Develop Settings ▸ Rebuild Hot Pixel Map**.
- **Sensor dust**: **Remove Dust** in the Remove panel (or **Photo ▸ Develop Settings ▸ Remove Dust**) adds heal spots for dust shadows. **Settings ▸ Import ▸ Remove dust on import** does it automatically (off by default).
- **Film dust**: **Infrared Dust Removal** at the top of the Remove panel fills in the dust, hairs and scratches a film scanner's infrared pass shows (an RGBI TIFF opens as an opaque picture with its pass kept beside it), with a view of the pass and of what it fills (shown only for a scan that has the pass).
- **Flat-Field Correction** follows a steep light fall-off to the frame's edges, so corners of wide lenses are no longer left too dark.
- **Plug-in Filters** section at the bottom of the Edit panel: pixel filters written as sandboxed WebAssembly modules. Install and manage them in the Plug-in Manager (**Pixel Filters** list; their page shows where each comes from). An older pixel filter needs confirming before it installs; a plug-in and a pixel filter cannot share an id. A filter can mark settings to remember (a negative converter's film type, say): each new copy you add starts with what you last set them to, on any photo, even after a restart.
- **Plug-ins** also add menu commands, export steps and metadata. See [Automation](../../automate/cli-and-mcp/).
