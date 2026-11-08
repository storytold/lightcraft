# Olympus compressed ORF: implementation history

The decoder introduced by #3775 was removed by #3916 pending provenance review.
Its earlier clean-room and derivation claims remain withdrawn; that implementation
is not restored here, and this note makes no legal determination.

This branch proposes the different decoder extracted from #1162. Its recorded
methodology, 23 pinned full-sensor references, reproduction command and limitations
are documented in [ORF decoding](orf.md). Output verification and historical
provenance acceptance are separate questions.

Uncompressed 16-bit, packed 12-bit, two-field 12-bit and 12.8 bpp support remains.
Recognized lossless streams decode from sensor data; unsupported or damaged files
retain embedded-JPEG fallback when a usable preview exists. Existing ORFs retain
the background status recheck and ORF-specific render-cache identity introduced
by #3916. The general smart-preview and virtual-copy corrections are separate PRs.
