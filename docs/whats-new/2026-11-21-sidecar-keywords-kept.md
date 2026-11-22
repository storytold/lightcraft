### Save Metadata to File keeps a sidecar's own keywords

- When a photo's XMP sidecar holds keywords the library doesn't have, and the library has never been in step with that sidecar (it appeared after import, or its keywords weren't imported), saving adds those keywords to the photo and writes them back instead of erasing them (issue #1310). Automatic XMP writes do the same.
