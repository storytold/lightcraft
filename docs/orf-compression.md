# Olympus compressed ORF: implementation history

The decoder introduced by #3775 was removed by #3916 pending provenance review.
Its earlier clean-room and derivation claims remain withdrawn; that implementation
is not restored here, and this note makes no legal determination.

This branch proposes the different decoder extracted from #1162. Its recorded
methodology, 23 pinned full-sensor references, reproduction command and limitations
are documented in [ORF decoding](orf.md). Output verification and historical
provenance acceptance are separate questions.

Uncompressed 16-bit, packed 12-bit, two-field 12-bit and 12.8 bpp support remains.
Recognized lossless streams decode from sensor data; files rejected by the decoder
retain embedded-JPEG fallback when a usable preview exists. This is not guaranteed
for every damaged file: a damaged compressed header or invalid compressed codes can
send a strip with at least two bytes per pixel through Word16 decoding, producing
distorted sensor data instead of reaching the JPEG fallback. Complete valid compressed
streams retain priority over Word16, including padded streams. See [ORF limits](orf.md#colour-and-limits).
Existing ORFs retain
the background status recheck and ORF-specific render-cache identity introduced
by #3916. Sensor ORFs also use a separate smart-preview identity, and Reload/recheck fill
missing source metadata on same-file virtual copies. Broader reload-family and proxy
invalidation changes remain separate PRs.
