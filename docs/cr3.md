# Canon CR3 decoding

The CR3 adapter reads the CRX Bayer track, `CMP1` compression descriptor,
`CDI1`/`IAD1` sensor geometry and timed `CTMD` maker notes. White balance,
black/white levels and the default crop come from the file. A Dual Pixel delta
track is never substituted for the primary Bayer track.

The EOS R6 Mark III writes `ColorData` version 66, which keeps version 64's as-shot white balance (word 105) and
black level (word 383) offsets. Measured on 33 files from 12 shoots (ISO 100–12800, 1/1250 s–25 s): the as-shot
R, G, G, B quadruple is followed by its colour temperature (3271–7424 K across the shoots) and then the fixed
preset table (Daylight 5200 K, Shade 7000 K, Cloudy 6000 K, Tungsten 3200 K, ...), and the black levels equal the
masked-column levels in every file (512 at low ISO, 2047–2048 above). Word 661 holds 14351 or 13995, but it is not
the clipping point: highlights pile up at 16383 and run smoothly through the range above 14351, so version 66
takes its white level from the data.

The in-camera aspect ratio (3:2, 4:3, 16:9, 1:1) is not applied to the sensor data. The maker note's
`AspectInfo` (tag `0x009a`: aspect code, cropped width, height, left, top) gives the shot's rectangle inside the
recommended crop, and the decoder uses it as the default crop, so the developed image has the shape the photographer
framed (PowerShot SX70 HS, EOS 250D, PowerShot G5 X Mark II, EOS M6 Mark II and EOS M50 samples). A rectangle that
is missing or does not fit inside the crop is ignored. The embedded JPEG still shows the whole frame.

The EOS R5 Mark II's APS-C crop-mode files (5376 x 3574) state an active area that starts in dark columns
(132..~256 sit at the black level) and stops ~130 columns short of the image, while their recommended crop
(5088 x 3392, the camera JPEG's size) lies on the image. When the recommended crop doesn't fit inside the stated
active area, it is used as the valid area: on raw.pixls.us 7883/7884 the render matches the camera JPEG
(high-pass correlation 0.99, was -0.13 with a black band on the left). No other of the 110 CC0 CR3s changes.

The decoder is independent Rust code shared by desktop, CLI and WebAssembly;
it has no operating-system imaging dependency or external decoder. Its sources
are the public prose descriptions and patents cited in
`crates/raw/src/vendor/crx.rs`. Unsupported coding variants retain the existing
embedded-JPEG fallback and its visible warning when a JPEG is present. So does
any other CR3 decode error (a corrupt file, or a body whose CRX stream the
decoder misreads): every CR3 opened from its embedded JPEG before this decoder,
and only four bodies are verified, so a CR3 never fails to import or load
where a preview exists. HEVC-only preview tracks are recognized but are not
decoded.

## Verified sensor decoding

CRX version `0x100`, four Bayer planes and `ff01`/`ff02`/`ff03` marker headers
support lossless RAW and three-level C-RAW with horizontal tiles. Uniform
quantization values 4–43 are independently verified; 44 continues the sequence (step 102, checked against a measured step on an EOS R50 C-RAW file). Version `0x200` C-RAW
supports the `ff11`/`ff12`/`ff13` marker family, a single 14-bit Bayer tile and
its adaptive quantization map. C-RAW is quantized, so next to clipped highlights
its reconstruction can overshoot the sensor range by a few quantization steps
(up to 36 codes above 16383 in 17 CC0 files); those samples are clamped to the
CMP1 bit depth. A lossless sample outside the bit depth is still corrupt data.
Other unverified coding variants return explicit unsupported errors. Header probing validates the same coding variants and
tile/plane layout as full decoding.

`crates/raw/tests/cr3_corpus.rs` checks every full-sensor sample against a
black-box reference, before cropping or colour processing. Public CC0 files
are downloaded by `cargo xtask corpus --download` and kept outside version
control. The exact inputs and reference hashes are:

| File | Input SHA-256 | Sensor FNV-1a (u16 little-endian) |
|---|---|---|
| Canon M50 RAW, raw.pixls.us 4657 | `1ba9ad6b51b315b88820eb0fe5fd7f52bc136ba37b1a5ca76825ebfe25b18058` | `62261f0ba81cfcd2` |
| Canon M50 C-RAW, raw.pixls.us 2663 | `15384b775867ec4c42b11882837f1e368cedc0561832ffab271221e6bb80be4c` | `9aacbfe66f505e1b` |
| Canon R100 RAW, raw.pixls.us 7896 | `6d83217d58a5e6d2dabcf470430e91a16676b60531c025b81fa7453fc74b0521` | `cbe5299ab9c52630` |
| Canon R100 C-RAW, raw.pixls.us 7897 | `0b66842b2fe00329ebc05fe8ab9357ddbbe1a0a2d2e69a2893102de7fdf5c942` | `341c706b37c38bbf` |
| Canon R8 RAW, raw.pixls.us 6585 | `7d5c6dbb11ff7e6ee58715d90a20f5d801e8b103f4711ff6ea329ceafcef254c` | `63c1c6d8d1eb2312` |
| Canon R8 C-RAW, raw.pixls.us 6587 | `df33cf394573645ce03dea1b2e9f0b5cc2b7734e9e3391ee7114151414cf2812` | `f836a663a795a375` |

The EOS R5 Mark II files (raw.pixls.us 7881–7884, CC0) are pinned to LightCraft's own decode; no external
decoder was run on them:

| File | Input SHA-256 | Sensor FNV-1a (u16 little-endian) |
|---|---|---|
| Canon R5 Mark II RAW, raw.pixls.us 7881 | `d6e6aa3b493ea8936dff0efb0cc17c87471437e7317bce37c8c82ba08b9cab7b` | `ff716fb30248496f` |
| Canon R5 Mark II C-RAW, raw.pixls.us 7882 | `b3c08a5c7a97299dfd58896a12b939e7b43ec78682761812524825e6e5d087be` | `af32b45357b534e5` |
| Canon R5 Mark II crop-mode RAW, raw.pixls.us 7883 | `9f88c0ff012924193b7f4497559d133dfe626766d600b5d67689156721739ead` | `31cf630afe868958` |
| Canon R5 Mark II crop-mode C-RAW, raw.pixls.us 7884 | `6a597497776a6abbfab8fcf12d0086ac7a3f8afea2879d61accf6bbf48d357d7` | `4721605b20339332` |

Reference pixels were observed from the DNGLab 0.8.0 macOS arm64 release
binary; no decoder source was read or copied, and it is not shipped or linked.
The release ZIP SHA-256 was
`ee70805cb60f18d5ed62548c4e595f6cee0a33407e15358eb1766a46afc1c16e`.
All six sensor comparisons pass. R8 HEVC preview tracks are distinguished from
the primary Bayer RAW; the HEVC preview itself is not decoded. Missing corpus
files are reported as skipped by the tests.

The version `0x200` QP predictor and entropy framing were checked independently.
Controlled input mutations establish floor averaging of paired QP rows before
the nonlinear integer table, then multiplication, division by eight, addition
of the subband base and a minimum step of one. QP values 131–167 are verified;
unmeasured values are rejected.

The QP map is coded as follows (established on the EOS R5 Mark II). Its first symbol is a 32-bit
big-endian literal, mapped like every other symbol and added to a prediction of zero. Every later
symbol is Rice coded with a limited-length escape: a quotient of 23 or more is written as 23 zeros,
the terminating 1 and the symbol in 8 bits. This is the escape layout of ITU-T T.87 for a 32-bit
code-length limit and 8-bit symbols, without that standard's minus-one offset. In the R8 maps only
the first symbol needs more than 15 leading zeros, and its bits read the same whether taken as a
32-bit literal or as an escape after 16 zeros with a 16-bit literal (the rule LightCraft used before).
The R7 maps decoded to their exact ends under that rule, which a later long prefix would have broken;
no R7 file was at hand to recheck them. R5 Mark II maps (QP 150–162) also jump by up to 12 where the Rice parameter is 0. That
gives 18- to 22-zero prefixes, which the 16-zero rule misread as escapes ("Canon CRX adaptive QP value
−21087" and other out-of-range values; the file then opened as a preview). It also gives real escapes,
symbols 23 and 24. Escapes were seen only where the Rice parameter is 0 (in 100 of 612 maps; all 351 inspected, up to 10 per map); at a larger
parameter they would use the same layout, which is unobserved. With this coding the QP maps of all
33,665 R5 Mark II C-RAW files in a private library (version `0x200`, 8480 × 5650) decode to the exact
end of their entropy streams, with values 150–162. So do the reduced-size tracks of 306 of those
files, the two CC0 R5 Mark II C-RAW files above and the R8 C-RAW fixture; all six fixture hashes are
unchanged. Under the 16-zero rule, 174 of those 306 files (57%) failed and opened as previews, all
with out-of-range values (none at QP 168). The two CC0 C-RAW files are low-ISO shots whose maps need no
long prefix: they decoded under the old rule too, so the escape is covered by synthetic unit tests in
`crx.rs` rather than by a public file. Render cache version 43 redraws thumbnails of files whose decode
changed; photos imported as preview-only switch over with Photo ▸ Reload from Disk.

The R5 Mark II sensor has 5650 rows (3574 in crop mode), so its Bayer planes have odd heights (2825,
1787). Lossless planes decode as before. In C-RAW every wavelet level has an odd height (2825 → 1413 →
707), and each subband's entropy stream ends exactly where the subband geometry says. The synthesis
uses the same boundary rules that are verified on the R8's odd lengths. Decoded rows show no step
down to the last sensor row, and the developed images match the camera's embedded JPEGs at 100 %.

An odd QP-map height (EOS R7: 4732 sensor rows give 1183 QP rows) leaves the
last row of the second-level high-frequency bands without a partner row; that
row's QP is used on its own. The odd-height map decodes to the exact end of its
entropy stream. This rule is not yet checked against a black-box reference:
it was checked on paired R7 RAW and C-RAW captures of the same scene (ISO 100
and 6400), whose developed images match visually and whose sensor means agree
within 0.1% at ISO 6400. Files with even-height maps never reach this rule, so
the six verified fixtures are unchanged. Partial-subband and
rounded-plane coding, vertical C-RAW tiling and adaptive low-frequency gains
remain unsupported. The decoder bounds image dimensions, sample count,
tile count, marker headers and temporary allocations. The full sensor buffer is
allocated only after the first plane decodes, and plane buffers grow row by row,
so a small crafted file fails before reserving memory for the image it claims.

## Colour limits

CR3 carries as-shot white balance but no measured camera matrix is imported
here. The existing guarded embedded-JPEG fitting path now includes CR3, with
both proxies oriented identically before gathering training pairs. A bounded
crop/translation registration of the JPEG is confirmed on held-out edge
directions before fitting colour; the existing held-out colour acceptance gates
are unchanged. No lens distortion correction is applied (LightCraft has no lens
profiles of its own yet), so lenses whose camera JPEG is strongly
distortion-corrected may fail the gates and keep the neutral matrix. Render
cache version 13 invalidates earlier thumbnails. All developed pixels still come
from the RAW sensor; a rejected colour fit retains the documented neutral
matrix fallback. See `camera-preview-colour.md` for the limits of this estimate.
