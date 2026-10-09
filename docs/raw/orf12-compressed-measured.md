# Independently measured compressed 12-bit ORF profile

Evidence and measurement tools are pinned to [corpus/research commit 9d8c927](https://github.com/voshart/Rust-Olympus-RAW-decoder/tree/9d8c927b8f181c6eea4358db30b3a71641922aaf). This source-only review package contains no photographs, sensor dumps or reference binaries.

Task M11.3, 2026-10-08. The frozen E01/V02 hypothesis now reconstructs every
stored sample on seventeen original compressed ORFs from nine camera models:
**434,555,200 samples, zero differences, no excluded sensor margins**. Both
81,036,800-sample E-M5 III high-resolution files are included. This is a measured
specification for the examined profile. Following the [separate review](orf12-review.md),
the product Rust reader in `crates/raw/src/vendor/olympus12.rs` reproduces those
the complete reference arrays. This verifies the measured fixed 12-bit scope.

The [aggregate report](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/compressed-full-frame-summary.json) contains
complete input/output hashes, declared CFA, geometry, per-file report links,
sample counts and final bit positions. Individual prediction reports preserve
every row hash and end bit. The reference is the separately installed binary
rawpy 0.27.1 / LibRaw 0.22.1 / NumPy 2.4.3 instrument. Its decoder source was
not opened; decoded samples are compared before cropping, black correction,
demosaicing or colour conversion.

## Provenance and limits

The first-row grammar and state were published at `15e7a61`. Later-row decisions
were fitted using explicit competing families and checked prospectively, then
captured in a local immutable snapshot before two user-supplied prose explanations
were read. The [checkpoint identities](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/pre-prose-checkpoint.json)
preserve all 96 file hashes. That local snapshot is not an external timestamp.
The later-row work was uncommitted when captured.

The supplied prose explicitly derives some descriptions from existing RawSpeed
and LibRaw implementations. Those source pages were not opened. A documentation
search incidentally exposed a generic libopenraw container-recognition code
snippet; the exposure is recorded, and it supplied no compressed coding rule.
The E01/V02 token, state and predictor rules predate those explanations.
The full-frame extension changed the research reader's storage and bounds, not
E01/V02. Full-raster completion and tail-bit observations were measured after
the prose review using that unchanged model. New source-derived 14-bit metadata
interpretations are not adopted as
independently measured rules. See [provenance](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/orf12-provenance.md).

All numerical comparisons use the same binary reference implementation, so
agreement is finite evidence rather than mathematical proof or a second
independently developed reference codec. The researcher is an AI-assisted author;
no assurance about absence of decoder knowledge in training is asserted.

## Examined container profile

These files use little-endian `IIRO` TIFF framing, one unsigned sensor component,
`BitsPerSample=16`, `Compression=1`, and one compressed strip. The standard TIFF
compression/depth fields alone do not distinguish this stream from packed or
word layouts. All examined raster dimensions are even; this does not establish
that odd dimensions are forbidden by every Olympus format.

The examined compressed strips start with exactly seven bytes:

```text
00 00 00 00 01 00 00
```

The first codeword begins at strip-relative bit 56. Bytes are read high bit first.
These are observed prefix bytes; their parameterized meaning is unresolved.
Do not treat the `.ORF` extension, camera name or raster size as a codec selector.
Packed layouts are documented separately in [orf12-format.md](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/orf12-format.md).

The inspector exposes Olympus `ValidBits=[12,0]` on the creator files and several
controls. It does not expose that observation on TG-7, OM-1 II or OM-3. The initial
tool excluded those three before prediction; its failures and original method
are retained. The corrected research tool permits absent depth observations and
measures the output range. Every unchanged original produced values in 0..4095,
without u16 wrapping. This tool correction is not a production identification
policy or evidence for the generalized 14-bit codec. A subsequent raw-field inspection
supports both OlympusNew and OM SYSTEM notes and observes SHORT[2] `[12,0]` plus
the same coding-field fingerprint on the examined originals. The reviewed product
gate uses that exact fingerprint, not the early inspector's missing observations
or a strip-density heuristic. Both packed controls instead have zero coding fields.
The inspection was motivated by the supplied source-derived prose; its field-role
interpretations are not adopted. See [the review](orf12-review.md) for exact fields
and the [recorded observations](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/bb4f216a13a4701321f202fee0336a56ddae533d/research/results/observed-profile-fields.json).

## Token grammar and adaptive state

Process pixels in row-major order. At the beginning of **every row**, initialize
two independent contexts, one for each column parity:

```text
A = 0  (previous unsigned high part)
N = 0  (consecutive small high-part count)
B = 0  (signed bias)
```

Continue reading at the next bit; there is no observed row alignment padding.
Spatially reconstructed previous rows remain available across that reset.

For the context selected by `x mod 2`, choose remainder width:

```text
if N < 3:  k = max(4, bit_length(A) - 2)
otherwise: k = max(2, bit_length(A))
bit_length(0) = 0
```

Read one token as follows:

| Field | Observed representation |
|---|---|
| Flags `f` | Three bits, high bit first |
| Zero prefix | Count zeros, stopping at the first one or after twelve zeros |
| Ordinary quotient `u` | If fewer than twelve zeros: the zero count, then consume the one terminator |
| Escape quotient `u` | At twelve zeros: read `15-k` bits, then consume one extra bit |
| Remainder `v` | Read `k` bits |

Set `q=(u<<k)+v`. The escape's extra bit is consumed; its value is not required
to be zero or one by this measured description. A discriminating mutation at
that field was invisible to all reference samples. The measured escape grammar
has a fixed twelve-zero limit; its payload width changes with `k`.

```text
h = ~q if (f & 4) != 0 else q
D = h + B
sample = spatial_prediction + 4*D + (f & 3)

B_next = floor((3*D + B) / 32)
A_next = q
N_next = N + 1 if q <= 16 else 0
```

`~q` is the signed integer complement, equivalent to `-q-1`. Signed division
uses floor, including for negative numerators. The flag's high bit applies to
the high part; this is not ordinary signed magnitude of the final sample.
The small-code comparison is inclusive. Context updates occur after the sample.

Research mutation comparisons used modulo 65,536 to match the instrument's u16
representation; mutated above-range values were never asserted to be valid
camera samples. Unmodified originals need no wrapping. Production code should
return an error for an invalid reconstructed range rather than silently wrap.
Bound all field widths, state arithmetic, offsets and allocations. The examined
maximum remainder width is nine; larger theoretical widths are not covered
merely by this finite sample set.

The [first-row account](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/compressed-first-row.md) links the mutation-supported
token boundaries, rejected signed-magnitude alternative, finite bias/width
families, inclusive-count refinement and discriminating escape observation.

## Spatial prediction

Coordinates refer to the full stored raster. The CFA repeats every two columns
and rows; crop offsets are applied after reconstruction.

| Position | Prediction |
|---|---|
| `y<2`, `x<2` | Zero |
| `y<2`, `x>=2` | Sample `(x-2,y)` |
| `y>=2`, `x<2` | Sample `(x,y-2)` |
| `y>=2`, `x>=2` | Conditional prediction below |

For the interior, let `L=S(x-2,y)`, `U=S(x,y-2)`, and `NW=S(x-2,y-2)`:

```text
if (L-NW)*(U-NW) < 0 and max(abs(L-NW), abs(U-NW)) <= 32:
    P = floor((L+U)/2)
else:
    P = median(L, U, L+U-NW)
```

The diagonal-between condition is strict; the threshold comparison is inclusive.
The initial seven reset families, four row alignments and four border seeds were
tested before third-row predictor fitting. An implementation bug made one border
control duplicate zero; its original report/method are retained, and the control
was corrected. The next 128 topology configurations had no exact match. The best
plain median hypothesis still differed at 863 third-row coordinates.

An exploratory trace then inferred 256 required predictor values from measured
tokens and reference neighbours. Nineteen required the average rather than the
median. A recorded metric/threshold family retained only thresholds 30/31/32/33.
Only 32 survived the extended E-M5 III and prospective E-M5 II checks. E-M1 II
rows did not distinguish those four thresholds and are not claimed to do so.

Evidence: [row boundary](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/first-boundary-comparisons.json),
[failed topologies](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/third-row-fisheye-comparisons.json),
[required predictor trace](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/fisheye-required-predictor-trace.json),
[all fitted choices](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/predictor-exploratory-fits.json), and
[prospective predictor comparisons](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/adaptive-predictor-comparisons.json).
The [frozen model](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/frozen-row-candidate.json) records the selected rules.

## Raster completion and unused bits

The current hypothesis reconstructs exactly `width*height` samples from the
bounded strip. All examined complete arrays agree with the reference, including
the last sample. Logical bit position is recorded separately from reservoir
refill bytes; no bits outside the declared strip satisfy a required field.

On these originals, the final token ends in the final declared strip
byte. There are zero to seven unused bits, and every unused bit is one. There
are **no complete unused bytes** in this sample set. This is an observed encoder
convention, not a universal all-one padding requirement or proof about every
Olympus variant. Do not invent an all-zero requirement.

Twenty-four final-byte mutations on E-M5 II, E-M5 III and E-M1 II had ten sample
effects and fourteen no-effect cases, without reference errors. On the two
seven-unused-bit tails, those seven bits could each be flipped without changing
any sensor value. Every affected case first changed the final raster sample.
See [registered influences](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/final-byte-influence-predictions.json),
[E-M5 II](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/termination-em5ii-last-byte.json),
[E-M5 III](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/termination-em5iii-last-byte.json), and
[E-M1 II](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/termination-em1ii-last-byte.json).

The research reservoir was checked against an independent bit-string oracle:
260 variable-width reads, 65,536 two-byte prefixes and 256 short final-byte
prefixes. Its complete final-token fields were replayed on the examined files;
all thirty byte cuts inside those tokens failed on missing required bits. Those
are reader/final-token checks, not a completed product fuzzing or recovery suite.
See [bounds observations](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/full-frame-reader-bounds.json).

These observations support count-directed decoding of this profile, with unused
final-byte bits separated from sample data. They do not independently prove the
absence of every unobserved terminal field. Syntactically decodable corruption
may still produce incorrect samples; successful reconstruction is not an input
integrity checksum. Reject missing required bits without zero filling. Retain
unknown variants as unsupported rather than guessing header or padding rules.

## Complete specimen coverage

All rows below have zero differing samples and equal candidate/reference full
sensor SHA-256 values. Exact identities, visible origins and dimensions, maker
crop observations and method hashes are in the
[aggregate report](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/9d8c927b8f181c6eea4358db30b3a71641922aaf/research/results/compressed-full-frame-summary.json). Active crops and
the reference's visible rectangle can differ; neither excluded stored samples
from the comparisons. In particular, all 10,400 columns of both E-M5 III high
resolution rasters were compared, including the reference's right-edge margins.

| Specimen | Camera/mode | Stored raster | Compared samples | Unused final bits |
|---|---|---|---:|---:|
| P6190137 | E-M5 II | 4640 x 3472 | 16,110,080 | 7 |
| P5230135 | E-M5 II | 4640 x 3472 | 16,110,080 | 1 |
| P5230140 | E-M5 II | 4640 x 3472 | 16,110,080 | 5 |
| P6150304 | E-M5 II | 4640 x 3472 | 16,110,080 | 2 |
| PC201904 | E-M5 II | 4640 x 3472 | 16,110,080 | 3 |
| P5121636 | E-M5 III | 5240 x 3912 | 20,498,880 | 6 |
| P5070002 | E-M5 III | 5240 x 3912 | 20,498,880 | 0 |
| P2153108 | E-M5 III | 5240 x 3912 | 20,498,880 | 7 |
| P5131023 | E-M5 III high resolution | 10400 x 7792 | 81,036,800 | 0 |
| PIXLS 3573 | E-M5 III high resolution | 10400 x 7792 | 81,036,800 | 3 |
| PIXLS 1993 | E-M1 II | 5240 x 3912 | 20,498,880 | 7 |
| PIXLS 2978 | PEN-F | 5200 x 3904 | 20,300,800 | 3 |
| PIXLS 6946 | TG-7 | 4040 x 3016 | 12,184,640 | 3 |
| PIXLS 1787 | E-M10 III | 4640 x 3472 | 16,110,080 | 0 |
| PIXLS 3041 | E-M1X | 5240 x 3912 | 20,498,880 | 1 |
| PIXLS 7262 | OM-1 II ordinary | 5220 x 3912 | 20,420,640 | 1 |
| PIXLS 7796 | OM-3 ordinary | 5220 x 3912 | 20,420,640 | 1 |

## Implementation and remaining work

The separate review preceded the new Rust implementation. The reader uses strict
EOF errors, bounded widths and dimensions, fallible sensor allocation, and sample
range checks before state updates. Independent literal vectors cover signed floor,
the inclusive gradient threshold, strict diagonal ordering, context resets without
row alignment, q=16, escapes and arbitrary unused final bits. Truncations, profile
mutations and arbitrary hostile payloads are checked. Tagged compressed header
probes validate layout/prefix/minimum length without reconstructing sensor pixels.

The [Rust replay report](https://github.com/voshart/Rust-Olympus-RAW-decoder/blob/bb4f216a13a4701321f202fee0336a56ddae533d/research/results/rust-compressed-full-frame.json)
records complete-array hashes, geometry, CFA, crop and header/full equality for all
the examined originals. No photographed media or binary reference is a product dependency.

Unresolved: generalized 14-bit parameters and header meanings; compressed
variants without the observed fingerprint remain unsupported; wider camera
coverage and independent reference implementations. No 14-bit reconstruction
rule is supplied by the newly read prose as measured evidence. Camera colour
calibration and lens correction remain separate from exact raw sample recovery.

## Maintainer review follow-up

The product keeps main's corpus-verified Exif CFA reader (repeat counts in either
byte order), data fallback and their tests. The coding-field gate was retained:
E-1, E-400 and XZ-2 still decode, and the original E-M1 (PIXLS 1051) now decodes
with its declared BGGR layout. Their complete stored sensors, the public
compressed files above, and the E-M5 II packed high-resolution control match
independent binary-reference SHA-256 values in [`docs/orf-corpus.json`](../orf-corpus.json).
The new compatibility reference is rawpy 0.27.1 / LibRaw 0.22.1, before cropping,
black subtraction or colour processing; its source was not inspected.

Reproduce the dedicated corpus CI locally:

```sh
cargo xtask corpus --download --orf-only
LIGHTCRAFT_REQUIRE_ORF_CORPUS=1 cargo test -p lightcraft-raw --locked --test corpus corpus_olympus_sensor_checksums -- --nocapture
```

Downloads and existing files are SHA-256 checked. Required mode fails if a file
is missing; ordinary workspace tests skip absent corpus files. The separate
`Olympus ORF corpus` workflow runs when relevant decoding/corpus files change,
not as a release-packaging step. No photographs or reference binaries are shipped.

Native `LIGHTCRAFT_PROFILE=1` reports ORF sensor decode and header timings in
addition to the pipeline's existing development-stage timings. A leading-zero
reservoir experiment preserved all literal and corpus results but did not improve
measured release times here, so the reviewed bit-at-a-time unary reader is retained.
Format rules and profile selection did not change in response to that experiment.

The [profile timing record](orf-profile-timings.json) preserves input identities,
individual runs and build settings. On this Windows Ryzen 9 7945HX machine,
release sensor-only medians were 379.9 ms for the creator's 20.50 MP E-M5 III
fisheye file and 839.2 ms for PIXLS 3573's 81.04 MP E-M5 III file. Input loading,
demosaic/render and cleanup are excluded. Native optimized-development CLI
rendering of the latter reported 1794.8 ms sensor decoding, then 61.6 ms binned
development at source edge 2560; those app numbers are not release timings.
The E-M1 headless app screenshot has kind=raw and previewOnly=null; CLI renders
of the public 20 MP and 81 MP specimens were visually inspected. Camera colour
still uses the neutral fallback, consistent with the readiness estimate above.
