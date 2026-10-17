### Folders and metadata badges on a network share
- The Folders panel lists the folders on disk without photos yet, and the grid checks each photo's XMP sidecar for
  its metadata badge, on threads of their own now. On a network share, listing a large folder tree can take many
  seconds once the system's cache of it has expired (22 s for 344 folders on SMB after two idle minutes), and the
  panel did it every time the catalog changed, so an edit could freeze the window. The panel now shows what it
  has at once and adds the new folders when their listing comes in (issue #123).
