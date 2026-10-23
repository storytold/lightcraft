---
title: Remove distractions from a photo
description: Paint over a small distraction, see when the repair takes effect, and adjust or undo it.
order: 35
verified: 0.8.0
---

## Remove a distraction

Paint over the distraction and release the mouse button. LightCraft applies the repair automatically; there is no **Apply** button.

For example, remove a small speck from a plain background:

1. Double-click the photo in the grid to open it in **Detail** view. If it is not in your library yet, [import the photo](import-photos.md) first.
2. Press `H`, choose **Window** > **Remove**, or click the **Remove** tool at the far right.
3. Choose **Remove** at the top of the panel.
4. Adjust **Size** so the brush covers the speck. Set **Opacity** to 100 for a full repair. **Feather** softens the edge.
5. Click once for a round spot, or hold the mouse button and drag to paint over a longer distraction.
6. Release the mouse button. LightCraft adds the repair and updates the photo. The painted area is not waiting for confirmation. Allow the preview to finish updating.
7. Press `E` to open **Edit** and inspect the result without the repair outlines. Return with `H` if you need to [adjust the repair](#adjust-or-undo-a-repair).
8. [Export the photo](export-photos.md) to save an edited copy. For a rendered photo with the repair, choose a format such as **JPEG**.

## Adjust or undo a repair

Each click or completed stroke creates one repair, called a spot in the panel.

1. Open **Remove** and click the repair's pin on the photo to select it. Clicking an existing pin selects that repair instead of adding another one.
2. Change **Size**, **Feather** or **Opacity** to adjust the selected repair. Drag its target pin to move the repaired area.
3. For **Remove**, **Heal** or **Clone**, drag the source outline to a cleaner area if the copied detail looks wrong. Or click **Refresh Source (/)** to ask LightCraft to choose another source. It may report that no other source fits.
4. To replace the painted shape or try another mode, click **Delete (⌫)**, choose the mode, and paint again. The mode buttons choose the mode for new repairs.
5. To undo your last change, choose **Edit** > **Undo**. To delete a particular repair, select its pin and click **Delete (⌫)**.

## Other repair modes

Choose the mode before painting a new repair.

| Mode | What it does in LightCraft 0.8.0 |
|---|---|
| **Remove** | Chooses a nearby source patch and blends its detail with the tone around the repaired area. You can move or refresh the source afterward. |
| **Content-Aware** | Rebuilds the painted area from patches of its surroundings. It does not use one source patch, so moving or refreshing a source does not change the fill. |
| **Heal** | Uses the same blending as **Remove** in this release. It also starts with an automatically chosen source that you can move or refresh. |
| **Clone** | Copies the source patch's colour and detail, with the edge and strength controlled by **Feather** and **Opacity**. It also starts with an automatically chosen source. |

## Common questions

### I painted an area. Why does it look unchanged?

1. Release the mouse button and let the preview update.
2. Check **Opacity**. At 0, a repair has no visible effect. Select the repair and raise it to 100.
3. Check that the brush covers the distraction. Clicking an existing pin only selects a repair.
4. Turn off **Visualize Spots (A)** to judge the normal photo. This black-and-white view helps reveal specks; it does not apply a repair.
5. If **View** > **Show Original** is on, choose it again to return to the edited photo.
6. If the copied patch is unsuitable, adjust its source or delete the repair and paint again.

### Is there an Apply button?

No. A click adds a round repair. A brush stroke is applied when you release the mouse button. Selecting another panel does not discard it.

### Why do I see circles, pins or an arrow?

These mark the repaired area and, for a selected repair that uses one, its source. They let you select and move the repair. Press `E` to inspect the photo without them. They are not part of the exported photo.

### How do I see the original?

1. Choose **View** > **Show Original** to see the original.
2. Choose **View** > **Show Original** again to return to the edited photo.

### Is my original photo overwritten?

No. Repairs are stored as edits in the library. Export an edited copy when you want a file to share; see [Export photos](export-photos.md).
