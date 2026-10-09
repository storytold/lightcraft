# Review of the measured compressed ORF profile

M11.3, 2026-10-08. Reviewed before product implementation in response to the
user's instruction to proceed. This is a separate review of the measured
specification and arithmetic; it is not a claim of independent third-party
approval or certainty about an AI model's training. The rejected decoder and
upstream compressed-decoder source pages were not opened. No web visits were made.

## Reviewed coding scope

The E01/V02 token, parity state, row reset and spatial predictor remain exactly
as described in [the measured specification](orf12-compressed-measured.md).
All examined complete arrays match the binary reference. The production reader
must preserve every stored sample, reject missing required bits, and reject
reconstructed values outside 0..4095 rather than applying the measuring
instrument's u16 wrap. The escape's extra bit is consumed without interpreting
its value. No compressed test encoder from rejected PR #240 is used.

The small-code counter is only tested against three. Saturating it at three is
therefore equivalent to the measured unbounded increment; a large code resets it
to zero. Signed bias division must use floor (`div_euclid(32)` for this positive
divisor), not truncation towards zero. The diagonal-between condition is strict;
the gradient threshold is inclusive at 32.

The spatial predictor stays within the range of previously validated samples:
the median is a clamp between left and above, and their average also lies there.
For a valid 12-bit result, `D=(sample-P-low_flags)/4` lies in -1024..1023. Starting
at zero, the update `floor((3*D+B)/32)` preserves `B` in -100..99. Consequently
`h=D-B` lies in -1123..1123 and unsigned `q` is at most 1123. Warm widths are at
most nine; a stable context's preceding small q is at most sixteen, so its width
is at most five. A nine-bit maximum remainder is thus supported both by the
measurements and the reviewed arithmetic for valid reconstructed samples.

With that width cap, an escape's combined quotient/remainder is at most 32767.
Invalid fields can therefore be rejected by the sample-range check before their
state becomes a predictor. Use checked arithmetic for input lengths and offsets,
fallible reserved allocation, and explicit dimension/sample caps.

## Production format identification

A further raw-container inspection found the same SHORT-valued fingerprint on
the examined compressed originals, including the OM SYSTEM maker notes missed
by the early Python inspector. LightCraft's existing metadata parser already
recognizes `OM SYSTEM\0` at its observed sixteen-byte IFD header offset.
The two verified padded E-M5 II files instead have zero values in these fields.

| Tag | Required observed value |
|---|---|
| 0x0611 | SHORT[2]: 12, 0 |
| 0x0640..0x0649 | SHORT[1] each: 0, 5, 0, 2, 5, 4, 3, 2, 3, 4 |
| 0x064a..0x064f | Absent |
| 0x0650..0x0653 | SHORT[1] each: 7, 15, 12, 5 |

This is an empirically observed profile fingerprint. The new inspection was
motivated by source-derived prose, and must be disclosed as such. It does not
establish the meaning of each field or authorize implementing generalized
metadata-driven decoding. Exact matches select the already measured fixed rules;
unfamiliar nonzero coding profiles remain unsupported. Absent/zero coding fields
retain the existing uncompressed/packed routes. No camera-model whitelist or
camera coefficient table is introduced.

The compressed route also requires little-endian TIFF, a single unsigned sensor
component, declared 16-bit storage, a full-height single strip, a valid per-file
Bayer tag, and even dimensions in the verified research budgets: at most 16,384
columns, 20,000 rows and 96 million samples. Require the seven-byte observed
prefix and enough declared input bits for the minimum coding length before
allocating sensor storage. These caps are safety/coverage policy, not universal
Olympus format limits.

Minimum payload bits are `6*width*height + 2*min(width,6)*height`: each token
needs three flags, at least one prefix bit and at least two remainder bits;
the first three tokens of each parity in a row need at least four remainder bits.
Add the 56 prefix bits. This provides a cheap dimension/truncation check for
header probes. Tagged compressed probes must not allocate or decode sensor data.

Decode exactly the declared sample count. Accept up to seven leftover bits without
requiring their values to be zero or one. An entire unused byte falls outside the
observed profile and returns unsupported. This stricter coverage policy is stated
explicitly instead of inventing a universal trailer rule. All examined originals
satisfy it. Missing required bits are corrupt input, not an unsupported marker.

## Required implementation verification

- Numerical Rust/candidate/reference equality for the complete examined rasters,
  preserving sensor dimensions, CFA, crop and every margin.
- Small independent fixtures for signed floor bias, threshold 32, strict diagonal
  ordering, row resets and inclusive q=16; no sole reliance on encoder round trips.
- Truncation, changed profiles/depth, invalid sample range, excessive dimensions
  and arbitrary hostile strip bytes return errors without panics.
- Header/full description equality and a probe that succeeds without interpreting
  an invalid body, proving the header path avoids sensor decoding.
- Full project CI, native and headless application import/render checks.

Implementation is approved for this measured fixed 12-bit scope in this session.
Generalized 14-bit compression, colour calibration, lens corrections and broader
unverified coding fingerprints remain separate work.
