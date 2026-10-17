# Nikon High Efficiency NEF

`crates/raw/src/vendor/nefhe.rs` decodes the verified four-component Nikon HE / HE★ profile
to 14-bit sensor samples. These files use TIFF compression 34713 but start with JPEG XS
SOC/CAP markers and do not contain the older Nikon Huffman linearization table.
The existing NEF path supplies the crop, CFA layout, orientation and metadata. The HE header
supplies the black level; the output white level is 16383. Unrecognized profiles return an
explicit unsupported error and retain the usual embedded-preview fallback.

## Implementation and provenance

- Original Rust container/profile adapter, using the MIT-licensed
  [oxideav-jpegxs 0.0.8](https://github.com/OxideAV/oxideav-jpegxs/tree/v0.0.8)
  entropy, one-dimensional 5/3 wavelet and Star-Tetrix primitives. Default features are off;
  this dependency adds no C/C++, FFI, codec subprocess or runtime library.
- The band/packet layout, half-resolution component grids, slice predictor reset and
  horizontal-before-vertical inverse lifting were established from file headers and numerical
  comparisons. Nikon's CDT adds a decomposition byte per component; component 2 bypasses the
  wavelet. Precincts contain eight packets with long headers even though PIH signals Lh=0.
- Nikon's Star-Tetrix origin differs from the standard primitive's origin. Reversing the
  component planes, reordering Ya/Cb/Cr/Delta and undoing the reversal on the outputs reproduces
  the sensor positions. Integer wavelet pass order matters: vertical-before-horizontal produced
  one-level differences on about 3% of the first Z50 II sample.
- The final integer nonlinear transfer and its three header fields follow the behavioral rule
  published by RdWing in the [September 11, 2026 discussion of LibRaw PR 826](https://github.com/LibRaw/LibRaw/pull/826).
  Its fixed-point gain rounds half up before squaring; the biased input is clamped to 16 bits.
  Other vendor transform fields are checked against the verified profile, not silently ignored.
- Reference sensor arrays came from running
  [redlamp-libraw](https://github.com/pdcgomes/redlamp-libraw) at
  `b9fd8fc63c5e323595c32ae5cefe0098d0c3d178` as a separate black-box executable.
  Its decoder implementation was not read, translated, linked, vendored or copied into the app.
  No Adobe application, SDK, profile, matrix or other application asset was used.

The parser checks marker sizes, declared codestream length, IFD/picture dimensions, slice indices,
weights, complete packet/precinct bounds and the end marker. Image allocation is capped at
64 Mi sensor samples (67,108,864). Unsupported precision, decomposition, column layouts and vendor
parameters fail explicitly. Coefficient and component bounds keep subsequent lifting arithmetic
within its integer range.

## Verification

Every sensor value, including borders, matches the independent reference on:

| Input | Sensor dimensions | Result |
|---|---:|---|
| CC0 raw.pixls.us Z50 II HE, sample 7764 | 5600 × 3728 | all 20,876,800 values exact |
| CC0 raw.pixls.us Z50 II HE★, sample 7763 | 5600 × 3728 | all 20,876,800 values exact |
| Private Z50 II HE★ photograph, kept local | 5600 × 3728 | all 20,876,800 values exact |
| Fenny's public Z6 III HE★ photograph | 6064 × 4040 | all 24,498,560 values exact, also against the photographer's DNG |

The Z6 III photograph and its DNG are from Fenny's
[“Beating Nikon High Efficieny NEFs”](https://discuss.pixls.us/t/beating-nikon-high-efficieny-nefs/59347)
(CC BY-SA 4.0). The photographer made the reference DNG with Adobe DNG Converter 18.
The app decodes that DNG and the NEF to the same complete sample array, SHA256
`eb1546abec8a945d1dc5113083bd8e64d3cdecdcca1e9b476e63ac3aa487c384` over little-endian u16 samples.
Both files remain local comparison inputs; neither is committed or included in the CC0 corpus tests.
Their input identities are:

- [NEF](https://discuss.pixls.us/uploads/short-url/pRJ75daXsaq4sXrfeu1nIvNqZYv.NEF):
  SHA256 `4111fa11f86a4b3f595999ecf035078944a834ef4283df625b2bb0a968e78946`.
- [Reference DNG](https://discuss.pixls.us/uploads/short-url/1HYsrQs0bWnCHt3MSdAyYOEPR3X.dng):
  SHA256 `42786ae1becd9bc9978ff029c53143ad8bc294d6de08cb8f2161e941c6553708`.

`nikon_he_corpus.rs` pins both CC0 input hashes and independently obtained sensor hashes. It skips
missing files, rejects changed fixtures, and tests HE and HE★ separately. To fetch these optional
fixtures into the ignored corpus directory:

```sh
mkdir -p corpus/nikon-he
curl -fL 'https://raw.pixls.us/getfile.php/7764/nice/Nikon%20-%20Z50_2%20-%208bit%20compressed%20%283:2%29.NEF' \
  -o corpus/nikon-he/Z50_2_3x2_HE_71c12d39.NEF
curl -fL 'https://raw.pixls.us/getfile.php/7763/nice/Nikon%20-%20Z50_2%20-%208bit%20compressed%20%283:2%29.NEF' \
  -o corpus/nikon-he/Z50_2_3x2_HEstar_0add1dad.NEF
cargo test -p lcc-raw --test nikon_he_corpus -- --nocapture
```

Synthetic tests run without downloads. They cover raw-GCLI packets, distinct Bayer positions,
partial four-coefficient groups, uneven wavelet splits, multiple slices, nonzero precinct padding,
both TIFF byte orders, native black/white levels, transfer rounding/clipping, every byte truncation,
invalid headers and randomized byte mutations.

The headless app import/Edit check also passes on a scratch copy of the private file: `catalog.query`
reports `previewOnly: null`, the loupe source is `render`, the preview-only notice is absent, and a
+1 EV edit changes the rendered output. No desktop window or GPU was used for this check.

## Remaining coverage

This establishes sensor decoding for the tested profile, not color/render parity with Lightroom.
Other Nikon bodies, crop modes and vendor profile variants still need reference checks. The
decoder currently requires 14-bit precision, even sensor width at least 64, height divisible by four,
one strip/precinct column, the 5-horizontal/1-vertical decomposition and the verified vendor gains.
It does not decode NEV video. These limits are enforced before reconstruction.

The model-wide coverage totals in `raw-parity.md` have not been re-measured for this contribution;
the HE entry remains partial pending wider per-body/profile verification.
