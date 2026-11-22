### Moving photos to an SMB share (macOS)
- On a Mac, moving photos into a folder on an SMB share kept every original ("could not make the copy's folder
  durable", Operation not supported), and an import that had to make a new folder on a share failed the same way. The
  folder is now flushed with a plain fsync there, as the copied files already are (upstream issue 1407).
