# README showcase

The README screenshots in `docs/images/classic/` are rendered headless (no window) from the procedural demo
library by [`classic.jsonl`](classic.jsonl), a list of control-protocol requests:

```sh
cargo run --release -p lcc-cli -- snapshot --demo --script docs/showcase/classic.jsonl --size 1600x1000
# compress for the repo (ImageMagick): 1280 px wide JPEGs, well under 300 KB each
magick mogrify -format jpg -resize 1280x -quality 82 docs/images/classic/*.png && rm docs/images/classic/*.png
```

`demo.jsonl` and `pd-edits.jsonl` are the upstream project's scripts for its own (non-Classic) layout; they drive a
running app over the control channel with `run.py`, which sends each JSON line (`{"method":…,"params":…}` or
`{"sleep": seconds}`) to `127.0.0.1:7980`. Their output is not kept in this repository.
