### Browser version: tested against damaged storage

- A damaged or partly written library in browser storage, or a corrupt stored photo, now has automated tests proving the browser version opens it or reports a clear error instead of crashing.
- A nightly check loads the browser version in a headless browser and fails if it does not start cleanly.
