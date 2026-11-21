# Fill locations from GPS

In **Info**, use **Fill Location** for the selection or **Fill view** for every eligible photo
in the current main view. The view can be the library, an album or a Local folder. Folder
subfolders, search and filters are respected. There is no need to select every photo.

The first lookup needs **Photo → Download Place Names (GeoNames)**. Once downloaded, lookups
run offline. Images and GPS coordinates are not uploaded.

Fill view snapshots the visible photos when clicked, skips photos without valid GPS, and fills
only missing City, State / Province and Country values. Existing text is kept. The shared activity
stack shows progress and Cancel. Cancel before completion leaves metadata unchanged. The completed
batch is one undo step. Changing GPS or editing fields during a lookup cannot apply stale values.

Values are saved in the catalog, including for photos browsed in Local, and survive reopening.
**All Metadata → Catalog · saved values** shows them separately from EXIF and XMP read from disk.
Writing metadata to disk remains controlled by LightCraft’s existing XMP settings / explicit write
commands (**Photo → Save Metadata to File**, or **File → Automatically Write Changes into XMP**). Filling locations does not rewrite original image files.

The GeoNames city list provides a nearby populated place, not an address or administrative boundary
lookup. The nearest city may be approximate, especially in rural or border areas. Precise Location
(street, neighbourhood or landmark) is left blank because this dataset does not provide it.

Automation clients use `metadata.fillLocations` (optional `ids`, otherwise the current view) and
`metadata.pumpLocations`. The desktop advances the job automatically in bounded batches; it is
listed/cancellable through the existing `activity.list` / `activity.cancel` commands.
