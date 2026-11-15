### Faster photos with several plug-in filters
- Each plug-in filter added to a photo no longer makes it slower than the last: a new filter at the end of the list runs on what the filters before it already made, and the loupe, the preview and the grid share one run of the filters instead of each doing it at once.
- Edits after adding a filter (exposure, white balance, any slider) reuse the filters' result instead of running them again, and a slider drag stops a render that is still filtering instead of queueing behind it.
