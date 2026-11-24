### Smoother start with very large libraries

- Background preview preparation no longer stalls the interface for seconds right after a library of hundreds of thousands of photos opens: it looks at the library a stretch at a time and prepares only the photos around the one you are on.
- The lens database is read in the background after a library opens, shown as a row in the activity stack, so the first photo that needs a lens profile doesn't wait for it.
