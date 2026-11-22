### Faster import of High Efficiency NEFs

- Importing Nikon High Efficiency (HE / HE★) NEFs no longer decodes each whole image just to read its size: the import reads only the file's headers, as it does for other raws. On a Z 9 archive this was about 90% of the import's CPU time.
