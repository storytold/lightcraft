### Remove Location is stricter for Original exports
- Exporting an Original with "Remove location" now also checks every XMP packet in a JPEG, altitude and place-name properties, and refuses a PNG or WebP whose chunks are cut short, instead of copying a location by mistake.
