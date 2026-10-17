### Recently Deleted and duplicates
- Recently Deleted can be emptied from a button in its view and from its row's right-click menu in the sidebar
  (also Photo → Empty Recently Deleted). Emptying and Delete Permanently ask first (Settings → General → Confirm
  before moving photos to Recently Deleted turns that off) and can be undone; they remove the photos from the
  library only, never the files on disk.
- The import dialog has Lightroom's "Don't Import Suspected Duplicates" (on by default). Untick it to import copies
  of photos the library already has as new photos. Files whose photo is in Recently Deleted are still restored,
  with their edits, rather than skipped. Agents: `library.import {"importDuplicates": true}`.
