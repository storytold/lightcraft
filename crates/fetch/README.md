# dac-fetch

Native model downloads in pure Rust (L0, Apache-2.0): HTTP/1.1 over std sockets and rustls with the RustCrypto provider, without ring, aws-lc, C or assembly build dependencies. The wasm32 crate is empty.

Call `download` with bounded file specifications and ordered mirrors, or `download_url` for an exact catalog URL (including its query). Each file has a local filename independent of the URL, a size cap and optional pinned size/SHA-256. Transfers resume pinned `.part` files, verify before renaming, report progress and check cancellation during reads. Run on a worker thread. No proxy support; DNS resolution uses the platform resolver. Tokens are off by default and never forwarded to a different redirect host.

Tests use local synthetic HTTP servers for redirects, resume, truncation, cancellation, stalls, hashes, size limits and hostile specifications; they make no internet requests.
